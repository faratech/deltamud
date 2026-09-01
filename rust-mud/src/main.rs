// DeltaMUD — Rust edition. Single-owner GameState heartbeat with async I/O
// at the socket edge. See the conversion plan for the batch roadmap.

// Port-in-progress: many faithfully-ported helper fns/consts are complete
// but not all wired yet; silence the dead-code noise (real issues surface as errors).
#![allow(dead_code)]

mod act;
mod aedit;
mod balance_audit;
mod alias;
mod arena;
mod auction;
mod autowiz;
mod ban;
mod boards;
mod castle;
mod character;
mod clan;
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
mod combat;
mod command_table;
mod cformat;
mod commands;
mod config;
mod connection;
mod constants;
mod database;
mod database_compat;
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
mod fight_messages;
mod file_loader;
mod flags;
mod game;
mod gcmd;
mod graph;
mod handler;
mod hedit;
mod house;
mod interpreter;
mod language;
mod limits;
mod lock_ok;
mod magic;
mod mail;
mod maputils;
mod medit;
mod metrics;
mod misc;
mod mobact;
mod mock_database;
mod modify;
mod object;
mod objsave;
mod oedit;
mod olc;
mod password;
mod quest;
mod races;
mod redit;
mod rng;
mod room;
mod sedit;
mod shop;
mod spec_assign;
mod spec_procs;
mod spell_parser;
mod spells;
mod state;
mod syslog;
mod town_life;
mod whohtml;
mod trigedit;
mod types;
mod weather;
mod world;
mod zedit;

use anyhow::Result;
use config::Config;
use log::{info, warn};
use std::collections::HashMap;
use std::net::IpAddr;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use types::ConnId;

/// Maximum number of simultaneous connections (descriptors). Overridable via the
/// `MUD_MAX_CONN` env var. The accept loop holds a Semaphore of this many
/// permits; each accepted connection takes one permit (passed into the task and
/// dropped on disconnect), so a flood can never spawn unbounded tasks/sockets.
const DEFAULT_MAX_CONN: usize = 256;

/// Per-IP new-connection rate limit: at most `CONN_BURST` connections from one
/// ip within any `CONN_WINDOW_MS` window. This permits legitimate bursts (many
/// users behind one NAT/proxy share an ip, and a single user may open a couple
/// of sockets) while still cutting off a true flood. Overridable via
/// `MUD_CONN_BURST` / `MUD_CONN_WINDOW_MS`.
const DEFAULT_CONN_BURST: u32 = 10;
const DEFAULT_CONN_WINDOW_MS: u64 = 1000;

/// One recovered playing connection inherited across a copyover exec
/// (comm.c copyover_recover, per-line `fd name host`).
struct CopyoverEntry {
    fd: RawFd,
    name: String,
    host: String,
}

/// Parse the `--copyover <port> <listener_fd>` argv tail produced by
/// do_copyover. Returns the inherited listener fd when present.
fn parse_copyover_args() -> Option<RawFd> {
    let args: Vec<String> = std::env::args().collect();
    let pos = args.iter().position(|a| a == "--copyover")?;
    // args[pos+1] = port (already in Config via env/default), args[pos+2] = fd.
    args.get(pos + 2).and_then(|s| s.parse::<RawFd>().ok())
}

