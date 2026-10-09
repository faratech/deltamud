//! Failed-login throttling that survives reconnects (issue #426).
//!
//! C (and the port) drop a connection after `MAX_BAD_PWS` wrong passwords, but
//! that counter lives on the descriptor, so a reconnect starts again at zero.
//! This tracker remembers recent failures and locks further attempts for an
//! exponentially growing period. It is consulted before any password hash or
//! database work. State is process memory only; a restart or copyover clears it.
//!
//! Three decaying failure counters, each a leaky bucket (a failure is
//! forgiven every `*_DECAY`, so a slow trickle never accumulates):
//!
//! * per (account, source address): [`PAIR_FREE_FAILURES`] wrong passwords for
//!   one account from one address lock that pair. A stranger guessing at a
//!   name only ever locks themselves out; the owner connecting from another
//!   address is unaffected.
//! * per source address: `MUD_LOGIN_IP_FAILURES` wrong passwords from one
//!   address across all accounts (password spraying) lock that address. A
//!   successful login forgives the failures that were made against the
//!   account that just logged in, so ordinary typos on a shared address do
//!   not pile up. Failures against other accounts stay, otherwise anyone able
//!   to register a character could reset a spray budget at will.
//! * per account, all sources: [`ACCOUNT_CEILING`] wrong passwords in total
//!   lock the account for every source that has not recently logged into it
//!   successfully. This bounds guessing spread over many addresses without
//!   letting a stranger lock the owner out of their usual addresses.
//!
//! Source addresses are canonicalised: IPv4-mapped IPv6 becomes IPv4 and any
//! other IPv6 address is reduced to its /64, so rotating through a /64 does
//! not defeat the per-address counters.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Wrong passwords for one account from one source before that pair locks.
pub const PAIR_FREE_FAILURES: u32 = 5;
/// Wrong passwords for one account across all sources before the account locks
/// for sources without a recent successful login.
pub const ACCOUNT_CEILING: u32 = 50;
/// Default wrong passwords per source address before lockouts begin
/// (`MUD_LOGIN_IP_FAILURES`; 0 disables the per-source throttle).
pub const DEFAULT_SOURCE_FREE_FAILURES: u32 = 20;
/// The first lockout; every failure past the allowance doubles it.
pub const BASE_LOCKOUT: Duration = Duration::from_secs(30);
pub const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);
/// One failure is forgiven per interval, per counter.
pub const PAIR_DECAY: Duration = Duration::from_secs(10 * 60);
pub const SOURCE_DECAY: Duration = Duration::from_secs(60);
pub const ACCOUNT_DECAY: Duration = Duration::from_secs(5 * 60);
/// A source that logged into an account this recently is exempt from that
/// account's ceiling (not from the per-pair or per-source locks).
pub const RECENT_SUCCESS_WINDOW: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// Per-table size limit. Settled entries are pruned first; if every entry is
/// still active the least significant one is evicted, so the tracker cannot be
/// used to exhaust memory.
const MAX_TRACKED_KEYS: usize = 4096;

/// Which counter started a lockout (for the operator log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockScope {
    /// Too many wrong passwords for this account from this address.
    AccountFromSource,
    /// Too many wrong passwords from this address across accounts.
    Source,
    /// Too many wrong passwords for this account across all addresses.
    Account,
}

impl LockScope {
    pub fn describe(self) -> &'static str {
        match self {
            LockScope::AccountFromSource => "this account from this address",
            LockScope::Source => "this address",
            LockScope::Account => "this account from unfamiliar addresses",
        }
    }
}

/// A lockout that a failure just started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedLockout {
    pub scope: LockScope,
    pub duration: Duration,
}

/// A decaying failure count plus the end of the current lockout, if any.
#[derive(Debug, Clone, Copy)]
struct Entry {
    count: u32,
    /// Start of the interval that the next forgiven failure is measured from.
    stamp: Instant,
    locked_until: Option<Instant>,
}

impl Entry {
    fn new(now: Instant) -> Self {
        Entry {
            count: 0,
            stamp: now,
            locked_until: None,
        }
    }

