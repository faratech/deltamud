use std::env;

use crate::types::RoomVnum;

// ===========================================================================
// World room-number constants (config.c)
// ===========================================================================

/// Number of distinct hometowns / start towns (structs.h `NUM_STARTROOMS`).
pub const NUM_STARTROOMS: usize = 3;

/// Per-hometown mortal start rooms (config.c `mortal_start_room[NUM_STARTROOMS + 1]`).
/// Index 0 is the newbie loadroom element; indices 1..=NUM_STARTROOMS are the
/// start towns. In this world every entry resolves to vnum 100; the table
/// structure is ported verbatim so per-town divergence stays faithful.
pub const MORTAL_START_ROOM: [RoomVnum; NUM_STARTROOMS + 1] = [
    100, // Newbie loadroom element
    100, // Itrius
    100, // Start Town 2
    100, // Start Town 3
];

/// vnum of room that immortals enter at by default (config.c `immort_start_room`).
pub const IMMORT_START_ROOM: RoomVnum = 1204;

/// vnum of room that frozen players enter at (config.c `frozen_start_room`).
pub const FROZEN_START_ROOM: RoomVnum = 1202;

#[derive(Clone)]
pub struct Config {
    /// C config.c:59 `jail_num = 400`.
    pub jail_num: RoomVnum,
    /// C config.c:77 `newbie_room = 2200`.
    pub newbie_room: RoomVnum,
    pub database_url: String,
    pub lib_path: String,
    pub port: u16,
    pub use_compat_mode: bool,
    pub use_mock_db: bool,
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
            jail_num: 400,
            newbie_room: 2200,
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
            jail_num: 400,
            newbie_room: 2200,
            lib_path: "./lib".to_string(),
            port: 4000,
            use_compat_mode: false,
            use_mock_db: true,
            rng_seed: None,
            no_specials: false,
        }
    }
}