/// Re-seed the player DB from the copyover char snapshot (cmd_wizard.rs writes
/// `copyover_chars.dat`). Only needed when the DB does not itself persist across
/// the exec (the in-memory MockDatabase): for each snapshot line we reconstruct
/// the score-relevant Character and create it in the DB *iff* it isn't already
/// there, so the real-MySQL path (player already persisted) is a no-op. Then the
/// per-socket recovery's enter_game load_player succeeds. The file is consumed
/// (deleted) here so it can't leak into a later boot.
async fn reseed_copyover_chars(db: &Arc<dyn DatabaseInterface>, lib_path: &str) {
    let path = std::path::Path::new(lib_path).join("copyover_chars.dat");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = std::fs::remove_file(&path);
    for line in contents.lines() {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() < 27 {
            continue;
        }
        let name = f[1].to_string();
        if name.is_empty() {
            continue;
        }
        // Don't clobber a DB that already holds the player (real MySQL path).
        if db.player_exists(&name).await.unwrap_or(false) {
            continue;
        }
        let pi = |i: usize| f.get(i).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let class = types::Class::from_u8(pi(3) as u8);
        let race = types::Race::from_u8(pi(4) as u8);
        let mut ch = character::Character::new_player(name.clone(), class, race);
        ch.idnum = pi(0);
        ch.player.level = pi(2) as types::Level;
        ch.player.sex = types::Gender::from_u8(pi(5) as u8);
        ch.alignment = pi(6) as i32;
        ch.player.hometown = pi(7) as types::RoomVnum;
        ch.points.gold = pi(8) as types::Gold;
        ch.points.bank_gold = pi(9) as types::Gold;
        ch.points.exp = pi(10) as types::Experience;
        ch.points.hit = pi(11) as i32;
        ch.points.max_hit = pi(12) as i32;
        ch.points.mana = pi(13) as i32;
        ch.points.max_mana = pi(14) as i32;
        ch.points.move_points = pi(15) as i32;
        ch.points.max_move = pi(16) as i32;
        ch.points.armor = pi(17) as types::ArmorClass;
        ch.points.hitroll = pi(18) as types::Hitroll;
        ch.points.damroll = pi(19) as types::Damroll;
        ch.real_abils.str = pi(20) as i8;
        ch.real_abils.str_add = pi(21) as i8;
        ch.real_abils.intel = pi(22) as i8;
        ch.real_abils.wis = pi(23) as i8;
        ch.real_abils.dex = pi(24) as i8;
        ch.real_abils.con = pi(25) as i8;
        ch.real_abils.cha = pi(26) as i8;
        let title = f.get(27).map(|s| s.to_string()).unwrap_or_default();
        if !title.is_empty() {
            ch.player.title = Some(title);
        }
        // BUG #15: restore the room stamp written by do_copyover (fields 28..30)
        // so enter_game drops the player where they stood, not at the temple.
        // Absent on older snapshots -> default (NOWHERE/-1), harmless.
        if let Some(t) = f.get(28).and_then(|s| s.parse::<i64>().ok()) {
            ch.tloadroom = t;
        }
        if let Some(mx) = f.get(29).and_then(|s| s.parse::<i64>().ok()) {
            ch.mapx = mx;
        }
        if let Some(my) = f.get(30).and_then(|s| s.parse::<i64>().ok()) {
            ch.mapy = my;
        }
        ch.aff_abils = ch.real_abils;
        // create_player assigns a fresh idnum; immediately save_player to persist
        // the reconstructed stats under that row so load_player returns them.
        if db.create_player(&ch, "!copyover!").await.is_ok() {
            let _ = db.save_player(&ch).await;
        }
    }
}

/// Read + unlink the copyover state file (comm.c copyover_recover: fopen, then
/// unlink immediately so a crash can't loop on it). First line is the listener
/// fd (already known from argv); subsequent lines are `fd name host`; a line of
/// `-1` terminates.
fn read_copyover_state(lib_path: &str) -> Vec<CopyoverEntry> {
    let path = std::path::Path::new(lib_path).join("copyover.dat");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let _ = std::fs::remove_file(&path); // C unlinks immediately.
    parse_copyover_entries(&contents)
}

/// The parsing half of copyover recovery, split out so hostile/truncated
/// `copyover.dat` content (written by the previous exec, but still untrusted)
/// has unit coverage. Skips the leading listener-fd line, stops at `-1` or a
/// blank line, and drops lines without a numeric fd + name.
fn parse_copyover_entries(contents: &str) -> Vec<CopyoverEntry> {
    let mut out = Vec::new();
    for (i, line) in contents.lines().enumerate() {
        if i == 0 {
            continue; // listener fd line; handled from argv.
        }
        let line = line.trim();
        if line == "-1" || line.is_empty() {
            break;
        }
        let mut it = line.split_whitespace();
        let fd = it.next().and_then(|s| s.parse::<RawFd>().ok());
        let name = it.next().map(|s| s.to_string());
        let host = it.next().map(|s| s.to_string()).unwrap_or_default();
        if let (Some(fd), Some(name)) = (fd, name) {
            out.push(CopyoverEntry { fd, name, host });
        }
    }
    out
}

