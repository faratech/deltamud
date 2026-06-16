// Game: the async shell around the synchronous GameState. It owns the world,
// drains the input channel, runs commands/nanny to completion against
// &mut GameState, drives the heartbeat, and flushes each descriptor's output
// buffer to its writer task. This is the only place async meets the world.

use crate::combat;
use crate::connection::{render_color, ConState, Descriptor, GameMessage};
use crate::interpreter::command_interpreter;
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
}

impl Game {
    pub fn new(state: GameState, db: Arc<dyn DatabaseInterface>) -> Self {
        Game {
            state,
            db,
            outputs: HashMap::new(),
            pending: HashMap::new(),
            lib_path: "./lib".to_string(),
        }
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

        // Place in start room: CircleMUD mortal_start_room (Itrius vnum 100),
        // then the player's hometown, then any loaded room (config.c).
        let home = self.state.get_char(id).map(|c| c.player.hometown).unwrap_or(100);
        let start = self
            .state
            .real_room(100)
            .or_else(|| self.state.real_room(home))
            .or_else(|| self.state.real_room(3001))
            .or_else(|| (!self.state.rooms.is_empty()).then_some(0));
        if let Some(rnum) = start {
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
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.heartbeat_inner();
        }));
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
    name.len() >= 2 && name.len() <= 16 && name.chars().all(|c| c.is_ascii_alphabetic())
}
