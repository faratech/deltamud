// DeltaMUD — Rust edition. Single-owner GameState heartbeat with async I/O
// at the socket edge. See the conversion plan for the batch roadmap.

// Port-in-progress: many faithfully-ported helper fns/consts are complete
// but not all wired yet; silence the dead-code noise (real issues surface as errors).
#![allow(dead_code)]

mod act;
mod aedit;
mod alias;
mod arena;
mod auction;
mod autowiz;
mod balance_audit;
mod ban;
mod boards;
mod castle;
mod cformat;
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
mod config;
mod connection;
mod constants;
mod copyover;
mod database;
mod database_compat;
mod database_timeout;
mod db_api;
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
mod gold;
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
mod player_sidecars;
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
mod text;
mod town_life;
mod trigedit;
mod types;
mod weather;
mod whohtml;
mod world;
mod zedit;

use anyhow::{Context, Result, bail};
use config::Config;
use log::{info, warn};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use types::ConnId;

// The database boundary lives in db_api.rs; re-exported so the crate-wide
// `crate::DatabaseInterface` paths keep resolving.
pub use db_api::{
    AuthorityUpdateOutcome, DatabaseInterface, ImplementorBootstrapOutcome,
    PasswordHashUpdateOutcome, PlayerAuthorityState,
};

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

/// The metrics listener is intentionally much smaller than the game listener:
/// Prometheus and health probes use short-lived connections, so 32 concurrent
/// exchanges leave ample headroom while bounding slowloris resource use.
const METRICS_MAX_CONNECTIONS: usize = 32;
const METRICS_IO_TIMEOUT: Duration = Duration::from_secs(2);
const METRICS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const READY_MAX_PULSE_AGE: Duration = Duration::from_secs(2);
/// A live MySQL-backed server owns a dedicated advisory-lock session. Polling
/// bounds the interval in which a broken DB connection could leave the server
/// running after MySQL has released its process-lifetime exclusion lease.
const RUNTIME_LEASE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const RUNTIME_LEASE_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
/// EX_TEMPFAIL is reserved here as the clean, explicit "restart me" status.
/// Normal operator/service stops return zero; unexpected errors return one.
const PROCESS_RESTART_EXIT_CODE: u8 = 75;

fn process_exit_status(disposition: state::ProcessDisposition) -> u8 {
    match disposition {
        state::ProcessDisposition::Stop => 0,
        state::ProcessDisposition::Restart => PROCESS_RESTART_EXIT_CODE,
    }
}

fn process_exit_code(disposition: state::ProcessDisposition) -> std::process::ExitCode {
    std::process::ExitCode::from(process_exit_status(disposition))
}

#[derive(Clone, Copy)]
struct MetricsHttpTimeouts {
    io: Duration,
    request: Duration,
}

const METRICS_TIMEOUTS: MetricsHttpTimeouts = MetricsHttpTimeouts {
    io: METRICS_IO_TIMEOUT,
    request: METRICS_REQUEST_TIMEOUT,
};

#[derive(Debug, PartialEq, Eq)]
enum MetricsConnectionOutcome {
    Complete,
    IoTimedOut,
    RequestTimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CopyoverArgs {
    port: u16,
    listener_fd: RawFd,
}

/// Strictly parse the `--copyover <port> <listener_fd>` argv tail produced by
/// do_copyover. Once the flag is present, missing/junk/unsafe numeric fields are
/// fatal rather than silently turning recovery into a fresh boot.
fn parse_copyover_args_from(args: &[String]) -> Result<Option<CopyoverArgs>> {
    let positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == "--copyover").then_some(index))
        .collect();
    let Some(&position) = positions.first() else {
        return Ok(None);
    };
    if positions.len() != 1 {
        bail!("copyover arguments contain duplicate --copyover flags");
    }
    let port = args
        .get(position + 1)
        .ok_or_else(|| anyhow::anyhow!("copyover arguments are missing the port"))?
        .parse::<u16>()
        .context("copyover port is not a valid u16")?;
    if port == 0 {
        bail!("copyover port must be nonzero");
    }
    let listener_fd = args
        .get(position + 2)
        .ok_or_else(|| anyhow::anyhow!("copyover arguments are missing the listener fd"))?
        .parse::<RawFd>()
        .context("copyover listener fd is not a valid integer")?;
    if listener_fd < 3 {
        bail!("copyover listener fd is reserved or invalid");
    }
    Ok(Some(CopyoverArgs { port, listener_fd }))
}

fn parse_bootstrap_implementor_from(args: &[String]) -> Result<Option<String>> {
    let positions: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == "--bootstrap-implementor").then_some(index))
        .collect();
    let Some(&position) = positions.first() else {
        return Ok(None);
    };
    if positions.len() != 1 {
        bail!("bootstrap arguments contain duplicate --bootstrap-implementor flags");
    }
    if args.iter().any(|arg| arg == "--copyover") {
        bail!("bootstrap and copyover modes cannot be combined");
    }
    let name = args
        .get(position + 1)
        .ok_or_else(|| anyhow::anyhow!("--bootstrap-implementor requires a character name"))?;
    if !(2..=20).contains(&name.len()) || !name.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        bail!("bootstrap character name must be 2-20 ASCII letters");
    }
    if args.get(position + 2).is_some() {
        bail!("unexpected arguments after bootstrap character name");
    }
    Ok(Some(name.clone()))
}

fn parse_migrate_from(args: &[String]) -> Result<bool> {
    let count = args
        .iter()
        .filter(|arg| arg.as_str() == "--migrate")
        .count();
    if count == 0 {
        return Ok(false);
    }
    if count != 1 {
        bail!("migration arguments contain duplicate --migrate flags");
    }
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--copyover" | "--bootstrap-implementor"))
    {
        bail!("migration, bootstrap, and copyover modes are mutually exclusive");
    }
    if args.len() != 2 {
        bail!("--migrate does not accept additional arguments");
    }
    Ok(true)
}