fn apply_cli_flags(config: &mut Config, args: impl IntoIterator<Item = String>) {
    for arg in args {
        if arg == "-s" {
            config.no_specials = true;
            info!("Suppressing assignment of special routines (no_specials).");
        }
    }
}

/// Minimal raw-TCP HTTP responder for the metrics + health endpoints. Bound on
/// `MUD_METRICS_PORT` when set. NO web framework: we read the request line, look
/// at the path, and write a fixed HTTP/1.1 response, one request per connection
/// (Connection: close). This keeps the dependency surface at zero new crates and
/// is plenty for a Prometheus scrape / liveness probe.
async fn serve_metrics(
    listener: TcpListener,
    metrics: Arc<metrics::Metrics>,
    who_snapshot: Arc<std::sync::RwLock<String>>,
) {
    loop {
        let (mut sock, _peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                warn!("metrics accept() error: {}", e);
                continue;
            }
        };
        let metrics = metrics.clone();
        let who_snapshot = who_snapshot.clone();
        tokio::spawn(async move {
            // Read just enough to see the request line. A real scrape sends a
            // short request; cap the read so a slow/hostile client can't pin us.
            let mut buf = [0u8; 1024];
            let n = match sock.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .split_whitespace()
                .nth(1) // METHOD <path> HTTP/1.1
                .unwrap_or("/");

            let (status, ctype, body) =
                if path == "/metrics" || path.starts_with("/metrics?") {
                    ("200 OK", "text/plain; version=0.0.4", metrics.render_prometheus())
                } else if path == "/health" || path.starts_with("/health?") {
                    (
                        "200 OK",
                        "text/plain; version=0.0.4",
                        format!("ok\nplayers {}\n", metrics.players_now()),
                    )
                } else if path == "/api/who" || path.starts_with("/api/who?") {
                    let snapshot = who_snapshot.read().map(|s| s.clone()).unwrap_or_default();
                    let body =
                        if snapshot.is_empty() { "{\"count\":0,\"players\":[]}".to_string() } else { snapshot };
                    ("200 OK", "application/json", body)
                } else {
                    ("404 Not Found", "text/plain; version=0.0.4", "not found\n".to_string())
                };

            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                status = status,
                ctype = ctype,
                len = body.len(),
                body = body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

/// SO_KEEPALIVE with an aggressive profile on accepted client sockets: a peer
/// that vanished without RST (power loss, NAT drop) is detected in ~80s
/// instead of never, so the reader loop wakes and the descriptor/connection
/// slot is reclaimed (W6 live-ops).
fn enable_tcp_keepalive(fd: RawFd) {
    use std::time::Duration;
    const ON: libc::c_int = 1;
    let secs = |d: Duration| d.as_secs() as libc::c_int;
    unsafe {
        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &ON as *const _ as *const libc::c_void,
            std::mem::size_of_val(&ON) as libc::socklen_t,
        ) != 0
        {
            return;
        }
        // Idle 60s, probe every 10s, 2 failed probes => dead (~80s total).
        let idle = secs(Duration::from_secs(60));
        let intvl = secs(Duration::from_secs(10));
        let cnt: libc::c_int = 2;
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPIDLE,
            &idle as *const _ as *const libc::c_void,
            std::mem::size_of_val(&idle) as libc::socklen_t,
        );
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPINTVL,
            &intvl as *const _ as *const libc::c_void,
            std::mem::size_of_val(&intvl) as libc::socklen_t,
        );
        let _ = libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPCNT,
            &cnt as *const _ as *const libc::c_void,
            std::mem::size_of_val(&cnt) as libc::socklen_t,
        );
    }
}

/// Database abstraction (CircleMUD dbinterface.c). Implemented by the
/// MySQL-backed `Database` and the in-memory `MockDatabase`.
/// C sysdep.h MAX_RAW_INPUT_LENGTH: the per-connection raw input bound.
pub const MAX_RAW_INPUT_LENGTH: usize = 2048;

