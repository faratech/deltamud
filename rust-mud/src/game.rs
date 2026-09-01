// Game: the async shell around the synchronous GameState. It owns the world,
// drains the input channel, runs commands/nanny to completion against
// &mut GameState, drives the heartbeat, and flushes each descriptor's output
// buffer to its writer task. This is the only place async meets the world.

use crate::character::Abilities;
use crate::combat;
use crate::connection::{render_color, ConState, Descriptor, GameMessage, QueuedInput};
use crate::interpreter::run_command;
use crate::metrics::Metrics;
use crate::state::GameState;
use crate::types::*;
use crate::DatabaseInterface;
use anyhow::Result;
use log::{error, info, warn};
use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

// Telnet ECHO negotiation (RFC 857). The server WILL-ECHO before a password
// prompt so the client suppresses its own local echo (cleartext password no
// longer appears on the user's screen), and WONT-ECHO when leaving the password
// state so normal local echo resumes. These three-byte control sequences must
// reach the socket verbatim; they are NOT routed through the outbuf/render_color
// String path (render_color iterates `.chars()`, which would mangle the lone
// 0xFF IAC byte). Instead they go straight down the per-conn output channel,
// exactly like connection.rs's negotiation-refusal path, which wraps raw telnet
// bytes via String::from_utf8_unchecked and relies on the writer only ever
// calling `.as_bytes()`.
const IAC_WILL_ECHO: [u8; 3] = [0xFF, 0xFB, 0x01]; // IAC WILL ECHO
const IAC_WONT_ECHO: [u8; 3] = [0xFF, 0xFC, 0x01]; // IAC WONT ECHO

// Telnet framing for out-of-band subnegotiations (GMCP/MSSP). A subneg is
// `IAC SB <opt> <payload> IAC SE`. These bytes, like the ECHO negotiation
// above, must reach the socket verbatim and so go down the raw-bytes channel,
// never through render_color (whose `.chars()` pass would mangle the lone 0xFF).
const IAC: u8 = 0xFF;
const SB: u8 = 0xFA; // Subnegotiation begin
const SE: u8 = 0xF0; // Subnegotiation end
const TELOPT_GMCP: u8 = 201; // Generic Mud Communication Protocol
const TELOPT_MSSP: u8 = 70; // Mud Server Status Protocol

// MSSP control bytes (Mud Server Status Protocol): each datum is
// `MSSP_VAR <name> MSSP_VAL <value>` inside the IAC SB MSSP ... IAC SE frame.
const MSSP_VAR: u8 = 1;
const MSSP_VAL: u8 = 2;

const PLR_SITEOK: i64 = 1 << 7;
// ---- C config.c:256-295: the login/menu strings, verbatim (#198) ----
pub const ANSI_QUESTION: &str = "\u{1b}[0;31;1mRED\u{1b}[31;0m \u{1b}[0;34;1mBLUE\u{1b}[34;0m \u{1b}[0;32;1mGREEN\u{1b}[32;0m\r\nIs the above text shown in color? ";

pub const MENU: &str = "\r\n\
&GWelcome to the DeltaMUD Menu&n\r\n\
&B------------------------------&n\r\n\
&R[&n&C0&n&R]&n Exit from DeltaMUD.\r\n\
&R[&n&C1&n&R]&n Enter the game.\r\n\
&R[&n&C2&n&R]&n Enter description.\r\n\
&R[&n&C3&n&R]&n Read the background story.\r\n\
&R[&n&C4&n&R]&n Read the latest news.\r\n\
&R[&n&C5&n&R]&n Read the game policy.\r\n\
&R[&n&C6&n&R]&n See who is online.\r\n\
&R[&n&C7&n&R]&n Change password.\r\n\
&R[&n&C8&n&R]&n Delete this character.\r\n\
&B------------------------------&n\r\n\r\n   Make your choice: ";

pub const ASK_NAME: &str = "\r\nPlease enter a name&R:&n ";

pub const WELC_MESSG: &str = "\r\n\
Welcome to the ever changing world of Deltania..may your life here\r\n\
be full of adventure and intrigue...\r\n\
\r\n\r\n";

pub const START_MESSG: &str = "\r\n\
This is your new DeltaMUD character!  You can now earn &Ygold&n,\r\n\
gain &Cexperience&n, find &Rweapons&n and &Mequipment&n, and much more.\r\n\
\r\nThe first thing you should do is read the Newbie Guide. You do that\r\n\
by typing 'read guide' (without the quotes, of course)\r\n\
\r\n\r\n";

const NEWBIE_STAT_EXPLANATION: &str = "\r\nHere is a brief explanation of each ability:\r\n\
[&YStr&n] - Strength determines how hard you hit your opponents in a fight.\r\n\r\n\
[&YInt&n] - Intelligence determines how well you hit your opponents in a fight,\r\n\
        and also the amount of magic points for spells (clerics and mages).\r\n\r\n\
[&YWis&n] - Wisdom determines how well you hit your opponents in a fight,\r\n\
        and also the amount of magic spells you can learn (clerics and mages).\r\n\r\n\
[&YDex&n] - Dexterity determines how well you fight in a battle, and also\r\n\
        how cunning and sneaky you are.\r\n\r\n\
[&YCon&n] - Constitution determines how much health you have.\r\n\r\n\
[&YCha&n] - Charisma determines how good you are with people :)\r\n\r\n";

/// Wrap a payload in an `IAC SB <opt> ... IAC SE` telnet subnegotiation frame.
/// 0xFF (IAC) bytes inside JSON/MSSP payloads are vanishingly unlikely (ASCII
/// JSON, printable MSSP values), so no IAC-doubling is needed for our content;
/// we emit the frame verbatim.
fn telnet_subneg(opt: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 5);
    v.push(IAC);
    v.push(SB);
    v.push(opt);
    v.extend_from_slice(payload);
    v.push(IAC);
    v.push(SE);
    v
}

/// Pre-escape GMCP payloads: drop control bytes (newlines, a lone IAC) and
/// strip the `&x` color-code introducers room/zone names carry, so the value
/// is clean text for the client's mapper. The JSON encoding itself is done by
/// serde_json (hostile names with quotes/backslashes/non-ASCII stay valid).
fn gmcp_clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // COLOURLIST codes are one letter/char after the & — drop both.
            chars.next();
            continue;
        }
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            continue;
        }
        out.push(c);
    }
    out
}

/// Encode one GMCP message: "<name> {json}" with serde_json handling the
/// escaping of every string value.
fn gmcp_message(name: &str, value: &serde_json::Value) -> String {
    format!("{name} {value}")
}

/// True if `s` is one of the password-entry connection states (the only states
/// whose prompts must suppress client-side echo).
fn is_password_state(s: ConState) -> bool {
    matches!(
        s,
        ConState::GetOldPassword
            | ConState::GetNewPassword
            | ConState::ConfirmPassword
            | ConState::ChPwdGetOld
            | ConState::ChPwdGetNew
            | ConState::ChPwdVerify
            | ConState::DelCnf1
    )
}

/// C comm.c:894-903: every drained input command resets the idle timer and,
/// if the character was pulled into the void by check_idling, returns them to
/// their previous room with "$n has returned." (issue #217).
fn reset_idle_on_input(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.timer = 0;
    }
    let was_in = g.get_char(ch).and_then(|c| c.was_in_room);
    if let Some(room) = was_in {
        if g.get_char(ch).and_then(|c| c.in_room).is_some() {
            g.char_from_room(ch);
        }
        g.char_to_room(ch, room);
        if let Some(c) = g.get_char_mut(ch) {
            c.was_in_room = None;
        }
        crate::act::act(
            g,
            "$n has returned.",
            true,
            ch,
            None,
            crate::act::ActArg::None,
            crate::act::To::Room,
        );
    }
}

/// Run a player command through the interpreter inside catch_unwind so a panic
/// in any single command (bad index, arithmetic overflow in debug, a stray
/// unwrap deep in the world) is contained to that command instead of killing
/// the whole single-threaded Game task (which would disconnect every player).
///
/// AssertUnwindSafe is required because `&mut GameState` is not UnwindSafe; we
/// accept the bounded risk that a panic caught mid-mutation leaves minor state
/// inconsistency — vastly preferable to the server dying. The recovered payload
/// is logged with the offending player + input, which is the key diagnostic.
fn dispatch_command_isolated(
    state: &mut GameState,
    ch: CharId,
    input: &str,
    context: &str,
) -> bool {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_command(state, ch, input);
    }));
    match res {
        Ok(()) => true,
        Err(payload) => {
            let msg = panic_payload_str(&payload);
            let pname = state
                .get_char(ch)
                .map(|c| c.get_name().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            error!(
                "PANIC contained in command [{}] from player '{}' input {:?}: {}",
                context, pname, input, msg
            );
            state.send_to_char(ch, "An error occurred processing that command.\r\n");
            false
        }
    }
}

/// Extract a human-readable message from a catch_unwind payload.
fn panic_payload_str(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Outcome of the shutdown save pass (W6): reported to the log and asserted
/// by the shutdown round-trip test.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub players_saved: u32,
    pub aliases_written: u32,
    pub save_errors: u32,
}

pub struct Game {
    state: GameState,
    db: Arc<dyn DatabaseInterface>,
    /// Async output channel per connection (the writer half lives in the
    /// connection task). The Descriptor (in GameState) only buffers text.
    outputs: HashMap<ConnId, mpsc::Sender<String>>,
    /// Character-creation choices accumulated across nanny steps.
    pending: HashMap<ConnId, PendingChoices>,
    /// Player records loaded at password-verify time (gates + motd choice)
    /// and consumed by menu option 1, so login loads the row once.
    pending_load: HashMap<ConnId, crate::character::Character>,
    /// Connections whose character was just created (their first menu-enter
    /// also runs the C `do_start` branch: START_MESSG + do_newbie).
    just_created: std::collections::HashSet<ConnId>,
    lib_path: String,
    /// Who-list JSON snapshot (Deltania Breathes W5), shared with the metrics
    /// HTTP task's /api/who route. Written by the Game once a second; readers
    /// take a short read-lock. Empty string = nothing published yet.
    who_snapshot: Arc<std::sync::RwLock<String>>,
    /// Updated on the heartbeat hot path (atomics, no mutex).
    metrics: Arc<Metrics>,
    /// Unix timestamp the Game task started, for the MSSP UPTIME datum (which
    /// reports the server boot time per the MSSP spec).
    started_at: i64,
    /// C db.c zone_update state: the 60-second accumulator (a static counter
    /// in C) and the reset queue of zones past their lifespan.
    zone_minute_timer: u64,
    zone_reset_queue: Vec<i32>,
    /// C comm.c mins_since_crashsave: minutes since the last autosave sweep.
    mins_since_crashsave: u32,
    /// Auto-reboot warning latch (one warning per armed schedule).
    reboot_warned: bool,
}

impl Game {
    pub fn new(state: GameState, db: Arc<dyn DatabaseInterface>) -> Self {
        Game {
            state,
            db,
            outputs: HashMap::new(),
            pending: HashMap::new(),
            pending_load: HashMap::new(),
            just_created: std::collections::HashSet::new(),
            lib_path: "./lib".to_string(),
            metrics: Arc::new(Metrics::new()),
            who_snapshot: Arc::new(std::sync::RwLock::new(String::new())),
            started_at: chrono::Utc::now().timestamp(),
            zone_minute_timer: 0,
            zone_reset_queue: Vec::new(),
            mins_since_crashsave: 0,
            reboot_warned: false,
        }
    }

    /// Install the shared metrics handle (main.rs creates one Arc and shares it
    /// with both the Game and the HTTP task). Defaults to a private Metrics so
    /// the Game is usable without one (e.g. in tests).
    /// Share the who-list snapshot with the metrics HTTP task (main.rs creates
    /// the Arc; /api/who reads it).
    pub fn set_who_snapshot(&mut self, snapshot: Arc<std::sync::RwLock<String>>) {
        self.who_snapshot = snapshot;
    }

    pub fn set_metrics(&mut self, metrics: Arc<Metrics>) {
        self.metrics = metrics;
    }

    pub fn state_mut(&mut self) -> &mut GameState {
        &mut self.state
    }

    pub async fn load_text_files(&mut self, lib_path: &str) {
        self.lib_path = lib_path.to_string();
        let text_dir = std::path::Path::new(lib_path).join("text");
        self.state.credits = tokio::fs::read_to_string(text_dir.join("credits"))
            .await
            .unwrap_or_default();
        self.state.news = tokio::fs::read_to_string(text_dir.join("news"))
            .await
            .unwrap_or_default();
        self.state.info = tokio::fs::read_to_string(text_dir.join("info"))
            .await
            .unwrap_or_default();
        self.state.handbook = tokio::fs::read_to_string(text_dir.join("handbook"))
            .await
            .unwrap_or_default();
        self.state.policies = tokio::fs::read_to_string(text_dir.join("policies"))
            .await
            .unwrap_or_default();
        self.state.motd = tokio::fs::read_to_string(text_dir.join("motd"))
            .await
            .unwrap_or_else(|_| "\r\nWelcome to DeltaMUD!\r\n".to_string());
        self.state.imotd = tokio::fs::read_to_string(text_dir.join("imotd"))
            .await
            .unwrap_or_default();
        self.state.circlemud = tokio::fs::read_to_string(text_dir.join("circlemud"))
            .await
            .unwrap_or_default();
        self.state.startup = tokio::fs::read_to_string(text_dir.join("startup"))
            .await
            .unwrap_or_default();
        self.state.background = tokio::fs::read_to_string(text_dir.join("background"))
            .await
            .unwrap_or_default();
    }

    pub fn prime_zones(&mut self) {
        // The initial zone reset moved to main (before House_boot, per
        // db.c boot order, #242); the Game task only primes live weather.
        let _ = &self.state;
        info!("Initial zone prime moved before house boot (db.c order)");
        // C boots the surface map (read_map) which calls init_weather, so the
        // world starts with MAX_WEATHER storms already on the map. Prime them
        // here so the weather map shows live storms from the first tick.
        crate::maputils::prime_weather(&mut self.state);
    }

    pub async fn run(&mut self, mut game_rx: mpsc::Receiver<GameMessage>) -> Result<()> {
        info!("Game loop starting...");
        let mut tick = interval(Duration::from_millis(100)); // 10 pulses/sec
        // A stall (blocked flush, slow DB) must not turn into a catch-up
        // burst of hundreds of back-to-back pulses on resume: Delay skips to
        // the next future deadline instead (tokio default is Burst).
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // SIGTERM stream (systemd stop / kill -TERM). Ctrl-C (SIGINT) is handled
        // by tokio::signal::ctrl_c. On either, we run a clean shutdown: crash-save
        // every player + their objects, flush descriptors, and return.
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!("Could not install SIGTERM handler: {} (Ctrl-C only)", e);
                    None
                }
            };

        loop {
            // When the SIGTERM stream failed to install, fall back to a future
            // that is pending forever so the select arm is inert.
            let sigterm_fut = async {
                match sigterm.as_mut() {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                Some(msg) = game_rx.recv() => self.handle_message(msg).await,
                _ = tick.tick() => self.heartbeat(),
                _ = tokio::signal::ctrl_c() => {
                    info!("Received Ctrl-C (SIGINT); beginning graceful shutdown.");
                    self.shutdown().await;
                    return Ok(());
                }
                _ = sigterm_fut => {
                    info!("Received SIGTERM; beginning graceful shutdown.");
                    self.shutdown().await;
                    return Ok(());
                }
            }
            // Async bridge for OFFLINE immortal commands (set/stat/show on a
            // logged-off player): cmd_wizard queues an OfflineOp; here — between
            // awaits, where &mut self.state is free for the sync replay — we load
            // the player, replay the command, save, and extract.
            self.drain_offline_ops().await;
        self.drain_deferred_db_ops().await;
            self.drain_player_save_requests().await;
            self.drain_pfileclean().await;
            self.flush_all().await;

            // The `shutdown` immortal command sets this (C circle_shutdown=1);
            // halt via the same graceful path as a SIGTERM so the server stops.
            if self.state.shutdown_requested {
                info!("shutdown requested by command; beginning graceful shutdown.");
                self.shutdown().await;
                return Ok(());
            }
        }
    }

    /// Graceful-shutdown sequence (CircleMUD's SIGTERM/hupsig + Crash_save_all):
    /// crash-save every in-world player and their objects to disk, push the
    /// final "shutting down" notice + any buffered output to every descriptor,
    /// log the count, and return so `run` exits cleanly instead of being killed
    /// with unsaved state.
    async fn shutdown(&mut self) {
        let report = self.shutdown_save().await;
        info!(
            "Shutting down, saved {} player(s) ({} alias files, {} save errors).",
            report.players_saved, report.aliases_written, report.save_errors
        );
    }

    /// The save-and-flush half of shutdown, extracted (W6 live-ops) so the
    /// persistence contract is callable and testable independently of the
    /// signal path: OLC save-list flush, crash-save rent files, mud calendar,
    /// per-player SQL rows + alias sidecars, then a bounded drain of every
    /// output channel so the shutdown notice actually reaches the sockets.
    /// Returns a report for the shutdown log / tests.
    async fn shutdown_save(&mut self) -> ShutdownReport {
        // C comm.c:458-510: flush the OLC save list before stopping (#262).
        crate::olc::flush_save_list_to_disk(&mut self.state);

        // Notify everyone still connected.
        let conn_ids: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        for cid in &conn_ids {
            self.out(
                *cid,
                "\r\nThe server is shutting down. Saving and disconnecting...\r\n",
            );
        }

        let mut report = ShutdownReport::default();

        // Crash-save all rent/inventory + persist every online player file.
        crate::objsave::crash_save_all(&mut self.state);
        crate::weather::write_mud_date_to_file(&self.state);
        for cid in &conn_ids {
            if let Some(ch) = self.state.descriptors.get(cid).and_then(|d| d.character) {
                report.players_saved += 1;
                if let Some(snapshot) = self.snapshot_online_player_for_save(ch) {
                    if let Err(e) =
                        crate::alias::write_aliases(&self.lib_path, snapshot.get_name(), snapshot.idnum)
                    {
                        warn!(
                            "shutdown write_aliases({}) failed: {}",
                            snapshot.get_name(),
                            e
                        );
                        report.save_errors += 1;
                    } else {
                        report.aliases_written += 1;
                    }
                    let host = self
                        .state
                        .descriptors
                        .get(cid)
                        .map(|d| d.host.as_str())
                        .unwrap_or("");
                    if let Err(e) = self.db.save_player_with_host(&snapshot, host).await {
                        warn!(
                            "shutdown save_player({}) failed: {}",
                            snapshot.get_name(),
                            e
                        );
                        report.save_errors += 1;
                    }
                }
            }
        }

        // Flush all buffered output (the shutdown notice) to the writer tasks.
        self.flush_all().await;
        // Deterministic drain: wait until the per-connection writer tasks have
        // consumed their queues (len() == 0), bounded at 2s so a dead socket
        // cannot stall shutdown. Replaces the old fixed 200ms sleep.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let any_pending = self.outputs.values().any(|tx| tx.capacity() < tx.max_capacity());
            if any_pending == false || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        report
    }

    async fn handle_message(&mut self, msg: GameMessage) {
        match msg {
            GameMessage::NewConnection {
                id,
                host,
                raw_fd,
                output_tx,
            } => {
                info!("New connection from {}", host);
                self.metrics.inc_connections();
                let mut d = Descriptor::with_fd(id, host, raw_fd);
                // C comm.c:1608: the colour question is the very first output;
                // the startup banner follows the answer (CON_QANSI) (#198).
                d.write(ANSI_QUESTION);
                self.state.descriptors.insert(id, d);
                self.outputs.insert(id, output_tx);
                self.write_prompt(id);
            }
            GameMessage::Recover {
                id,
                host,
                raw_fd,
                name,
                output_tx,
            } => {
                self.recover_player(id, host, raw_fd, name, output_tx).await;
            }
            GameMessage::Input { conn_id, input } => {
                self.handle_input(conn_id, input).await;
            }
            GameMessage::EnableGmcp { conn_id } => {
                self.enable_gmcp(conn_id);
            }
            GameMessage::SendMssp { conn_id } => {
                self.send_mssp(conn_id);
            }
            GameMessage::Disconnect { conn_id } => {
                self.disconnect(conn_id).await;
            }
        }
    }

        /// C comm.c perform_subst (1911-1960): "^telm^tell" replaces the first
    /// occurrence of the text between the carets in `orig` with the
    /// replacement. Returns None when the syntax is bad or the search text is
    /// absent (caller prints "Invalid substitution.").
    fn perform_subst(orig: &str, subst: &str) -> Option<String> {
        let rest = &subst[1..];
        let idx = rest.find('^')?;
        let first = &rest[..idx];
        let second = &rest[idx + 1..];
        let pos = orig.find(first)?;
        let mut new = String::with_capacity(orig.len() + second.len());
        new.push_str(&orig[..pos]);
        new.push_str(second);
        new.push_str(&orig[pos + first.len()..]);
        Some(
            new.chars()
                .take(crate::types::MAX_INPUT_LENGTH)
                .collect(),
        )
    }