fn parse_metrics_addr(port: Option<&str>, bind: Option<&str>) -> Result<Option<SocketAddr>> {
    let Some(port) = port else {
        if bind.is_some() {
            bail!("MUD_METRICS_BIND requires MUD_METRICS_PORT");
        }
        return Ok(None);
    };
    let port = port
        .parse::<u16>()
        .context("MUD_METRICS_PORT must be a nonzero u16")?;
    if port == 0 {
        bail!("MUD_METRICS_PORT must be a nonzero u16");
    }
    let bind = bind.unwrap_or("127.0.0.1");
    let ip = bind
        .parse::<IpAddr>()
        .context("MUD_METRICS_BIND must be a valid IPv4 or IPv6 address")?;
    Ok(Some(SocketAddr::new(ip, port)))
}

async fn bootstrap_implementor(db: &Arc<dyn DatabaseInterface>, name: &str) -> Result<()> {
    match db
        .bootstrap_implementor(name)
        .await
        .with_context(|| format!("bootstrap Implementor identity {name}"))?
    {
        ImplementorBootstrapOutcome::Promoted => Ok(()),
        ImplementorBootstrapOutcome::AlreadyExists(implementor) => bail!(
            "an Implementor already exists ({}); use authenticated in-game administration",
            implementor
        ),
        ImplementorBootstrapOutcome::TargetNotFound => {
            bail!("bootstrap character {name} is not a durable player character")
        }
    }
}

/// Verify every durable player row before socket adoption. Only an explicitly
/// ephemeral mock database may be rebuilt from the checked snapshot; a missing
/// production row is a recovery failure and is never silently created with the
/// mock copyover password.
async fn prepare_copyover_database(
    db: &Arc<dyn DatabaseInterface>,
    snapshot: &copyover::SnapshotPayload,
    allow_ephemeral_reseed: bool,
) -> Result<()> {
    for entry in &snapshot.entries {
        let name = &entry.character.name;
        if db.player_exists(name).await? {
            let durable = db.load_player(name).await?;
            if durable.idnum != entry.character.idnum {
                bail!("copyover durable player id mismatch for {name}");
            }
            continue;
        }
        if !allow_ephemeral_reseed {
            bail!("copyover durable player row is missing for {name}");
        }
        let character = entry.character.to_character();
        db.create_player(&character, "!copyover!").await?;
        db.save_player(&character).await?;
    }
    Ok(())
}

/// Validate the complete versioned/checksummed recovery set before unlinking
/// it or wrapping any inherited client socket.
fn read_copyover_state(lib_path: &str, listener_fd: RawFd) -> Result<copyover::RecoverySnapshot> {
    let path = std::path::Path::new(lib_path).join("copyover.dat");
    copyover::RecoverySnapshot::open(&path, listener_fd)
}

struct PreparedRecoveredConnection {
    snapshot: copyover::ConnectionSnapshot,
    stream: tokio::net::TcpStream,
}

/// Take ownership only after `validate_inherited_fds` has accepted the entire
/// set. Any conversion failure drops already-prepared streams, returns an error,
/// and leaves the RecoverySnapshot uncommitted on disk.
fn prepare_recovered_connections(
    snapshot: &copyover::SnapshotPayload,
) -> Result<Vec<PreparedRecoveredConnection>> {
    let mut prepared = Vec::with_capacity(snapshot.entries.len());
    for entry in &snapshot.entries {
        // SAFETY: the complete set was checked with non-owning fcntl/getsockopt
        // calls, and each structurally unique client fd is adopted exactly once.
        let std_stream = unsafe { std::net::TcpStream::from_raw_fd(entry.fd) };
        std_stream
            .set_nonblocking(true)
            .with_context(|| format!("set copyover client fd {} nonblocking", entry.fd))?;
        let stream = tokio::net::TcpStream::from_std(std_stream)
            .with_context(|| format!("adopt copyover client fd {}", entry.fd))?;
        prepared.push(PreparedRecoveredConnection {
            snapshot: entry.clone(),
            stream,
        });
    }
    Ok(prepared)
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
async fn handle_metrics_connection<S>(
    mut sock: S,
    metrics: Arc<metrics::Metrics>,
    who_snapshot: Arc<std::sync::RwLock<String>>,
    timeouts: MetricsHttpTimeouts,
) -> MetricsConnectionOutcome
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let exchange = async {
        // Read just enough to see the request line. A real scrape sends a short
        // request; cap the read and its duration so a slow client cannot pin us.
        let mut buf = [0u8; 1024];
        let n = match timeout(timeouts.io, sock.read(&mut buf)).await {
            Err(_) => return MetricsConnectionOutcome::IoTimedOut,
            Ok(Ok(0)) | Ok(Err(_)) => return MetricsConnectionOutcome::Complete,
            Ok(Ok(n)) => n,
        };
        let req = String::from_utf8_lossy(&buf[..n]);
        let request_line = req.lines().next().unwrap_or_default();
        let mut fields = request_line.split_whitespace();
        let method = fields.next();
        let path = fields.next();
        let version = fields.next();
        let valid_version = matches!(version, Some("HTTP/1.0" | "HTTP/1.1"));
        let valid_shape = method.is_some()
            && path.is_some()
            && valid_version
            && fields.next().is_none()
            && (n < buf.len() || request_line.ends_with('\n'));

        let (status, ctype, body) = if !valid_shape {
            (
                "400 Bad Request",
                "text/plain; version=0.0.4",
                "bad request\n".to_string(),
            )
        } else if method != Some("GET") {
            (
                "405 Method Not Allowed",
                "text/plain; version=0.0.4",
                "method not allowed\n".to_string(),
            )
        } else if path == Some("/metrics") || path.is_some_and(|path| path.starts_with("/metrics?"))
        {
            (
                "200 OK",
                "text/plain; version=0.0.4",
                metrics.render_prometheus(),
            )
        } else if path == Some("/live") || path.is_some_and(|path| path.starts_with("/live?")) {
            ("200 OK", "text/plain; version=0.0.4", "live\n".to_string())
        } else if path == Some("/health") || path.is_some_and(|path| path.starts_with("/health?")) {
            (
                "200 OK",
                "text/plain; version=0.0.4",
                format!("ok\nplayers {}\n", metrics.players_now()),
            )
        } else if path == Some("/ready") || path.is_some_and(|path| path.starts_with("/ready?")) {
            match metrics.readiness(READY_MAX_PULSE_AGE) {
                Ok(age) => (
                    "200 OK",
                    "text/plain; version=0.0.4",
                    format!(
                        "ready\npulse {}\nage_ms {}\n",
                        metrics.pulse.load(std::sync::atomic::Ordering::Relaxed),
                        age.as_millis()
                    ),
                ),
                Err(reason) => (
                    "503 Service Unavailable",
                    "text/plain; version=0.0.4",
                    format!("not ready: {reason}\n"),
                ),
            }
        } else if path == Some("/api/who") || path.is_some_and(|path| path.starts_with("/api/who?"))
        {
            let snapshot = who_snapshot.read().map(|s| s.clone()).unwrap_or_default();
            let body = if snapshot.is_empty() {
                "{\"count\":0,\"players\":[]}".to_string()
            } else {
                snapshot
            };
            ("200 OK", "application/json", body)
        } else {
            (
                "404 Not Found",
                "text/plain; version=0.0.4",
                "not found\n".to_string(),
            )
        };

        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            status = status,
            ctype = ctype,
            len = body.len(),
            body = body
        );
        match timeout(timeouts.io, sock.write_all(resp.as_bytes())).await {
            Err(_) => return MetricsConnectionOutcome::IoTimedOut,
            Ok(Err(_)) => return MetricsConnectionOutcome::Complete,
            Ok(Ok(())) => {}
        }
        match timeout(timeouts.io, sock.shutdown()).await {
            Err(_) => MetricsConnectionOutcome::IoTimedOut,
            Ok(_) => MetricsConnectionOutcome::Complete,
        }
    };

    match timeout(timeouts.request, exchange).await {
        Ok(outcome) => outcome,
        Err(_) => MetricsConnectionOutcome::RequestTimedOut,
    }
}

