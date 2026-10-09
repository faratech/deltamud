//! Failed-login throttling that survives reconnects (issue #426).
//!
//! C (and the port) drop a connection after `MAX_BAD_PWS` wrong passwords, but
//! that counter lives on the descriptor, so a reconnect starts again at zero.
//! This tracker remembers recent failures per account and per source address
//! and, once a free allowance is spent, locks further attempts for an
//! exponentially growing period. It is consulted before any password hash or
//! database work. State is process memory only; a restart clears it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Wrong passwords per account (across connections) before lockouts begin.
pub const ACCOUNT_FREE_FAILURES: u32 = 5;
/// Default wrong passwords per source address before lockouts begin
/// (`MUD_LOGIN_IP_FAILURES`; 0 disables the per-source throttle).
pub const DEFAULT_SOURCE_FREE_FAILURES: u32 = 20;
/// The first lockout; every further failure doubles it, up to the cap.
pub const BASE_LOCKOUT: Duration = Duration::from_secs(30);
pub const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);
/// A key with no failure for this long starts over.
pub const FAILURE_MEMORY: Duration = Duration::from_secs(60 * 60);
/// Past this many remembered keys, expired entries are pruned.
const MAX_TRACKED_KEYS: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct FailureWindow {
    failures: u32,
    last_failure: Instant,
    locked_until: Option<Instant>,
}

impl FailureWindow {
    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.locked_until
            .filter(|until| now < *until)
            .map(|until| until - now)
    }

    fn expired(&self, now: Instant) -> bool {
        self.remaining(now).is_none()
            && now.saturating_duration_since(self.last_failure) >= FAILURE_MEMORY
    }
}

#[derive(Debug, Default)]
pub struct LoginThrottle {
    accounts: HashMap<String, FailureWindow>,
    sources: HashMap<String, FailureWindow>,
}

impl LoginThrottle {
    /// Remaining lockout for this account or source, if either is locked.
    pub fn lockout(&self, account: &str, source: &str, now: Instant) -> Option<Duration> {
        let account = self
            .accounts
            .get(&account.to_lowercase())
            .and_then(|window| window.remaining(now));
        let source = self
            .sources
            .get(source)
            .and_then(|window| window.remaining(now));
        account.max(source)
    }

    /// Count one wrong password. Returns the lockout this failure started, if
    /// any, so the caller can log it.
    pub fn record_failure(
        &mut self,
        account: &str,
        source: &str,
        source_free_failures: u32,
        now: Instant,
    ) -> Option<Duration> {
        let account_lock = bump(
            &mut self.accounts,
            account.to_lowercase(),
            ACCOUNT_FREE_FAILURES,
            now,
        );
        let source_lock = bump(
            &mut self.sources,
            source.to_string(),
            source_free_failures,
            now,
        );
        account_lock.max(source_lock)
    }

    /// A verified password clears the account's history. The source history
    /// is kept, so logging into an owned account cannot reset a spray budget.
    pub fn record_success(&mut self, account: &str) {
        self.accounts.remove(&account.to_lowercase());
    }
}

fn bump(
    windows: &mut HashMap<String, FailureWindow>,
    key: String,
    free_failures: u32,
    now: Instant,
) -> Option<Duration> {
    if free_failures == 0 || key.is_empty() {
        return None;
    }
    if windows.len() >= MAX_TRACKED_KEYS && !windows.contains_key(&key) {
        windows.retain(|_, window| !window.expired(now));
    }
    let window = windows.entry(key).or_insert(FailureWindow {
        failures: 0,
        last_failure: now,
        locked_until: None,
    });
    if window.expired(now) {
        window.failures = 0;
        window.locked_until = None;
    }
    window.failures = window.failures.saturating_add(1);
    window.last_failure = now;
    if window.failures < free_failures {
        return None;
    }
    let doublings = (window.failures - free_failures).min(16);
    let lock = BASE_LOCKOUT
        .saturating_mul(1u32 << doublings)
        .min(MAX_LOCKOUT);
    window.locked_until = Some(now + lock);
    Some(lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_lockout_starts_at_the_allowance_and_doubles_to_the_cap() {
        let mut throttle = LoginThrottle::default();
        let mut now = Instant::now();
        for _ in 1..ACCOUNT_FREE_FAILURES {
            assert_eq!(throttle.record_failure("Victim", "192.0.2.1", 0, now), None);
            assert_eq!(throttle.lockout("victim", "192.0.2.1", now), None);
        }
        assert_eq!(
            throttle.record_failure("Victim", "192.0.2.2", 0, now),
            Some(BASE_LOCKOUT)
        );
        // Locked for every source and any name casing.
        assert_eq!(
            throttle.lockout("VICTIM", "198.51.100.7", now),
            Some(BASE_LOCKOUT)
        );
        assert_eq!(throttle.lockout("other", "198.51.100.7", now), None);

        now += BASE_LOCKOUT;
        assert_eq!(throttle.lockout("victim", "192.0.2.1", now), None);
        assert_eq!(
            throttle.record_failure("victim", "192.0.2.1", 0, now),
            Some(BASE_LOCKOUT * 2)
        );
        for _ in 0..20 {
            now += MAX_LOCKOUT;
            throttle.record_failure("victim", "192.0.2.1", 0, now);
        }
        assert_eq!(
            throttle.lockout("victim", "192.0.2.1", now),
            Some(MAX_LOCKOUT)
        );
    }

    #[test]
    fn success_and_quiet_periods_reset_the_account_but_not_the_source() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..ACCOUNT_FREE_FAILURES - 1 {
            throttle.record_failure("Owner", "203.0.113.5", 3, now);
        }
        // The source spent its allowance of 3 on the way.
        assert!(throttle.lockout("someone", "203.0.113.5", now).is_some());
        throttle.record_success("owner");
        assert_eq!(throttle.lockout("owner", "192.0.2.9", now), None);
        assert!(throttle.lockout("owner", "203.0.113.5", now).is_some());

        // An hour without failures forgets the account entirely.
        let later = now + FAILURE_MEMORY;
        for _ in 1..ACCOUNT_FREE_FAILURES {
            throttle.record_failure("Quiet", "192.0.2.10", 0, now);
        }
        assert_eq!(
            throttle.record_failure("Quiet", "192.0.2.10", 0, later),
            None
        );
    }

    #[test]
    fn source_throttle_can_be_disabled() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for index in 0..100 {
            throttle.record_failure(&format!("name{index}"), "127.0.0.1", 0, now);
        }
        assert_eq!(throttle.lockout("fresh", "127.0.0.1", now), None);
    }
}
