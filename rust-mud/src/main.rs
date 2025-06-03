mod types;
mod character;
mod room;
mod object;
mod connection;
mod world;
mod game;
mod database;
mod database_compat;
mod mock_database;
mod combat;
mod magic;
mod file_loader;
mod commands;
mod config;

use tokio::net::TcpListener;
use tokio::sync::mpsc;
use std::sync::Arc;
use parking_lot::RwLock;
use anyhow::Result;
use log::info;
use config::Config;

// Database trait for polymorphism
#[async_trait::async_trait]
pub trait DatabaseInterface: Send + Sync {
    async fn init_tables(&self) -> Result<()>;
    async fn player_exists(&self, name: &str) -> Result<bool>;
    async fn create_player(&self, character: &character::Character, password: &str) -> Result<u64>;
    async fn load_player(&self, name: &str) -> Result<character::Character>;
    async fn save_player(&self, character: &character::Character) -> Result<()>;
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool>;
}

// Implement trait for standard database
#[async_trait::async_trait]
impl DatabaseInterface for database::Database {
    async fn init_tables(&self) -> Result<()> { self.init_tables().await }
    async fn player_exists(&self, name: &str) -> Result<bool> { self.player_exists(name).await }
    async fn create_player(&self, character: &character::Character, password: &str) -> Result<u64> { 
        self.create_player(character, password).await 
    }
    async fn load_player(&self, name: &str) -> Result<character::Character> { self.load_player(name).await }
    async fn save_player(&self, character: &character::Character) -> Result<()> { self.save_player(character).await }
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> { 
        self.verify_password(name, password).await 
    }
}

// Implement trait for compat database
#[async_trait::async_trait]
impl DatabaseInterface for database_compat::CompatDatabase {
    async fn init_tables(&self) -> Result<()> { 
        // Compat mode assumes tables exist
        Ok(()) 
    }
    async fn player_exists(&self, name: &str) -> Result<bool> { 
        // Use same method as standard
        use mysql_async::prelude::*;
        let mut conn = self.pool.get_conn().await?;
        let result: Option<mysql_async::Row> = conn
            .exec_first("SELECT idnum FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(result.is_some())
    }
    async fn create_player(&self, _character: &character::Character, _password: &str) -> Result<u64> { 
        Err(anyhow::anyhow!("Cannot create new players in compatibility mode. Use standard mode."))
    }
    async fn load_player(&self, name: &str) -> Result<character::Character> { 
        self.load_player_compat(name).await 
    }
    async fn save_player(&self, character: &character::Character) -> Result<()> { 
        self.save_player_compat(character).await 
    }
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> { 
        self.verify_password_compat(name, password).await 
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();
    
    info!("DeltaMUD Rust Edition starting...");
    
    // Load configuration
    let config = Config::from_env();
    
    // Initialize database (mock, standard, or compat mode)
    let db: Arc<dyn DatabaseInterface> = if config.use_mock_db {
        info!("Using mock database mode for testing");
        Arc::new(mock_database::MockDatabase::new())
    } else if config.use_compat_mode {
        info!("Using database compatibility mode for existing DeltaMUD data");
        Arc::new(database_compat::CompatDatabase::new(&config.database_url)?)
    } else {
        info!("Using standard database mode");
        let db = database::Database::new(&config.database_url)?;
        db.init_tables().await?;
        Arc::new(db)
    };
    info!("Database initialized");
    
    // Create world
    let mut world = world::World::new();
    
    // Load world files
    if let Err(e) = file_loader::FileLoader::load_world(&mut world, &config.lib_path).await {
        info!("Could not load world files: {}. Using test world.", e);
        world.load_world_files().await?;
    }
    
    let world = Arc::new(RwLock::new(world));
    
    // Create game message channel
    let (game_tx, game_rx) = mpsc::channel(100);
    
    // Start game loop
    let game_world = world.clone();
    let game_db = db.clone();
    let _game_handle = tokio::spawn(async move {
        let mut game = game::Game::new(game_world, game_db);
        if let Err(e) = game.run(game_rx).await {
            eprintln!("Game loop error: {}", e);
        }
    });
    
    // Start TCP listener
    let addr = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Server listening on {}", addr);
    
    let mut conn_id = 1u64;
    
    loop {
        let (stream, addr) = listener.accept().await?;
        let game_tx = game_tx.clone();
        let id = conn_id;
        conn_id += 1;
        
        tokio::spawn(async move {
            if let Err(e) = connection::handle_client(stream, addr, id, game_tx).await {
                eprintln!("Error handling client {}: {}", addr, e);
            }
        });
    }
}

// Add bitflags dependency
extern crate bitflags;