async fn handle_input(&mut self, conn_id: ConnId, input: String) {
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };

        // C comm.c process_input (1836-1960), applied to every completed line
        // regardless of connection state:
        //   1. every '$' is doubled on entry (act() renders '$$' as one
        //      literal '$', so 'say Hi $n' says 'Hi $n') (#222);
        //   2. the line is capped at MAX_INPUT_LENGTH (256) with C's
        //      'Line too long. Truncated to:' notice (#224);
        //   3. '!' repeats last_input and '^old^new' performs the csh-style
        //      substitution on it; otherwise last_input records the line.
        let mut doubled = String::with_capacity(input.len() + 8);
        for c in input.chars() {
            if c == '$' {
                doubled.push_str("$$");
            } else {
                doubled.push(c);
            }
        }
        let max_len = crate::types::MAX_INPUT_LENGTH;
        let mut line = if doubled.chars().count() > max_len {
            let truncated: String = doubled.chars().take(max_len).collect();
            self.out(
                conn_id,
                &format!("Line too long.  Truncated to:\r\n{}\r\n", truncated),
            );
            truncated
        } else {
            doubled
        };
        if line.starts_with('!') {
            let last = self
                .state
                .descriptors
                .get(&conn_id)
                .map(|d| d.last_input.clone())
                .unwrap_or_default();
            line = last;
        } else if line.starts_with('^') {
            let last = self
                .state
                .descriptors
                .get(&conn_id)
                .map(|d| d.last_input.clone())
                .unwrap_or_default();
            match Game::perform_subst(&last, &line) {
                Some(new) => {
                    line = new;
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.last_input = line.clone();
                    }
                }
                None => {
                    self.out(conn_id, "Invalid substitution.\r\n");
                    return;
                }
            }
        } else if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.last_input = line.clone();
        }
        let input = line;

        if state == ConState::Playing {
            if crate::modify::page_active(conn_id) {
                crate::modify::page_input(&mut self.state, conn_id, &input);
            } else if crate::modify::editing(&self.state, conn_id) {
                if !crate::modify::editor_input(&mut self.state, conn_id, &input) {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.editors.pop();
                    }
                }
            } else if crate::olc::in_olc(conn_id) {
                crate::olc::olc_input(&mut self.state, conn_id, &input);
            } else {
                // Gameplay command: queue it instead of dispatching now. The
                // heartbeat's process_input_queues drains one per pulse once the
                // descriptor's WAIT_STATE lag (d.wait) expires, and sends the
                // prompt after the command actually runs.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.input_queue.push_back(QueuedInput::raw(input));
                }
                return;
            }
        } else {
            self.nanny(conn_id, input).await;
        }

        // Re-send the appropriate prompt unless the connection is closing or
        // the nanny arm printed its own inline prompt.
        let suppress = self
            .state
            .descriptors
            .get_mut(&conn_id)
            .map(|d| d.suppress_prompt)
            .unwrap_or(false);
        if suppress {
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.suppress_prompt = false;
            }
        }
        let st = self.state.descriptors.get(&conn_id).map(|d| d.state);
        if st.is_some() && st != Some(ConState::Close) && !suppress {
            self.write_prompt(conn_id);
        }
    }

    /// Drain one queued command per descriptor whose WAIT_STATE lag has expired
    /// (C game_loop: `if ((--d->wait) <= 0 && get_from_q(...))`). Decrement every
    /// playing descriptor's wait each pulse; when it reaches <= 0 and input is
    /// queued, run one command (resetting wait to 1 first, so a command's own
    /// WAIT_STATE call overrides it) and send the prompt.
    fn process_input_queues(&mut self) {
        let conn_ids: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        for cid in conn_ids {
            let ready = match self.state.descriptors.get_mut(&cid) {
                Some(d) if d.state == ConState::Playing => {
                    d.wait = (d.wait - 1).max(-1);
                    d.wait <= 0 && !d.input_queue.is_empty()
                }
                _ => false,
            };
            if !ready {
                continue;
            }
            let queued = match self.state.descriptors.get_mut(&cid) {
                Some(d) => {
                    d.wait = 1;
                    d.input_queue.pop_front()
                }
                None => None,
            };
            let queued = match queued {
                Some(i) => i,
                None => continue,
            };
            if let Some(ch) = self.state.descriptors.get(&cid).and_then(|d| d.character) {
                reset_idle_on_input(&mut self.state, ch);
                let mut input = queued.line;
                if !queued.aliased {
                    match crate::alias::alias_expand(&self.state, ch, &input) {
                        Some(crate::alias::AliasExpansion::Simple(line)) => {
                            input = line;
                        }
                        Some(crate::alias::AliasExpansion::Complex(lines)) => {
                            if let Some(d) = self.state.descriptors.get_mut(&cid) {
                                for line in lines.into_iter().rev() {
                                    d.input_queue.push_front(QueuedInput::aliased(line));
                                }
                                input = match d.input_queue.pop_front() {
                                    Some(q) => q.line,
                                    None => continue,
                                };
                            } else {
                                continue;
                            }
                        }
                        None => {}
                    }
                }
                self.metrics.inc_commands();
                dispatch_command_isolated(&mut self.state, ch, &input, "input-queue");
            }
            let st = self.state.descriptors.get(&cid).map(|d| d.state);
            if st.is_some() && st != Some(ConState::Close) {
                self.write_prompt(cid);
            }
        }
    }

    // ---- Login / character creation (CircleMUD nanny) -------------------
    async fn nanny(&mut self, conn_id: ConnId, input: String) {
        let input = input.trim().to_string();
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };

        match state {
            ConState::QAnsi => {
                // C interpreter.c:1706-1735 CON_QANSI (#198).
                let first = input.chars().next().map(|c| c.to_ascii_lowercase());
                if input.is_empty() || first == Some('y') {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.wants_colour = Some(true);
                    }
                    self.out(conn_id, "Your terminal will now receive color.\r\n\r\n\r\n");
                } else if first == Some('n') {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.wants_colour = Some(false);
                    }
                    self.out(conn_id, "Your terminal will not receive color.\r\n\r\n\r\n");
                } else {
                    self.out(conn_id, "That is not a proper response.\r\n\r\n");
                    self.out(conn_id, ANSI_QUESTION);
                    return;
                }
                let startup = self.state.startup.clone();
                self.out(conn_id, &startup);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::GetName;
                }
            }
            ConState::GetName => {
                if input.is_empty() {
                    // C interpreter.c:1744: an empty name closes the socket.
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }
                let name = normalize_name(&input);
                // C interpreter.c:1747-1752: _parse_name length/alpha checks,
                // fill_word/reserved_word, and Valid_Name (xnames substrings +
                // mob-keyword collisions) (#223).
                if !valid_name(&name)
                    || reserved_or_fill_word(&name)
                    || !crate::ban::valid_name_in(&self.state, &name)
                {
                    // C interpreter.c:1739: the message carries its own
                    // 'Name: ' prompt.
                    self.out(conn_id, "Invalid name, please try another.\r\nName: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.suppress_prompt = true;
                    }
                    return;
                }
                let exists = self.db.player_exists(&name).await.unwrap_or(false);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_name = Some(name.clone());
                    d.state = if exists {
                        ConState::GetOldPassword
                    } else {
                        ConState::ConfirmName
                    };
                }
            }
            ConState::ConfirmName => {
                let yes = input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes");
                if yes {
                    let host = self.descriptor_host(conn_id);
                    let banned = crate::ban::isbanned(&host);
                    if banned >= crate::ban::BanType::New {
                        self.out(
                            conn_id,
                            "Sorry, new characters are not allowed from your site!\r\n",
                        );
                        warn!(
                            "Request for new char {} denied from [{}] (siteban)",
                            self.descriptor_name(conn_id),
                            host
                        );
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        return;
                    }
                    // C interpreter.c:1826: wizlock refuses NEW characters too.
                    if crate::cmd_wizard::circle_restrict() > 0 {
                        warn!(
                            "Request for new char {} denied from [{}] (wizlock)",
                            self.descriptor_name(conn_id),
                            host
                        );
                        self.out(
                            conn_id,
                            "Sorry, new players can't be created at the moment.\r\n",
                        );
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        return;
                    }
                    self.out(conn_id, "New character.\r\n");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetNewPassword;
                    }
                } else if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_name = None;
                    d.state = ConState::GetName;
                }
            }
            ConState::GetOldPassword => {
                // C interpreter.c:1869-2020 CON_PASSWORD.
                let name = self.descriptor_name(conn_id);
                let ok = self
                    .db
                    .verify_password(&name, &input)
                    .await
                    .unwrap_or(false);
                if !ok {
                    // C 1897-1911: mudlog the attempt, bump GET_BAD_PWS (and
                    // persist it), re-prompt; disconnect at max_bad_pws (#194).
                    let host = self.descriptor_host(conn_id);
                    warn!("Bad PW: {} [{}]", name, host);
                    if let Ok(mut rec) = self.db.load_player(&name).await {
                        rec.bad_pws = rec.bad_pws.saturating_add(1);
                        let _ = self.db.save_player(&rec).await;
                    }
                    let tries = {
                        let d = self
                            .state
                            .descriptors
                            .get_mut(&conn_id)
                            .expect("descriptor present in its own state arm");
                        d.bad_pws += 1;
                        d.bad_pws
                    };
                    if tries >= crate::config::MAX_BAD_PWS as u32 {
                        self.out(conn_id, "Wrong password... disconnecting.\r\n");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                    } else {
                        self.out(conn_id, "Wrong password.\r\nPassword: ");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            // stay in GetOldPassword; echo stays off
                        }
                    }
                    return;
                }

                // Password was correct.
                let host = self.descriptor_host(conn_id);
                let mut rec = match self.db.load_player(&name).await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("load player {} failed: {}", name, e);
                        self.out(conn_id, "Error loading your character.\r\n");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        return;
                    }
                };
                let load_result = rec.bad_pws;
                if load_result > 0 {
                    rec.bad_pws = 0;
                    let _ = self.db.save_player(&rec).await;
                }

                // C 1914-1952: automatic upgrade of legacy password hashes (#219).
                if let Ok(Some(hash)) = self.db.get_password_hash(&name).await {
                    if crate::password::password_needs_upgrade(&hash) {
                        info!("Upgrading password security for {}", name);
                        rec.pending_password_hash = Some(crate::password::hash_password(&input));
                        let _ = self.db.save_player(&rec).await;
                        rec.pending_password_hash = None;
                    }
                }

                // Cache the session password hash so `unlock <password>`
                // (act.other.c do_lockout) can verify against the real account
                // password (C compares against GET_PASSWD(ch)) (#313).
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.password_hash = Some(crate::password::hash_password(&input));
                }

                // C 1957-1967: BAN_SELECT without PLR_SITEOK.
                let banned = crate::ban::isbanned(&host);
                if banned >= crate::ban::BanType::Select && rec.act_flags & PLR_SITEOK == 0 {
                    self.out(
                        conn_id,
                        "Sorry, this char has not been cleared for login from your site!\r\n",
                    );
                    warn!("Connection attempt for {} denied from {}", name, host);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }

                // C 1968-1979: multiplay gate (comm.c check_multiplaying;
                // the C build returns 1 immediately — dev-mode bypass kept).
                if !crate::cmd_comm::check_multiplaying(&self.state, &host)
                    && rec.player.level < LVL_IMMORT
                    && rec.act_flags & crate::flags::PLR_MULTIOK == 0
                {
                    self.out(
                        conn_id,
                        "\r\nSorry, there is already more then one connection to the MUD from your host.\r\n\
If you are playing from a shared connection please e-mail help@deltamud.net\r\n\
for access.\r\n\r\n",
                    );
                    warn!("Connection attempt for {} denied from {} - multi-play", name, host);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }
                // C 1980-1989: wizlock (#202).
                let restrict = crate::cmd_wizard::circle_restrict();
                if restrict > 0 && (rec.player.level as i32) < restrict {
                    self.out(conn_id, "The game is temporarily restricted.. try again later.\r\n");
                    warn!("Request for login denied for {} [{}] (wizlock)", name, host);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }
                // C 1990: perform_dupe_check — on dupe, take over the live
                // body and go straight to Playing (no MOTD) (#218).
                if self.perform_dupe_check(conn_id, rec.idnum).await {
                    return;
                }

                // C 1991-2019: motd/imotd, "has connected" mudlog, the
                // bad-pw notice, do_time, and PRESS RETURN -> CON_RMOTD.
                self.pending_load.insert(conn_id, rec.clone());
                let motd = if rec.player.level >= LVL_IMMORT {
                    self.state.imotd.clone()
                } else {
                    self.state.motd.clone()
                };
                self.out(conn_id, &motd);
                self.user_cntr(conn_id);
                info!("{} [{}] has connected.", name, host);
                if load_result > 0 {
                    self.out(
                        conn_id,
                        &format!(
                            "\r\n\r\n\x07\x07\x07{} LOGIN FAILURE{} SINCE LAST SUCCESSFUL LOGIN.\r\n",
                            load_result,
                            if load_result > 1 { "S" } else { "" }
                        ),
                    );
                }
                self.out(conn_id, "\r\n");
                {
                    // C runs do_time for the (still-unplaced) character.
                    let stub = self.login_stub(conn_id);
                    crate::cmd_informative::do_time(&mut self.state, stub, "", 0);
                    self.state.extract_char(stub);
                }
                self.out(conn_id, "\r\n\n*** PRESS RETURN: ");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            ConState::GetNewPassword => {
                // C interpreter.c:2043-2045: empty, >64, <3, or equal to the
                // name are all 'Illegal password.' with a 'Password: ' retry.
                if input.is_empty()
                    || input.len() > 64
                    || input.len() < 3
                    || input.eq_ignore_ascii_case(&self.descriptor_name(conn_id))
                {
                    self.out(conn_id, "\r\nIllegal password.\r\nPassword: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.suppress_prompt = true;
                    }
                    return;
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_password = Some(input);
                    d.state = ConState::ConfirmPassword;
                }
            }
            ConState::ConfirmPassword => {
                let matches = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .and_then(|d| d.temp_password.clone())
                    .map(|p| p == input)
                    .unwrap_or(false);
                if matches {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetNewbie;
                        // Session password hash, for the `unlock` gate.
                        d.password_hash = d
                            .temp_password
                            .as_ref()
                            .map(|p| crate::password::hash_password(p));
                    }
                } else {
                    // C interpreter.c:2057: '...start over.' + inline prompt.
                    self.out(conn_id, "\r\nPasswords don't match... start over.\r\nPassword: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.temp_password = None;
                        d.state = ConState::GetNewPassword;
                        d.suppress_prompt = true;
                    }
                }
            }
            ConState::GetNewbie => {
                match input.chars().next().map(|c| c.to_ascii_lowercase()) {
                    Some('y') => self.pending.entry(conn_id).or_default().newbie = 1,
                    Some('n') => self.pending.entry(conn_id).or_default().newbie = 0,
                    _ => {
                        self.out(conn_id, "Please type Yes or No: ");
                        return;
                    }
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::GetSex;
                }
            }
            ConState::GetSex => {
                let sex = match input.to_lowercase().chars().next() {
                    Some('m') => Some(Gender::Male),
                    Some('f') => Some(Gender::Female),
                    _ => None,
                };
                match sex {
                    Some(s) => {
                        self.set_temp_sex(conn_id, s);
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::GetRace;
                        }
                    }
                    None => {
                        // C interpreter.c:2145: the retry carries its own
                        // 'What IS your sex? ' prompt.
                        self.out(conn_id, "That is not a sex..\r\nWhat IS your sex? ");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.suppress_prompt = true;
                        }
                    }
                }
            }
            ConState::GetRace => {
                if input
                    .get(..4)
                    .map(|s| s.eq_ignore_ascii_case("help"))
                    .unwrap_or(false)
                {
                    let race_letter = input.chars().nth(5).unwrap_or(' ');
                    let race = crate::races::parse_race(race_letter);
                    if race == crate::races::RACE_UNDEFINED {
                        self.out(conn_id, "\r\nThat's not a race.\r\n");
                    } else {
                        let avg = |stat| {
                            (crate::races::get_race_min(race, stat)
                                + crate::races::get_race_max(race, stat))
                                / 2
                        };
                        self.out(
                            conn_id,
                            &format!(
                                "\r\nAt 11 as the universal statistic average, your race averages the following abilities:\r\n\
Str: {:2} Int: {:2} Wis: {:2} Dex: {:2} Con: {:2} Cha: {:2}\r\n",
                                avg(1),
                                avg(2),
                                avg(3),
                                avg(4),
                                avg(5),
                                avg(6)
                            ),
                        );
                    }
                    return;
                }

                let parsed = input
                    .chars()
                    .next()
                    .map(crate::races::parse_race)
                    .unwrap_or(crate::races::RACE_UNDEFINED);
                if parsed == crate::races::RACE_UNDEFINED {
                    self.out(conn_id, "\r\nThat's not a race.\r\n");
                } else {
                    self.set_temp_race(conn_id, Race::from_u8(parsed as u8), parsed);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetDeity;
                    }
                }
            }
            ConState::GetDeity => {
                let parsed = input
                    .chars()
                    .next()
                    .map(crate::deity::parse_deity)
                    .unwrap_or(crate::deity::DEITY_UNDEFINED);
                if parsed == crate::deity::DEITY_UNDEFINED {
                    self.out(conn_id, "\r\nThat's not a deity.\r\n");
                } else {
                    self.pending.entry(conn_id).or_default().deity = parsed as u8;
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetClass;
                    }
                }
            }
            ConState::GetClass => {
                let parsed = input
                    .chars()
                    .next()
                    .map(crate::class::parse_class)
                    .unwrap_or(crate::class::CLASS_UNDEFINED);
                if parsed == crate::class::CLASS_UNDEFINED {
                    self.out(conn_id, "\r\nThat's not a class.\r\n");
                } else {
                    self.set_temp_class(conn_id, Class::from_u8(parsed as u8));
                    let newbie = self.pending.get(&conn_id).map(|p| p.newbie).unwrap_or(1);
                    if newbie == 0 {
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::GetHometown;
                        }
                    } else {
                        self.pending.entry(conn_id).or_default().hometown = 1;
                        self.out(
                            conn_id,
                            "\r\nYour hometown has been set to the capital city of Anacreon.\r\n\r\n",
                        );
                        self.begin_stat_roll(conn_id, true);
                    }
                }
            }
            ConState::GetHometown => {
                let parsed = input
                    .chars()
                    .next()
                    .map(crate::class::parse_town)
                    .unwrap_or(-1);
                if parsed < 0 {
                    self.out(conn_id, "\r\nThat's not a town.\r\n");
                } else {
                    self.pending.entry(conn_id).or_default().hometown = parsed as RoomVnum;
                    self.begin_stat_roll(conn_id, false);
                }
            }
            ConState::RollStats => match input.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('y') => {
                    self.create_and_enter(conn_id).await;
                }
                _ => self.begin_stat_roll(conn_id, false),
            },
            ConState::ReadMotd => {
                // C interpreter.c:2243-2246 CON_RMOTD: any input -> MENU (#198).
                self.out(conn_id, MENU);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Menu;
                }
            }
            ConState::Menu => self.menu_choice(conn_id, &input).await,
            ConState::ExDesc => {
                // The string editor owns this input (modify::editing is checked
                // before the nanny); if we ever get here the editor is gone —
                // return to the menu like C's fall-through.
                self.out(conn_id, MENU);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Menu;
                }
            }
            ConState::ChPwdGetOld => {
                // C interpreter.c:2348-2364.
                let name = self.descriptor_name(conn_id);
                let ok = self
                    .db
                    .verify_password(&name, &input)
                    .await
                    .unwrap_or(false);
                if ok {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::ChPwdGetNew;
                    }
                } else {
                    self.out(conn_id, "\r\nIncorrect password.\r\n");
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Menu;
                    }
                }
            }
            ConState::ChPwdGetNew => {
                // C interpreter.c:2022-2039 CON_NEWPASSWD (shared).
                if input.is_empty() || input.len() > 64 || input.len() < 3
                    || input.eq_ignore_ascii_case(&self.descriptor_name(conn_id))
                {
                    self.out(conn_id, "\r\nIllegal password.\r\nPassword: ");
                    return;
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_password = Some(input);
                    d.state = ConState::ChPwdVerify;
                }
            }
            ConState::ChPwdVerify => {
                // C interpreter.c:2041-2068 CON_CHPWD_VRFY: save immediately.
                let matches = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .and_then(|d| d.temp_password.clone())
                    .map(|p| p == input)
                    .unwrap_or(false);
                if !matches {
                    self.out(conn_id, "\r\nPasswords don't match... start over.\r\nPassword: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::ChPwdGetNew;
                    }
                    return;
                }
                let name = self.descriptor_name(conn_id);
                let mut rec = match self.db.load_player(&name).await {
                    Ok(c) => c,
                    Err(_) => {
                        self.out(conn_id, MENU);
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Menu;
                        }
                        return;
                    }
                };
                rec.pending_password_hash = Some(crate::password::hash_password(&input));
                let _ = self.db.save_player(&rec).await;
                self.out(conn_id, "\r\nDone.\n\r");
                self.out(conn_id, MENU);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_password = None;
                    d.state = ConState::Menu;
                }
            }
            ConState::DelCnf1 => {
                // C interpreter.c:2366-2387 CON_DELCNF1.
                let name = self.descriptor_name(conn_id);
                let ok = self
                    .db
                    .verify_password(&name, &input)
                    .await
                    .unwrap_or(false);
                if ok {
                    self.out(
                        conn_id,
                        "\r\nYOU ARE ABOUT TO DELETE THIS CHARACTER PERMANENTLY.\r\n\
ARE YOU ABSOLUTELY SURE?\r\n\r\nPlease type \"yes\" to confirm: ",
                    );
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::DelCnf2;
                    }
                } else {
                    self.out(conn_id, "\r\nIncorrect password.\r\n");
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Menu;
                    }
                }
            }
            ConState::DelCnf2 => {
                // C interpreter.c:2389-2430 CON_DELCNF2.
                if input == "yes" || input == "YES" {
                    let name = self.descriptor_name(conn_id);
                    if let Ok(mut rec) = self.db.load_player(&name).await {
                        if rec.act_flags & crate::flags::PLR_FROZEN != 0 {
                            self.out(
                                conn_id,
                                "You try to kill yourself, but the ice stops you.\r\nCharacter not deleted.\r\n\r\n",
                            );
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.state = ConState::Close;
                            }
                            return;
                        }
                        if rec.player.level < LVL_GRGOD {
                            rec.act_flags |= crate::flags::PLR_DELETED;
                        }
                        let level = rec.player.level;
                        let _ = self.db.save_player(&rec).await;
                        crate::objsave::crash_delete_file_by_name(&self.lib_path, &name);
                        self.out(conn_id, &format!("Character '{}' deleted!\r\nGoodbye.\r\n", name));
                        info!("{} (lev {}) has self-deleted.", name, level);
                    }
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                } else {
                    self.out(conn_id, "\r\nThat was not \"yes\". Character not deleted.\r\n");
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Menu;
                    }
                }
            }
            _ => {}
        }

        // If this input was a password entry and we have now transitioned OUT of
        // the password flow (login success -> Playing, login fail -> Close, or
        // new-password confirmed -> GetNewbie), tell the client the server WONT echo
        // so normal local echo resumes. Staying within the password flow
        // (GetNewPassword -> ConfirmPassword, or a retry) keeps echo suppressed.
        if is_password_state(state) {
            let new_state = self.state.descriptors.get(&conn_id).map(|d| d.state);
            let still_password = new_state.map(is_password_state).unwrap_or(false);
            if !still_password {
                self.send_raw_bytes(conn_id, &IAC_WONT_ECHO);
            }
        }
    }

    // Pending creation choices are held between C nanny states until stat
    // acceptance finalizes the new player.
    fn set_temp_sex(&mut self, conn_id: ConnId, s: Gender) {
        self.pending.entry(conn_id).or_default().sex = s;
    }
    fn set_temp_class(&mut self, conn_id: ConnId, c: Class) {
        self.pending.entry(conn_id).or_default().class = c;
    }
    fn set_temp_race(&mut self, conn_id: ConnId, r: Race, race_index: i32) {
        let pending = self.pending.entry(conn_id).or_default();
        pending.race = r;
        pending.race_index = race_index;
    }

    fn begin_stat_roll(&mut self, conn_id: ConnId, explain: bool) {
        let (class, race_index) = {
            let pending = self.pending.entry(conn_id).or_default();
            (pending.class, pending.race_index)
        };
        let rolled = crate::class::roll_abilities_for(&mut self.state, class, race_index);
        self.pending.entry(conn_id).or_default().rolled = rolled;
        if explain {
            self.out(conn_id, NEWBIE_STAT_EXPLANATION);
        }
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.state = ConState::RollStats;
        }
    }

    fn descriptor_host(&self, conn_id: ConnId) -> String {
        self.state
            .descriptors
            .get(&conn_id)
            .map(|d| d.host.clone())
            .unwrap_or_default()
    }

    fn snapshot_online_player_for_save(
        &mut self,
        ch: CharId,
    ) -> Option<crate::character::Character> {
        let now = chrono::Utc::now();
        let c = self.state.get_char_mut(ch)?;
        if c.is_npc {
            return None;
        }
        let elapsed = (now - c.last_logon).num_seconds().max(0);
        c.player.time_played = c.player.time_played.saturating_add(elapsed);
        c.last_logon = now;
        Some(c.clone())
    }

    async fn create_and_enter(&mut self, conn_id: ConnId) {
        let (name, pass) = {
            let d = match self.state.descriptors.get(&conn_id) {
                Some(d) => d,
                None => return,
            };
            (
                d.temp_name.clone().unwrap_or_default(),
                d.temp_password.clone().unwrap_or_default(),
            )
        };
        let choices = self.pending.remove(&conn_id).unwrap_or_default();
        let mut ch =
            crate::character::Character::new_player(name.clone(), choices.class, choices.race);
        ch.player.sex = choices.sex;
        ch.player.deity = choices.deity;
        ch.player.hometown = choices.hometown;
        ch.newbie = choices.newbie;
        ch.real_abils = if choices.rolled.str > 0 {
            choices.rolled
        } else {
            crate::class::roll_abilities_for(&mut self.state, choices.class, choices.race_index)
        };
        ch.aff_abils = ch.real_abils;
        ch.clan = -1;
        ch.clan_rank = -1;
        ch.tloadroom = -1;
        ch.mapx = -1;
        ch.mapy = -1;
        ch.prf_flags |= crate::flags::PRF_NOLOOKSTACK
            | crate::flags::PRF_DISPHP
            | crate::flags::PRF_DISPMANA
            | crate::flags::PRF_DISPMOVE
            | crate::flags::PRF_DISPEXP;
        ch.prf2_flags |= crate::flags::PRF2_DISPMOB;

        let temp_id = self.state.create_char(ch);
        crate::class::do_start_init(&mut self.state, temp_id);
        let mut ch = match self.state.get_char(temp_id).cloned() {
            Some(ch) => ch,
            None => {
                self.out(conn_id, "Couldn't create your character. Try later.\r\n");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Close;
                }
                return;
            }
        };
        self.state.extract_char(temp_id);

        match self.db.create_player(&ch, &pass).await {
            Ok(idnum) => {
                // The in-memory char MUST take the assigned idnum, or the
                // save_player below (which keys on idnum) writes idnum=0 with an
                // empty pwd and REPLACE-clobbers the just-created row (name is
                // UNIQUE), losing the password and orphaning skills/affects.
                ch.idnum = idnum;
                // CircleMUD convention: the first character created becomes the
                // Implementor (nanny CON_QRACE).
                if idnum == 1 {
                    ch.player.level = LVL_IMPL;
                    ch.player.level = LVL_IMPL;
                    ch.player.title = Some("the Implementor".to_string());
                    // Grant the Implementor every god-command bit (act.wizard.c
                    // do_advance:1738-1745). Without this the new godcmd gate in
                    // the interpreter would lock idnum 1 out of ALL god commands
                    // and the game would be unadministrable. Persisted via the
                    // save_player below and reloaded in enter_game.
                    crate::gcmd::grant_advance(
                        &mut ch.godcmds1,
                        &mut ch.godcmds2,
                        &mut ch.godcmds3,
                        &mut ch.godcmds4,
                        LVL_IMPL,
                        crate::types::LVL_IMMORT,
                        LVL_IMPL,
                    );
                }
                let host = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .map(|d| d.host.as_str())
                    .unwrap_or("");
                if let Err(e) = self.db.save_player_with_host(&ch, host).await {
                    warn!("save new player {} failed: {}", name, e);
                }
                crate::alias::clear_aliases(ch.idnum);
                // Register the new player in the in-memory index immediately (C
                // create_entry appends to player_table) so name<->idnum lookups
                // — ignore-by-name, mail, `last` — resolve them at once, before
                // they ever log in elsewhere. enter_game refreshes last_logon.
                let host = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .map(|d| d.host.clone())
                    .unwrap_or_default();
                self.state.update_player_index_from_character(
                    &ch,
                    ch.last_logon.timestamp(),
                    &host,
                );
                crate::mail::mail_register_player(ch.idnum, &name);
                // C interpreter.c start_player (1637-1653): the new character
                // gets the MOTD + PRESS RETURN and lands at the MENU; the
                // actual world-enter happens at menu option 1.
                self.just_created.insert(conn_id);
                self.pending_load.insert(conn_id, ch);
                let motd = self.state.motd.clone();
                self.out(conn_id, &motd);
                self.out(conn_id, "\r\n\n*** PRESS RETURN: ");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Err(e) => {
                warn!("create player {} failed: {}", name, e);
                self.out(conn_id, "Couldn't create your character. Try later.\r\n");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Close;
                }
            }
        }
    }

    /// C act.informative.c:2934 user_cntr: bump the raw binary USRCNT logon
    /// counter (8-byte long, beside lib/ as in C's cwd) and tell the player
    /// their ordinal (#347).
    fn user_cntr(&mut self, conn_id: ConnId) {
        // C resolves "USRCNT" against the server cwd, which is always the
        // directory containing lib/. Prefer the configured lib's parent.
        let lib = if !self.lib_path.is_empty() && self.lib_path != "./lib" {
            self.lib_path.clone()
        } else {
            self.state.config.lib_path.clone()
        };
        let path = std::path::Path::new(&lib)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.join("USRCNT"))
            .unwrap_or_else(|| std::path::PathBuf::from("USRCNT"));
        let mut count: i64 = std::fs::read(&path)
            .ok()
            .and_then(|bytes| {
                if bytes.len() >= 8 {
                    Some(i64::from_le_bytes(bytes[..8].try_into().unwrap()))
                } else {
                    None
                }
            })
            .unwrap_or(0);
        count += 1;
        if std::fs::write(&path, count.to_le_bytes()).is_ok() {
            self.out(
                conn_id,
                &format!(
                    "\r\n  You are player #{} to logon since April 13, 1998\r\n",
                    count
                ),
            );
        }
    }

    /// C interpreter.c:2254-2360 CON_MENU: the DeltaMUD main menu (#198).
    async fn menu_choice(&mut self, conn_id: ConnId, input: &str) {
        match input.chars().next() {
            Some('0') => {
                self.out(
                    conn_id,
                    "\r\nYou awaken, and find yourself in a land called reality.\r\nWe hope you come back to Deltania soon!\r\n\r\n",
                );
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Close;
                }
            }
            Some('1') => {
                self.enter_game(conn_id, false).await;
            }
            Some('2') => {
                // C 2287-2307 CON_EXDESC: the string editor writes the new
                // description; it is applied to the player at enter-game.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    if d.temp_description.is_some() {
                        d.write("Current description:\r\n");
                        d.write(&d.temp_description.clone().unwrap_or_default());
                        d.write("\r\n");
                    }
                    d.write(
                        "Enter the new text you'd like others to see when they look at you.\r\n(/s saves /h for help)\r\n",
                    );
                }
                crate::modify::start_login_description_editing(&mut self.state, conn_id, 8192);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ExDesc;
                }
            }
            Some('3') => {
                let background = self.state.background.clone();
                crate::modify::page_string(&mut self.state, conn_id, &background);
                // C sets CON_RMOTD: when paging (or RETURN) ends, the next
                // input re-shows the menu.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('4') => {
                let news = self.state.news.clone();
                crate::modify::page_string(&mut self.state, conn_id, &news);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('5') => {
                let policies = self.state.policies.clone();
                crate::modify::page_string(&mut self.state, conn_id, &policies);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('6') => {
                // C 2339-2344: run do_who against a transient stand-in
                // character (not registered in players_by_name, so it does not
                // list itself), then back to the menu via PRESS RETURN.
                let stub = self.login_stub(conn_id);
                crate::cmd_informative::do_who(&mut self.state, stub, "", 0);
                self.state.extract_char(stub);
                self.out(conn_id, "\r\n\n*** PRESS RETURN: ");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('7') => {
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ChPwdGetOld;
                }
            }
            Some('8') => {
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::DelCnf1;
                }
            }
            _ => {
                self.out(conn_id, "\r\nThat's not a menu choice!\r\n");
                self.out(conn_id, MENU);
            }
        }
    }

    /// A transient stand-in Character for pre-login menu commands (who /
    /// do_time). Carries the login name + loaded record's level so
    /// CAN_SEE/level checks behave; never placed in a room; extracted by the
    /// caller. Extracting requires the id NOT to be in players_by_name.
    fn login_stub(&mut self, conn_id: ConnId) -> CharId {
        let name = self.descriptor_name(conn_id);
        let rec = self.pending_load.get(&conn_id).cloned();
        let mut ch = crate::character::Character::new_player(
            name.into(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        if let Some(rec) = &rec {
            ch.player.level = rec.player.level;
            ch.prf_flags = rec.prf_flags;
        }
        // Route the stub's output to the logging-in connection (C runs these
        // commands on d->character, which IS attached to the descriptor).
        ch.desc = Some(conn_id);
        self.state.create_char(ch)
    }

    /// C interpreter.c:1418-1530 perform_dupe_check: disconnect other
    /// descriptors controlling the same idnum and adopt the live body
    /// (#218). Returns true when THIS connection should go straight to
    /// Playing (dupe handled).
    async fn perform_dupe_check(&mut self, conn_id: ConnId, idnum: i64) -> bool {
        let dupes: Vec<(ConnId, CharId, bool)> = self
            .state
            .descriptors
            .iter()
            .filter(|&(c, d)| {
                *c != conn_id
                    && d.character
                        .map(|cid| {
                            self.state
                                .get_char(cid)
                                .map(|ch| !ch.is_npc && ch.idnum == idnum)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
            })
            .map(|(&c, d)| (c, d.character.unwrap(), d.state == ConState::Playing))
            .collect();
        if dupes.is_empty() {
            return false;
        }
        let mut adopted: Option<CharId> = None;
        for (old_conn, old_char, was_playing) in dupes {
            if adopted.is_none() && was_playing {
                // USURP: the old socket is told its body was taken.
                self.out(old_conn, "\r\nThis body has been usurped!\r\n");
                adopted = Some(old_char);
            } else if adopted.is_none() {
                adopted = Some(old_char);
            }
            self.out(old_conn, "\r\nMultiple login detected -- disconnecting.\r\n");
            if let Some(d) = self.state.descriptors.get_mut(&old_conn) {
                // Detach WITHOUT the save/extract disconnect path: the body
                // lives on under this connection (C: k->character = NULL).
                d.character = None;
                d.state = ConState::Close;
            }
            // C 1521-1533: USURP room line + messages to the taker.
            if was_playing {
                if let Some(cid) = adopted {
                    crate::act::act(
                        &mut self.state,
                        "$n suddenly keels over in pain, surrounded by a white aura...\r\n$n's body has been taken over by a new spirit!",
                        true,
                        cid,
                        None,
                        crate::act::ActArg::None,
                        crate::act::To::Room,
                    );
                }
                self.out(conn_id, "You take over your own body, already in use!\r\n");
            } else {
                self.out(conn_id, "Reconnecting.\r\n");
            }
            info!("{} has re-logged in ... disconnecting old socket.", self.descriptor_name(conn_id));
        }
        let Some(body) = adopted else { return false; };
        // Re-attach this descriptor to the existing entity.
        if let Some(c) = self.state.get_char_mut(body) {
            c.desc = Some(conn_id);
        }
        self.state
            .players_by_name
            .insert(self.descriptor_name(conn_id).to_lowercase(), body);
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.character = Some(body);
            d.state = ConState::Playing;
        }
        self.write_prompt(conn_id);
        true
    }

    /// Load (or, for fresh chars, re-load) the player, place them in the
    /// world, and start play.
    async fn enter_game(&mut self, conn_id: ConnId, is_new: bool) {
        // C interpreter.c enter_player_game. The record was usually already
        // loaded at password-verify (pending_load) — consume it so login hits
        // the DB once.
        let name = self.descriptor_name(conn_id);
        let mut ch = match self.pending_load.remove(&conn_id) {
            Some(c) if c.get_name().eq_ignore_ascii_case(&name) => c,
            _ => match self.db.load_player(&name).await {
                Ok(c) => c,
                Err(e) => {
                    warn!("load player {} failed: {}", name, e);
                    self.out(conn_id, "Error loading your character.\r\n");
                    return;
                }
            },
        };
        // CON_QANSI answer and menu option 2's description land here, the way
        // C carries them on d->character (#198).
        if let Some(d) = self.state.descriptors.get(&conn_id) {
            if let Some(want) = d.wants_colour {
                if want {
                    ch.prf_flags |= crate::flags::PRF_COLOR_1 | crate::flags::PRF_COLOR_2;
                } else {
                    ch.prf_flags &= !(crate::flags::PRF_COLOR_1 | crate::flags::PRF_COLOR_2);
                }
            }
            if let Some(desc) = &d.temp_description {
                ch.player.description = desc.clone();
            }
        }
        let is_new_char = self.just_created.remove(&conn_id);
        if let Err(e) = crate::alias::read_aliases(&self.lib_path, ch.get_name(), ch.idnum) {
            warn!("read_aliases({}) failed: {}", ch.get_name(), e);
        }
        ch.desc = Some(conn_id);
        ch.aff_abils = ch.real_abils;
        // The player file/DB carries no object references (C semantics): the real
        // objects come entirely from the rent/crash file via crash_load below.
        // The mock DB clones the whole Character, so its carrying/equipment hold
        // stale ObjIds from the previous session — clear them or crash_load's
        // auto_equip sees the slots "occupied" and dumps worn items to inventory.
        ch.carrying.clear();
        ch.equipment = [None; NUM_WEARS];
        let id = self.state.create_char(ch);
        self.state.affect_total(id);
        self.state.players_by_name.insert(name.to_lowercase(), id);
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.character = Some(id);
            d.state = ConState::Playing;
        }

        // Refresh the index for this login: stamp last_logon to now and record
        // the connecting host (C sets GET_LAST_LOGON/host at enter), so a later
        // `last <name>` for this player shows their most recent session.
        let host = self
            .state
            .descriptors
            .get(&conn_id)
            .map(|d| d.host.clone())
            .unwrap_or_default();
        let now = chrono::Utc::now().timestamp();
        let index_snapshot = if let Some(c) = self.state.get_char_mut(id) {
            c.last_logon = chrono::Utc::now();
            Some(c.clone())
        } else {
            None
        };
        if let Some(c) = index_snapshot.as_ref() {
            self.state.update_player_index_from_character(c, now, &host);
        }

        // Room selection — interpreter.c enter_player_game (BUG #15). The C
        // precedence: GET_LOADROOM (saved.load_room) is honored first; a valid
        // saved.tloadroom (temporary, higher-priority — this is what do_copyover
        // stamps with the player's CURRENT room) overrides it and is then
        // cleared; valid surface-map coordinates (mapx/mapy) override both and
        // are cleared; finally, if nothing resolved, fall back to the normal
        // start room. Without this a copyover dumped everyone at the temple.
        //
        // PLR_* bits use the C structs.h values (the runtime act_flags column is
        // the raw C bitfield); defined locally to match enter_player_game.
        const PLR_FROZEN_C: i64 = 1 << 2;
        const PLR_KILLER_C: i64 = 1 << 0;
        const NEWBIE_ROOM: crate::types::RoomVnum = 2200; // config.c newbie_room
        const JAIL_NUM: crate::types::RoomVnum = 400; // config.c jail_num

        // Snapshot the saved room fields + flags (clone scalars before any
        // mutation; house style).
        let (saved_load, saved_tload, saved_mapx, saved_mapy, newbie, level, act_flags, prf2_flags) =
            self.state
                .get_char(id)
                .map(|c| {
                    (
                        c.load_room,
                        c.tloadroom,
                        c.mapx,
                        c.mapy,
                        c.newbie,
                        c.player.level,
                        c.act_flags,
                        c.prf2_flags,
                    )
                })
                .unwrap_or((crate::types::NOWHERE, 0, -1, -1, 0, 1, 0, 0));

        // GET_LOADROOM: real_room(saved.load_room) if it's a real vnum.
        let mut load_rnum: Option<RoomRnum> = if saved_load != crate::types::NOWHERE {
            self.state.real_room(saved_load)
        } else {
            None
        };

        // tloadroom (temporary copyover loadroom): if it resolves to a real
        // room, it WINS over load_room, and C clears it (set to -1) so it is
        // one-shot. C only clears tloadroom when it WAS valid (the assignment is
        // inside the `if (real_room(tloadroom) != NOWHERE)` block).
        //
        // C's saved.tloadroom sentinel is -1 (NOWHERE), but this port defaults
        // the field to 0 and may persist 0, and room vnum 0 ("The Void") IS a
        // real loadable room — so without a >=1 guard a normal (non-copyover)
        // login with tloadroom==0 would teleport into the Void. do_copyover only
        // ever stamps a real, positive room vnum, so treat anything < 1 as unset.
        let tload_vnum = saved_tload as crate::types::RoomVnum;
        if saved_tload >= 1 {
            if let Some(rnum) = self.state.real_room(tload_vnum) {
                load_rnum = Some(rnum);
                if let Some(c) = self.state.get_char_mut(id) {
                    c.tloadroom = -1; // C: saved.tloadroom = -1; (one-shot)
                }
            }
        }

        // If the resolved load_room is an IMPL-only room (ROOM_IMPROOM) and the
        // player is below LVL_GRGOD, discard it so they fall through to the start
        // room (C interpreter.c enter_player_game 1579-1581).
        const ROOM_IMPROOM_C: u32 = 1 << 16;
        if let Some(rnum) = load_rnum {
            if level < crate::types::LVL_GRGOD
                && self.state.room(rnum).room_flags.bits() & ROOM_IMPROOM_C != 0
            {
                load_rnum = None;
            }
        }

        // newbie loadroom (C: newbie == 1 && level < 5 -> newbie_room).
        if newbie == 1 && level < 5 {
            if let Some(rnum) = self.state.real_room(NEWBIE_ROOM) {
                load_rnum = Some(rnum);
            }
        }

        // Surface-map coordinates override (C: find_room_by_coords of mapx/mapy
        // when 1 <= mapx <= max_map_x && 1 <= mapy <= max_map_y), then C clears
        // mapx/mapy back to -1 unconditionally.
        if saved_mapx >= 1
            && saved_mapx <= self.state.max_map_x as i64
            && saved_mapy >= 1
            && saved_mapy <= self.state.max_map_y as i64
        {
            if let Some(rnum) = self
                .state
                .map_coords_to_rnum(saved_mapx as i32, saved_mapy as i32)
            {
                load_rnum = Some(rnum);
            }
        }
        if let Some(c) = self.state.get_char_mut(id) {
            c.mapx = -1;
            c.mapy = -1;
        }

        // Fall back to the normal start room when nothing above resolved (C: if
        // load_room == NOWHERE -> immort/mortal start room). Preserve the
        // existing Rust fallback chain (vnum 100 / hometown / 3001 / first room).
        let home = self
            .state
            .get_char(id)
            .map(|c| c.player.hometown)
            .unwrap_or(100);
        if load_rnum.is_none() {
            let start_vnum = if level >= crate::types::LVL_IMMORT {
                crate::config::IMMORT_START_ROOM
            } else {
                100
            };
            load_rnum = self
                .state
                .real_room(start_vnum)
                .or_else(|| self.state.real_room(home))
                .or_else(|| self.state.real_room(3001))
                .or_else(|| (!self.state.rooms.is_empty()).then_some(0));
        }

        // Frozen, then killer (C applies them in this order AFTER the fallback,
        // so killer wins if a player is somehow both). Each only overrides when
        // the override room actually exists, else the prior choice stands.
        if act_flags & PLR_FROZEN_C != 0 {
            if let Some(r) = self.state.real_room(crate::config::FROZEN_START_ROOM) {
                load_rnum = Some(r);
            }
        }
        if act_flags & PLR_KILLER_C != 0 {
            if let Some(r) = self.state.real_room(JAIL_NUM) {
                load_rnum = Some(r);
            }
        }

        // A ghost (PRF2_INTANGIBLE) who is not actively map-building
        // (PRF2_MBUILDING) always enters at room 99. This is the LAST override in
        // enter_player_game, so it wins over frozen/killer (C 1616-1618).
        const PRF2_INTANGIBLE_C: i64 = 1 << 9;
        const PRF2_MBUILDING_C: i64 = 1 << 6;
        if prf2_flags & PRF2_INTANGIBLE_C != 0 && prf2_flags & PRF2_MBUILDING_C == 0 {
            if let Some(r) = self.state.real_room(99) {
                load_rnum = Some(r);
            }
        }

        if let Some(rnum) = load_rnum {
            self.state.char_to_room(id, rnum);
        }
        // Restore the player's rented/crash-saved objects (objsave.c).
        crate::objsave::crash_load(&mut self.state, id, &self.lib_path);

        // C interpreter.c menu '1' (2261-2268): WELC_MESSG, then for a fresh
        // character do_start + START_MESSG + do_newbie; then the first look.
        // do_start ran in create_and_enter (before the DB write); do_newbie —
        // the starter item (obj 190, "an unfinished player's guide"),
        // recall level and wimpy 1 — runs here, in the world, past crash_load
        // (issue #207).
        self.state.send_to_char(id, WELC_MESSG);
        if is_new_char {
            self.state.send_to_char(id, START_MESSG);
            crate::class::do_newbie(&mut self.state, id);
        }
        crate::cmd_informative::look_at_room(&mut self.state, id, true);
        // C 2271-2272: "You have mail waiting."
        let idnum = self.state.get_char(id).map(|c| c.idnum).unwrap_or(0);
        if crate::mail::has_mail(idnum) {
            self.state.send_to_char(id, "You have mail waiting.\r\n");
        }
        let rnum = self.state.get_char(id).and_then(|c| c.in_room);
        if let Some(rnum) = rnum {
            crate::act::act(
                &mut self.state,
                "$n has entered the game.",
                true,
                id,
                None,
                crate::act::ActArg::None,
                crate::act::To::Room,
            );
            let _ = rnum;
        }
    }

    /// Copyover recovery (comm.c copyover_recover, per-player branch). The socket
    /// fd was inherited across execv and `name` was playing before the reboot.
    /// Register the descriptor (already wired to the live writer), load the player
    /// straight into Playing state (no nanny), and send the C "reboot completed"
    /// message. If the player file/DB load fails, send the C "lost in copyover"
    /// line and close the socket.
    async fn recover_player(
        &mut self,
        conn_id: ConnId,
        host: String,
        raw_fd: RawFd,
        name: String,
        output_tx: mpsc::Sender<String>,
    ) {
        info!("Copyover recovery: re-attaching {} (fd {})", name, raw_fd);
        let mut d = Descriptor::with_fd(conn_id, host, raw_fd);
        // The player was already greeted/logged-in pre-reboot; mark the name so
        // descriptor_name() / enter_game pick the right pfile, and start in
        // GetName so enter_game's state transition to Playing is well-defined.
        d.temp_name = Some(name.clone());
        d.state = ConState::GetName;
        self.state.descriptors.insert(conn_id, d);
        self.outputs.insert(conn_id, output_tx);

        // "\n\rRestoring from copyover...\n\r" was already written to the fd by
        // the OLD process right before exec (do_copyover); here we emit the C
        // "reboot completed" confirmation, then enter the world.
        let exists = self.db.player_exists(&name).await.unwrap_or(false);
        if !exists {
            // C: "\n\rSomehow, your character was lost in the copyover. Sorry.\n\r"
            self.out(
                conn_id,
                "\n\rSomehow, your character was lost in the copyover. Sorry.\n\r",
            );
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.state = ConState::Close;
            }
            return;
        }
        self.out(
            conn_id,
            "\n\rThe reboot has been completed. You may continue playing.\n\r",
        );
        // enter_game loads the pfile by descriptor_name(), places the char, runs
        // crash_load + look_at_room + "$n has entered the game." — exactly the
        // tail of copyover_recover (enter_player_game + look_at_room).
        self.enter_game(conn_id, false).await;
        self.write_prompt(conn_id);
    }

    async fn disconnect(&mut self, conn_id: ConnId) {
        // If the player was mid-OLC, drop the editor's working copy and release
        // the lock on the edited vnum (C frees the editor on connection
        // teardown; without this the per-conn state + vnum lock leak until the
        // next reboot — BUG #21). No-op if not editing.
        crate::olc::abort_editor(conn_id);
        let ch = self
            .state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.character);
        if let Some(cid) = ch {
            let mut alias_id_to_clear = None;
            // Persist then remove the character from the world.
            if let Some(snapshot) = self.snapshot_online_player_for_save(cid) {
                // Keep the index current with the saved record (level can
                // have changed this session); host carries over the last
                // login's host (update_player_index ignores an empty host).
                let (idnum, pname, llogon) = (
                    snapshot.idnum,
                    snapshot.get_name().to_string(),
                    snapshot.last_logon.timestamp(),
                );
                alias_id_to_clear = Some(idnum);
                self.state
                    .update_player_index_from_character(&snapshot, llogon, "");
                if let Err(err) = crate::alias::write_aliases(&self.lib_path, &pname, idnum) {
                    warn!("write_aliases({}) failed: {}", pname, err);
                }
                let db = self.db.clone();
                let host = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .map(|d| d.host.clone())
                    .unwrap_or_default();
                tokio::spawn(async move {
                    let _ = db.save_player_with_host(&snapshot, &host).await;
                });
            }
            crate::objsave::crash_save(&mut self.state, cid, &self.lib_path);
            self.state.extract_char(cid);
            if let Some(idnum) = alias_id_to_clear {
                crate::alias::clear_aliases(idnum);
            }
        }
        self.state.descriptors.remove(&conn_id);
        self.outputs.remove(&conn_id);
        info!("Connection {} closed", conn_id);
    }

    /// Async bridge for OFFLINE immortal commands (set/stat/show on a logged-off
    /// player's full record). cmd_wizard's offline branch queues an OfflineOp
    /// (GameState::queue_offline_op) instead of degrading to "no such player";
    /// this drains the queue. For each op we mirror C's retrieve_player_entry +
    /// edit + save_char: load the player from the DB, splice it into the world
    /// (like enter_game, minus the descriptor / start-room / look), REPLAY the
    /// immortal's verbatim command through command_interpreter — so the normal
    /// ONLINE do_set/do_stat/do_show logic applies and the immortal sees the
    /// usual output — then persist the (possibly edited) record and extract the
    /// char so it doesn't linger in the world. Runs between awaits in the run
    /// loop, so &mut self.state is free for the sync command_interpreter call.
    /// Drain clan-related deferred SQL (queued from sync command paths, #165).
    async fn drain_deferred_db_ops(&mut self) {
        let ops: Vec<crate::state::DeferredDbOp> =
            std::mem::take(&mut self.state.deferred_db_ops);
        for op in ops {
            let r = match op {
                crate::state::DeferredDbOp::ClanDestroyFixup(n) => {
                    self.db.clan_destroy_fixup(n).await
                }
                crate::state::DeferredDbOp::ClanLowerRanks(n) => {
                    self.db.clan_lower_ranks(n).await
                }
            };
            if let Err(e) = r {
                log::warn!("deferred clan DB op failed: {}", e);
            }
        }
    }

    async fn drain_offline_ops(&mut self) {
        // Take the queue so a replayed command that itself queued (it won't,
        // since the target is now present) wouldn't be processed re-entrantly.
        let ops = std::mem::take(&mut self.state.offline_ops);
        for op in ops {
            // The requester must still be online to receive the output.
            if !self.state.char_exists(op.requester) {
                continue;
            }
            // If the target raced back online (logged in between queue + drain),
            // just replay against the live char — no load/extract needed.
            let key = op.target.to_lowercase();
            if self.state.players_by_name.contains_key(&key) {
                dispatch_command_isolated(
                    &mut self.state,
                    op.requester,
                    &op.command,
                    "offline-op-live",
                );
                continue;
            }

            let mut chr = match self.db.load_player(&op.target).await {
                Ok(c) => c,
                Err(_) => {
                    self.state
                        .send_to_char(op.requester, "There is no such player.\r\n");
                    continue;
                }
            };
            // No live connection; clear stale object refs (the DB clone carries
            // last session's ObjIds — same hygiene as enter_game) so nothing in
            // the world dangles when we extract.
            chr.desc = None;
            chr.carrying.clear();
            chr.equipment = [None; NUM_WEARS];

            // Splice into the world and register the name so the replayed
            // command's online lookup (get_player_vis / find_player_by_name)
            // resolves it.
            let id = self.state.create_char(chr);
            self.state.affect_total(id);
            self.state.players_by_name.insert(key.clone(), id);
            // Place in a holding room (void vnum 3, else room 0) for in_room
            // safety; immortals target world-wide so the room is immaterial.
            if let Some(r) = self.state.real_room(3).or_else(|| self.state.real_room(0)) {
                self.state.char_to_room(id, r);
            }

            // Replay the immortal's verbatim command. Because the target is now
            // present, the handler's normal online branch applies the change (and
            // the immortal sees the standard output); the offline branch can't
            // re-trigger (the name resolves), so there's no re-deferral.
            dispatch_command_isolated(
                &mut self.state,
                op.requester,
                &op.command,
                "offline-op-replay",
            );

            // Snapshot the (possibly edited) record, drop it from the world, and
            // persist — mirroring C's save_char(ch, NOWHERE) after the edit.
            let snap = self.state.get_char(id).cloned();
            self.state.players_by_name.remove(&key);
            if let Some(ref s) = snap {
                self.state
                    .update_player_index_from_character(s, s.last_logon.timestamp(), "");
            }
            self.state.extract_char(id);
            if let Some(s) = snap {
                let _ = self.db.save_player(&s).await;
            }
        }
    }

    async fn drain_pfileclean(&mut self) {
        if !self.state.take_pfileclean_request() {
            return;
        }

        match self.db.delete_deleted_players().await {
            Ok(deleted) => {
                info!("pfileclean deleted {} PLR_DELETED player row(s)", deleted);
                match self.db.list_players().await {
                    Ok(players) => {
                        self.state.player_table = players;
                    }
                    Err(err) => {
                        warn!("pfileclean deleted rows but failed to rebuild player index: {err}");
                    }
                }
            }
            Err(err) => {
                warn!("pfileclean failed to delete PLR_DELETED player rows: {err}");
            }
        }
    }

    async fn drain_player_save_requests(&mut self) {
        let requests = self.state.take_player_save_requests();
        if requests.is_empty() {
            return;
        }

        let mut snapshots = Vec::new();
        for cid in requests {
            if let Some(snapshot) = self.snapshot_online_player_for_save(cid) {
                snapshots.push(snapshot);
            }
        }

        for snapshot in snapshots {
            let host = snapshot
                .desc
                .and_then(|conn| self.state.descriptors.get(&conn))
                .map(|d| d.host.clone())
                .unwrap_or_default();
            self.state.update_player_index_from_character(
                &snapshot,
                snapshot.last_logon.timestamp(),
                &host,
            );
            if let Err(err) =
                crate::alias::write_aliases(&self.lib_path, snapshot.get_name(), snapshot.idnum)
            {
                warn!(
                    "queued write_aliases({}) failed: {}",
                    snapshot.get_name(),
                    err
                );
            }
            if let Err(err) = self.db.save_player_with_host(&snapshot, &host).await {
                warn!(
                    "queued save_player({}) failed: {}",
                    snapshot.get_name(),
                    err
                );
            }
        }
    }

    // ---- Heartbeat ------------------------------------------------------
    fn heartbeat(&mut self) {
        // Crash-isolate the whole pulse: a panic in any handler (a mob script,
        // combat, weather, ...) must NOT kill the single Game task and freeze the
        // server. Catch it, log it (the panic hook also records a backtrace), and
        // continue on the next pulse. (Does not protect against a stack overflow /
        // abort — those are not unwinding panics.)
        // Time the whole pulse (the perf-relevant work lives in heartbeat_inner).
        // std::time::Instant is monotonic and cheap; record the duration in
        // microseconds into the lock-free metrics so /metrics can expose a tiny
        // deltamud_heartbeat_tick_micros and its high-water mark.
        let start = std::time::Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.heartbeat_inner();
        }));
        let micros = start.elapsed().as_micros() as u64;
        self.metrics.record_tick_micros(micros);
        self.metrics.set_pulse(self.state.pulse);

        // Refresh the world-size gauges periodically (every 10 pulses ~= 1s) to
        // keep the per-pulse cost negligible. players = playing descriptors;
        // mobs = NPC characters; objs = world objects.
        if self.state.pulse % 10 == 0 {
            self.refresh_who_snapshot();
            let players = self
                .state
                .descriptors
                .values()
                .filter(|d| d.state == ConState::Playing && d.character.is_some())
                .count() as u64;
            let total_chars = self.state.chars.len() as u64;
            // mobs = all characters minus the player-controlled ones.
            let mobs = total_chars.saturating_sub(players);
            self.metrics.set_players(players);
            self.metrics.set_mobs(mobs);
            self.metrics.set_objs(self.state.objs.len() as u64);
        }

        if let Err(e) = r {
            log::error!(
                "PANIC in heartbeat (pulse {}): {} — skipping rest of pulse",
                self.state.pulse,
                panic_payload_str(&e)
            );
        }
    }

    fn heartbeat_inner(&mut self) {
        self.state.pulse = self.state.pulse.wrapping_add(1);
        let pulse = self.state.pulse;

        // Drain queued player input through the WAIT_STATE gate (C game_loop:
        // `--d->wait <= 0 && get_from_q(...)`), one command per descriptor.
        self.process_input_queues();

        // C comm.c:1001-1058 heartbeat(): stage order and cadences below
        // mirror the oracle exactly (issues #192/#225). Input draining is the
        // game_loop's job and stays above.
        crate::dg_event::process_events(&mut self.state);
        // PULSE_DG_SCRIPT (dg_scripts.h): random/idle trigger scan.
        if pulse % 130 == 0 {
            crate::dg_scripts::script_trigger_check(&mut self.state);
        }
        if pulse % PULSE_ZONE == 0 {
            self.zone_update();
        }
        // 15 seconds: reap sockets sitting at login prompts, then auctions.
        if pulse % (15 * PASSES_PER_SEC) == 0 {
            self.check_idle_passwords();
        }
        if pulse % (15 * PASSES_PER_SEC) == 0 {
            crate::auction::auction_update(&mut self.state);
        }
        if pulse % PULSE_MOBILE == 0 {
            crate::mobact::mobile_activity(&mut self.state);
        }
        if pulse % PULSE_VIOLENCE == 0 {
            combat::perform_violence(&mut self.state);
        }
        // Live surface weather (storms spawn/move/collide/expire) every 30
        // RL-seconds.
        if pulse % (30 * PASSES_PER_SEC) == 0 {
            crate::maputils::weather_activity(&mut self.state);
        }
        // Autoquest update + room blood decay, every minute.
        if pulse % (60 * PASSES_PER_SEC) == 0 {
            crate::quest::quest_update(&mut self.state);
            crate::maputils::blood_update(&mut self.state);
        }
        // Mud-hour block (SECS_PER_MUD_HOUR * PASSES_PER_SEC = 750 pulses):
        // calendar/sky, affect aging (comm.c:1038, #96), then regen/conditions.
        if pulse % 750 == 0 { // SECS_PER_MUD_HOUR(75) * PASSES_PER_SEC(10)
            crate::weather::weather_and_time(&mut self.state);
            crate::magic::affect_update(&mut self.state);
            crate::limits::point_update(&mut self.state);
        }
        // 1-minute autosave block (C: auto_save && pulse % 60s) with the
        // autosave_time (config.c:174 = 5) minute gate: Crash_save_all +
        // House_save_all (#192; the old 75-second crash-save tick was 4x
        // C's cadence and houses were never saved at all).
        if pulse % (60 * PASSES_PER_SEC) == 0 {
            self.mins_since_crashsave += 1;
            if self.mins_since_crashsave >= crate::config::AUTOSAVE_TIME {
                self.mins_since_crashsave = 0;
                crate::objsave::crash_save_all(&mut self.state);
                crate::house::house_save_all(&mut self.state);
            }
        }

        // GMCP drain (W5): mob pulses, combat rounds and regen all ran above
        // and marked stale connections; push fresh snapshots so a client's
        // gauges track mob-initiated damage without waiting for a command.
        let stale: Vec<ConnId> = self.state.gmcp_dirty.drain().collect();
        for conn_id in stale {
            if let Some(d) = self.state.descriptors.get(&conn_id) {
                if d.gmcp && d.state == ConState::Playing {
                    self.push_gmcp_update(conn_id);
                }
            }
        }
    }

    /// C comm.c:2049-2069 check_idle_passwords(): a descriptor sitting at a
    /// name/password prompt for two consecutive 15-second ticks is disconnected
    /// with C's message.
    fn check_idle_passwords(&mut self) {
        let mut to_close: Vec<ConnId> = Vec::new();
        for (cid, d) in self.state.descriptors.iter_mut() {
            if matches!(
                d.state,
                ConState::GetName
                    | ConState::GetOldPassword
                    | ConState::GetNewPassword
                    | ConState::ConfirmPassword
                    | ConState::ConfirmName
            ) {
                d.idle_tics += 1;
                if d.idle_tics >= 2 {
                    d.outbuf.push_str("\r\nTimed out... goodbye.\r\n");
                    to_close.push(*cid);
                }
            }
        }
        for cid in to_close {
            if let Some(d) = self.state.descriptors.get_mut(&cid) {
                d.state = ConState::Close;
            }
        }
    }

    fn zone_update(&mut self) {
        // C db.c:1877-1952 zone_update (#231). A 60-second accumulator ages
        // the zones (NOT one age tick per 10-second PULSE_ZONE call); zones
        // reaching their lifespan are queued (age = ZO_DEAD) and at most ONE
        // queued zone is reset per tick, gated on room emptiness unless
        // reset_mode == 2.
        const ZO_DEAD: i32 = crate::world::ZONE_DEAD;
        self.zone_minute_timer += 1;
        if (self.zone_minute_timer * PULSE_ZONE) / PASSES_PER_SEC >= 60 {
            self.zone_minute_timer = 0;
            let mut enqueue: Vec<i32> = Vec::new();
            for z in self.state.zones.iter_mut() {
                if z.age < z.lifespan && z.reset_mode != 0 {
                    z.age += 1;
                }
                if z.age >= z.lifespan && z.age < ZO_DEAD && z.reset_mode != 0 {
                    enqueue.push(z.number);
                    z.age = ZO_DEAD;
                }
            }
            self.zone_reset_queue.extend(enqueue);
        }
        if self.zone_reset_queue.is_empty() {
            return;
        }
        let mut idx = 0;
        while idx < self.zone_reset_queue.len() {
            let zn = self.zone_reset_queue[idx];
            let reset_mode = self
                .state
                .zones
                .iter()
                .find(|z| z.number == zn)
                .map(|z| z.reset_mode)
                .unwrap_or(0);
            if reset_mode == 2 || self.zone_is_empty(zn) {
                self.zone_reset_queue.remove(idx);
                self.state.reset_zone(zn);
                let name = self
                    .state
                    .zones
                    .iter()
                    .find(|z| z.number == zn)
                    .map(|z| z.name.clone())
                    .unwrap_or_default();
                crate::syslog::mudlog(
                    &mut self.state,
                    &format!("Auto zone reset: {}", name),
                    crate::syslog::CMP,
                    LVL_GOD,
                );
                break;
            }
            idx += 1;
        }
    }

    /// C db.c:2150 is_empty(zone_nr): true when no playing descriptor's
    /// character stands in the zone.
    fn zone_is_empty(&self, zone_number: i32) -> bool {
        for d in self.state.descriptors.values() {
            if d.state != ConState::Playing {
                continue;
            }
            if let Some(cid) = d.character {
                if let Some(c) = self.state.get_char(cid) {
                    if let Some(rnum) = c.in_room {
                        if let Some(room) = self.state.room_opt(rnum) {
                            if room.zone == zone_number {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        true
    }

    // ---- Output flushing ------------------------------------------------
    /// C comm.c:762 auto-reboot clock (finish-the-game activation): once a
    /// minute, compare wall-clock time to the setreboot schedule; warn at the
    /// warn time and save-all + graceful shutdown at the reboot time.
    fn autoreboot_check(&mut self) {
        if !self.state.config.autoreboot {
            return;
        }
        let (rh, rm, wh, wm) = crate::cmd_wizard::reboot_schedule();
        if rh < 0 {
            return;
        }
        use chrono::Timelike;
        let now = chrono::Utc::now();
        let (hr, min) = (now.hour() as i32, now.minute() as i32);
        if hr == wh && min == wm && !self.reboot_warned {
            self.reboot_warned = true;
            let msg = format!(
                "&m[&YINFO&m]&n The game will reboot in {} minutes. Please rent.\r\n",
                if rm >= wm { rm - wm } else { 60 - (wm - rm) }
            );
            self.state.send_to_all_players(&msg);
            crate::syslog::mudlog(&mut self.state, "Automatic reboot imminent.", crate::syslog::NRM, 0);
        }
        if hr == rh && min == rm {
            info!("Auto-reboot triggered; saving and restarting.");
            crate::syslog::mudlog(&mut self.state, "Automatic reboot triggered.", crate::syslog::NRM, 0);
            crate::objsave::crash_save_all(&mut self.state);
            crate::house::house_save_all(&mut self.state);
            crate::olc::flush_save_list_to_disk(&mut self.state);
            self.state.shutdown_requested = true;
        }
    }

    async fn flush_all(&mut self) {
        let conns: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        let mut to_close = Vec::new();
        for conn_id in conns {
            let (text, closing) = {
                let d = match self.state.descriptors.get_mut(&conn_id) {
                    Some(d) => d,
                    None => continue,
                };
                let t = std::mem::take(&mut d.outbuf);
                (t, d.state == ConState::Close)
            };
            if !text.is_empty() {
                // C comm.c:1637-1642 (#221): the whole buffer (output + prompt)
                // is proc_color'd with the viewer's colour mode — mortals in a
                // magic-fog room get the -1 scramble, others get
                // clr(ch, C_NRM) (level >= 2 renders, below strips).
                let mode = {
                    let ch_id = self
                        .state
                        .descriptors
                        .get(&conn_id)
                        .and_then(|d| d.character);
                    match ch_id.map(|c| self.state.get_char(c)).flatten() {
                        Some(c) => {
                            let in_fog = c
                                .in_room
                                .map(|r| {
                                    self.state.room(r).weather
                                        == crate::maputils::WEATHER_MAGICFOG as i32
                                })
                                .unwrap_or(false);
                            if in_fog && c.player.level < LVL_IMMORT {
                                -1
                            } else if crate::olc::colour_level(&self.state, ch_id.unwrap()) >= 2 {
                                1
                            } else {
                                0
                            }
                        }
                        None => 1,
                    }
                };
                let rendered = crate::connection::proc_color(&text, mode, |max| {
                    1 + self.state.rng.dice(1, max)
                });
                if let Some(tx) = self.outputs.get(&conn_id) {
                    // C comm.c:1713 closes on would-block rather than waiting:
                    // a client that stops reading must not park the Game task
                    // (a full bounded channel means the writer is stalled on
                    // TCP backpressure). try_send + close on Full is the
                    // non-blocking equivalent; the loop's to_close pass
                    // disconnects the descriptor below.
                    if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
                        tx.try_send(rendered)
                    {
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                    }
                }
            }
            if closing {
                to_close.push(conn_id);
            }
        }
        for conn_id in to_close {
            self.disconnect(conn_id).await;
        }
    }

    /// C comm.c make_prompt (1213-1293) for playing descriptors (#220): the
    /// invis prefix, the DISPHP/DISPMANA/DISPMOVE vitals, AFK, the
    /// DISPEXP-to-level counter, the DISPMOB opponent condition, mail-waiting
    /// and drunk indicators, and the final prompt mark.
    fn make_playing_prompt(&mut self, conn_id: ConnId) -> String {
        use crate::flags::{
            PRF_AFK, PRF_DISPEXP, PRF_DISPHP, PRF_DISPMANA, PRF_DISPMOVE, PRF2_DISPMOB,
        };
        let Some(cid) = self
            .state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.character)
        else {
            return String::new();
        };
        let c = match self.state.get_char(cid) {
            Some(c) => c,
            None => return String::new(),
        };
        let mut prompt = String::new();
        let invis = c.invis_level;
        if invis > 0 {
            prompt.push_str(&format!("&Ri&Y{}&n ", invis));
        }
        if c.prf_flags & PRF_DISPHP != 0 {
            prompt.push_str(&format!("&G{}&ghp&w ", c.points.hit));
        }
        if c.prf_flags & PRF_DISPMANA != 0 {
            prompt.push_str(&format!("&C{}&cmp&w ", c.points.mana));
        }
        if c.prf_flags & PRF_DISPMOVE != 0 {
            match c.riding.map(|rid| self.state.get_char(rid)).flatten() {
                Some(mount) => prompt.push_str(&format!(
                    "&M{}&m&ym&mmv&w ",
                    mount.points.move_points
                )),
                None => prompt.push_str(&format!("&M{}&mmv&w ", c.points.move_points)),
            }
        }
        let mut fighting_diag: Option<String> = None;
        if c.prf_flags & PRF_AFK != 0 {
            prompt.push_str("&W(&naway&W)&n ");
        } else {
            if c.prf_flags & PRF_DISPEXP != 0 && c.player.level < LVL_HERO {
                let need = crate::class::exp_to_level(c.player.level as i32);
                prompt.push_str(&format!("&W(&n{}&W) ", need - c.points.exp));
            }
            if c.prf_flags & PRF2_DISPMOB != 0 {
                if let Some(vict) = c.fighting {
                    if let Some(v) = self.state.get_char(vict) {
                        let percent = if v.points.max_hit > 0 {
                            (100 * v.points.hit) / v.points.max_hit
                        } else {
                            -1
                        };
                        // C act.informative.c:239-266 prompt_diag.
                        let word = match percent {
                            p if p >= 100 => "excellent",
                            p if p >= 90 => "scratched",
                            p if p >= 75 => "bruised",
                            p if p >= 50 => "wounded",
                            p if p >= 30 => "nasty",
                            p if p >= 15 => "hurt",
                            p if p >= 0 => "awful",
                            _ => "bleeding",
                        };
                        fighting_diag = Some(word.to_string());
                    }
                }
            }
        }
        if let Some(word) = fighting_diag {
            prompt.push_str(&format!("&R(&n{}&R) ", word));
        }
        let idnum = c.idnum;
        if crate::mail::has_mail(idnum) {
            prompt.push_str("&B(&Ymail&B)&n ");
        }
        if c.conditions[DRUNK] > 4 {
            prompt.push_str("&G(&ndrunk&G)&n ");
        }
        prompt.push_str("&R>&w ");
        prompt
    }

    fn write_prompt(&mut self, conn_id: ConnId) {
        // C make_prompt (comm.c:1220-1226): an active pager or string editor
        // owns the prompt, whatever the connection state (#229).
        if crate::modify::page_active(conn_id) {
            let (page, count) = crate::modify::page_position(conn_id);
            let prompt = format!(
                "\r[ Return to continue, (q)uit, (r)efresh, (b)ack, or page number ({}/{}) ]",
                page, count
            );
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.write(&prompt);
            }
            return;
        }
        if crate::modify::editing_any(conn_id) {
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.write("] ");
            }
            return;
        }
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };
        let name = self
            .state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.temp_name.clone());
        let prompt = match state {
            ConState::QAnsi => String::new(), // question sent on connect / on retry
            ConState::GetName => ASK_NAME.to_string(),
            ConState::ConfirmName => {
                // C interpreter.c:1759: "Did I get that right, %s &c(&YY&c/&YN&c)&n? "
                format!(
                    "Did I get that right, {} &c(&YY&c/&YN&c)&n? ",
                    name.unwrap_or_default()
                )
            }
            ConState::GetOldPassword => "Password: ".to_string(),
            ConState::GetNewPassword => format!(
                "Give me a password for {}: ",
                name.unwrap_or_default()
            ),
            ConState::ConfirmPassword => "\r\nPlease retype password: ".to_string(),
            ConState::GetNewbie => {
                "Are you completely new to MUDing &c(&YY&c/&YN&c)&n? ".to_string()
            }
            ConState::GetSex => "\r\nWhat is your sex &c(&YM&c/&YF&c)&n? ".to_string(),
            ConState::GetRace => format!(
                "{}\r\nTo see a race's average statistics type help <race letter>.\r\nRace: ",
                crate::races::RACE_MENU
            ),
            ConState::GetDeity => format!("{}\r\nDeity: ", crate::deity::DEITY_MENU),
            ConState::GetClass => {
                format!("{}\r\nClass: ", crate::class::CLASS_MENU)
            }
            ConState::GetHometown => format!("{}\r\nTown: ", crate::class::TOWN_MENU),
            ConState::RollStats => self
                .pending
                .get(&conn_id)
                .map(|p| stat_roll_prompt(p.rolled))
                .unwrap_or_default(),
            ConState::ReadMotd => String::new(), // "*** PRESS RETURN" sent on transition
            ConState::Menu => String::new(),     // MENU sent on transition
            ConState::ExDesc => String::new(),   // string editor owns the input
            ConState::ChPwdGetOld => "\r\nEnter your old password: ".to_string(),
            ConState::ChPwdGetNew => "\r\nEnter a new password: ".to_string(),
            ConState::ChPwdVerify => "\r\nPlease retype password: ".to_string(),
            ConState::DelCnf1 => "\r\nEnter your password for verification: ".to_string(),
            ConState::DelCnf2 => "\r\nYOU ARE ABOUT TO DELETE THIS CHARACTER PERMANENTLY.\r\n\
 ARE YOU ABSOLUTELY SURE?\r\n\r\nPlease type \"yes\" to confirm: "
            .to_string(),
            ConState::Playing => {
                // C comm.c:1213-1293 make_prompt: the full PRF_* chain (#220).
                self.make_playing_prompt(conn_id)
            }
            _ => String::new(),
        };
        // Before a password prompt, tell the client the server WILL echo so it
        // suppresses local echo (cleartext password no longer shows). The IAC
        // bytes go straight down the output channel; the prompt text follows in
        // the next outbuf flush, so the client sees WILL-ECHO first.
        if is_password_state(state) {
            self.send_raw_bytes(conn_id, &IAC_WILL_ECHO);
        }
        if !prompt.is_empty() {
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.write(&prompt);
            }
        }

        // Out-of-band GMCP push: after the prompt, but only when something
        // actually made this connection's state stale since the last push
        // (W5 event-driven GMCP: idle players no longer get a per-command
        // re-send of identical JSON; players in combat DO get fresh vitals
        // from mob pulses via the heartbeat drain below).
        if state == ConState::Playing && self.state.gmcp_dirty.remove(&conn_id) {
            self.push_gmcp_update(conn_id);
        }
    }

    // ---- small helpers --------------------------------------------------
    fn out(&mut self, conn_id: ConnId, msg: &str) {
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.write(msg);
        }
    }

    /// Send raw bytes straight down a connection's output channel, bypassing the
    /// outbuf/render_color String pipeline (used for telnet IAC control
    /// sequences whose lone 0xFF byte must not pass through `.chars()`). Mirrors
    /// connection.rs's negotiation-refusal path: the writer only ever calls
    /// `.as_bytes()`, so wrapping arbitrary bytes in a String is lossless.
    fn send_raw_bytes(&self, conn_id: ConnId, bytes: &[u8]) {
        if let Some(tx) = self.outputs.get(&conn_id) {
            // SAFETY: the downstream writer only calls `.as_bytes()` on this
            // String and never treats it as UTF-8 text; the bytes round-trip
            // unchanged (same contract as connection.rs run_input_loop refusals).
            let s = unsafe { String::from_utf8_unchecked(bytes.to_vec()) };
            // try_send avoids making this async; the bounded(256) channel is
            // effectively never full for a 3-byte control sequence, and dropping
            // an echo-negotiation byte under extreme backpressure is harmless.
            let _ = tx.try_send(s);
        }
    }
    fn descriptor_name(&self, conn_id: ConnId) -> String {
        self.state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.temp_name.clone())
            .unwrap_or_default()
    }

    // ---- GMCP (out-of-band JSON) ---------------------------------------

    /// Handle `GameMessage::EnableGmcp`: flip the descriptor's gmcp flag (set in
    /// connection.rs after we replied `IAC WILL GMCP`) and, if the connection is
    /// already in-world, push an initial Char.Vitals/Room.Info so a client that
    /// negotiates mid-session lights its gauges/mapper immediately rather than
    /// waiting for the next command.
    fn enable_gmcp(&mut self, conn_id: ConnId) {
        let playing = match self.state.descriptors.get_mut(&conn_id) {
            Some(d) => {
                d.gmcp = true;
                d.state == ConState::Playing
            }
            None => return,
        };
        if playing {
            self.push_gmcp_update(conn_id);
        }
    }

    /// Send the per-command GMCP snapshot (`Char.Vitals` + `Room.Info`) to a
    /// GMCP-enabled descriptor that has a playing character. JSON is hand-rolled
    /// (no serde dep): small, one-line, with `"`/`\` escaped in names. Bytes go
    /// down the raw-bytes channel verbatim, never through render_color.
    fn push_gmcp_update(&self, conn_id: ConnId) {
        for message in self.gmcp_snapshots(conn_id) {
            self.send_raw_bytes(conn_id, &telnet_subneg(TELOPT_GMCP, message.as_bytes()));
        }
    }

    /// Pure snapshot builder: the GMCP messages (names + JSON payloads) for a
    /// connection, or empty when the connection is not GMCP-enabled/playing.
    /// Split from push_gmcp_update so tests can assert on payloads without a
    /// live output channel.
    fn gmcp_snapshots(&self, conn_id: ConnId) -> Vec<String> {
        let d = match self.state.descriptors.get(&conn_id) {
            Some(d) if d.gmcp => d,
            _ => return Vec::new(),
        };
        let ch = match d.character {
            Some(c) => c,
            None => return Vec::new(),
        };
        let c = match self.state.get_char(ch) {
            Some(c) => c,
            None => return Vec::new(),
        };
        let mut messages = Vec::with_capacity(2);

        // Char.Vitals — current/max HP, mana, move.
        let p = &c.points;
        let vitals = gmcp_message(
            "Char.Vitals",
            &serde_json::json!({
                "hp": p.hit,
                "maxhp": p.max_hit,
                "mana": p.mana,
                "maxmana": p.max_mana,
                "move": p.move_points,
                "maxmove": p.max_move,
                "level": c.player.level,
            }),
        );
        messages.push(vitals);

        // Room.Info — vnum, name, zone, exits as {dir: dest-vnum}, plus the
        // closed/locked door lists the mapper needs (W5). Occupancy lists the
        // other characters in the room so GUIs can draw fellow players.
        if let Some(rnum) = c.in_room {
            if let Some(room) = self.state.room_opt(rnum) {
                let zone_name = self
                    .state
                    .zones
                    .get(room.zone as usize)
                    .map(|z| z.name.as_str())
                    .unwrap_or("");
                let dir_keys = ["n", "e", "s", "w", "u", "d"];
                let mut exits = serde_json::Map::new();
                let mut doors: Vec<&str> = Vec::new();
                let mut locked: Vec<&str> = Vec::new();
                for (i, key) in dir_keys.iter().enumerate() {
                    if let Some(ex) = room.exits.get(i).and_then(|e| e.as_ref()) {
                        exits.insert(key.to_string(), serde_json::json!(ex.to_room));
                        if ex.exit_info & crate::room::EX_CLOSED != 0 {
                            doors.push(key);
                            if ex.exit_info & crate::room::EX_LOCKED != 0 {
                                locked.push(key);
                            }
                        }
                    }
                }
                let occupants: Vec<String> = room
                    .people
                    .iter()
                    .filter(|&&other| other != ch)
                    .filter_map(|&other| self.state.get_char(other))
                    .filter(|other| !other.is_npc)
                    .map(|other| other.get_name().to_string())
                    .collect();
                let room_info = gmcp_message(
                    "Room.Info",
                    &serde_json::json!({
                        "num": room.number,
                        "name": gmcp_clean(&room.name),
                        "zone": gmcp_clean(zone_name),
                        "exits": exits,
                        "doors": doors,
                        "locked": locked,
                        "players": occupants,
                        "map": {
                            "x": room.map_x.unwrap_or(0),
                            "y": room.map_y.unwrap_or(0),
                        },
                    }),
                );
                messages.push(room_info);
            }
        }
        messages
    }

    /// Rebuild the /api/who JSON snapshot (same visibility rules as the
    /// who2html walk: playing, non-npc, no invis level, not AFF_INVISIBLE).
    fn refresh_who_snapshot(&mut self) {
        use serde_json::json;
        let mut entries: Vec<(u8, serde_json::Value)> = Vec::new();
        let ids: Vec<CharId> = self.state.players_by_name.values().copied().collect();
        for cid in ids {
            let Some(c) = self.state.get_char(cid) else { continue };
            if c.is_npc {
                continue;
            }
            if c.invis_level > 0 || c.affect_flags & crate::flags::AFF_INVISIBLE != 0 {
                continue;
            }
            entries.push((
                c.player.level,
                json!({
                    "name": c.get_name(),
                    "level": c.player.level,
                    "race": crate::whohtml::race_name(&self.state, cid),
                    "class": crate::whohtml::class_name(&self.state, cid),
                    "immortal": c.player.level >= LVL_IMMORT,
                    "title": c.player.title.clone().unwrap_or_default(),
                }),
            ));
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let names: Vec<serde_json::Value> = entries.into_iter().map(|(_, v)| v).collect();
        let doc = json!({
            "count": names.len(),
            "players": names,
            "generated_at": self.started_at,
        });
        if let Ok(mut slot) = self.who_snapshot.write() {
            *slot = doc.to_string();
        }
    }

    // ---- MSSP (Mud Server Status Protocol) -----------------------------

    /// Handle `GameMessage::SendMssp`: build and send the one-shot MSSP status
    /// block (`IAC SB MSSP <VAR name VAL value>... IAC SE`). Crawlers/listing
    /// sites read this to index the server. PLAYERS/UPTIME need the live Game,
    /// which is why this is driven from here rather than connection.rs.
    fn send_mssp(&self, conn_id: ConnId) {
        // Count players currently in-world (a character attached, in Playing).
        let players = self
            .state
            .descriptors
            .values()
            .filter(|d| d.state == ConState::Playing && d.character.is_some())
            .count();
        // Listen port: read MUD_PORT exactly as config.rs does (the Game isn't
        // handed the Config), defaulting to the CircleMUD default 4000.
        let port: u16 = std::env::var("MUD_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);

        let mut payload: Vec<u8> = Vec::with_capacity(128);
        let mut add = |name: &str, value: &str| {
            payload.push(MSSP_VAR);
            payload.extend_from_slice(name.as_bytes());
            payload.push(MSSP_VAL);
            payload.extend_from_slice(value.as_bytes());
        };
        add("NAME", "DeltaMUD");
        add("PLAYERS", &players.to_string());
        // MSSP UPTIME = unix timestamp the server booted.
        add("UPTIME", &self.started_at.to_string());
        add("PORT", &port.to_string());
        add("CODEBASE", "DeltaMUD-Rust");
        add("FAMILY", "CircleMUD");

        self.send_raw_bytes(conn_id, &telnet_subneg(TELOPT_MSSP, &payload));
    }
}

/// Pending character-creation choices held between nanny steps.
#[derive(Clone, Copy)]
struct PendingChoices {
    sex: Gender,
    class: Class,
    race: Race,
    race_index: i32,
    newbie: u8,
    deity: u8,
    hometown: RoomVnum,
    rolled: Abilities,
}
impl Default for PendingChoices {
    fn default() -> Self {
        PendingChoices {
            sex: Gender::Neutral,
            class: Class::Warrior,
            race: Race::Human,
            race_index: crate::races::RACE_HUMAN,
            newbie: 1,
            deity: crate::deity::DEITY_AETOS as u8,
            hometown: 1,
            rolled: Abilities::default(),
        }
    }
}

fn stat_roll_prompt(abils: Abilities) -> String {
    format!(
        "\r\nStr: {} Int: {} Wis: {} Dex: {} Con: {} Cha: {}\r\n\
Are these values acceptable? (Y/&YN&n): ",
        abils.str, abils.intel, abils.wis, abils.dex, abils.con, abils.cha
    )
}

fn normalize_name(s: &str) -> String {
    let mut c = s.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn valid_name(name: &str) -> bool {
    // C: MAX_NAME_LENGTH == 20 (structs.h) — the player-name field is 20+1, and
    // the nanny name-entry path caps names at MAX_NAME_LENGTH, not 16 (BUG #16).
    name.len() >= 2 && name.len() <= 20 && name.chars().all(|c| c.is_ascii_alphabetic())
}

/// C interpreter.c:694-718: fill words ("in from with the on at to") and the
/// reserved list ("a an self me all room someone something") are both refused
/// as player names (#223).
fn reserved_or_fill_word(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "a", "an", "self", "me", "all", "room", "someone", "something",
    ];
    crate::interpreter::FILL_WORDS
        .iter()
        .any(|f| f.eq_ignore_ascii_case(name))
        || RESERVED
            .iter()
            .any(|r| r.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mock_database::MockDatabase;
    use crate::DatabaseInterface;
    use std::sync::Arc;
    use std::sync::{Mutex, OnceLock};

    pub(super) fn test_game(db: Arc<MockDatabase>) -> Game {
        let db_trait: Arc<dyn DatabaseInterface> = db;
        let mut cfg = Config::default();
        // Keep the user_cntr USRCNT write (lib/../USRCNT) out of the repo.
        cfg.lib_path = std::env::temp_dir()
            .join(format!("deltamud-game-lib-{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&cfg.lib_path);
        Game::new(GameState::new(cfg), db_trait)
    }

    fn attach_descriptor(game: &mut Game, conn: ConnId) {
        attach_descriptor_host(game, conn, "example.test");
    }

    pub(super) fn attach_descriptor_host(game: &mut Game, conn: ConnId, host: &str) {
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, host.to_string()));
    }

    /// Attach a descriptor and answer the CON_QANSI colour question so the
    /// connection sits at GetName (tests written against the pre-#198 flow).
    async fn attach_descriptor_at_name(game: &mut Game, conn: ConnId, host: &str) {
        attach_descriptor_host(game, conn, host);
        game.nanny(conn, "y".to_string()).await;
        assert_eq!(descriptor_state(game, conn), ConState::GetName);
    }

    fn descriptor_state(game: &Game, conn: ConnId) -> ConState {
        game.state.descriptors.get(&conn).unwrap().state
    }

    #[test]
    fn online_save_snapshot_accumulates_played_time_and_resets_logon() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(1);
        attach_descriptor(&mut game, conn);

        let mut ch = crate::character::Character::new_player(
            "Timer".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.desc = Some(conn);
        ch.player.time_played = 40;
        ch.last_logon = chrono::Utc::now() - chrono::Duration::seconds(90);
        let cid = game.state.create_char(ch);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(cid);

        let snapshot = game.snapshot_online_player_for_save(cid).unwrap();

        assert!(snapshot.player.time_played >= 130);
        let live = game.state.get_char(cid).unwrap();
        assert_eq!(live.player.time_played, snapshot.player.time_played);
        assert_eq!(live.last_logon, snapshot.last_logon);
        assert!((chrono::Utc::now() - live.last_logon).num_seconds() <= 1);
    }

    fn ban_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn zone_update_ages_once_per_minute_and_queues_resets() {
        // C db.c:1877-1952 (#231): six PULSE_ZONE ticks make one minute;
        // a zone reaching its lifespan is queued (age = ZO_DEAD). An OCCUPIED
        // zone (reset_mode 1) is not reset until a tick finds it empty.
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        game.state.zones.push(crate::world::Zone {
            number: 30,
            name: "Test Zone".into(),
            builders: String::new(),
            lifespan: 1,
            age: 0,
            top: 3099,
            reset_mode: 1,
            min_level: 0,
            max_level: 0,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        });
        let rnum = game
            .state
            .add_room(crate::room::Room::new(3001, 30, "z".into(), "".into()));

        // An idle player inside the zone keeps zone_is_empty() false.
        let conn = ConnId(55);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "example.test".to_string()));
        let mut occupant = crate::character::Character::new_player(
            "Zoner".to_string(),
            Class::Warrior,
            Race::Human,
        );
        occupant.desc = Some(conn);
        let oid = game.state.create_char(occupant);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(oid);
        game.state.descriptors.get_mut(&conn).unwrap().state = ConState::Playing;
        game.state.char_to_room(oid, rnum);

        for _ in 0..5 {
            game.zone_update();
        }
        assert_eq!(game.state.zones[0].age, 0, "no minute has fully passed");

        game.zone_update(); // 6th tick = 60 s
        assert_eq!(game.state.zones[0].age, crate::world::ZONE_DEAD);
        assert_eq!(game.zone_reset_queue, vec![30]);

        // Occupied: the queued reset must NOT fire.
        game.zone_update();
        assert_eq!(game.zone_reset_queue, vec![30], "occupied zone waits");

        // The occupant leaves: the next tick resets the zone.
        game.state.char_from_room(oid);
        game.zone_update();
        assert!(game.zone_reset_queue.is_empty(), "empty zone resets");
        assert_eq!(game.state.zones[0].age, 0);
    }

    #[test]
    fn drained_input_resets_idle_timer_and_returns_from_void() {
        // C comm.c:894-903 (#217): a drained command zeroes the idle timer
        // and returns a void-idled character to their previous room.
        let mut g = GameState::new(Config::default());
        g.add_room(crate::room::Room::new(3001, 30, "Home".into(), "".into()));
        g.add_room(crate::room::Room::new(3002, 30, "Elsewhere".into(), "".into()));
        let mut ch =
            crate::character::Character::new_player("Idler".to_string(), Class::Warrior, Race::Human);
        ch.timer = 9; // past the >8 void threshold
        let cid = g.create_char(ch);

        let conn = ConnId(77);
        g.descriptors.insert(conn, Descriptor::new(conn, "example.test".to_string()));
        let mut observer = crate::character::Character::new_player(
            "Watcher".to_string(),
            Class::Warrior,
            Race::Human,
        );
        observer.desc = Some(conn);
        let obs = g.create_char(observer);
        g.descriptors.get_mut(&conn).unwrap().character = Some(obs);

        g.char_to_room(obs, 0);
        g.char_to_room(cid, 0);
        // Simulate the void pull (limits.rs check_idling): was_in saved, char
        // parked elsewhere.
        g.get_char_mut(cid).unwrap().was_in_room = Some(0);
        g.char_to_room(cid, 1);

        reset_idle_on_input(&mut g, cid);

        let c = g.get_char(cid).unwrap();
        assert_eq!(c.timer, 0, "drained command must reset the idle timer");
        assert_eq!(c.in_room, Some(0));
        assert_eq!(c.was_in_room, None);
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("has returned"), "observer saw: {out:?}");
    }

    fn temp_ban_lib(name: &str, badsites: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("deltamud-ban-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("etc")).unwrap();
        std::fs::create_dir_all(path.join("misc")).unwrap();
        std::fs::write(path.join("etc/badsites"), badsites).unwrap();
        std::fs::write(path.join("misc/xnames"), "").unwrap();
        path.to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn creation_walks_c_nanny_choice_sequence() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(1);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Alice".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ConfirmName);
        game.nanny(conn, "y".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetNewPassword);
        game.nanny(conn, "secret".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::ConfirmPassword);
        game.nanny(conn, "secret".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetNewbie);
        game.nanny(conn, "n".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetSex);
        game.nanny(conn, "f".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetRace);
        game.nanny(conn, "a".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetDeity);
        game.nanny(conn, "b".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetClass);
        game.nanny(conn, "c".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetHometown);
        game.nanny(conn, "b".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::RollStats);

        let pending = game.pending.get(&conn).unwrap();
        assert_eq!(pending.newbie, 0);
        assert_eq!(pending.sex, Gender::Female);
        assert_eq!(pending.race_index, crate::races::RACE_HUMAN);
        assert_eq!(pending.deity, crate::deity::DEITY_CORGUS as u8);
        assert_eq!(pending.class, Class::Warrior);
        assert_eq!(pending.hometown, 2);
        assert!(pending.rolled.str > 0);
        assert!(pending.rolled.con > 0);
    }

    #[tokio::test]
    async fn accepted_creation_stats_are_started_and_saved() {
        let db = Arc::new(MockDatabase::new());
        let seed = crate::character::Character::new_player(
            "Seed".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&seed, "seedpass").await.unwrap();

        let mut game = test_game(db.clone());
        let conn = ConnId(2);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Bob".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "m".to_string()).await;
        game.nanny(conn, "d".to_string()).await;
        game.nanny(conn, "c".to_string()).await;
        game.nanny(conn, "b".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::RollStats);
        let accepted = game.pending.get(&conn).unwrap().rolled;

        game.nanny(conn, "y".to_string()).await;
        // C start_player: creation ends at MOTD -> PRESS RETURN -> MENU (#198).
        assert_eq!(descriptor_state(&game, conn), ConState::ReadMotd);
        game.nanny(conn, String::new()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        game.nanny(conn, "1".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Playing);

        let saved = db.load_player("Bob").await.unwrap();
        assert_eq!(saved.idnum, 2);
        assert_eq!(saved.player.level, 1);
        assert_eq!(saved.trust, 1);
        assert_eq!(saved.points.exp, 1);
        assert_eq!(saved.player.sex, Gender::Male);
        assert_eq!(saved.race_index_for_test(), crate::races::RACE_DWARF);
        assert_eq!(saved.player.deity, crate::deity::DEITY_LYTHERN as u8);
        assert_eq!(saved.player.class, Class::Thief);
        assert_eq!(saved.player.hometown, 1);
        assert_eq!(saved.newbie, 1);
        assert_eq!(saved.clan, -1);
        assert_eq!(saved.clan_rank, -1);
        assert_eq!(saved.tloadroom, -1);
        assert_eq!(saved.real_abils.str, accepted.str);
        assert_eq!(saved.aff_abils.dex, accepted.dex);
        assert_eq!(saved.points.hit, saved.points.max_hit);
        assert_eq!(saved.points.mana, saved.points.max_mana);
        assert_eq!(saved.points.move_points, saved.points.max_move);
        assert_eq!(saved.conditions[THIRST], 24);
        assert_eq!(saved.conditions[FULL], 24);
        assert_eq!(saved.conditions[DRUNK], 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPHP != 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPMANA != 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPMOVE != 0);
        assert!(saved.prf_flags & crate::flags::PRF_DISPEXP != 0);
        assert!(saved.prf_flags & crate::flags::PRF_NOLOOKSTACK != 0);
        assert!(saved.prf2_flags & crate::flags::PRF2_DISPMOB != 0);
    }

    #[tokio::test]
    async fn first_created_character_still_bootstraps_implementor() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let conn = ConnId(3);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "First".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "secret".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "m".to_string()).await;
        game.nanny(conn, "a".to_string()).await;
        game.nanny(conn, "a".to_string()).await;
        game.nanny(conn, "c".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, String::new()).await; // RMOTD -> MENU
        game.nanny(conn, "1".to_string()).await; // enter the game

        let saved = db.load_player("First").await.unwrap();
        assert_eq!(saved.idnum, 1);
        assert_eq!(saved.player.level, LVL_IMPL);
        assert_eq!(saved.player.title.as_deref(), Some("the Implementor"));
        assert_ne!(
            saved.godcmds1 | saved.godcmds2 | saved.godcmds3 | saved.godcmds4,
            0
        );
    }

    #[tokio::test]
    async fn ban_new_blocks_new_character_confirmation_by_host() {
        let _guard = ban_test_lock();
        let lib = temp_ban_lib("new", "new *.blocked.test 0 Root\n");
        crate::ban::boot_ban(&lib);

        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(5);
        attach_descriptor_at_name(&mut game, conn, "sub.blocked.test").await;

        game.nanny(conn, "Denied".to_string()).await;
        game.nanny(conn, "y".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("new characters are not allowed"));
        let empty = temp_ban_lib("empty-new", "");
        crate::ban::boot_ban(&empty);
    }

    #[tokio::test]
    async fn ban_select_blocks_login_without_siteok_after_password() {
        let _guard = ban_test_lock();
        let lib = temp_ban_lib("select", "select blocked.test 0 Root\n");
        crate::ban::boot_ban(&lib);

        let db = Arc::new(MockDatabase::new());
        let ch = crate::character::Character::new_player(
            "Blocked".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&ch, "secret").await.unwrap();
        let mut game = test_game(db);
        let conn = ConnId(6);
        attach_descriptor_at_name(&mut game, conn, "blocked.test").await;

        game.nanny(conn, "Blocked".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);
        game.nanny(conn, "secret".to_string()).await;

        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("has not been cleared for login"));
        let empty = temp_ban_lib("empty-select", "");
        crate::ban::boot_ban(&empty);
    }

    #[test]
    fn complex_alias_expands_through_descriptor_queue_one_pulse_at_a_time() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(4);
        let mut ch = crate::character::Character::new_player(
            "Aliaser".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.idnum = 44;
        ch.desc = Some(conn);
        let cid = game.state.create_char(ch);

        crate::alias::set_aliases(
            44,
            vec![crate::alias::AliasEntry {
                alias: "combo".to_string(),
                replacement: "bogus-one;bogus-two".to_string(),
                atype: 1,
            }],
        );

        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.state = ConState::Playing;
        d.character = Some(cid);
        d.input_queue
            .push_back(QueuedInput::raw("combo".to_string()));
        game.state.descriptors.insert(conn, d);

        game.process_input_queues();
        let queued = &game.state.descriptors.get(&conn).unwrap().input_queue;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued.front().unwrap().line, "bogus-two");
        assert!(queued.front().unwrap().aliased);

        game.process_input_queues();
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .input_queue
            .is_empty());
        crate::alias::clear_aliases(44);
    }

    trait RaceIndexForTest {
        fn race_index_for_test(&self) -> i32;
    }

    impl RaceIndexForTest for crate::character::Character {
        fn race_index_for_test(&self) -> i32 {
            self.player.race as u8 as i32
        }
    }

    #[tokio::test]
    async fn wrong_password_reprompts_then_disconnects_at_max_bad_pws() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let conn = ConnId(20);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        let seed = crate::character::Character::new_player(
            "Pwtest".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&seed, "right").await.unwrap();
        game.nanny(conn, "Pwtest".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);

        // First failure: re-prompt (C max_bad_pws = 2) (#194).
        game.nanny(conn, "wrong".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::GetOldPassword);
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Wrong password."));
        assert_eq!(db.load_player("Pwtest").await.unwrap().bad_pws, 1);

        // Second failure: disconnect.
        game.nanny(conn, "wrong".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Wrong password... disconnecting."));
    }

    #[tokio::test]
    async fn duplicate_login_usurps_the_live_body() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        let seed = crate::character::Character::new_player(
            "Dupe".to_string(),
            Class::Warrior,
            Race::Human,
        );
        db.create_player(&seed, "pw").await.unwrap();

        // First login walks all the way into the game.
        let c1 = ConnId(21);
        attach_descriptor_at_name(&mut game, c1, "a.test").await;
        game.nanny(c1, "Dupe".to_string()).await;
        game.nanny(c1, "pw".to_string()).await;
        game.nanny(c1, String::new()).await;
        game.nanny(c1, "1".to_string()).await;
        assert_eq!(descriptor_state(&game, c1), ConState::Playing);
        let body = game.state.descriptors.get(&c1).unwrap().character.unwrap();

        // Second login on another connection takes the body over (#218).
        let c2 = ConnId(22);
        attach_descriptor_at_name(&mut game, c2, "b.test").await;
        game.nanny(c2, "Dupe".to_string()).await;
        game.nanny(c2, "pw".to_string()).await;
        assert_eq!(descriptor_state(&game, c2), ConState::Playing);
        assert_eq!(game.state.descriptors.get(&c2).unwrap().character, Some(body));
        // The old socket is detached and closing, with the usurp message.
        assert_eq!(game.state.descriptors.get(&c1).unwrap().character, None);
        assert_eq!(descriptor_state(&game, c1), ConState::Close);
        assert!(game
            .state
            .descriptors
            .get(&c1)
            .unwrap()
            .outbuf
            .contains("This body has been usurped!"));
        // Exactly one entity carries the idnum.
        let owners: Vec<CharId> = game
            .state
            .descriptors
            .values()
            .filter_map(|d| d.character)
            .collect();
        assert_eq!(owners, vec![body]);
    }

    #[tokio::test]
    async fn menu_option_zero_says_goodbye() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(23);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        game.nanny(conn, String::new()).await;
        // QANSI 'y' lands at GetName, not ReadMotd; walk: the ReadMotd arm
        // is reachable directly for this check.
        if let Some(d) = game.state.descriptors.get_mut(&conn) {
            d.state = ConState::ReadMotd;
        }
        game.nanny(conn, String::new()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        game.nanny(conn, "9".to_string()).await;
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("That's not a menu choice!"));
        assert_eq!(descriptor_state(&game, conn), ConState::Menu);
        game.nanny(conn, "0".to_string()).await;
        assert_eq!(descriptor_state(&game, conn), ConState::Close);
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("land called reality"));
    }

    #[tokio::test]
    async fn input_doubles_dollars_and_supports_history() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(30);
        attach_descriptor(&mut game, conn);
        let mut ch = crate::character::Character::new_player(
            "Hist".to_string(),
            Class::Warrior,
            Race::Human,
        );
        ch.desc = Some(conn);
        let cid = game.state.create_char(ch);
        game.state.descriptors.get_mut(&conn).unwrap().character = Some(cid);
        game.state.descriptors.get_mut(&conn).unwrap().state = ConState::Playing;

        // '$' is doubled on entry so act() renders one literal '$' (#222).
        game.handle_input(conn, "say Hi $n".to_string()).await;
        assert_eq!(
            game.state.descriptors.get(&conn).unwrap().input_queue.back().map(|q| q.line.clone()),
            Some("say Hi $$n".to_string())
        );

        // '!' repeats the previous line, '^old^new' substitutes (#224).
        game.state.descriptors.get_mut(&conn).unwrap().input_queue.clear();
        game.handle_input(conn, "!".to_string()).await;
        assert_eq!(
            game.state.descriptors.get(&conn).unwrap().input_queue.back().map(|q| q.line.clone()),
            Some("say Hi $$n".to_string())
        );
        game.handle_input(conn, "^Hi^Bye".to_string()).await;
        assert_eq!(
            game.state.descriptors.get(&conn).unwrap().input_queue.back().map(|q| q.line.clone()),
            Some("say Bye $$n".to_string())
        );
        // Bad substitution refuses cleanly.
        game.handle_input(conn, "^zzz^qqq".to_string()).await;
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Invalid substitution."));
    }

    #[test]
    fn perform_subst_mirrors_the_c_semantics() {
        // C comm.c:1911-1960: '^telm^tell' repairs the typo in last_input.
        assert_eq!(
            Game::perform_subst("telm bob hello", "^telm^tell").as_deref(),
            Some("tell bob hello")
        );
        assert_eq!(
            Game::perform_subst("say Hi", "^Hi^Bye").as_deref(),
            Some("say Bye")
        );
        assert_eq!(Game::perform_subst("say Hi", "^zzz^qqq"), None);
        assert_eq!(Game::perform_subst("say Hi", "^Hi"), None);
    }

    #[test]
    fn scratch_name_debug() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let name = game_normalize(&mut game, "Wanderer");
        println!("normalized: {:?}", name);
        println!("valid_name: {}", valid_name(&name));
        println!("reserved_or_fill: {}", reserved_or_fill_word(&name));
        println!("ban::valid_name_in: {}", crate::ban::valid_name_in(&game.state, &name));
    }

    fn game_normalize(game: &mut Game, s: &str) -> String {
        let _ = game;
        normalize_name(s)
    }

    #[test]
    fn reserved_and_fill_words_are_rejected_as_names() {
        // C interpreter.c:694-718 (#223).
        assert!(reserved_or_fill_word("me"));
        assert!(reserved_or_fill_word("all"));
        assert!(reserved_or_fill_word("something"));
        assert!(reserved_or_fill_word("the"));
        assert!(!reserved_or_fill_word("Thrall"));
    }

    fn test_mob_proto(vnum: MobVnum, name: &str) -> crate::world::MobileProto {
        crate::world::MobileProto {
            vnum,
            name: name.to_string(),
            short_desc: name.to_string(),
            long_desc: format!("{} is here.\r\n", name),
            description: String::new(),
            level: 1,
            hitpoints: 1,
            hit_dice: (0, 0, 1),
            experience: 0,
            gold: 0,
            position: Position::Standing,
            default_pos: Position::Standing,
            sex: Gender::Neutral,
            alignment: 0,
            act_flags: 0,
            affect_flags: 0,
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 1,
            power: 0,
            mpower: 0,
            defense: 0,
            mdefense: 0,
            technique: 0,
            abilities: None,
            attack_type: 0,
        }
    }

    #[tokio::test]
    async fn mob_keyword_names_are_rejected() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        // A mob prototype whose keywords include "dragon".
        let proto = test_mob_proto(3001, "red dragon Dragon");
        game.state.mob_protos.insert(3001, proto);
        let conn = ConnId(40);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        game.nanny(conn, "dragon".to_string()).await;
        // Still at GetName with the C refusal, not ConfirmName.
        assert_eq!(descriptor_state(&game, conn), ConState::GetName);
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Invalid name, please try another."));
    }

    #[tokio::test]
    async fn quest_e2e_kill_quest_assigns_and_rewards() {
        // The shipped lib (sibling of the crate) carries the authored quest
        // content; skip on exotic checkouts without it.
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        if !std::path::Path::new(&format!("{}/world/worldmap", lib)).exists() {
            return;
        }
        let mut g = crate::state::GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib).await.unwrap();
        g.prime_zones();

        let room100 = g.real_room(100).unwrap();
        let mut player = crate::character::Character::new_player("Rmeln".into(), Class::Warrior, Race::Human);
        player.player.level = 3;
        let pl = g.create_char(player);
        g.char_to_room(pl, room100);

        let qm = crate::quest::find_questmaster(&g, pl)
            .expect("questmaster must be present in room 100");

        // C denies probabilistically (qchance(15) + the 99-candidate lottery),
        // so retry until a target is assigned.
        let live: Vec<u32> = g
            .char_ids()
            .into_iter()
            .filter(|c| g.get_char(*c).map(|c| c.is_npc).unwrap_or(false))
            .filter_map(|c| g.get_char(c).map(|c| c.nr as u32))
            .collect();
        // C rolls 50/50 between kill quests and object quests, denies
        // probabilistically, and locks out re-requests on deny — so retry,
        // clearing the deny lockout, until a KILL quest is assigned.
        let mut qmob = 0i32;
        for _ in 0..40 {
            crate::quest::do_autoquest(&mut g, pl, "request", 0);
            qmob = g.get_char(pl).unwrap().quest_mob;
            if qmob > 0 {
                break;
            }
            if let Some(c) = g.get_char_mut(pl) {
                c.next_quest = 0;
                c.quest_obj = 0; // drop an object-quest draw; we want a kill quest
                c.act_flags &= !(1 << 16); // PLR_QUESTOR
                c.quest_countdown = 0;
            }
        }
        assert!(qmob > 0, "a kill-target quest must be assigned, got {}", qmob);

        let victim = g
            .char_ids()
            .into_iter()
            .find(|c| g.get_char(*c).map(|c| c.nr == qmob).unwrap_or(false))
            .expect("target instance must exist");
        assert!(crate::quest::quest_on_kill(&mut g, pl, victim));
        assert!(g.get_char(pl).unwrap().quest_mob < 0);

        g.get_char_mut(pl).unwrap().quest_countdown = 5;
        crate::quest::do_autoquest(&mut g, pl, "complete", 0);
        let pts = g.get_char(pl).unwrap().quest_points;
        assert!(pts > 0, "reward quest points must be granted, got {}", pts);
        let _ = qm;
    }


    #[tokio::test]
    async fn creation_password_guards_match_c() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(50);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;

        game.nanny(conn, "Guard".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        // "New character." banner precedes the password prompt (C 1774).
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("New character."));

        // C interpreter.c:2043-2045: >64 chars, name-equality, and <3 all
        // refuse with 'Illegal password.' (#319).
        for bad in ["a", &"x".repeat(65), "Guard"] {
            game.nanny(conn, bad.to_string()).await;
            assert_eq!(descriptor_state(&game, conn), ConState::GetNewPassword);
            assert!(game
                .state
                .descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Illegal password."));
        }

        // A legal password proceeds; mismatch shows C's 'start over.' text.
        game.nanny(conn, "goodpw".to_string()).await;
        game.nanny(conn, "otherpw".to_string()).await;
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Passwords don't match... start over."));
        assert_eq!(descriptor_state(&game, conn), ConState::GetNewPassword);
    }

    #[tokio::test]
    async fn sex_retry_uses_c_inline_prompt() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db);
        let conn = ConnId(51);
        attach_descriptor_at_name(&mut game, conn, "example.test").await;
        game.nanny(conn, "Sexer".to_string()).await;
        game.nanny(conn, "y".to_string()).await;
        game.nanny(conn, "pw12345".to_string()).await;
        game.nanny(conn, "pw12345".to_string()).await;
        game.nanny(conn, "y".to_string()).await; // newbie
        game.nanny(conn, "q".to_string()).await; // invalid sex
        assert!(game
            .state
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("That is not a sex..\r\nWhat IS your sex? "));
        assert_eq!(descriptor_state(&game, conn), ConState::GetSex);
    }
}

