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

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// All MUD observability counters. Gauges and monotonic counters share the
/// struct; `render_prometheus` labels each with the correct `# TYPE`.
pub struct Metrics {
    /// Current number of in-game (Playing) players. Gauge.
    pub players: AtomicU64,
    /// Total TCP connections accepted since boot. Monotonic counter.
    pub connections_total: AtomicU64,
    /// Total player commands dispatched since boot. Monotonic counter.
    pub commands_total: AtomicU64,
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
    /// Process start, for uptime. Not exported directly; see `uptime_seconds`.
    start_instant: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Metrics {
            players: AtomicU64::new(0),
            connections_total: AtomicU64::new(0),
            commands_total: AtomicU64::new(0),
            last_tick_micros: AtomicU64::new(0),
            max_tick_micros: AtomicU64::new(0),
            mobs: AtomicU64::new(0),
            objs: AtomicU64::new(0),
            pulse: AtomicU64::new(0),
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
    pub fn set_pulse(&self, p: u64) {
        self.pulse.store(p, Ordering::Relaxed);
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
        let last_tick = self.last_tick_micros.load(Ordering::Relaxed);
        let max_tick = self.max_tick_micros.load(Ordering::Relaxed);
        let mobs = self.mobs.load(Ordering::Relaxed);
        let objs = self.objs.load(Ordering::Relaxed);
        let pulse = self.pulse.load(Ordering::Relaxed);
        let uptime = self.uptime_seconds();

        let mut s = String::with_capacity(1024);

        s.push_str("# HELP deltamud_players Current number of in-game players.\n");
        s.push_str("# TYPE deltamud_players gauge\n");
        s.push_str(&format!("deltamud_players {}\n", players));

        s.push_str("# HELP deltamud_connections_total Total TCP connections accepted since boot.\n");
        s.push_str("# TYPE deltamud_connections_total counter\n");
        s.push_str(&format!("deltamud_connections_total {}\n", connections));

        s.push_str("# HELP deltamud_commands_total Total player commands dispatched since boot.\n");
        s.push_str("# TYPE deltamud_commands_total counter\n");
        s.push_str(&format!("deltamud_commands_total {}\n", commands));

        s.push_str("# HELP deltamud_heartbeat_tick_micros Duration of the most recent heartbeat pulse in microseconds.\n");
        s.push_str("# TYPE deltamud_heartbeat_tick_micros gauge\n");
        s.push_str(&format!("deltamud_heartbeat_tick_micros {}\n", last_tick));

        s.push_str("# HELP deltamud_heartbeat_tick_micros_max High-water mark of any single heartbeat pulse in microseconds.\n");
        s.push_str("# TYPE deltamud_heartbeat_tick_micros_max gauge\n");
        s.push_str(&format!("deltamud_heartbeat_tick_micros_max {}\n", max_tick));

        s.push_str("# HELP deltamud_mobs Current number of mobiles (NPCs) in the world.\n");
        s.push_str("# TYPE deltamud_mobs gauge\n");
        s.push_str(&format!("deltamud_mobs {}\n", mobs));

        s.push_str("# HELP deltamud_objs Current number of objects in the world.\n");
        s.push_str("# TYPE deltamud_objs gauge\n");
        s.push_str(&format!("deltamud_objs {}\n", objs));

        s.push_str("# HELP deltamud_pulse Heartbeat pulse counter since boot.\n");
        s.push_str("# TYPE deltamud_pulse counter\n");
        s.push_str(&format!("deltamud_pulse {}\n", pulse));

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