fn try_spawn_metrics_connection<S>(
    sock: S,
    metrics: Arc<metrics::Metrics>,
    who_snapshot: Arc<std::sync::RwLock<String>>,
    permits: &Arc<Semaphore>,
    timeouts: MetricsHttpTimeouts,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let permit = match Arc::clone(permits).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            metrics.inc_metrics_rejected();
            return false;
        }
    };

    tokio::spawn(async move {
        let _permit = permit;
        let outcome =
            handle_metrics_connection(sock, metrics.clone(), who_snapshot, timeouts).await;
        if matches!(
            outcome,
            MetricsConnectionOutcome::IoTimedOut | MetricsConnectionOutcome::RequestTimedOut
        ) {
            metrics.inc_metrics_timeout();
        }
    });
    true
}

async fn serve_metrics(
    listener: TcpListener,
    metrics: Arc<metrics::Metrics>,
    who_snapshot: Arc<std::sync::RwLock<String>>,
) {
    let permits = Arc::new(Semaphore::new(METRICS_MAX_CONNECTIONS));
    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                warn!("metrics accept() error: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !try_spawn_metrics_connection(
            sock,
            metrics.clone(),
            who_snapshot.clone(),
            &permits,
            METRICS_TIMEOUTS,
        ) {
            warn!(
                "Metrics connection limit ({}) reached; rejecting {}",
                METRICS_MAX_CONNECTIONS, peer
            );
        }
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

/// Exact permission values granted by the historical `do_advance` path when a
/// mortal reaches Implementor. Shared by the durable and mock targeted update
/// implementations so bootstrap never needs a broad Character save.
pub(crate) fn implementor_command_grants() -> (i64, i64, i64, i64) {
    let (mut godcmds1, mut godcmds2, mut godcmds3, mut godcmds4) = (0, 0, 0, 0);
    gcmd::grant_advance(
        &mut godcmds1,
        &mut godcmds2,
        &mut godcmds3,
        &mut godcmds4,
        types::LVL_IMPL,
        types::LVL_IMMORT,
        types::LVL_IMPL,
    );
    (godcmds1, godcmds2, godcmds3, godcmds4)
}

async fn run_server() -> Result<state::ProcessDisposition> {
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
    let process_args = std::env::args().collect::<Vec<_>>();
    let bootstrap_name = parse_bootstrap_implementor_from(&process_args)?;
    let migrate = parse_migrate_from(&process_args)?;
    let mut config = Config::from_env()?;

    // Faithful port of the C `-s` command-line flag (comm.c:272
    // `no_specials = 1`, "Suppressing assignment of special routines.").
    // `-q` is quickboot in C and must not suppress specials.
    apply_cli_flags(&mut config, process_args.iter().skip(1).cloned());

    // Parse and validate recovery evidence before database/world startup opens
    // more descriptors that a stale numeric fd could otherwise be confused
    // with. The inherited port is authoritative across the exec.
    let copyover_args = parse_copyover_args_from(&process_args)?;
    if let Some(args) = copyover_args {
        config.port = args.port;
    }
    let copyover_recovery = if let Some(args) = copyover_args {
        let recovery = read_copyover_state(&config.lib_path, args.listener_fd)?;
        copyover::validate_inherited_fds(recovery.payload())?;
        Some(recovery)
    } else {
        None
    };

    // Seed the mud clock from the effective lib path before any world/helper
    // can call time_now()/sunlight() and lock in Config::default().lib_path.
    weather::initialize_clock(&config.lib_path);

    let mut runtime_lease = None;
    let raw_db: Arc<dyn DatabaseInterface> = if config.use_mock_db {
        info!("Using in-memory mock database");
        Arc::new(mock_database::MockDatabase::new())
    } else {
        info!("Using MySQL database");
        let mysql = Arc::new(database::Database::new(&config.database_url)?);
        if !migrate && bootstrap_name.is_none() {
            runtime_lease = Some(
                timeout(
                    Duration::from_secs(config.db_timeout_secs),
                    mysql.acquire_runtime_lease(),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "database runtime exclusion lease acquisition timed out after {}s",
                        config.db_timeout_secs
                    )
                })??,
            );
            info!("Acquired database runtime exclusion lease");
        }
        mysql
    };
    let db: Arc<dyn DatabaseInterface> = Arc::new(database_timeout::TimedDatabase::new(
        raw_db,
        Duration::from_secs(config.db_timeout_secs),
    ));
    if migrate {
        if config.use_mock_db {
            bail!("--migrate requires the durable MySQL database");
        }
        db.init_tables().await?;
        info!(
            "Database migrated to schema version {}; server startup intentionally skipped",
            database::EXPECTED_SCHEMA_VERSION
        );
        return Ok(state::ProcessDisposition::Stop);
    }

    db.verify_schema().await?;

    if let Some(name) = bootstrap_name {
        if config.use_mock_db {
            bail!("--bootstrap-implementor requires the durable MySQL database");
        }
        bootstrap_implementor(&db, &name).await?;
        info!(
            "Promoted {} to Implementor; server startup intentionally skipped",
            name
        );
        return Ok(state::ProcessDisposition::Stop);
    }

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
    // Validate the durable OLC journal before any independent legacy loader
    // consumes an index. An unreadable marker set is a startup error: ignoring
    // it could expose a zone whose six files were only partly committed.
    let pending_new_zones = olc::pending_new_zone_publications(&config.lib_path)
        .context("validate durable new-zone publication gates")?;
    olc::register_pending_new_zone_publication_blockers(&pending_new_zones);
    for zone_number in &pending_new_zones {
        warn!(
            "New-zone publication {} is incomplete; all of its indexed components will stay hidden until an Implementor retries `zedit new {}`",
            zone_number, zone_number
        );
    }
    dg_scripts::boot_dg_scripts(&mut state, &config.lib_path);

    file_loader::FileLoader::load_world(&mut state, &config.lib_path)
        .await
        .context("load world")?;

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
    cmd_social::boot_socials(
        &mut state,
        Some(&format!("{}/misc/socials", config.lib_path)),
    )
    .context("load mandatory social command table")?;
    // db.c:299-300 index_boot(DB_BOOT_HLP) - serve the 73k-line help index
    // to the live `help` command (#232).
    hedit::boot_help_table(&config.lib_path).context("load mandatory help table")?;

    // Load the combat hit-messages (fight.c load_messages, lib/misc/messages):
    // flavourful per-skill / per-weapon death/hit/miss/god messages.
    fight_messages::load_messages(&mut state, &config.lib_path);

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
        .filter(|r| r.room_flags.contains(room::RoomFlags::DEATH) && r.map_x.is_none())
        .map(|r| r.number)
        .collect();
    spec_assign::set_death_trap_rooms(&mut state, death_rooms);

    // Build the vnum->special-procedure tables (spec_assign.c assign_*). Must
    // come after shops/boards/mail so their data is available to the procs.
    // C's boot_db() gates the whole assign_mobiles/shopkeepers/objects/rooms
    // block on `if (!no_specials)` (db.c:317); skipping it here leaves every
    // spec table empty, so the interpreter's special() walk and the MOB_SPEC
    // pulse both find nothing to dispatch — exactly the C behaviour.
    if !config.no_specials {
        spec_assign::assign_specs(&mut state);
    }

    // Build the in-memory player name<->idnum index (C build_player_index,
    // called from boot_db after the world loads). Lets offline players resolve
    // for `last`, ignore-by-name, mail, and name<->id lookups without an async
    // DB hit. Empty on a brand-new DB; kept fresh by update_player_index as
    // players are created / enter / save.
    // An empty index is valid only when the database successfully reports no
    // players. Treating a timeout/error as empty would disable offline-name
    // collision and authority checks for the whole process lifetime (#411).
    let pt = db
        .list_players()
        .await
        .context("load authoritative player name index")?;
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

    // Optional metrics/health HTTP endpoint. Invalid explicit configuration or
    // a bind failure is fatal: the release manager relies on /ready and must
    // never mistake a running-but-unobservable process for a healthy one.
    let metrics_port = std::env::var("MUD_METRICS_PORT").ok();
    let metrics_bind = std::env::var("MUD_METRICS_BIND").ok();
    let metrics_addr = parse_metrics_addr(metrics_port.as_deref(), metrics_bind.as_deref())?;
    let metrics_listener = match metrics_addr {
        Some(addr) => Some((
            TcpListener::bind(addr)
                .await
                .with_context(|| format!("bind metrics endpoint {addr}"))?,
            addr,
        )),
        None => None,
    };

    // Keep a handle to the shared DB for checked copyover preparation. Only an
    // explicitly configured in-memory mock may be re-seeded after exec.
    let db_for_recovery = db.clone();

    let mut game = game::Game::new(state, db);
    game.set_metrics(metrics.clone());
    game.set_who_snapshot(who_snapshot.clone());
    game.load_text_files(&lib_path).await;
    game.prime_zones();

    // Copyover detection (comm.c init_game: `if (!fCopyOver) init_socket(port)`).
    // When re-exec'd by do_copyover, the listener fd was inherited (FD_CLOEXEC
    // cleared before exec); rebuild the tokio listener from it instead of bind().
    let copyover_listener_fd = copyover_recovery
        .as_ref()
        .map(|recovery| recovery.payload().listener_fd);
    let listener = if let Some(lfd) = copyover_listener_fd {
        info!("Copyover recovery: inheriting listener fd {}", lfd);
        // SAFETY: lfd was a live listening socket in the previous image; execv
        // kept it open (CLOEXEC cleared) and nothing else owns it now.
        let std_listener = unsafe { std::net::TcpListener::from_raw_fd(lfd) };
        std_listener.set_nonblocking(true)?;
        TcpListener::from_std(std_listener)?
    } else {
        let addr = std::net::SocketAddr::new(config.bind_ip, config.port);
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

    // World/database boot may be lengthy. Re-prove ownership immediately
    // before readiness/acceptance so a lease session lost during startup can
    // never expose a server built concurrently with offline maintenance.
    if let Some(lease) = runtime_lease.as_mut() {
        timeout(RUNTIME_LEASE_CHECK_TIMEOUT, lease.verify_owned())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "runtime database exclusion lease pre-readiness check timed out after {}s",
                    RUNTIME_LEASE_CHECK_TIMEOUT.as_secs_f64()
                )
            })??;
    }

    // World content, database/index state, both configured listeners, and the
    // Game object are now initialized. Only now may /ready become green.
    metrics.mark_boot_complete();
    if let Some((mlistener, addr)) = metrics_listener {
        info!(
            "Metrics endpoint listening on {} (/metrics, /live, /ready, /api/who)",
            addr
        );
        let m = metrics.clone();
        let who = who_snapshot.clone();
        tokio::spawn(async move { serve_metrics(mlistener, m, who).await });
    }
    // `run` returns only after its graceful shutdown path, or with a real
    // failure that the process supervisor must propagate rather than
    // misclassifying as a clean exit.
    let mut game_handle = tokio::spawn(async move { game.run(game_rx).await });

    let mut next_conn: u64 = 1;
    let mut client_tasks = tokio::task::JoinSet::new();

    // If we came up via copyover, re-attach every previously-playing socket.
    if let Some(recovery) = copyover_recovery {
        prepare_copyover_database(&db_for_recovery, recovery.payload(), config.use_mock_db).await?;
        let prepared = prepare_recovered_connections(recovery.payload())?;
        info!(
            "Copyover recovery: {} player socket(s) to restore",
            prepared.len()
        );
        // All validation, DB work, nonblocking conversion, and Tokio wrapping
        // succeeded for the complete set. This is the sole evidence-unlink.
        recovery.commit()?;
        for recovered in prepared {
            let e = recovered.snapshot;
            let id = ConnId(next_conn);
            next_conn += 1;
            let name = e.character.name;
            let tx = game_tx.clone();
            client_tasks.spawn(async move {
                if let Err(err) = connection::handle_recovered(
                    recovered.stream,
                    id,
                    e.fd,
                    name.clone(),
                    e.host,
                    tx,
                )
                .await
                {
                    warn!("recovered client {} error: {}", name, err);
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
    let reverse_dns = connection::ReverseDnsConfig {
        enabled: config.reverse_dns,
        timeout: Duration::from_millis(config.reverse_dns_timeout_ms),
    };
    let resolver_slots = Arc::new(Semaphore::new(config.reverse_dns_max_inflight));
    info!(
        "Reverse DNS: {}, timeout={}ms, max_inflight={}",
        if reverse_dns.enabled {
            "FCrDNS enabled"
        } else {
            "disabled"
        },
        reverse_dns.timeout.as_millis(),
        config.reverse_dns_max_inflight
    );
    // Per-IP recent-connection timestamps for the sliding-window rate limit.
    // Pruned lazily so the map can't grow without bound.
    let mut recent_connects: HashMap<IpAddr, Vec<Instant>> = HashMap::new();

    // Main is the sole OS-signal owner. It forwards stop requests through the
    // Game queue so failed OLC persistence can deliberately abort shutdown and
    // keep the server online without a process-level timeout killing the task.
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let mut external_shutdown_requested = false;
    let mut shutdown_error = None;
    let mut process_disposition = state::ProcessDisposition::Stop;
    let mut runtime_lease_tick = tokio::time::interval(RUNTIME_LEASE_CHECK_INTERVAL);
    runtime_lease_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` fires immediately; acquisition already proved ownership, so
    // begin health checks after one real interval instead of issuing a bonus
    // startup query.
    runtime_lease_tick.tick().await;

    loop {
        let sigterm_fut = async {
            match sigterm.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };

        let (mut stream, peer) = tokio::select! {
            res = listener.accept() => match res {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("accept() error: {}", e);
                    continue;
                }
            },
            _ = tokio::signal::ctrl_c() => {
                info!("main: received Ctrl-C; requesting an OLC-safe graceful shutdown.");
                external_shutdown_requested = true;
                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                if game_tx.send(connection::GameMessage::SystemShutdown { result_tx }).await.is_err() {
                    warn!("game task command channel closed while forwarding Ctrl-C");
                } else if matches!(
                    result_rx.await,
                    Ok(connection::SystemShutdownResult::Refused)
                ) {
                    external_shutdown_requested = false;
                }
                continue;
            }
            _ = sigterm_fut => {
                info!("main: received SIGTERM; requesting an OLC-safe graceful shutdown.");
                external_shutdown_requested = true;
                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                if game_tx.send(connection::GameMessage::SystemShutdown { result_tx }).await.is_err() {
                    warn!("game task command channel closed while forwarding SIGTERM");
                } else if matches!(
                    result_rx.await,
                    Ok(connection::SystemShutdownResult::Refused)
                ) {
                    external_shutdown_requested = false;
                }
                continue;
            }
            result = &mut game_handle => {
                match result {
                    Ok(Ok(disposition)) => {
                        process_disposition = if external_shutdown_requested {
                            state::ProcessDisposition::Stop
                        } else {
                            disposition
                        };
                    }
                    Ok(Err(error)) => {
                        warn!("game task ended with an error: {error:#}");
                        shutdown_error = Some(error);
                    }
                    Err(error) => {
                        warn!("game task ended unexpectedly: {error}");
                        shutdown_error = Some(anyhow::anyhow!("game task failed to join: {error}"));
                    }
                }
                break;
            }
            result = client_tasks.join_next(), if !client_tasks.is_empty() => {
                if let Some(Err(error)) = result {
                    warn!("client task failed: {}", error);
                }
                continue;
            }
            _ = runtime_lease_tick.tick(), if runtime_lease.is_some() => {
                let verification = timeout(
                    RUNTIME_LEASE_CHECK_TIMEOUT,
                    runtime_lease
                        .as_mut()
                        .expect("select guard requires a runtime lease")
                        .verify_owned(),
                )
                .await
                .map_err(|_| anyhow::anyhow!(
                    "runtime database exclusion lease verification timed out after {}s",
                    RUNTIME_LEASE_CHECK_TIMEOUT.as_secs_f64()
                ))
                .and_then(|result| result);
                if let Err(error) = verification {
                    // Once exclusion is uncertain, do not perform a DB-backed
                    // graceful save: an offline maintenance process may now be
                    // changing the same rows. Stop the world task immediately
                    // and let the supervisor surface a hard failure.
                    warn!(
                        "Runtime database exclusion lease was lost; stopping immediately: {error:#}"
                    );
                    metrics.mark_not_ready();
                    game_handle.abort();
                    let _ = (&mut game_handle).await;
                    shutdown_error = Some(error.context(
                        "runtime database exclusion lease lost; server stopped fail-closed"
                    ));
                    break;
                }
                continue;
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
        if connection::reject_ban_all(&mut stream, &connection::PeerIdentity::numeric(ip)).await {
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
        let resolver_slots = Arc::clone(&resolver_slots);
        client_tasks.spawn(async move {
            // Hold the permit for the lifetime of the connection; dropping it on
            // task exit frees a slot for the next client.
            let _permit = permit;
            if let Err(e) =
                connection::handle_client(stream, peer, id, tx, reverse_dns, resolver_slots).await
            {
                warn!("client {} error: {}", peer, e);
            }
        });
    }

    client_tasks.abort_all();
    while let Some(result) = client_tasks.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                warn!("client task failed during shutdown: {}", error);
            }
        }
    }
    if let Some(lease) = runtime_lease.take() {
        let release_result = timeout(RUNTIME_LEASE_CHECK_TIMEOUT, lease.release())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "runtime database exclusion lease release timed out after {}s",
                    RUNTIME_LEASE_CHECK_TIMEOUT.as_secs_f64()
                )
            })
            .and_then(|result| result);
        if let Err(error) = release_result {
            warn!("Runtime database exclusion lease release failed: {error:#}");
            if shutdown_error.is_none() {
                shutdown_error = Some(error);
            }
        }
    }
    info!("Server exiting.");
    match shutdown_error {
        Some(error) => Err(error),
        None => Ok(process_disposition),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run_server().await {
        Ok(disposition) => process_exit_code(disposition),
        Err(error) => {
            log::error!("server failed: {error:#}");
            eprintln!("Error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_disposition_has_distinct_systemd_exit_statuses() {
        assert_eq!(process_exit_status(state::ProcessDisposition::Stop), 0);
        assert_eq!(
            process_exit_status(state::ProcessDisposition::Restart),
            PROCESS_RESTART_EXIT_CODE
        );
        assert_ne!(PROCESS_RESTART_EXIT_CODE, 0);
    }

    #[tokio::test]
    async fn metrics_connection_limit_rejects_excess_and_reclaims_permits() {
        const TEST_LIMIT: usize = METRICS_MAX_CONNECTIONS;
        const TEST_TIMEOUTS: MetricsHttpTimeouts = MetricsHttpTimeouts {
            io: Duration::from_secs(5),
            request: Duration::from_secs(5),
        };
        assert_eq!(METRICS_MAX_CONNECTIONS, 32);
        assert_eq!(METRICS_IO_TIMEOUT, Duration::from_secs(2));
        assert_eq!(METRICS_REQUEST_TIMEOUT, Duration::from_secs(2));

        let permits = Arc::new(Semaphore::new(TEST_LIMIT));
        let metrics = Arc::new(metrics::Metrics::new());
        let who_snapshot = Arc::new(std::sync::RwLock::new(String::new()));
        let mut clients = Vec::with_capacity(TEST_LIMIT);
        for _ in 0..TEST_LIMIT {
            let (client, server) = tokio::io::duplex(64);
            assert!(try_spawn_metrics_connection(
                server,
                metrics.clone(),
                who_snapshot.clone(),
                &permits,
                TEST_TIMEOUTS,
            ));
            clients.push(client);
        }
        let (mut excess_client, excess_server) = tokio::io::duplex(64);
        assert_eq!(permits.available_permits(), 0);
        assert!(!try_spawn_metrics_connection(
            excess_server,
            metrics.clone(),
            who_snapshot.clone(),
            &permits,
            TEST_TIMEOUTS,
        ));
        assert_eq!(
            metrics
                .metrics_rejected_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );

        let mut byte = [0u8; 1];
        let rejected_read = timeout(Duration::from_secs(1), excess_client.read(&mut byte))
            .await
            .expect("rejected metrics stream did not close");
        assert_eq!(rejected_read.expect("rejected stream read failed"), 0);

        drop(clients);
        let all_permits = timeout(
            Duration::from_secs(2),
            Arc::clone(&permits).acquire_many_owned(TEST_LIMIT as u32),
        )
        .await
        .expect("metrics permits were not reclaimed")
        .expect("metrics semaphore unexpectedly closed");
        drop(all_permits);
        assert_eq!(permits.available_permits(), TEST_LIMIT);
    }

    #[tokio::test]
    async fn metrics_connection_enforces_read_write_and_request_timeouts() {
        const SHORT_IO: MetricsHttpTimeouts = MetricsHttpTimeouts {
            io: Duration::from_millis(20),
            request: Duration::from_millis(250),
        };
        const SHORT_REQUEST: MetricsHttpTimeouts = MetricsHttpTimeouts {
            io: Duration::from_millis(250),
            request: Duration::from_millis(20),
        };
        let metrics = Arc::new(metrics::Metrics::new());
        let who_snapshot = Arc::new(std::sync::RwLock::new(String::new()));

        let permits = Arc::new(Semaphore::new(1));
        let (_idle_client, idle_server) = tokio::io::duplex(64);
        assert!(try_spawn_metrics_connection(
            idle_server,
            metrics.clone(),
            who_snapshot.clone(),
            &permits,
            SHORT_IO,
        ));
        let reclaimed_permit =
            timeout(Duration::from_secs(1), Arc::clone(&permits).acquire_owned())
                .await
                .expect("idle metrics connection retained its permit past the timeout")
                .expect("metrics semaphore unexpectedly closed");
        drop(reclaimed_permit);
        assert_eq!(
            metrics
                .metrics_timeouts_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );

        let (_idle_client, idle_server) = tokio::io::duplex(64);
        let read_outcome = timeout(
            Duration::from_secs(1),
            handle_metrics_connection(idle_server, metrics.clone(), who_snapshot.clone(), SHORT_IO),
        )
        .await
        .expect("idle metrics read exceeded its outer test deadline");
        assert_eq!(read_outcome, MetricsConnectionOutcome::IoTimedOut);

        let (mut blocked_client, blocked_server) = tokio::io::duplex(64);
        blocked_client
            .write_all(b"GET /metrics HTTP/1.1\r\n\r\n")
            .await
            .expect("could not seed metrics request");
        let write_outcome = timeout(
            Duration::from_secs(1),
            handle_metrics_connection(
                blocked_server,
                metrics.clone(),
                who_snapshot.clone(),
                SHORT_IO,
            ),
        )
        .await
        .expect("blocked metrics write exceeded its outer test deadline");
        assert_eq!(write_outcome, MetricsConnectionOutcome::IoTimedOut);

        let (_idle_client, idle_server) = tokio::io::duplex(64);
        let request_outcome = timeout(
            Duration::from_secs(1),
            handle_metrics_connection(idle_server, metrics, who_snapshot, SHORT_REQUEST),
        )
        .await
        .expect("metrics exchange exceeded its outer test deadline");
        assert_eq!(request_outcome, MetricsConnectionOutcome::RequestTimedOut);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_metrics_clients_do_not_stall_game_pulses_or_the_next_scrape() {
        const TEST_LIMIT: usize = METRICS_MAX_CONNECTIONS;
        const SATURATION_TIMEOUTS: MetricsHttpTimeouts = MetricsHttpTimeouts {
            io: Duration::from_millis(750),
            request: Duration::from_secs(1),
        };

        let permits = Arc::new(Semaphore::new(TEST_LIMIT));
        let metrics = Arc::new(metrics::Metrics::new());
        let who_snapshot = Arc::new(std::sync::RwLock::new(String::new()));

        let db: Arc<dyn DatabaseInterface> = Arc::new(mock_database::MockDatabase::new());
        let mut game = game::Game::new(state::GameState::new(Config::default()), db);
        game.set_metrics(metrics.clone());
        let (_game_tx, game_rx) = mpsc::channel(1);
        let game_task = tokio::spawn(async move { game.run(game_rx).await });

        // Half of the clients never send a request (slowloris); the other half
        // send a valid request but never read the response. Keeping every peer
        // open forces the server-side read/write deadlines to reclaim capacity.
        let mut stalled_clients = Vec::with_capacity(TEST_LIMIT);
        for index in 0..TEST_LIMIT {
            let (mut client, server) = tokio::io::duplex(64);
            if index % 2 == 1 {
                client
                    .write_all(b"GET /metrics HTTP/1.1\r\n\r\n")
                    .await
                    .expect("could not seed non-reading metrics client");
            }
            assert!(try_spawn_metrics_connection(
                server,
                metrics.clone(),
                who_snapshot.clone(),
                &permits,
                SATURATION_TIMEOUTS,
            ));
            stalled_clients.push(client);
        }
        assert_eq!(permits.available_permits(), 0);

        let pulse_while_saturated = timeout(Duration::from_millis(500), async {
            loop {
                let pulse = metrics.pulse.load(std::sync::atomic::Ordering::Relaxed);
                if pulse >= 3 {
                    break pulse;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("saturated metrics tasks stalled the game heartbeat");
        assert_eq!(
            permits.available_permits(),
            0,
            "the heartbeat proof must complete while metrics capacity is saturated"
        );

        let all_permits = timeout(
            Duration::from_secs(2),
            Arc::clone(&permits).acquire_many_owned(TEST_LIMIT as u32),
        )
        .await
        .expect("metrics deadlines did not reclaim all saturated permits")
        .expect("metrics semaphore unexpectedly closed");
        drop(all_permits);
        assert_eq!(
            metrics
                .metrics_timeouts_total
                .load(std::sync::atomic::Ordering::Relaxed),
            TEST_LIMIT as u64
        );

        let (mut scrape_client, scrape_server) = tokio::io::duplex(16 * 1024);
        assert!(try_spawn_metrics_connection(
            scrape_server,
            metrics.clone(),
            who_snapshot,
            &permits,
            SATURATION_TIMEOUTS,
        ));
        scrape_client
            .write_all(b"GET /metrics HTTP/1.1\r\n\r\n")
            .await
            .expect("could not send the recovery scrape");
        let mut response = Vec::new();
        timeout(
            Duration::from_secs(1),
            scrape_client.read_to_end(&mut response),
        )
        .await
        .expect("valid scrape did not complete after permit reclamation")
        .expect("valid scrape read failed");
        let response = String::from_utf8(response).expect("metrics response was not UTF-8");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let exposed_pulse = response
            .lines()
            .find_map(|line| line.strip_prefix("deltamud_pulse "))
            .expect("metrics response omitted the pulse counter")
            .parse::<u64>()
            .expect("metrics pulse was not numeric");
        assert!(exposed_pulse >= pulse_while_saturated);

        game_task.abort();
        let _ = game_task.await;
        drop(stalled_clients);
    }

    async fn metrics_response_with(request: &[u8], metrics: Arc<metrics::Metrics>) -> String {
        let who_snapshot = Arc::new(std::sync::RwLock::new(String::new()));
        let (mut client, server) = tokio::io::duplex(4096);
        client.write_all(request).await.unwrap();
        handle_metrics_connection(server, metrics, who_snapshot, METRICS_TIMEOUTS).await;
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    }

    async fn metrics_response(request: &[u8]) -> String {
        metrics_response_with(request, Arc::new(metrics::Metrics::new())).await
    }

    #[tokio::test]
    async fn metrics_http_rejects_bad_shape_and_unsupported_methods() {
        let malformed = metrics_response(b"GET /health\r\n\r\n").await;
        assert!(malformed.starts_with("HTTP/1.1 400 Bad Request\r\n"));

        let post = metrics_response(b"POST /metrics HTTP/1.1\r\n\r\n").await;
        assert!(post.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));

        let missing = metrics_response(b"GET /missing HTTP/1.1\r\n\r\n").await;
        assert!(missing.starts_with("HTTP/1.1 404 Not Found\r\n"));

        let healthy = metrics_response(b"GET /health HTTP/1.1\r\n\r\n").await;
        assert!(healthy.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(healthy.ends_with("ok\nplayers 0\n"));

        let live = metrics_response(b"GET /live HTTP/1.1\r\n\r\n").await;
        assert!(live.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(live.ends_with("live\n"));

        let not_ready = metrics_response(b"GET /ready HTTP/1.1\r\n\r\n").await;
        assert!(not_ready.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(not_ready.ends_with("not ready: boot incomplete\n"));

        let metrics = Arc::new(metrics::Metrics::new());
        metrics.mark_boot_complete();
        metrics.set_pulse(1);
        let ready = metrics_response_with(b"GET /ready HTTP/1.1\r\n\r\n", metrics).await;
        assert!(ready.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(ready.contains("\r\n\r\nready\npulse 1\nage_ms "));
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

    #[test]
    fn bootstrap_arguments_are_explicit_and_exclusive() {
        let strings = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parse_bootstrap_implementor_from(&strings(&[
                "mud",
                "--bootstrap-implementor",
                "Founder"
            ]))
            .unwrap(),
            Some("Founder".to_string())
        );
        assert!(
            parse_bootstrap_implementor_from(&strings(&["mud", "--bootstrap-implementor"]))
                .is_err()
        );
        assert!(
            parse_bootstrap_implementor_from(&strings(&["mud", "--bootstrap-implementor", "Bad1"]))
                .is_err()
        );
        assert!(
            parse_bootstrap_implementor_from(&strings(&[
                "mud",
                "--bootstrap-implementor",
                "Founder",
                "--copyover",
                "4000",
                "7"
            ]))
            .is_err()
        );
    }

    #[test]
    fn metrics_configuration_is_optional_strict_and_loopback_by_default() {
        assert_eq!(parse_metrics_addr(None, None).unwrap(), None);
        assert_eq!(
            parse_metrics_addr(Some("19595"), None).unwrap(),
            Some("127.0.0.1:19595".parse().unwrap())
        );
        assert_eq!(
            parse_metrics_addr(Some("19595"), Some("::1")).unwrap(),
            Some("[::1]:19595".parse().unwrap())
        );
        for invalid in [
            parse_metrics_addr(None, Some("127.0.0.1")),
            parse_metrics_addr(Some("0"), None),
            parse_metrics_addr(Some("not-a-port"), None),
            parse_metrics_addr(Some("19595"), Some("localhost")),
        ] {
            assert!(invalid.is_err());
        }
    }

    #[tokio::test]
    async fn explicit_bootstrap_promotes_once_and_refuses_a_second_implementor() {
        let mock = Arc::new(mock_database::MockDatabase::new());
        let mut founder = character::Character::new_player(
            "Founder".to_string(),
            types::Class::Warrior,
            types::Race::Human,
        );
        founder.idnum = mock.create_player(&founder, "secret").await.unwrap();
        let mut successor = character::Character::new_player(
            "Successor".to_string(),
            types::Class::Cleric,
            types::Race::Human,
        );
        successor.idnum = mock
            .create_player(&successor, "another-secret")
            .await
            .unwrap();
        let db: Arc<dyn DatabaseInterface> = mock.clone();

        bootstrap_implementor(&db, "Founder").await.unwrap();
        let promoted = mock.load_player("Founder").await.unwrap();
        assert_eq!(promoted.player.level, types::LVL_IMPL);
        assert_eq!(promoted.trust, i32::from(types::LVL_IMPL));
        assert_eq!(promoted.player.title.as_deref(), Some("the Implementor"));
        assert_ne!(
            promoted.godcmds1 | promoted.godcmds2 | promoted.godcmds3 | promoted.godcmds4,
            0
        );

        let error = bootstrap_implementor(&db, "Successor")
            .await
            .expect_err("a second Implementor must require in-game administration");
        assert!(error.to_string().contains("already exists"));
        assert_eq!(mock.load_player("Successor").await.unwrap().player.level, 1);
    }

    #[test]
    fn copyover_arguments_are_strict_once_recovery_is_requested() {
        let strings = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(parse_copyover_args_from(&strings(&["mud"])).unwrap(), None);
        assert_eq!(
            parse_copyover_args_from(&strings(&["mud", "--copyover", "4000", "7"])).unwrap(),
            Some(CopyoverArgs {
                port: 4000,
                listener_fd: 7,
            })
        );
        assert!(parse_copyover_args_from(&strings(&["mud", "--copyover"])).is_err());
        assert!(parse_copyover_args_from(&strings(&["mud", "--copyover", "junk", "7"])).is_err());
        assert!(parse_copyover_args_from(&strings(&["mud", "--copyover", "4000", "0"])).is_err());
        assert!(
            parse_copyover_args_from(&strings(&[
                "mud",
                "--copyover",
                "4000",
                "7",
                "--copyover",
                "4000",
                "8",
            ]))
            .is_err()
        );
    }

    fn recovery_payload() -> copyover::SnapshotPayload {
        let mut character = character::Character::new_player(
            "Recovery".to_string(),
            types::Class::Warrior,
            types::Race::Human,
        );
        character.idnum = 42;
        copyover::SnapshotPayload {
            listener_fd: 7,
            entries: vec![copyover::ConnectionSnapshot {
                fd: 8,
                host: "example.test".to_string(),
                character: copyover::CharacterSnapshot::from_character(&character),
            }],
        }
    }

    #[tokio::test]
    async fn production_copyover_never_reseeds_a_missing_player_row() {
        let mock = Arc::new(mock_database::MockDatabase::new());
        let db: Arc<dyn DatabaseInterface> = mock.clone();

        assert!(
            prepare_copyover_database(&db, &recovery_payload(), false)
                .await
                .is_err()
        );
        assert!(!mock.player_exists("Recovery").await.unwrap());

        prepare_copyover_database(&db, &recovery_payload(), true)
            .await
            .unwrap();
        let recovered = mock.load_player("Recovery").await.unwrap();
        assert_eq!(recovered.idnum, 42);
    }
}