#[cfg(test)]
mod gmcp_tests {
    use super::tests::{attach_descriptor_host, test_game};
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::mock_database::MockDatabase;
    use crate::room::{Exit, Room};
    use crate::types::{Class, Race};
    use std::sync::Arc;

    #[test]
    fn movement_marks_gmcp_dirty_and_heartbeat_drains_it() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game.state.add_room(Room::new(100, 1, "A".into(), String::new()));
        let b = game.state.add_room(Room::new(101, 1, "B".into(), String::new()));
        let conn = ConnId(60);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        let ch = playing_char(&mut game, conn, "Gmcp", a);

        // A room transfer marks the mover and the bystanders stale.
        game.state.char_from_room(ch);
        game.state.char_to_room(ch, b);
        assert!(
            game.state.gmcp_dirty.contains(&conn),
            "transfer must mark the mover's connection dirty"
        );

        // The heartbeat drain pushes a snapshot and empties the set.
        game.heartbeat_inner();
        assert!(game.state.gmcp_dirty.is_empty(), "drain must clear the set");
    }

    #[test]
    fn gmcp_room_info_carries_doors_and_valid_json_names() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game.state.add_room(Room::new(
            100,
            1,
            "The \"Quoted\" &RRoom".into(),
            String::new(),
        ));
        let b = game.state.add_room(Room::new(101, 1, "B".into(), String::new()));
        game.state.rooms[a].exits[EAST] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: crate::room::EX_CLOSED | crate::room::EX_LOCKED,
            key: -1,
            to_room: 101,
        });
        game.state.rooms[b].exits[WEST] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: -1,
            to_room: 100,
        });
        let conn = ConnId(61);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        playing_char(&mut game, conn, "Doors", a);

        let messages = game.gmcp_snapshots(conn);
        let room_info = messages
            .iter()
            .find(|m| m.starts_with("Room.Info "))
            .expect("Room.Info must be part of the snapshot");
        let json = room_info.split_once(' ').unwrap().1;
        let value: serde_json::Value =
            serde_json::from_str(json).expect("Room.Info must be valid JSON");
        assert_eq!(value["num"], 100);
        assert_eq!(value["name"], "The \"Quoted\" Room", "&R color code stripped");
        assert_eq!(value["exits"]["e"], 101);
        assert_eq!(value["doors"][0], "e");
        assert_eq!(value["locked"][0], "e");
    }

    #[test]
    fn combat_damage_marks_both_sides_dirty() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game.state.add_room(Room::new(100, 1, "A".into(), String::new()));
        let conn = ConnId(63);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        let ch = playing_char(&mut game, conn, "Punched", a);
        let mut npc = Character::new_npc(500);
        npc.position = crate::types::Position::Standing;
        let npc = game.state.create_char(npc);
        game.state.char_to_room(npc, a);

        game.state.gmcp_dirty.clear();
        crate::combat::damage(&mut game.state, npc, ch, 5);
        assert!(
            game.state.gmcp_dirty.contains(&conn),
            "damage must stale the victim's vitals"
        );

        // Snapshot contains fresh vitals.
        let messages = game.gmcp_snapshots(conn);
        assert!(messages.iter().any(|m| m.starts_with("Char.Vitals ")));
    }

    #[test]
    fn non_gmcp_descriptors_get_no_snapshots() {
        let mut game = test_game(Arc::new(MockDatabase::new()));
        let a = game.state.add_room(Room::new(100, 1, "A".into(), String::new()));
        let conn = ConnId(64);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "t".into()));
        playing_char(&mut game, conn, "Plain", a);
        game.state.descriptors.get_mut(&conn).unwrap().gmcp = false;

        assert!(game.gmcp_snapshots(conn).is_empty());
        // Marking still happens (cheap) but the drain filters by d.gmcp.
        game.state.note_gmcp_room(a);
        game.heartbeat_inner();
        assert!(game.state.gmcp_dirty.is_empty(), "drain clears everything");
    }

    fn playing_char(game: &mut Game, conn: ConnId, name: &str, room: usize) -> CharId {
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.player.level = 10;
        let cid = game.state.create_char(ch);
        game.state.char_to_room(cid, room);
        let d = game.state.descriptors.get_mut(&conn).unwrap();
        d.gmcp = true;
        d.state = ConState::Playing;
        d.character = Some(cid);
        if let Some(c) = game.state.get_char_mut(cid) {
            c.desc = Some(conn);
        }
        cid
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::tests::test_game;
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::mock_database::MockDatabase;
    use crate::room::Room;
    use crate::types::{Class, Race};
    use std::sync::Arc;

    /// W6: the extracted shutdown_save must persist a playing character
    /// (SQL row + alias sidecar + rent file) and report what it did, so a
    /// real SIGTERM shutdown is a verified path, not a hope.
    #[tokio::test]
    async fn shutdown_save_persists_player_inventory_and_reports() {
        let db = Arc::new(MockDatabase::new());
        let mut game = test_game(db.clone());
        // test_game points lib_path at a fresh temp dir; plrobjs lives under it.
        let plrobjs = format!("{}/plrobjs", game.state.config.lib_path);
        std::fs::create_dir_all(&plrobjs).unwrap();

        let room = game.state.add_room(Room::new(3001, 30, "Save Room".into(), String::new()));
        let conn = ConnId(70);
        game.state
            .descriptors
            .insert(conn, Descriptor::new(conn, "saver.example.test".into()));

        let mut ch = Character::new_player("Shutdownee".to_string(), Class::Warrior, Race::Human);
        ch.player.level = 22;
        ch.points.gold = 4321;
        ch.player.title = Some("the Persisted".to_string());
        // A playing character carries a persistent idnum (create_player row);
        // save_player_with_host UPDATEs by it.
        ch.idnum = db.create_player(&ch, "pw").await.expect("create row");
        // enter_game sets PLR_CRASH on login: it is the crash_save trigger.
        ch.act_flags |= crate::objsave::PLR_CRASH;
        let cid = game.state.create_char(ch);
        game.state.char_to_room(cid, room);

        // Inventory: a real loaded object so crash_save has something to write.
        game.state.obj_protos.insert(
            9010,
            crate::world::ObjectProto {
                vnum: 9010,
                name: "brick gold".into(),
                short_desc: "a gold brick".into(),
                description: "A gold brick sits here.".into(),
                obj_type: crate::object::ObjectType::Armor,
                wear_flags: crate::object::WearFlags::TAKE,
                extra_flags: crate::object::ExtraFlags::empty(),
                weight: 20,
                cost: 50000,
                rent: 5000,
                values: [0; 4],
                curr_slots: 0,
                total_slots: 0,
                obj_class: 0,
                min_level: 0,
                bitvector: 0,
                action_description: String::new(),
                affects: Vec::new(),
                ex_descriptions: Vec::new(),
            },
        );
        let obj = game.state.load_object(9010).expect("brick loads");
        game.state.obj_to_char(obj, cid);

        // Attach as Playing.
        {
            let d = game.state.descriptors.get_mut(&conn).unwrap();
            d.state = ConState::Playing;
            d.character = Some(cid);
        }
        if let Some(c) = game.state.get_char_mut(cid) {
            c.desc = Some(conn);
        }
        // Register an output channel so flush_all has something to drain.
        let (tx, mut rx) = mpsc::channel(256);
        game.outputs.insert(conn, tx);

        let report = game.shutdown_save().await;

        assert_eq!(report.players_saved, 1);
        assert_eq!(report.save_errors, 0);
        // The shutdown notice + prompt went through the output channel.
        let drained = rx.recv().await.expect("shutdown notice flushed");
        assert!(drained.contains("shutting down"), "notice must be flushed");

        // SQL row: reload through the db and check the core fields survived.
        let loaded = db.load_player("Shutdownee").await.expect("player persisted");
        assert_eq!(loaded.player.level, 22);
        assert_eq!(loaded.points.gold, 4321);
        assert_eq!(loaded.player.title.as_deref(), Some("the Persisted"));

        // Rent file for the inventory: plrobjs/<bucket>/<name>.objs (the
        // bucket is the name's first-letter range, e.g. A-E / U-Z).
        let mut found = false;
        for entry in std::fs::read_dir(&plrobjs).unwrap().filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                if let Ok(files) = std::fs::read_dir(&p) {
                    for f in files.filter_map(|f| f.ok()) {
                        if f.file_name()
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains("shutdownee")
                        {
                            found = true;
                        }
                    }
                }
            }
        }
        fn walk(dir: &std::path::Path, depth: usize) {
            if depth > 3 { return; }
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                eprintln!("TREE: {}", p.display());
                if p.is_dir() { walk(&p, depth + 1); }
            }
        }
        if !found {
            walk(std::path::Path::new(&plrobjs), 0);
        }
        assert!(found, "rent file must exist for the saved player");
    }
}