    /// Forgive one failure per elapsed `interval`, keeping the remainder of a
    /// partly elapsed interval.
    fn decay(&mut self, now: Instant, interval: Duration) {
        if self.count == 0 || interval.is_zero() {
            self.stamp = now;
            return;
        }
        let elapsed = now.saturating_duration_since(self.stamp);
        let steps = u32::try_from(elapsed.as_nanos() / interval.as_nanos()).unwrap_or(u32::MAX);
        if steps >= self.count {
            self.count = 0;
            self.stamp = now;
        } else if steps > 0 {
            self.count -= steps;
            // steps * interval <= elapsed, so this cannot pass `now`.
            self.stamp += interval * steps;
        }
    }

    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.locked_until
            .filter(|until| now < *until)
            .map(|until| until - now)
    }

    /// Nothing left to remember: no failures after decay and no live lockout.
    fn settled(&self, now: Instant, interval: Duration) -> bool {
        let mut copy = *self;
        copy.decay(now, interval);
        copy.count == 0 && copy.remaining(now).is_none()
    }
}

/// Count one failure under `key`. Returns the lockout this failure started.
fn bump<K: Hash + Eq + Clone>(
    table: &mut HashMap<K, Entry>,
    key: K,
    free_failures: u32,
    interval: Duration,
    now: Instant,
) -> Option<Duration> {
    if free_failures == 0 {
        return None;
    }
    if table.len() >= MAX_TRACKED_KEYS && !table.contains_key(&key) {
        make_room(table, now, interval);
    }
    let entry = table.entry(key).or_insert_with(|| Entry::new(now));
    entry.decay(now, interval);
    entry.count = entry.count.saturating_add(1);
    if entry.count < free_failures {
        return None;
    }
    let doublings = (entry.count - free_failures).min(16);
    let lock = BASE_LOCKOUT
        .saturating_mul(1u32 << doublings)
        .min(MAX_LOCKOUT);
    // Never shorten a lockout that is already running.
    let until = now + lock;
    entry.locked_until = Some(entry.locked_until.map_or(until, |old| old.max(until)));
    Some(lock)
}

/// Free at least one slot: drop settled entries, and if every entry is still
/// active evict the unlocked one with the fewest remembered failures (or, when
/// all are locked, the one whose lockout ends first).
fn make_room<K: Hash + Eq + Clone>(
    table: &mut HashMap<K, Entry>,
    now: Instant,
    interval: Duration,
) {
    table.retain(|_, entry| !entry.settled(now, interval));
    if table.len() < MAX_TRACKED_KEYS {
        return;
    }
    let victim = table
        .iter()
        .min_by_key(|(_, entry)| {
            (
                entry.remaining(now).is_some(),
                entry.count,
                entry.locked_until,
            )
        })
        .map(|(key, _)| key.clone());
    if let Some(victim) = victim {
        table.remove(&victim);
    }
}

/// Canonical throttle key for a source address string. Hostnames (descriptors
/// without a parsed peer address) are kept as is.
pub fn normalize_source(source: &str) -> String {
    match source.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.to_string(),
        Ok(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let segments = v6.segments();
                format!(
                    "{:x}:{:x}:{:x}:{:x}::/64",
                    segments[0], segments[1], segments[2], segments[3]
                )
            }
        },
        Err(_) => source.to_ascii_lowercase(),
    }
}

#[derive(Debug, Default)]
pub struct LoginThrottle {
    pairs: HashMap<(String, String), Entry>,
    sources: HashMap<String, Entry>,
    accounts: HashMap<String, Entry>,
    /// (account, source) -> time of the last verified login.
    trusted: HashMap<(String, String), Instant>,
}

impl LoginThrottle {
    /// Remaining lockout that applies to this attempt, if any.
    pub fn lockout(&self, account: &str, source: &str, now: Instant) -> Option<Duration> {
        let account = account.to_lowercase();
        let source = normalize_source(source);
        let pair = self
            .pairs
            .get(&(account.clone(), source.clone()))
            .and_then(|entry| entry.remaining(now));
        let by_source = self
            .sources
            .get(&source)
            .and_then(|entry| entry.remaining(now));
        let by_account = if self.is_trusted(&account, &source, now) {
            None
        } else {
            self.accounts
                .get(&account)
                .and_then(|entry| entry.remaining(now))
        };
        pair.max(by_source).max(by_account)
    }

    fn is_trusted(&self, account: &str, source: &str, now: Instant) -> bool {
        self.trusted
            .get(&(account.to_string(), source.to_string()))
            .is_some_and(|at| now.saturating_duration_since(*at) < RECENT_SUCCESS_WINDOW)
    }