#[async_trait::async_trait]
pub trait DatabaseInterface: Send + Sync {
    async fn init_tables(&self) -> Result<()>;
    async fn player_exists(&self, name: &str) -> Result<bool>;
    async fn create_player(&self, character: &character::Character, password: &str) -> Result<i64>;
    async fn load_player(&self, name: &str) -> Result<character::Character>;
    async fn save_player(&self, character: &character::Character) -> Result<()>;
    async fn save_player_with_host(
        &self,
        character: &character::Character,
        host: &str,
    ) -> Result<()> {
        let _ = host;
        self.save_player(character).await
    }
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool>;
    /// The stored password hash, for the automatic legacy-hash upgrade at
    /// login (C interpreter.c:1914-1952 password_needs_upgrade path) (#219).
    async fn get_password_hash(&self, name: &str) -> Result<Option<String>>;
    async fn delete_deleted_players(&self) -> Result<u64>;
    async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>>;
    /// C clan.c:242-255 clan_destroy: shift every player row's clan past the
    /// destroyed one and clear the destroyed clan's members (offline rows
    /// included) (#165).
    async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()>;
    /// C clan.c:388-405 lower_entire_clan: set clan_rank = 1 for every
    /// member of `clan` whose rank != -1 (#165).
    async fn clan_lower_ranks(&self, clan: i32) -> Result<()>;
    /// Every player's index row {idnum,name,level,last_logon,host} for the
    /// boot-time player_table build (C build_player_index, db.c).
    async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>>;
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
    async fn save_player_with_host(&self, c: &character::Character, host: &str) -> Result<()> {
        self.save_player_with_host(c, host).await
    }
    async fn verify_password(&self, name: &str, p: &str) -> Result<bool> {
        self.verify_password(name, p).await
    }
    async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        self.get_password_hash(name).await
    }
    async fn delete_deleted_players(&self) -> Result<u64> {
        self.delete_deleted_players().await
    }
    async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>> {
        self.clan_member_counts().await
    }
    async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()> {
        self.clan_destroy_fixup(destroyed).await
    }
    async fn clan_lower_ranks(&self, clan: i32) -> Result<()> {
        self.clan_lower_ranks(clan).await
    }
    async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>> {
        self.list_players().await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Log every panic with a captured backtrace (the default hook only prints a
    // terse line). catch_unwind around command dispatch + the heartbeat keeps the
    // server alive; this hook makes the cause diagnosable when it does fire.
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        log::error!("PANIC at {}: {}\nbacktrace:\n{}", loc, payload, bt);
        eprintln!("PANIC at {}: {}\nbacktrace:\n{}", loc, payload, bt);
    }));

    info!("DeltaMUD (Rust) starting...");
    let mut config = Config::from_env();

    // Faithful port of the C `-s` command-line flag (comm.c:272
    // `no_specials = 1`, "Suppressing assignment of special routines.").
    // `-q` is quickboot in C and must not suppress specials.
    apply_cli_flags(&mut config, std::env::args().skip(1));

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
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1)
    });
    state.rng.srandom(seed);

    // DG Scripts must boot BEFORE the world: the world loader's `T <vnum>` lines
    // call dg_db_scripts::record_proto, which rejects (and logs "non-existant")
    // any trigger vnum not yet in trig_index. C orders index_boot(DB_BOOT_TRG)
    // ahead of WLD/MOB/OBJ for exactly this reason.
    dg_scripts::boot_dg_scripts(&config.lib_path);

    if let Err(e) = file_loader::FileLoader::load_world(&mut state, &config.lib_path).await {
        warn!("World load failed: {} (continuing with whatever loaded)", e);
    }

    // Splice the surface ("outside") world-map cells into the room table as real
    // rooms (maputils.c read_map). This MUST come straight after the real world
    // is loaded and before any pass that wants every room (DG triggers, zone
    // priming, weather) — it appends the map block AFTER the real rooms so real
    // rnums are untouched, records GameState.map_start_rnum, and wires the city
    // EntryPoint links so do_enter/do_leave and weather damage work.
    maputils::integrate_map_rooms(&mut state);

    // Deltania Breathes: precompute the cross-town caravan routes over the
    // (now spliced) surface map. Must run after integrate_map_rooms.
    town_life::boot_town_life(&state);

    // Load socials (CircleMUD boot_social_messages); spliced into command
    // lookup as a fallback since they are not in the static command table.
    cmd_social::boot_socials(Some(&format!("{}/misc/socials", config.lib_path)));
    // db.c:299-300 index_boot(DB_BOOT_HLP) - serve the 73k-line help index
    // to the live `help` command (#232).
    hedit::boot_help_table(&config.lib_path);

    // Load the combat hit-messages (fight.c load_messages, lib/misc/messages):
    // flavourful per-skill / per-weapon death/hit/miss/god messages.
    fight_messages::load_messages(&config.lib_path);

    // Content/economy subsystem boot (Batch 11). Shop data load mirrors C's
    // DB_BOOT_SHP, which boot_world() skips under no_specials (db.c:261
    // `if (!no_specials) index_boot(DB_BOOT_SHP)`).
    if !config.no_specials {
        shop::boot_shops(&config.lib_path);
    }
    clan::boot_clans(&config.lib_path);
    match db.clan_member_counts().await {
        Ok(counts) => clan::recount_member_counts(&counts),
        Err(e) => warn!("Could not recount clan members from player_main: {}", e),
    }
    boards::boot_boards(&config.lib_path);
    ban::boot_ban(&config.lib_path);
    mail::boot_mail(&config.lib_path);
    quest::boot_quest(&config.lib_path);
    auction::boot_auction(&config.lib_path);
    // C boot_db order (db.c:358-365 vs 369-373): the initial zone reset
    // (world population) runs BEFORE House_boot, so stored house objects are
    // not present during the first reset - otherwise an 'R' command could
    // extract a stored house item (#242).
    state.prime_zones();
    house::house_boot(&mut state);

    // DG Scripts: trigger prototypes were loaded above (before the world). Now
    // that rooms exist, attach prototype triggers to every already-loaded room
    // (mob/obj triggers attach when an instance is loaded; the file_loader
    // recorded the proto bindings via attach_trigger_to_{mob,obj,room} during
    // world load, which now succeeds because the trig_index is populated).
    dg_db_scripts::assign_room_triggers(&mut state);

    // Capture ROOM_DEATH rooms for the dts_are_dumps dump registration (C
    // assign_rooms reads world[] directly; our table build has no GameState
    // borrow, so we stash the vnums first). dts_are_dumps is YES in DeltaMUD.
    // C assign_rooms (spec_assign.c:152-166) iterates i < top_of_world over
    // the LOADED world only - the 2M+ surface-map block is spliced after and
    // its synthetic death rooms must not get dump spec procs (#238).
    let death_rooms: Vec<types::RoomVnum> = state
        .rooms
        .iter()
        .filter(|r| {
            r.room_flags.contains(room::RoomFlags::DEATH) && r.map_x.is_none()
        })
        .map(|r| r.number)
        .collect();
    spec_assign::set_death_trap_rooms(death_rooms);

    // Build the vnum->special-procedure tables (spec_assign.c assign_*). Must
    // come after shops/boards/mail so their data is available to the procs.
    // C's boot_db() gates the whole assign_mobiles/shopkeepers/objects/rooms
    // block on `if (!no_specials)` (db.c:317); skipping it here leaves every
    // spec table empty, so the interpreter's special() walk and the MOB_SPEC
    // pulse both find nothing to dispatch — exactly the C behaviour.
    if !config.no_specials {
        spec_assign::assign_specs();
    }

    // Build the in-memory player name<->idnum index (C build_player_index,
    // called from boot_db after the world loads). Lets offline players resolve
    // for `last`, ignore-by-name, mail, and name<->id lookups without an async
    // DB hit. Empty on a brand-new DB; kept fresh by update_player_index as
    // players are created / enter / save.
    let pt = db.list_players().await.unwrap_or_default();
    info!("Loaded {} player(s) into the name index.", pt.len());
    state.player_table = pt;
    // Mirror the index into the mail subsystem's private name<->id table so
    // offline senders/recipients resolve there too (mail.rs keeps its own copy).
    for p in &state.player_table {
        mail::mail_register_player(p.idnum, &p.name);
    }

    let lib_path = config.lib_path.clone();
    let (game_tx, game_rx) = mpsc::channel(256);

    // Lock-free observability counters, shared between the Game task (writer on
    // the heartbeat hot path) and the optional metrics HTTP task (reader).
    let metrics = Arc::new(metrics::Metrics::new());

    // Who-list JSON snapshot for /api/who (W5): written once a second by the
    // Game task, served read-only here.
    let who_snapshot = Arc::new(std::sync::RwLock::new(String::new()));

    // Optional metrics/health HTTP endpoint. Disabled unless MUD_METRICS_PORT is
    // set (no extra listening socket in the default config). Raw-TCP responder —
    // no web framework, no new crates.
    if let Ok(mport) = std::env::var("MUD_METRICS_PORT") {
        match mport.parse::<u16>() {
            Ok(port) if port > 0 => {
                let addr = format!("0.0.0.0:{}", port);
                match TcpListener::bind(&addr).await {
                    Ok(mlistener) => {
                        info!("Metrics endpoint listening on {} (/metrics, /health)", addr);
                        let m = metrics.clone();
                        let who = who_snapshot.clone();
                        info!("  (also serving /api/who)");
                        tokio::spawn(async move { serve_metrics(mlistener, m, who).await });
                    }
                    Err(e) => warn!("Could not bind metrics port {}: {}", port, e),
                }
            }
            _ => warn!(
                "MUD_METRICS_PORT={:?} is not a valid port; metrics disabled",
                mport
            ),
        }
    }

    // Keep a handle to the shared DB for copyover re-seeding (same Arc the game
    // task uses, so re-seeds land in the live MockDatabase before recovery).
    let db_for_recovery = db.clone();

    let game_metrics = metrics.clone();
    let game_who = who_snapshot.clone();
    let game_handle = tokio::spawn(async move {
        let mut game = game::Game::new(state, db);
        game.set_metrics(game_metrics);
        game.set_who_snapshot(game_who);
        game.load_text_files(&lib_path).await;
        game.prime_zones();
        if let Err(e) = game.run(game_rx).await {
            eprintln!("Game loop error: {}", e);
        }
        // game.run() returns only on graceful shutdown (SIGTERM/Ctrl-C).
    });

    // Copyover detection (comm.c init_game: `if (!fCopyOver) init_socket(port)`).
    // When re-exec'd by do_copyover, the listener fd was inherited (FD_CLOEXEC
    // cleared before exec); rebuild the tokio listener from it instead of bind().
    let copyover_listener_fd = parse_copyover_args();
    let listener = if let Some(lfd) = copyover_listener_fd {
        info!("Copyover recovery: inheriting listener fd {}", lfd);
        // SAFETY: lfd was a live listening socket in the previous image; execv
        // kept it open (CLOEXEC cleared) and nothing else owns it now.
        let std_listener = unsafe { std::net::TcpListener::from_raw_fd(lfd) };
        std_listener.set_nonblocking(true)?;
        TcpListener::from_std(std_listener)?
    } else {
        let addr = format!("0.0.0.0:{}", config.port);
        let l = TcpListener::bind(&addr).await?;
        info!("Server listening on {}", addr);
        l
    };

    // Publish the (possibly inherited) listener fd so do_copyover can re-inherit
    // it on the NEXT copyover. Mirrors C keeping `mother_desc` live.
    {
        use std::os::unix::io::AsRawFd;
        state::set_listener_fd(listener.as_raw_fd());
    }

    let mut next_conn: u64 = 1;

    // If we came up via copyover, re-attach every previously-playing socket.
    if copyover_listener_fd.is_some() {
        // Re-seed the DB from the char snapshot first (no-op if the DB already
        // persists the players, e.g. real MySQL), so enter_game can load them.
        reseed_copyover_chars(&db_for_recovery, &config.lib_path).await;
        let entries = read_copyover_state(&config.lib_path);
        info!(
            "Copyover recovery: {} player socket(s) to restore",
            entries.len()
        );
        for e in entries {
            let id = ConnId(next_conn);
            next_conn += 1;
            // SAFETY: e.fd is a live connected socket inherited across exec.
            let std_stream = unsafe { std::net::TcpStream::from_raw_fd(e.fd) };
            if std_stream.set_nonblocking(true).is_err() {
                warn!("copyover: fd {} for {} not usable, dropping", e.fd, e.name);
                continue;
            }
            let stream = match tokio::net::TcpStream::from_std(std_stream) {
                Ok(s) => s,
                Err(err) => {
                    warn!("copyover: from_std failed for {}: {}", e.name, err);
                    continue;
                }
            };
            let tx = game_tx.clone();
            tokio::spawn(async move {
                if let Err(err) =
                    connection::handle_recovered(stream, id, e.fd, e.name.clone(), e.host, tx).await
                {
                    warn!("recovered client {} error: {}", e.name, err);
                }
            });
        }
    }

    // Connection caps + rate limiting (DoS / runaway-client protection).
    let max_conn = std::env::var("MUD_MAX_CONN")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_CONN);
    let conn_burst = std::env::var("MUD_CONN_BURST")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_CONN_BURST);
    let conn_window = Duration::from_millis(
        std::env::var("MUD_CONN_WINDOW_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CONN_WINDOW_MS),
    );
    info!(
        "Connection limits: max_conn={}, per-IP burst={}/{}ms",
        max_conn,
        conn_burst,
        conn_window.as_millis()
    );
    let conn_sem = Arc::new(Semaphore::new(max_conn));
    // Per-IP recent-connection timestamps for the sliding-window rate limit.
    // Pruned lazily so the map can't grow without bound.
    let mut recent_connects: HashMap<IpAddr, Vec<Instant>> = HashMap::new();

    // SIGTERM stream for the accept loop (the Game task installs its own; both
    // need to know so the process exits, not just the game loop). On either
    // signal — or when the game task itself returns (it returns only on its own
    // graceful shutdown) — break the accept loop and exit the process cleanly.
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    loop {
        let sigterm_fut = async {
            match sigterm.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };

        let (stream, peer) = tokio::select! {
            res = listener.accept() => match res {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("accept() error: {}", e);
                    continue;
                }
            },
            _ = tokio::signal::ctrl_c() => {
                info!("main: received Ctrl-C; waiting for game task to finish saving.");
                let _ = game_handle.await;
                break;
            }
            _ = sigterm_fut => {
                info!("main: received SIGTERM; waiting for game task to finish saving.");
                let _ = game_handle.await;
                break;
            }
        };

        let ip = peer.ip();
        enable_tcp_keepalive(stream.as_raw_fd());

        // Task 3: reject banned hosts at accept, before spawning a handler.
        // BanType::All means "no connection at all"; we also reject BanType::New
        // here only insofar as the login nanny still gives BAN_NEW/BAN_SELECT
        // sites their chance to log in an existing PLR_SITEOK char — so at the
        // socket level we only hard-drop BAN_ALL. (New/Select are enforced in
        // the login path where the char's flags are known.)
        if ban::isbanned(&ip.to_string()) == ban::BanType::All {
            warn!("Rejected banned host {}", ip);
            let mut s = stream;
            let _ = s.write_all(b"You are banned from this server.\r\n").await;
            let _ = s.shutdown().await;
            continue;
        }

        // Task 5b: per-IP sliding-window new-connection rate limit. Keep only the
        // timestamps within the window, then reject if the burst cap is reached.
        let now = Instant::now();
        let times = recent_connects.entry(ip).or_default();
        times.retain(|t| now.duration_since(*t) < conn_window);
        if times.len() as u32 >= conn_burst {
            warn!("Rate-limiting connection flood from {}", ip);
            let mut s = stream;
            let _ = s
                .write_all(b"You are connecting too rapidly. Please wait a moment.\r\n")
                .await;
            let _ = s.shutdown().await;
            continue;
        }
        times.push(now);
        // Lazily prune ips with no recent connections so the map can't grow
        // without bound under a wide source-ip spread.
        if recent_connects.len() > max_conn * 4 {
            recent_connects.retain(|_, ts| {
                ts.retain(|t| now.duration_since(*t) < conn_window);
                !ts.is_empty()
            });
        }

        // Task 5a: cap concurrent connections. try_acquire so a flood is
        // rejected immediately instead of queuing unbounded accepted sockets.
        let permit = match Arc::clone(&conn_sem).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                warn!("Connection limit ({}) reached; rejecting {}", max_conn, ip);
                let mut s = stream;
                let _ = s
                    .write_all(b"The server is full. Please try again later.\r\n")
                    .await;
                let _ = s.shutdown().await;
                continue;
            }
        };

        let id = ConnId(next_conn);
        next_conn += 1;
        let tx = game_tx.clone();
        tokio::spawn(async move {
            // Hold the permit for the lifetime of the connection; dropping it on
            // task exit frees a slot for the next client.
            let _permit = permit;
            if let Err(e) = connection::handle_client(stream, peer, id, tx).await {
                warn!("client {} error: {}", peer, e);
            }
        });
    }

    info!("Server exiting.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- copyover state parsing (W6 live-ops: the recovery file is untrusted
    //    input written by the previous exec) -------------------------------

    #[test]
    fn copyover_parse_reads_fd_name_host_lines() {
        let text = "7\n10 Mulder 10.0.0.1\n11 Belgarion host.example\n-1\n";
        let entries = parse_copyover_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].fd, 10);
        assert_eq!(entries[0].name, "Mulder");
        assert_eq!(entries[0].host, "10.0.0.1");
        assert_eq!(entries[1].fd, 11);
        assert_eq!(entries[1].host, "host.example");
    }

    #[test]
    fn copyover_parse_stops_at_minus_one_and_blank() {
        // Garbage after the -1 terminator must be ignored.
        let text = "3\n8 A a.host\n-1\n999 Z z.h\n";
        assert_eq!(parse_copyover_entries(text).len(), 1);
        // A blank line terminates too.
        let text2 = "3\n8 A a.host\n\n9 B b.h\n";
        assert_eq!(parse_copyover_entries(text2).len(), 1);
    }

    #[test]
    fn copyover_parse_drops_bad_lines_and_defaults_host() {
        // Non-numeric fd: dropped. Missing host: empty. Extra fields: host
        // keeps the third token only.
        let text = "0\nnope A a.h\n12 Hostless\n13 Trunc a.h extra junk\n-1\n";
        let entries = parse_copyover_entries(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].fd, 12);
        assert_eq!(entries[0].host, "");
        assert_eq!(entries[1].fd, 13);
        assert_eq!(entries[1].host, "a.h");
    }

    #[test]
    fn copyover_parse_survives_empty_and_listener_only_files() {
        assert!(parse_copyover_entries("").is_empty());
        assert!(parse_copyover_entries("5\n").is_empty());
        assert!(parse_copyover_entries("5\n-1\n").is_empty());
    }

    #[test]
    fn copyover_writer_reader_round_trip() {
        // The exact lines do_copyover emits: listener fd, one `fd name host`
        // per playing descriptor, then -1.
        let listener_fd = 9;
        let players = [
            (10, "Mulder", "1.2.3.4"),
            (11, "Skinner", "corp.example.test"),
        ];
        let mut text = format!("{listener_fd}\n");
        for (fd, name, host) in players {
            text.push_str(&format!("{fd} {name} {host}\n"));
        }
        text.push_str("-1\n");
        let entries = parse_copyover_entries(&text);
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].fd, entries[0].name.as_str()), (10, "Mulder"));
        assert_eq!(entries[1].host, "corp.example.test");
    }

    #[test]
    fn cli_no_specials_flag_matches_c_s_only() {
        let mut q_config = Config::default();
        apply_cli_flags(&mut q_config, ["-q".to_string()]);
        assert!(!q_config.no_specials);

        let mut s_config = Config::default();
        apply_cli_flags(&mut s_config, ["-s".to_string()]);
        assert!(s_config.no_specials);
    }
}
