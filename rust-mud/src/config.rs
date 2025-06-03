use std::env;

pub struct Config {
    pub database_url: String,
    pub lib_path: String,
    pub port: u16,
    pub use_compat_mode: bool,
    pub use_mock_db: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "mysql://root:password@localhost/deltamud".to_string()),
            lib_path: env::var("MUD_LIB_PATH")
                .unwrap_or_else(|_| "./lib".to_string()),
            port: env::var("MUD_PORT")
                .unwrap_or_else(|_| "4000".to_string())
                .parse()
                .unwrap_or(4000),
            use_compat_mode: env::var("MUD_COMPAT_MODE")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false),
            use_mock_db: env::var("MUD_MOCK_DB")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false),
        }
    }
}