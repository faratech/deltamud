use std::env;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub lib_path: String,
    pub port: u16,
    pub use_compat_mode: bool,
    pub use_mock_db: bool,
    /// Pinned PRNG seed (MUD_RNG_SEED) for deterministic golden tests.
    pub rng_seed: Option<u64>,
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "mysql://root:password@localhost/deltamud".to_string()),
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
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            database_url: String::new(),
            lib_path: "./lib".to_string(),
            port: 4000,
            use_compat_mode: false,
            use_mock_db: true,
            rng_seed: None,
        }
    }
}
