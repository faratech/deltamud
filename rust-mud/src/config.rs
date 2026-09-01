use std::env;

use crate::types::RoomVnum;

// ===========================================================================
// World room-number constants (config.c)
// ===========================================================================

/// Number of distinct hometowns / start towns (structs.h `NUM_STARTROOMS`).
pub const NUM_STARTROOMS: usize = 3;

/// Per-hometown mortal start rooms (config.c `mortal_start_room[NUM_STARTROOMS + 1]`).
/// Index 0 is the newbie loadroom element; indices 1..=NUM_STARTROOMS are the
/// start towns. C's comment labels the four slots "Newbie loadroom / Itrius /
/// Start Town 2 / Start Town 3"; the finish-the-game program built those
/// towns (zones 2 and 3), so each slot now resolves to its own town
/// (registered divergence: C shipped all four = 100).
pub const MORTAL_START_ROOM: [RoomVnum; NUM_STARTROOMS + 1] = [
    200, // Newbie loadroom element -> the Newbie School (zone 2)
    100, // Itrius
    210, // Start Town 2 (zone 2 town center)
    300, // Start Town 3 (zone 3 town center)
];

/// vnum of room that immortals enter at by default (config.c `immort_start_room`).
pub const IMMORT_START_ROOM: RoomVnum = 1204;

/// vnum of room that frozen players enter at (config.c `frozen_start_room`).
pub const FROZEN_START_ROOM: RoomVnum = 1202;

/// C config.c: donation_room_2/3 shipped as NOWHERE ("room for expansion");
/// the finish-the-game program built the towns that use them.
pub const DONATION_ROOM_2: RoomVnum = 211; // Newhaven (zone 2)
pub const DONATION_ROOM_3: RoomVnum = 301; // Locris Ferry (zone 3)

#[derive(Clone)]
pub struct Config {
    /// C config.c:59 `jail_num = 400`.
    pub jail_num: RoomVnum,
    /// C config.c:77 `newbie_room` (finish-the-game: the school was built in
    /// zone 2; C's placeholder value pointed at unrelated forest rooms).
    pub newbie_room: RoomVnum,
    pub database_url: String,
    /// Maximum wall time for one database operation. This bounds pool waits,
    /// connects, reads, writes, and queries at the application boundary.
    pub db_timeout_secs: u64,
    /// Resolve accepted peer addresses to hostnames at the async socket edge.
    /// Hostnames are used only after forward-confirming that they resolve back
    /// to the canonical peer IP; IP ban checks always remain authoritative.
    pub reverse_dns: bool,
    /// Whole reverse+forward-confirmation budget for one accepted connection,
    /// clamped to 1..=10,000 ms.
    pub reverse_dns_timeout_ms: u64,
    /// Maximum number of simultaneous blocking system-resolver calls. Timed-out
    /// callers fall back to the peer IP while the bounded resolver slot remains
    /// held until the underlying libc call returns. Clamped to 1..=256.
    pub reverse_dns_max_inflight: usize,
    pub lib_path: String,
    pub port: u16,
    pub use_compat_mode: bool,
    pub use_mock_db: bool,
    /// C config.c www_who (shipped NO): generate the web who-list page.
    /// Toggled with MUD_WWW_WHO=1; output dir via MUD_WWW_WHO_DIR.
    pub www_who: bool,
    pub www_who_dir: String,
    /// C config.c autoreboot (shipped 0): the scheduled reboot clock.
    pub autoreboot: bool,
    /// C config.c pt_markable (shipped NO, act.other.c:726): a successful
    /// player-thief is branded PLR_THIEF. Default off to match the oracle.
    pub pt_markable: bool,
    /// Pinned PRNG seed (MUD_RNG_SEED) for deterministic golden tests.
    pub rng_seed: Option<u64>,
    /// Suppress assignment of special routines (comm.c `no_specials`, set by the
    /// C `-s` command-line flag). When true, shop data and all spec-proc
    /// func-pointer tables are skipped at boot, and the MOB_SPEC pulse call is
    /// gated off (mobact.c:51 `MOB_FLAGGED(ch, MOB_SPEC) && !no_specials`). The
    /// Rust port has no argv parser by default, so this is also readable from the
    /// `MUD_NO_SPECIALS` env var; main.rs additionally honours the `-s` argv flag.
    pub no_specials: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "mysql://root:password@localhost/deltamud".to_string()),
            db_timeout_secs: env::var("MUD_DB_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|&seconds| seconds > 0)
                .unwrap_or(5),
            reverse_dns: env::var("MUD_REVERSE_DNS")
                .map(|value| !matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no"))
                .unwrap_or(true),
            reverse_dns_timeout_ms: env::var("MUD_REVERSE_DNS_TIMEOUT_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .map(|milliseconds| milliseconds.clamp(1, 10_000))
                .unwrap_or(1_000),
            reverse_dns_max_inflight: env::var("MUD_REVERSE_DNS_MAX_INFLIGHT")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .map(|lookups| lookups.clamp(1, 256))
                .unwrap_or(16),
            jail_num: 400,
            newbie_room: 200,
            www_who: env::var("MUD_WWW_WHO").map(|v| v == "1").unwrap_or(false),
            www_who_dir: env::var("MUD_WWW_WHO_DIR").unwrap_or_else(|_| "./www".to_string()),
            autoreboot: env::var("MUD_AUTOREBOOT")
                .map(|v| v == "1")
                .unwrap_or(false),
            pt_markable: env::var("MUD_PT_MARKABLE")
                .map(|v| v == "1")
                .unwrap_or(false),
            lib_path: env::var("MUD_LIB_PATH").unwrap_or_else(|_| "./lib".to_string()),
            port: env::var("MUD_PORT")
                .unwrap_or_else(|_| "4000".to_string())
                .parse()
                .unwrap_or(4000),
            use_compat_mode: env::var("MUD_COMPAT_MODE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            use_mock_db: env::var("MUD_MOCK_DB")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            rng_seed: env::var("MUD_RNG_SEED").ok().and_then(|s| s.parse().ok()),
            no_specials: env::var("MUD_NO_SPECIALS")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            database_url: String::new(),
            db_timeout_secs: 5,
            reverse_dns: true,
            reverse_dns_timeout_ms: 1_000,
            reverse_dns_max_inflight: 16,
            jail_num: 400,
            newbie_room: 200,
            www_who: env::var("MUD_WWW_WHO").map(|v| v == "1").unwrap_or(false),
            www_who_dir: env::var("MUD_WWW_WHO_DIR").unwrap_or_else(|_| "./www".to_string()),
            autoreboot: env::var("MUD_AUTOREBOOT")
                .map(|v| v == "1")
                .unwrap_or(false),
            pt_markable: env::var("MUD_PT_MARKABLE")
                .map(|v| v == "1")
                .unwrap_or(false),
            lib_path: "./lib".to_string(),
            port: 4000,
            use_compat_mode: false,
            use_mock_db: true,
            rng_seed: None,
            no_specials: false,
        }
    }
}

/// C config.c:174 `autosave_time = 5` - minutes between Crash_save_all /
/// House_save_all sweeps (comm.c heartbeat autosave block).
pub const AUTOSAVE_TIME: u32 = 5;
/// C config.c:235 max_bad_pws: consecutive bad passwords before a login is
/// disconnected (#194).
pub const MAX_BAD_PWS: u32 = 2;
