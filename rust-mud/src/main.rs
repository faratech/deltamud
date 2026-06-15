// DeltaMUD — Rust edition. Single-owner GameState heartbeat with async I/O
// at the socket edge. See the conversion plan for the batch roadmap.

// Port-in-progress: many faithfully-ported helper fns/consts are complete
// but not all wired yet; silence the dead-code noise (real issues surface as errors).
#![allow(dead_code)]

mod act;
mod alias;
mod arena;
mod auction;
mod ban;
mod boards;
mod castle;
mod character;
mod class;
mod cmd_comm;
mod cmd_create;
mod cmd_informative;
mod cmd_item;
mod cmd_movement;
mod cmd_offensive;
mod cmd_other;
mod cmd_social;
mod cmd_wizard;
mod clan;
mod combat;
mod command_table;
mod commands;
mod config;
mod connection;
mod constants;
mod database;
mod deity;
mod dg_comm;
mod dg_db_scripts;
mod dg_event;
mod dg_handler;
mod dg_mobcmd;
mod dg_objcmd;
mod dg_scripts;
mod dg_triggers;
mod dg_wldcmd;
mod file_loader;
mod flags;
mod game;
mod graph;
mod house;
mod handler;
mod interpreter;
mod language;
mod limits;
mod magic;
mod mail;
mod maputils;
mod misc;
mod mobact;
mod mock_database;
mod modify;
mod olc;
mod redit;
mod oedit;
mod medit;
mod zedit;
mod sedit;
mod aedit;
mod hedit;
mod trigedit;
mod object;
mod password;
mod objsave;
mod quest;
mod races;
mod rng;
mod room;
mod shop;
mod spec_assign;
mod spec_procs;
mod spell_parser;
mod spells;
mod state;
mod types;
mod weather;
mod world;

use anyhow::Result;
use config::Config;
use log::{info, warn};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use types::ConnId;

/// Database abstraction (CircleMUD dbinterface.c). Implemented by the
/// MySQL-backed `Database` and the in-memory `MockDatabase`.
#[async_trait::async_trait]
pub trait DatabaseInterface: Send + Sync {
    async fn init_tables(&self) -> Result<()>;
    async fn player_exists(&self, name: &str) -> Result<bool>;
    async fn create_player(&self, character: &character::Character, password: &str) -> Result<i64>;
    async fn load_player(&self, name: &str) -> Result<character::Character>;
    async fn save_player(&self, character: &character::Character) -> Result<()>;
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool>;
}

#[async_trait::async_trait]
impl DatabaseInterface for database::Database {
    async fn init_tables(&self) -> Result<()> {
        self.init_tables().await
    }
    async fn player_exists(&self, name: &str) -> Result<bool> {
        self.player_exists(name).await
    }
    async fn create_player(&self, c: &character::Character, p: &str) -> Result<i64> {
        self.create_player(c, p).await
    }
    async fn load_player(&self, name: &str) -> Result<character::Character> {
        self.load_player(name).await
    }
    async fn save_player(&self, c: &character::Character) -> Result<()> {
        self.save_player(c).await
    }
    async fn verify_password(&self, name: &str, p: &str) -> Result<bool> {
        self.verify_password(name, p).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("DeltaMUD (Rust) starting...");
    let config = Config::from_env();

    let db: Arc<dyn DatabaseInterface> = if config.use_mock_db {
        info!("Using in-memory mock database");
        Arc::new(mock_database::MockDatabase::new())
    } else {
        info!("Using MySQL database");
        let d = database::Database::new(&config.database_url)?;
        d.init_tables().await?;
        Arc::new(d)
    };

    // Build the world.
    let mut state = state::GameState::new(config.clone());

    // Seed the PRNG (pinned for golden tests, else from the clock).
    let seed = config.rng_seed.unwrap_or_else(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1)
    });
    state.rng.srandom(seed);

    if let Err(e) = file_loader::FileLoader::load_world(&mut state, &config.lib_path).await {
        warn!("World load failed: {} (continuing with whatever loaded)", e);
    }

    // Load socials (CircleMUD boot_social_messages); spliced into command
    // lookup as a fallback since they are not in the static command table.
    cmd_social::boot_socials(Some(&format!("{}/misc/socials", config.lib_path)));

    // Content/economy subsystem boot (Batch 11).
    shop::boot_shops(&config.lib_path);
    clan::boot_clans(&config.lib_path);
    boards::boot_boards(&config.lib_path);
    ban::boot_ban(&config.lib_path);
    mail::boot_mail(&config.lib_path);
    quest::boot_quest(&config.lib_path);
    auction::boot_auction(&config.lib_path);
    house::house_boot(&mut state);

    // DG Scripts: load trigger prototypes (lib/world/trg), reset runtime trigger
    // tables + the wait-event queue, then attach prototype triggers to every
    // already-loaded room (mob/obj triggers attach when an instance is loaded;
    // the file_loader records the proto bindings via dg_db_scripts::
    // attach_trigger_to_{mob,obj,room} as it parses the T lines).
    dg_scripts::boot_dg_scripts(&config.lib_path);
    dg_db_scripts::assign_room_triggers(&mut state);

    // Capture ROOM_DEATH rooms for the dts_are_dumps dump registration (C
    // assign_rooms reads world[] directly; our table build has no GameState
    // borrow, so we stash the vnums first). dts_are_dumps is YES in DeltaMUD.
    let death_rooms: Vec<types::RoomVnum> = state
        .rooms
        .iter()
        .filter(|r| r.room_flags.contains(room::RoomFlags::DEATH))
        .map(|r| r.number)
        .collect();
    spec_assign::set_death_trap_rooms(death_rooms);

    // Build the vnum->special-procedure tables (spec_assign.c assign_*). Must
    // come after shops/boards/mail so their data is available to the procs.
    spec_assign::assign_specs();

    let lib_path = config.lib_path.clone();
    let (game_tx, game_rx) = mpsc::channel(256);

    let _game = tokio::spawn(async move {
        let mut game = game::Game::new(state, db);
        game.load_text_files(&lib_path).await;
        game.prime_zones();
        if let Err(e) = game.run(game_rx).await {
            eprintln!("Game loop error: {}", e);
        }
    });

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Server listening on {}", addr);

    let mut next_conn: u64 = 1;
    loop {
        let (stream, peer) = listener.accept().await?;
        let id = ConnId(next_conn);
        next_conn += 1;
        let tx = game_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = connection::handle_client(stream, peer, id, tx).await {
                warn!("client {} error: {}", peer, e);
            }
        });
    }
}
