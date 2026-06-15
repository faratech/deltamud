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
use log::{info, warn};
use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

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
        loop {
            tokio::select! {
                Some(msg) = game_rx.recv() => self.handle_message(msg).await,
                _ = tick.tick() => self.heartbeat(),
            }
            self.flush_all().await;
        }
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
                command_interpreter(&mut self.state, ch, &input);
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

    // ---- Heartbeat ------------------------------------------------------
    fn heartbeat(&mut self) {
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
