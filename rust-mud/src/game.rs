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
            GameMessage::NewConnection { id, host, output_tx } => {
                info!("New connection from {}", host);
                let mut d = Descriptor::new(id, host);
                d.write("\r\n&YWelcome to DeltaMUD!&n\r\n\r\n");
                self.state.descriptors.insert(id, d);
                self.outputs.insert(id, output_tx);
                self.write_prompt(id);
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
            } else if let Some(ch) = self.state.descriptors.get(&conn_id).and_then(|d| d.character) {
                command_interpreter(&mut self.state, ch, &input);
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
                // CircleMUD convention: the first character created becomes the
                // Implementor (nanny CON_QRACE).
                if idnum == 1 {
                    ch.idnum = 1;
                    ch.player.level = LVL_IMPL;
                    ch.player.title = Some("the Implementor".to_string());
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
        // Autoquest update (C: every 60s) and the live auction cascade.
        if pulse % 600 == 0 {
            crate::quest::quest_update(&mut self.state);
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