    /// Count one wrong password. Returns the longest lockout this failure
    /// started, if any, so the caller can log it.
    pub fn record_failure(
        &mut self,
        account: &str,
        source: &str,
        source_free_failures: u32,
        now: Instant,
    ) -> Option<StartedLockout> {
        let account = account.to_lowercase();
        let source = normalize_source(source);
        if account.is_empty() || source.is_empty() {
            return None;
        }
        let started = [
            (
                LockScope::AccountFromSource,
                bump(
                    &mut self.pairs,
                    (account.clone(), source.clone()),
                    PAIR_FREE_FAILURES,
                    PAIR_DECAY,
                    now,
                ),
            ),
            (
                LockScope::Source,
                bump(
                    &mut self.sources,
                    source,
                    source_free_failures,
                    SOURCE_DECAY,
                    now,
                ),
            ),
            (
                LockScope::Account,
                bump(
                    &mut self.accounts,
                    account,
                    ACCOUNT_CEILING,
                    ACCOUNT_DECAY,
                    now,
                ),
            ),
        ];
        started
            .into_iter()
            .filter_map(|(scope, lock)| lock.map(|duration| StartedLockout { scope, duration }))
            .max_by_key(|started| started.duration)
    }

    /// A verified password. The failures made against this account from this
    /// source are forgiven (including from the source's own counter) and the
    /// source is remembered as familiar for the account-wide ceiling. The
    /// source's failures against other accounts, and the account's failures
    /// from other sources, are kept.
    pub fn record_success(&mut self, account: &str, source: &str, now: Instant) {
        let account = account.to_lowercase();
        let source = normalize_source(source);
        if account.is_empty() || source.is_empty() {
            return;
        }
        if let Some(mut pair) = self.pairs.remove(&(account.clone(), source.clone())) {
            pair.decay(now, PAIR_DECAY);
            let settled = self.sources.get_mut(&source).map(|entry| {
                entry.decay(now, SOURCE_DECAY);
                entry.count = entry.count.saturating_sub(pair.count);
                entry.count == 0 && entry.remaining(now).is_none()
            });
            if settled == Some(true) {
                self.sources.remove(&source);
            }
        }
        if self.trusted.len() >= MAX_TRACKED_KEYS
            && !self
                .trusted
                .contains_key(&(account.clone(), source.clone()))
        {
            self.trusted
                .retain(|_, at| now.saturating_duration_since(*at) < RECENT_SUCCESS_WINDOW);
            if self.trusted.len() >= MAX_TRACKED_KEYS {
                let oldest = self
                    .trusted
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    self.trusted.remove(&oldest);
                }
            }
        }
        self.trusted.insert((account, source), now);
    }

    #[cfg(test)]
    fn table_sizes(&self) -> (usize, usize, usize, usize) {
        (
            self.pairs.len(),
            self.sources.len(),
            self.accounts.len(),
            self.trusted.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "192.0.2.1";
    const B: &str = "198.51.100.7";

    fn fail(throttle: &mut LoginThrottle, account: &str, source: &str, now: Instant) {
        throttle.record_failure(account, source, DEFAULT_SOURCE_FREE_FAILURES, now);
    }

    #[test]
    fn pair_lockout_starts_at_the_allowance_and_doubles_to_the_cap() {
        let mut throttle = LoginThrottle::default();
        let mut now = Instant::now();
        for _ in 1..PAIR_FREE_FAILURES {
            assert_eq!(throttle.record_failure("Victim", A, 0, now), None);
            assert_eq!(throttle.lockout("victim", A, now), None);
        }
        assert_eq!(
            throttle.record_failure("Victim", A, 0, now),
            Some(StartedLockout {
                scope: LockScope::AccountFromSource,
                duration: BASE_LOCKOUT
            })
        );
        // Any casing of the name, same address.
        assert_eq!(throttle.lockout("VICTIM", A, now), Some(BASE_LOCKOUT));

        now += BASE_LOCKOUT;
        assert_eq!(throttle.lockout("victim", A, now), None);
        // Each further failure doubles the lockout, up to the cap.
        let mut expected = BASE_LOCKOUT;
        for _ in 0..8 {
            expected = (expected * 2).min(MAX_LOCKOUT);
            assert_eq!(
                throttle
                    .record_failure("victim", A, 0, now)
                    .map(|started| started.duration),
                Some(expected)
            );
        }
        assert_eq!(expected, MAX_LOCKOUT);
        assert_eq!(throttle.lockout("victim", A, now), Some(MAX_LOCKOUT));
    }

    #[test]
    fn a_stranger_locks_only_their_own_address_not_the_owner() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..PAIR_FREE_FAILURES {
            throttle.record_failure("Owner", A, 0, now);
        }
        assert!(throttle.lockout("owner", A, now).is_some());
        // The owner from another address, and the stranger against other names.
        assert_eq!(throttle.lockout("owner", B, now), None);
        assert_eq!(throttle.lockout("someoneelse", A, now), None);
    }

    #[test]
    fn counts_decay_instead_of_waiting_for_an_hour_of_quiet() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for _ in 1..PAIR_FREE_FAILURES {
            throttle.record_failure("Slow", A, 0, now);
        }
        // Two failures are forgiven after two decay intervals, so three more
        // are needed (not one) to reach the allowance again.
        let later = now + PAIR_DECAY * 2;
        assert_eq!(throttle.record_failure("Slow", A, 0, later), None);
        assert_eq!(throttle.record_failure("Slow", A, 0, later), None);
        assert!(throttle.record_failure("Slow", A, 0, later).is_some());

        // A trickle of one failure per 15 minutes, forever, never locks: the
        // old fixed "forget after an hour without failures" window never reset
        // while failures kept arriving that often.
        let mut now = later + MAX_LOCKOUT * 2;
        for _ in 0..200 {
            now += Duration::from_secs(15 * 60);
            assert_eq!(throttle.record_failure("Trickle", A, 20, now), None);
            assert_eq!(throttle.lockout("trickle", A, now), None);
        }
    }

    #[test]
    fn shared_source_does_not_reach_its_limit_from_a_slow_stream_of_typos() {
        // Everyone behind one address (a proxy that hides client IPs): 100
        // players each mistype once, one every 90 seconds, then log in.
        let mut throttle = LoginThrottle::default();
        let mut now = Instant::now();
        for index in 0..100 {
            now += Duration::from_secs(90);
            let account = format!("player{index}");
            throttle.record_failure(&account, A, DEFAULT_SOURCE_FREE_FAILURES, now);
            assert_eq!(throttle.lockout(&account, A, now), None);
            throttle.record_success(&account, A, now);
        }
        assert_eq!(throttle.lockout("player0", A, now), None);
        assert_eq!(throttle.lockout("brandnew", A, now), None);
    }

    #[test]
    fn source_counter_locks_spraying_and_decays() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for index in 0..19 {
            let started = throttle.record_failure(&format!("victim{index}"), A, 20, now);
            assert_eq!(started, None);
        }
        let started = throttle.record_failure("victim19", A, 20, now).unwrap();
        assert_eq!(started.scope, LockScope::Source);
        assert_eq!(started.duration, BASE_LOCKOUT);
        // Locked for every account from that address only.
        assert!(throttle.lockout("anyone", A, now).is_some());
        assert_eq!(throttle.lockout("anyone", B, now), None);
        // After the lock ends and some failures decay, it takes more than one
        // failure to lock again.
        let later = now + BASE_LOCKOUT + SOURCE_DECAY * 5;
        assert_eq!(throttle.lockout("anyone", A, later), None);
        assert_eq!(throttle.record_failure("fresh1", A, 20, later), None);
    }

    #[test]
    fn success_forgives_that_accounts_failures_from_the_source_counter() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        // Limit 5 so the arithmetic is easy to follow.
        for _ in 0..3 {
            throttle.record_failure("Owner", A, 5, now);
        }
        throttle.record_success("owner", A, now);
        // The owner's 3 failures are gone: 4 more from the source stay below 5.
        for index in 0..4 {
            assert_eq!(
                throttle.record_failure(&format!("other{index}"), A, 5, now),
                None
            );
        }
        assert_eq!(throttle.lockout("fresh", A, now), None);
    }

    #[test]
    fn success_does_not_reset_a_spray_budget_made_against_other_accounts() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        // An attacker with a real character of their own sprays other names...
        for index in 0..4 {
            throttle.record_failure(&format!("victim{index}"), A, 5, now);
        }
        // ...then logs into their own character to try to wipe the slate.
        throttle.record_success("mine", A, now);
        assert!(
            throttle
                .record_failure("victim4", A, 5, now)
                .is_some_and(|started| started.scope == LockScope::Source)
        );
        assert!(throttle.lockout("victim5", A, now).is_some());
    }

    #[test]
    fn success_unlocks_nothing_and_clears_only_the_pair() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..3 {
            throttle.record_failure("Owner", A, 0, now);
            throttle.record_failure("Owner", B, 0, now);
        }
        throttle.record_success("owner", A, now);
        // A's pair is clean again; B's three failures remain.
        throttle.record_failure("Owner", A, 0, now);
        throttle.record_failure("Owner", A, 0, now);
        assert_eq!(throttle.lockout("owner", A, now), None);
        throttle.record_failure("Owner", B, 0, now);
        assert_eq!(throttle.lockout("owner", B, now), None);
        assert!(throttle.record_failure("Owner", B, 0, now).is_some());
        assert!(throttle.lockout("owner", B, now).is_some());
    }

    #[test]
    fn one_sources_success_does_not_clear_another_sources_lockout_or_counter() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        // B (an attacker) fails 4 times against Owner; A (Owner's own address)
        // fails twice and then logs in.
        for _ in 0..4 {
            throttle.record_failure("Owner", B, 6, now);
        }
        for _ in 0..2 {
            throttle.record_failure("Owner", A, 6, now);
        }
        throttle.record_success("owner", A, now);
        assert_eq!(throttle.lockout("owner", A, now), None);

        // B's pair count is intact: its fifth failure against Owner locks the
        // (Owner, B) pair, which A's success did not reset.
        let started = throttle.record_failure("Owner", B, 6, now).unwrap();
        assert_eq!(started.scope, LockScope::AccountFromSource);
        assert!(throttle.lockout("owner", B, now).is_some());
        // B's source counter is intact too (5 so far): the sixth failure from
        // B, against another account, trips the per-source limit of 6.
        let started = throttle.record_failure("Other", B, 6, now).unwrap();
        assert_eq!(started.scope, LockScope::Source);
        // Throughout, A stayed unlocked.
        assert_eq!(throttle.lockout("owner", A, now), None);
    }

    #[test]
    fn an_attacker_does_not_lock_the_victim_out_until_the_account_ceiling() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        let home = "203.0.113.200";
        let newcomer = "203.0.113.201";
        throttle.record_success("Victim", home, now);

        // One attacker address hammers the account far past the pair limit.
        for _ in 0..PAIR_FREE_FAILURES + 10 {
            throttle.record_failure("Victim", B, 0, now);
        }
        assert_eq!(throttle.lockout("victim", B, now), Some(MAX_LOCKOUT));
        // The victim's own address, and an address they have never used, are
        // not affected: the lock is on (account, attacker address) only.
        assert_eq!(throttle.lockout("victim", home, now), None);
        assert_eq!(throttle.lockout("victim", newcomer, now), None);

        // Spread the guessing over many addresses until the ceiling trips.
        for index in 0..ACCOUNT_CEILING {
            let source = format!("203.0.113.{}", index + 1);
            throttle.record_failure("Victim", &source, 0, now);
        }
        // Unfamiliar addresses are now refused; the address with a recent
        // successful login is exempt; the exemption is per account.
        assert!(throttle.lockout("victim", newcomer, now).is_some());
        assert_eq!(throttle.lockout("victim", home, now), None);
        assert_eq!(throttle.lockout("someoneelse", newcomer, now), None);
    }

    #[test]
    fn entry_decay_forgives_one_failure_per_interval_and_keeps_the_remainder() {
        let t0 = Instant::now();
        let minute = Duration::from_secs(60);
        let mut entry = Entry {
            count: 5,
            stamp: t0,
            locked_until: None,
        };
        entry.decay(t0 + Duration::from_secs(90), minute);
        assert_eq!(entry.count, 4);
        assert_eq!(entry.stamp, t0 + minute);
        // 59 s past the last forgiveness is not yet a full interval.
        entry.decay(t0 + Duration::from_secs(119), minute);
        assert_eq!(entry.count, 4);
        entry.decay(t0 + Duration::from_secs(120), minute);
        assert_eq!(entry.count, 3);
        assert_eq!(entry.stamp, t0 + minute * 2);
        // Plenty of time forgives everything.
        entry.decay(t0 + Duration::from_secs(10 * 3600), minute);
        assert_eq!(entry.count, 0);
        assert!(entry.settled(t0 + Duration::from_secs(10 * 3600), minute));
    }

    #[test]
    fn account_ceiling_locks_unfamiliar_sources_but_not_recent_successful_ones() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        throttle.record_success("Owner", A, now);
        // Distributed guessing: one failure each from many addresses never
        // trips a per-pair or per-source lock, only the account ceiling.
        let mut started = None;
        for index in 0..ACCOUNT_CEILING {
            let source = format!("203.0.{}.{}", index / 200, index % 200 + 1);
            started = throttle.record_failure("Owner", &source, DEFAULT_SOURCE_FREE_FAILURES, now);
            if index + 1 < ACCOUNT_CEILING {
                assert_eq!(started, None);
            }
        }
        assert_eq!(
            started.map(|started| started.scope),
            Some(LockScope::Account)
        );
        // A new address and any other address are refused for the account...
        assert_eq!(throttle.lockout("owner", B, now), Some(BASE_LOCKOUT));
        assert_eq!(
            throttle.lockout("OWNER", "2001:db8::1", now),
            Some(BASE_LOCKOUT)
        );
        // ...other accounts are untouched...
        assert_eq!(throttle.lockout("other", B, now), None);
        // ...and the owner's familiar address still gets in.
        assert_eq!(throttle.lockout("owner", A, now), None);

        // The exemption lapses with the recent-success window.
        let much_later = now + RECENT_SUCCESS_WINDOW;
        for index in 0..ACCOUNT_CEILING {
            let source = format!("203.0.1.{}", index + 1);
            throttle.record_failure("Owner", &source, 0, much_later);
        }
        assert!(throttle.lockout("owner", A, much_later).is_some());
    }

    #[test]
    fn familiar_source_is_still_bound_by_the_pair_lock() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        throttle.record_success("Owner", A, now);
        for _ in 0..PAIR_FREE_FAILURES {
            throttle.record_failure("Owner", A, 0, now);
        }
        assert!(throttle.lockout("owner", A, now).is_some());
    }

    #[test]
    fn a_running_lockout_is_never_shortened() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        // 25 failures with an allowance of 20: the lockout is at the cap.
        for _ in 0..25 {
            throttle.record_failure("Spray", A, 20, now);
        }
        assert_eq!(throttle.lockout("anyone", A, now), Some(MAX_LOCKOUT));
        // Three minutes later three failures have decayed, so one more failure
        // computes a 4x lockout (240 s), shorter than the 12 minutes left.
        let later = now + SOURCE_DECAY * 3;
        throttle.record_failure("Spray", A, 20, later);
        assert_eq!(
            throttle.lockout("anyone", A, later),
            Some(MAX_LOCKOUT - SOURCE_DECAY * 3)
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

    #[test]
    fn ipv6_sources_share_a_slash_64_and_mapped_ipv4_is_ipv4() {
        assert_eq!(
            normalize_source("2001:db8:1:2:aaaa:bbbb:cccc:dddd"),
            normalize_source("2001:db8:1:2::1")
        );
        assert_ne!(
            normalize_source("2001:db8:1:2::1"),
            normalize_source("2001:db8:1:3::1")
        );
        assert_eq!(normalize_source("::ffff:192.0.2.1"), "192.0.2.1");
        assert_eq!(normalize_source("Example.TEST"), "example.test");

        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for index in 0..PAIR_FREE_FAILURES {
            throttle.record_failure("Victim", &format!("2001:db8:1:2:{index}::9"), 0, now);
        }
        assert!(
            throttle
                .lockout("victim", "2001:db8:1:2:ffff::1", now)
                .is_some()
        );
        assert_eq!(throttle.lockout("victim", "2001:db8:1:3::1", now), None);
    }

    #[test]
    fn tables_stay_bounded_when_every_entry_is_active() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for index in 0..MAX_TRACKED_KEYS + 500 {
            let source = format!(
                "10.{}.{}.{}",
                index / 65536,
                (index / 256) % 256,
                index % 256
            );
            let account = format!("account{index}");
            throttle.record_failure(&account, &source, 20, now);
            throttle.record_success(&account, &source, now);
            throttle.record_failure(&account, &source, 20, now);
        }
        let (pairs, sources, accounts, trusted) = throttle.table_sizes();
        assert!(pairs <= MAX_TRACKED_KEYS, "pairs {pairs}");
        assert!(sources <= MAX_TRACKED_KEYS, "sources {sources}");
        assert!(accounts <= MAX_TRACKED_KEYS, "accounts {accounts}");
        assert!(trusted <= MAX_TRACKED_KEYS, "trusted {trusted}");
    }

    #[test]
    fn locked_entries_survive_eviction_pressure() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..PAIR_FREE_FAILURES {
            throttle.record_failure("Victim", A, 0, now);
        }
        for index in 0..MAX_TRACKED_KEYS + 500 {
            let source = format!(
                "10.{}.{}.{}",
                index / 65536,
                (index / 256) % 256,
                index % 256
            );
            fail(&mut throttle, "Noise", &source, now);
        }
        assert!(throttle.lockout("victim", A, now).is_some());
    }
}
