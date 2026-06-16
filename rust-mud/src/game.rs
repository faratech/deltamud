// Game: the async shell around the synchronous GameState. It owns the world,
// drains the input channel, runs commands/nanny to completion against
// &mut GameState, drives the heartbeat, and flushes each descriptor's output
// buffer to its writer task. This is the only place async meets the world.

use crate::combat;
use crate::connection::{render_color, ConState, Descriptor, GameMessage};
use crate::interpreter::command_interpreter;
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

/// Minimal JSON string escaper for hand-rolled GMCP payloads: escapes the two
/// characters that would break a JSON string literal (`"` and `\`) and strips
/// control bytes (newlines, the lone IAC) that have no place in a one-line
/// GMCP message. Room/zone names can carry color codes (`&x`) or quotes; this
/// keeps the emitted JSON valid without pulling in serde.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Drop other control chars (incl. a stray 0xFF) and the color-code
            // introducer so the JSON stays clean for the client/mapper.
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// True if `s` is one of the password-entry connection states (the only states
/// whose prompts must suppress client-side echo).
fn is_password_state(s: ConState) -> bool {
    matches!(
        s,
        ConState::GetOldPassword | ConState::GetNewPassword | ConState::ConfirmPassword
    )
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
        command_interpreter(state, ch, input);
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

pub struct Game {
    state: GameState,
    db: Arc<dyn DatabaseInterface>,
    /// Async output channel per connection (the writer half lives in the
    /// connection task). The Descriptor (in GameState) only buffers text.
    outputs: HashMap<ConnId, mpsc::Sender<String>>,
    /// Character-creation choices accumulated across nanny steps.
    pending: HashMap<ConnId, PendingChoices>,
    lib_path: String,
    /// Lock-free observability counters, shared with the metrics HTTP task.
    /// Updated on the heartbeat hot path (atomics, no mutex).
    metrics: Arc<Metrics>,
    /// Unix timestamp the Game task started, for the MSSP UPTIME datum (which
    /// reports the server boot time per the MSSP spec).
    started_at: i64,
}

impl Game {
    pub fn new(state: GameState, db: Arc<dyn DatabaseInterface>) -> Self {
        Game {
            state,
            db,
            outputs: HashMap::new(),
            pending: HashMap::new(),
            lib_path: "./lib".to_string(),
            metrics: Arc::new(Metrics::new()),
            started_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Install the shared metrics handle (main.rs creates one Arc and shares it
    /// with both the Game and the HTTP task). Defaults to a private Metrics so
    /// the Game is usable without one (e.g. in tests).
    pub fn set_metrics(&mut self, metrics: Arc<Metrics>) {
        self.metrics = metrics;
    }

    pub fn state_mut(&mut self) -> &mut GameState {
        &mut self.state
    }

    pub async fn load_text_files(&mut self, lib_path: &str) {
        self.lib_path = lib_path.to_string();
        let motd_path = std::path::Path::new(lib_path).join("text").join("motd");
        match tokio::fs::read_to_string(&motd_path).await {
            Ok(s) => self.state.motd = s,
            Err(_) => self.state.motd = "\r\nWelcome to DeltaMUD!\r\n".to_string(),
        }
    }

    pub fn prime_zones(&mut self) {
        let (mobs, objs) = self.state.prime_zones();
        info!("Initial zone prime: +{} mobs, +{} objs", mobs, objs);
        // C boots the surface map (read_map) which calls init_weather, so the
        // world starts with MAX_WEATHER storms already on the map. Prime them
        // here so the weather map shows live storms from the first tick.
        crate::maputils::prime_weather(&mut self.state);
    }

    pub async fn run(&mut self, mut game_rx: mpsc::Receiver<GameMessage>) -> Result<()> {
        info!("Game loop starting...");
        let mut tick = interval(Duration::from_millis(100)); // 10 pulses/sec

        // SIGTERM stream (systemd stop / kill -TERM). Ctrl-C (SIGINT) is handled
        // by tokio::signal::ctrl_c. On either, we run a clean shutdown: crash-save
        // every player + their objects, flush descriptors, and return.
        let mut sigterm = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
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
        // Count online players (those with a character attached) before saving.
        let n_players = self
            .state
            .descriptors
            .values()
            .filter(|d| d.character.is_some())
            .count();

        // Notify everyone still connected.
        let conn_ids: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        for cid in &conn_ids {
            self.out(*cid, "\r\nThe server is shutting down. Saving and disconnecting...\r\n");
        }

        // Crash-save all rent/inventory + persist every online player file.
        crate::objsave::crash_save_all(&mut self.state);
        for cid in &conn_ids {
            if let Some(ch) = self.state.descriptors.get(cid).and_then(|d| d.character) {
                if let Some(c) = self.state.get_char(ch) {
                    if !c.is_npc {
                        let snapshot = c.clone();
                        if let Err(e) = self.db.save_player(&snapshot).await {
                            warn!("shutdown save_player({}) failed: {}", snapshot.get_name(), e);
                        }
                    }
                }
            }
        }

        // Flush all buffered output (the shutdown notice) to the writer tasks.
        self.flush_all().await;
        // Give the per-connection writer tasks a moment to drain to the socket
        // before the process exits and their channels are dropped.
        tokio::time::sleep(Duration::from_millis(200)).await;

        info!("Shutting down, saved {} player(s).", n_players);
    }

    async fn handle_message(&mut self, msg: GameMessage) {
        match msg {
            GameMessage::NewConnection { id, host, raw_fd, output_tx } => {
                info!("New connection from {}", host);
                self.metrics.inc_connections();
                let mut d = Descriptor::with_fd(id, host, raw_fd);
                d.write("\r\n&YWelcome to DeltaMUD!&n\r\n\r\n");
                self.state.descriptors.insert(id, d);
                self.outputs.insert(id, output_tx);
                self.write_prompt(id);
            }
            GameMessage::Recover { id, host, raw_fd, name, output_tx } => {
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

    async fn handle_input(&mut self, conn_id: ConnId, input: String) {
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };

        if state == ConState::Playing {
            if crate::modify::page_active(conn_id) {
                crate::modify::page_input(&mut self.state, conn_id, &input);
            } else if crate::modify::editing(&self.state, conn_id) {
                crate::modify::editor_input(&mut self.state, conn_id, &input);
            } else if crate::olc::in_olc(conn_id) {
                crate::olc::olc_input(&mut self.state, conn_id, &input);
            } else {
                // Gameplay command: queue it instead of dispatching now. The
                // heartbeat's process_input_queues drains one per pulse once the
                // descriptor's WAIT_STATE lag (d.wait) expires, and sends the
                // prompt after the command actually runs.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.input_queue.push_back(input);
                }
                return;
            }
        } else {
            self.nanny(conn_id, input).await;
        }

        // Re-send the appropriate prompt unless the connection is closing.
        let st = self.state.descriptors.get(&conn_id).map(|d| d.state);
        if st.is_some() && st != Some(ConState::Close) {
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
            let input = match self.state.descriptors.get_mut(&cid) {
                Some(d) => {
                    d.wait = 1;
                    d.input_queue.pop_front()
                }
                None => None,
            };
            let input = match input {
                Some(i) => i,
                None => continue,
            };
            if let Some(ch) = self.state.descriptors.get(&cid).and_then(|d| d.character) {
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
            ConState::GetName => {
                let name = normalize_name(&input);
                if !valid_name(&name) {
                    self.out(conn_id, "Invalid name, please try another.\r\n");
                    return;
                }
                let exists = self.db.player_exists(&name).await.unwrap_or(false);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_name = Some(name.clone());
                    d.state = if exists { ConState::GetOldPassword } else { ConState::ConfirmName };
                }
            }
            ConState::ConfirmName => {
                let yes = input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes");
                if yes {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetNewPassword;
                    }
                } else if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_name = None;
                    d.state = ConState::GetName;
                }
            }
            ConState::GetOldPassword => {
                let name = self.descriptor_name(conn_id);
                let ok = self.db.verify_password(&name, &input).await.unwrap_or(false);
                if ok {
                    self.enter_game(conn_id, false).await;
                } else {
                    self.out(conn_id, "Wrong password.\r\n");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                }
            }
            ConState::GetNewPassword => {
                if input.len() < 3 {
                    self.out(conn_id, "Password too short.\r\n");
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
                        d.state = ConState::GetSex;
                    }
                } else {
                    self.out(conn_id, "Passwords don't match.\r\n");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.temp_password = None;
                        d.state = ConState::GetNewPassword;
                    }
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
                            d.state = ConState::GetClass;
                        }
                    }
                    None => self.out(conn_id, "That's not a sex.\r\n"),
                }
            }
            ConState::GetClass => {
                let class = match input.to_lowercase().chars().next() {
                    Some('w') => Some(Class::Warrior),
                    Some('c') => Some(Class::Cleric),
                    Some('t') => Some(Class::Thief),
                    Some('m') => Some(Class::MagicUser),
                    Some('a') => Some(Class::Artisan),
                    _ => None,
                };
                match class {
                    Some(c) => {
                        self.set_temp_class(conn_id, c);
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::GetRace;
                        }
                    }
                    None => self.out(conn_id, "That's not a class.\r\n"),
                }
            }
            ConState::GetRace => {
                let race = match input.to_lowercase().chars().next() {
                    Some('h') => Some(Race::Human),
                    Some('e') => Some(Race::Elf),
                    Some('d') => Some(Race::Dwarf),
                    Some('g') => Some(Race::Gnome),
                    _ => None,
                };
                match race {
                    Some(r) => {
                        self.set_temp_race(conn_id, r);
                        self.create_and_enter(conn_id).await;
                    }
                    None => self.out(conn_id, "That's not a race.\r\n"),
                }
            }
            _ => {}
        }

        // If this input was a password entry and we have now transitioned OUT of
        // the password flow (login success -> Playing, login fail -> Close, or
        // new-password confirmed -> GetSex), tell the client the server WONT echo
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

    // Pending creation choices are stashed on the descriptor via a scratch
    // Character; simplest is to keep them in temp fields. We store them in a
    // small side map keyed by conn_id.
    fn set_temp_sex(&mut self, conn_id: ConnId, s: Gender) {
        self.pending.entry(conn_id).or_default().sex = s;
    }
    fn set_temp_class(&mut self, conn_id: ConnId, c: Class) {
        self.pending.entry(conn_id).or_default().class = c;
    }
    fn set_temp_race(&mut self, conn_id: ConnId, r: Race) {
        self.pending.entry(conn_id).or_default().race = r;
    }

    async fn create_and_enter(&mut self, conn_id: ConnId) {
        let (name, pass) = {
            let d = match self.state.descriptors.get(&conn_id) {
                Some(d) => d,
                None => return,
            };
            (d.temp_name.clone().unwrap_or_default(), d.temp_password.clone().unwrap_or_default())
        };
        let choices = self.pending.remove(&conn_id).unwrap_or_default();
        let mut ch = crate::character::Character::new_player(name.clone(), choices.class, choices.race);
        ch.player.sex = choices.sex;
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
                if let Err(e) = self.db.save_player(&ch).await {
                    warn!("save new player {} failed: {}", name, e);
                }
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
                self.state.update_player_index(
                    ch.idnum,
                    &name,
                    ch.player.level,
                    ch.last_logon.timestamp(),
                    &host,
                );
                crate::mail::mail_register_player(ch.idnum, &name);
                self.enter_game(conn_id, true).await;
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

    /// Load (or, for fresh chars, re-load) the player, place them in the
    /// world, and start play.
    async fn enter_game(&mut self, conn_id: ConnId, _is_new: bool) {
        let name = self.descriptor_name(conn_id);
        let mut ch = match self.db.load_player(&name).await {
            Ok(c) => c,
            Err(e) => {
                warn!("load player {} failed: {}", name, e);
                self.out(conn_id, "Error loading your character.\r\n");
                return;
            }
        };
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
        if let Some(c) = self.state.get_char_mut(id) {
            c.last_logon = chrono::Utc::now();
        }
        let (pidnum, plevel) = self
            .state
            .get_char(id)
            .map(|c| (c.idnum, c.player.level))
            .unwrap_or((-1, 1));
        self.state.update_player_index(pidnum, &name, plevel, now, &host);

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
            if let Some(rnum) =
                self.state.map_coords_to_rnum(saved_mapx as i32, saved_mapy as i32)
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
        let home = self.state.get_char(id).map(|c| c.player.hometown).unwrap_or(100);
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

        let motd = self.state.motd.clone();
        self.state.send_to_char(id, &motd);
        self.state.send_to_char(id, "\r\n\r\n");
        crate::cmd_informative::look_at_room(&mut self.state, id, true);
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
        let ch = self.state.descriptors.get(&conn_id).and_then(|d| d.character);
        if let Some(cid) = ch {
            // Persist then remove the character from the world.
            if let Some(c) = self.state.get_char(cid) {
                if !c.is_npc {
                    let snapshot = c.clone();
                    // Keep the index current with the saved record (level can
                    // have changed this session); host carries over the last
                    // login's host (update_player_index ignores an empty host).
                    let (idnum, pname, plevel, llogon) = (
                        snapshot.idnum,
                        snapshot.get_name().to_string(),
                        snapshot.player.level,
                        snapshot.last_logon.timestamp(),
                    );
                    self.state.update_player_index(idnum, &pname, plevel, llogon, "");
                    let db = self.db.clone();
                    tokio::spawn(async move {
                        let _ = db.save_player(&snapshot).await;
                    });
                }
            }
            crate::objsave::crash_save(&mut self.state, cid, &self.lib_path);
            self.state.extract_char(cid);
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
                    self.state.send_to_char(op.requester, "There is no such player.\r\n");
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
                self.state.update_player_index(
                    s.idnum,
                    s.get_name(),
                    s.player.level,
                    s.last_logon.timestamp(),
                    "",
                );
            }
            self.state.extract_char(id);
            if let Some(s) = snap {
                let _ = self.db.save_player(&s).await;
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

        if pulse % PULSE_VIOLENCE == 0 {
            combat::perform_violence(&mut self.state);
        }
        if pulse % PULSE_MOBILE == 0 {
            crate::mobact::mobile_activity(&mut self.state);
        }
        if pulse % PULSE_ZONE == 0 {
            self.zone_update();
        }
        // DG Scripts: drain the wait/event queue every pulse (C process_events
        // in heartbeat), and run the periodic random-trigger scan every
        // PULSE_DG_SCRIPT (13 RL_SEC = 130 pulses, offset from PULSE_MOBILE).
        crate::dg_event::process_events(&mut self.state);
        if pulse % 130 == 0 {
            crate::dg_scripts::script_trigger_check(&mut self.state);
        }
        // CircleMUD point_update + weather/time run once per mud-hour
        // (SECS_PER_MUD_HOUR=75 => 750 pulses): regen, hunger/thirst, idle,
        // object timers, corpse decay; weather advances the clock & sky.
        if pulse % 750 == 0 {
            crate::limits::point_update(&mut self.state);
            crate::weather::weather_and_time(&mut self.state);
        }
        // Live surface weather (storms spawn/move/collide/expire) every 30
        // RL-seconds (comm.c: 30 * PASSES_PER_SEC = 300 pulses).
        if pulse % 300 == 0 {
            crate::maputils::weather_activity(&mut self.state);
        }
        // Autoquest update + room blood decay (C: every 60s = 600 pulses).
        if pulse % 600 == 0 {
            crate::quest::quest_update(&mut self.state);
            crate::maputils::blood_update(&mut self.state);
        }
        if pulse % 100 == 0 {
            crate::auction::auction_update(&mut self.state);
        }
        if pulse % 750 == 0 {
            crate::objsave::crash_save_all(&mut self.state);
        }
    }

    fn zone_update(&mut self) {
        // Age zones; reset those past their lifespan (CircleMUD zone_update).
        let due: Vec<i32> = {
            let mut v = Vec::new();
            for z in self.state.zones.iter_mut() {
                z.age += 1;
                if z.lifespan > 0 && z.age >= z.lifespan && z.reset_mode != 0 {
                    v.push(z.number);
                }
            }
            v
        };
        for zn in due {
            self.state.reset_zone(zn);
        }
    }


    // ---- Output flushing ------------------------------------------------
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
                if let Some(tx) = self.outputs.get(&conn_id) {
                    let _ = tx.send(render_color(&text)).await;
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

    fn write_prompt(&mut self, conn_id: ConnId) {
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };
        let name = self.state.descriptors.get(&conn_id).and_then(|d| d.temp_name.clone());
        let prompt = match state {
            ConState::GetName => "By what name do you wish to be known? ".to_string(),
            ConState::ConfirmName => {
                format!("Did I get that right, {} (Y/N)? ", name.unwrap_or_default())
            }
            ConState::GetOldPassword => "Password: ".to_string(),
            ConState::GetNewPassword => "Give me a password for your character: ".to_string(),
            ConState::ConfirmPassword => "Please retype password: ".to_string(),
            ConState::GetSex => "\r\nWhat is your sex (M/F)? ".to_string(),
            ConState::GetClass => {
                "\r\nSelect a class:\r\n  [W]arrior [C]leric [T]hief [M]agic-user [A]rtisan\r\nClass: "
                    .to_string()
            }
            ConState::GetRace => {
                "\r\nSelect a race:\r\n  [H]uman [E]lf [D]warf [G]nome\r\nRace: ".to_string()
            }
            ConState::Playing => {
                if let Some(cid) = self.state.descriptors.get(&conn_id).and_then(|d| d.character) {
                    if let Some(c) = self.state.get_char(cid) {
                        format!(
                            "&g{}H &c{}M &y{}V&n> ",
                            c.points.hit, c.points.mana, c.points.move_points
                        )
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
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

        // Out-of-band GMCP push: after the prompt (i.e. after every command, so
        // the state is fresh) send Char.Vitals + Room.Info to a GMCP-enabled,
        // in-world descriptor. Mudlet's gauges + the GMCP mapper feed off these.
        if state == ConState::Playing {
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
        let d = match self.state.descriptors.get(&conn_id) {
            Some(d) if d.gmcp => d,
            _ => return,
        };
        let ch = match d.character {
            Some(c) => c,
            None => return,
        };
        let c = match self.state.get_char(ch) {
            Some(c) => c,
            None => return,
        };

        // Char.Vitals — current/max HP, mana, move.
        let p = &c.points;
        let vitals = format!(
            "Char.Vitals {{\"hp\":{},\"maxhp\":{},\"mana\":{},\"maxmana\":{},\"move\":{},\"maxmove\":{}}}",
            p.hit, p.max_hit, p.mana, p.max_mana, p.move_points, p.max_move
        );
        self.send_raw_bytes(conn_id, &telnet_subneg(TELOPT_GMCP, vitals.as_bytes()));

        // Room.Info — vnum, name, zone, and the open cardinal exits as
        // {dir: dest-vnum}. Only emitted when the char is in a real room.
        if let Some(rnum) = c.in_room {
            if let Some(room) = self.state.room_opt(rnum) {
                let zone_name = self
                    .state
                    .zones
                    .get(room.zone as usize)
                    .map(|z| z.name.as_str())
                    .unwrap_or("");
                let dir_keys = ["n", "e", "s", "w", "u", "d"];
                let mut exits = String::new();
                let mut first = true;
                for (i, key) in dir_keys.iter().enumerate() {
                    if let Some(ex) = room.exits.get(i).and_then(|e| e.as_ref()) {
                        if !first {
                            exits.push(',');
                        }
                        first = false;
                        exits.push_str(&format!("\"{}\":{}", key, ex.to_room));
                    }
                }
                let room_info = format!(
                    "Room.Info {{\"num\":{},\"name\":\"{}\",\"zone\":\"{}\",\"exits\":{{{}}}}}",
                    room.number,
                    json_escape(&room.name),
                    json_escape(zone_name),
                    exits
                );
                self.send_raw_bytes(conn_id, &telnet_subneg(TELOPT_GMCP, room_info.as_bytes()));
            }
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
}
impl Default for PendingChoices {
    fn default() -> Self {
        PendingChoices { sex: Gender::Neutral, class: Class::Warrior, race: Race::Human }
    }
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
