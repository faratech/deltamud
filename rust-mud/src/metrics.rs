// metrics.rs — lock-free observability counters for the Rust DeltaMUD.
//
// A single `Metrics` struct of atomics, shared behind an `Arc` between the Game
// task (which updates them on the heartbeat hot path) and a tiny raw-TCP HTTP
// task (which reads them to serve `/metrics` and `/health`). Everything is an
// atomic with Relaxed ordering: these are monitoring counters, not
// synchronization primitives, so we never want a lock anywhere near the pulse.
//
// The HTTP exposition is hand-rolled Prometheus text format (no prometheus
// crate, no hyper/axum) to keep the dependency surface at zero new crates.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// All MUD observability counters. Gauges and monotonic counters share the
/// struct; `render_prometheus` labels each with the correct `# TYPE`.
pub struct Metrics {
    /// True after database, world, and player-index boot have completed.
    boot_complete: AtomicBool,
    /// Current number of in-game (Playing) players. Gauge.
    pub players: AtomicU64,
    /// Total TCP connections accepted since boot. Monotonic counter.
    pub connections_total: AtomicU64,
    /// Total player commands dispatched since boot. Monotonic counter.
    pub commands_total: AtomicU64,
    /// Metrics connections rejected because the independent scrape limit was full.
    pub metrics_rejected_total: AtomicU64,
    /// Metrics exchanges terminated by an I/O or whole-request deadline.
    pub metrics_timeouts_total: AtomicU64,
    /// Descriptor text batches truncated by the pending/rendered byte ceiling.
    pub output_overflows_total: AtomicU64,
    /// Game clients closed after their bounded writer channel stopped accepting data.
    pub output_closed_clients_total: AtomicU64,
    /// Wall-clock of the most recent heartbeat pulse, microseconds. Gauge.
    pub last_tick_micros: AtomicU64,
    /// High-water mark of any single pulse, microseconds. Gauge (max-so-far).
    pub max_tick_micros: AtomicU64,
    /// Current mob count (non-player characters in the world). Gauge.
    pub mobs: AtomicU64,
    /// Current object count in the world. Gauge.
    pub objs: AtomicU64,
    /// Heartbeat pulse counter (mirrors GameState.pulse). Monotonic counter.
    pub pulse: AtomicU64,
    /// Milliseconds since process start when the latest pulse was published,
    /// stored as elapsed+1 so zero remains the unambiguous "never pulsed" value.
    last_pulse_elapsed_ms: AtomicU64,
    /// Process start, for uptime. Not exported directly; see `uptime_seconds`.
    start_instant: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Metrics {
            boot_complete: AtomicBool::new(false),
            players: AtomicU64::new(0),
            connections_total: AtomicU64::new(0),
            commands_total: AtomicU64::new(0),
            metrics_rejected_total: AtomicU64::new(0),
            metrics_timeouts_total: AtomicU64::new(0),
            output_overflows_total: AtomicU64::new(0),
            output_closed_clients_total: AtomicU64::new(0),
            last_tick_micros: AtomicU64::new(0),
            max_tick_micros: AtomicU64::new(0),
            mobs: AtomicU64::new(0),
            objs: AtomicU64::new(0),
            pulse: AtomicU64::new(0),
            last_pulse_elapsed_ms: AtomicU64::new(0),
            start_instant: Instant::now(),
        }
    }

    // ---- setters / incrementers (all Relaxed — pure counters) -----------

    #[inline]
    pub fn set_players(&self, n: u64) {
        self.players.store(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_connections(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_commands(&self) {
        self.commands_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_metrics_rejected(&self) {
        self.metrics_rejected_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_metrics_timeout(&self) {
        self.metrics_timeouts_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_output_overflow(&self) {
        self.output_overflows_total.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc_output_closed_client(&self) {
        self.output_closed_clients_total
            .fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn set_pulse(&self, p: u64) {
        self.pulse.store(p, Ordering::Relaxed);
        let elapsed = self
            .start_instant
            .elapsed()
            .as_millis()
            .min((u64::MAX - 1) as u128) as u64;
        self.last_pulse_elapsed_ms
            .store(elapsed.saturating_add(1), Ordering::Relaxed);
    }

    /// Mark the immutable boot prerequisites complete. Readiness additionally
    /// requires at least one recent heartbeat, so setting this before the Game
    /// task begins cannot produce a false-positive ready response.
    pub fn mark_boot_complete(&self) {
        self.boot_complete.store(true, Ordering::Release);
    }

    /// Revoke readiness before a fatal process-level invariant failure begins
    /// shutdown. This closes the brief window in which the heartbeat is still
    /// recent even though the game task has already been aborted.
    pub fn mark_not_ready(&self) {
        self.boot_complete.store(false, Ordering::Release);
    }

    /// Return the latest heartbeat age when the process is ready. A stale or
    /// never-started heartbeat is not ready even though the HTTP task is alive.
    pub fn readiness(&self, max_pulse_age: Duration) -> Result<Duration, &'static str> {
        if !self.boot_complete.load(Ordering::Acquire) {
            return Err("boot incomplete");
        }
        let encoded = self.last_pulse_elapsed_ms.load(Ordering::Relaxed);
        if encoded == 0 || self.pulse.load(Ordering::Relaxed) == 0 {
            return Err("heartbeat not started");
        }
        let last_ms = encoded - 1;
        let now_ms = self
            .start_instant
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let age = Duration::from_millis(now_ms.saturating_sub(last_ms));
        if age > max_pulse_age {
            return Err("heartbeat stale");
        }
        Ok(age)
    }

    #[inline]
    pub fn set_mobs(&self, n: u64) {
        self.mobs.store(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn set_objs(&self, n: u64) {
        self.objs.store(n, Ordering::Relaxed);
    }

    /// Record one pulse's duration: store it as the last tick, and bump the
    /// high-water mark if it is a new max. Lock-free; the max update is a plain
    /// load+store (single-writer — only the Game task calls this).
    #[inline]
    pub fn record_tick_micros(&self, micros: u64) {
        self.last_tick_micros.store(micros, Ordering::Relaxed);
        if micros > self.max_tick_micros.load(Ordering::Relaxed) {
            self.max_tick_micros.store(micros, Ordering::Relaxed);
        }
    }

    /// Seconds since process start.
    #[inline]
    pub fn uptime_seconds(&self) -> u64 {
        self.start_instant.elapsed().as_secs()
    }

    /// Current online player count (for the `/health` body).
    #[inline]
    pub fn players_now(&self) -> u64 {
        self.players.load(Ordering::Relaxed)
    }

    /// Prometheus text-format exposition of every counter. One `# HELP` /
    /// `# TYPE` pair per metric, then the value line. Plain ASCII, no labels.
    pub fn render_prometheus(&self) -> String {
        let players = self.players.load(Ordering::Relaxed);
        let connections = self.connections_total.load(Ordering::Relaxed);
        let commands = self.commands_total.load(Ordering::Relaxed);
        let metrics_rejected = self.metrics_rejected_total.load(Ordering::Relaxed);
        let metrics_timeouts = self.metrics_timeouts_total.load(Ordering::Relaxed);
        let output_overflows = self.output_overflows_total.load(Ordering::Relaxed);
        let output_closed_clients = self.output_closed_clients_total.load(Ordering::Relaxed);
        let last_tick = self.last_tick_micros.load(Ordering::Relaxed);
        let max_tick = self.max_tick_micros.load(Ordering::Relaxed);
        let mobs = self.mobs.load(Ordering::Relaxed);
        let objs = self.objs.load(Ordering::Relaxed);
        let pulse = self.pulse.load(Ordering::Relaxed);
        let ready = u8::from(self.readiness(Duration::from_secs(2)).is_ok());
        let heartbeat_age_ms = self
            .readiness(Duration::from_secs(u64::MAX))
            .map(|age| age.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or(u64::MAX);
        let uptime = self.uptime_seconds();

        let mut s = String::with_capacity(1024);

        s.push_str("# HELP deltamud_players Current number of in-game players.\n");
        s.push_str("# TYPE deltamud_players gauge\n");
        s.push_str(&format!("deltamud_players {}\n", players));

        s.push_str(
            "# HELP deltamud_connections_total Total TCP connections accepted since boot.\n",
        );
        s.push_str("# TYPE deltamud_connections_total counter\n");
        s.push_str(&format!("deltamud_connections_total {}\n", connections));

        s.push_str("# HELP deltamud_commands_total Total player commands dispatched since boot.\n");
        s.push_str("# TYPE deltamud_commands_total counter\n");
        s.push_str(&format!("deltamud_commands_total {}\n", commands));

        s.push_str("# HELP deltamud_metrics_rejected_total Metrics connections rejected at the scrape concurrency limit.\n");
        s.push_str("# TYPE deltamud_metrics_rejected_total counter\n");
        s.push_str(&format!(
            "deltamud_metrics_rejected_total {}\n",
            metrics_rejected
        ));

        s.push_str("# HELP deltamud_metrics_timeouts_total Metrics exchanges closed by an I/O or request deadline.\n");
        s.push_str("# TYPE deltamud_metrics_timeouts_total counter\n");
        s.push_str(&format!(
            "deltamud_metrics_timeouts_total {}\n",
            metrics_timeouts
        ));

        s.push_str("# HELP deltamud_output_overflows_total Descriptor output batches truncated at the byte ceiling.\n");
        s.push_str("# TYPE deltamud_output_overflows_total counter\n");
        s.push_str(&format!(
            "deltamud_output_overflows_total {}\n",
            output_overflows
        ));

        s.push_str("# HELP deltamud_output_closed_clients_total Clients closed after writer-channel backpressure or failure.\n");
        s.push_str("# TYPE deltamud_output_closed_clients_total counter\n");
        s.push_str(&format!(
            "deltamud_output_closed_clients_total {}\n",
            output_closed_clients
        ));

        s.push_str("# HELP deltamud_heartbeat_tick_micros Duration of the most recent heartbeat pulse in microseconds.\n");
        s.push_str("# TYPE deltamud_heartbeat_tick_micros gauge\n");
        s.push_str(&format!("deltamud_heartbeat_tick_micros {}\n", last_tick));

        s.push_str("# HELP deltamud_heartbeat_tick_micros_max High-water mark of any single heartbeat pulse in microseconds.\n");
        s.push_str("# TYPE deltamud_heartbeat_tick_micros_max gauge\n");
        s.push_str(&format!(
            "deltamud_heartbeat_tick_micros_max {}\n",
            max_tick
        ));

        s.push_str("# HELP deltamud_mobs Current number of mobiles (NPCs) in the world.\n");
        s.push_str("# TYPE deltamud_mobs gauge\n");
        s.push_str(&format!("deltamud_mobs {}\n", mobs));

        s.push_str("# HELP deltamud_objs Current number of objects in the world.\n");
        s.push_str("# TYPE deltamud_objs gauge\n");
        s.push_str(&format!("deltamud_objs {}\n", objs));

        s.push_str("# HELP deltamud_pulse Heartbeat pulse counter since boot.\n");
        s.push_str("# TYPE deltamud_pulse counter\n");
        s.push_str(&format!("deltamud_pulse {}\n", pulse));

        s.push_str("# HELP deltamud_ready Whether boot completed and the heartbeat is recent.\n");
        s.push_str("# TYPE deltamud_ready gauge\n");
        s.push_str(&format!("deltamud_ready {}\n", ready));

        s.push_str("# HELP deltamud_heartbeat_age_milliseconds Age of the most recently published heartbeat; u64 max means unavailable.\n");
        s.push_str("# TYPE deltamud_heartbeat_age_milliseconds gauge\n");
        s.push_str(&format!(
            "deltamud_heartbeat_age_milliseconds {}\n",
            heartbeat_age_ms
        ));

        s.push_str("# HELP deltamud_uptime_seconds Seconds since process start.\n");
        s.push_str("# TYPE deltamud_uptime_seconds counter\n");
        s.push_str(&format!("deltamud_uptime_seconds {}\n", uptime));

        s
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_boot_and_a_recent_pulse() {
        let metrics = Metrics::new();
        assert_eq!(
            metrics.readiness(Duration::from_secs(2)),
            Err("boot incomplete")
        );
        metrics.mark_boot_complete();
        assert_eq!(
            metrics.readiness(Duration::from_secs(2)),
            Err("heartbeat not started")
        );
        metrics.set_pulse(1);
        assert!(metrics.readiness(Duration::from_secs(2)).is_ok());
        metrics.mark_not_ready();
        assert_eq!(
            metrics.readiness(Duration::from_secs(2)),
            Err("boot incomplete")
        );
    }
}
