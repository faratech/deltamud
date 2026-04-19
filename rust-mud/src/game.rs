use crate::connection::{Connection, ConnectionState, GameMessage};
use crate::world::World;
use crate::room::Room;
use crate::types::*;
use crate::character::Character;
use crate::DatabaseInterface;
use crate::combat::{Combat, PULSE_VIOLENCE};
use crate::magic::affect_update;
use crate::commands::Commands;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use anyhow::Result;
use log::{info, warn, error};

pub struct Game {
    world: Arc<RwLock<World>>,
    database: Arc<dyn DatabaseInterface>,
    connections: HashMap<u64, Connection>,
    next_conn_id: u64,
    violence_timer: u64,
    motd: String,
}

impl Game {
    pub fn new(world: Arc<RwLock<World>>, database: Arc<dyn DatabaseInterface>) -> Self {
        Game {
            world,
            database,
            connections: HashMap::new(),
            next_conn_id: 1,
            violence_timer: 0,
            motd: String::new(),
        }
    }

    pub async fn load_text_files(&mut self, lib_path: &str) {
        let motd_path = std::path::Path::new(lib_path).join("text").join("motd");
        match tokio::fs::read_to_string(&motd_path).await {
            Ok(s) => {
                info!("Loaded MOTD from {}", motd_path.display());
                self.motd = s;
            }
            Err(e) => {
                warn!("Could not read MOTD at {}: {}", motd_path.display(), e);
                self.motd = "Welcome to DeltaMUD!".to_string();
            }
        }
    }

    async fn send_to_char(&self, ch_id: u64, msg: &str) -> Result<()> {
        for conn in self.connections.values() {
            if let Some(conn_ch) = &conn.character {
                if conn_ch.read().id == ch_id {
                    conn.send_line(msg).await?;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    async fn act_to_room(
        &self,
        room: &Arc<RwLock<Room>>,
        exclude_ch_id: u64,
        msg: &str,
    ) -> Result<()> {
        let recipients: Vec<u64> = {
            let room = room.read();
            room.people
                .iter()
                .filter_map(|w| w.upgrade())
                .map(|ch| ch.read().id)
                .filter(|id| *id != exclude_ch_id)
                .collect()
        };
        for id in recipients {
            self.send_to_char(id, msg).await?;
        }
        Ok(())
    }

    pub async fn run(&mut self, mut game_rx: mpsc::Receiver<GameMessage>) -> Result<()> {
        info!("Game loop starting...");
        
        // Create tick interval (100ms = 10 ticks/second)
        let mut tick_interval = interval(Duration::from_millis(100));
        
        loop {
            tokio::select! {
                // Handle game messages
                Some(msg) = game_rx.recv() => {
                    self.handle_message(msg).await?;
                }
                
                // Handle game tick
                _ = tick_interval.tick() => {
                    self.game_tick().await?;
                }
            }
        }
    }
    
    async fn handle_message(&mut self, msg: GameMessage) -> Result<()> {
        match msg {
            GameMessage::NewConnection { id, addr, output_tx } => {
                info!("New connection from {}", addr);
                let conn = Connection::new(id, addr, output_tx);
                conn.send_prompt().await?;
                self.connections.insert(id, conn);
            }
            
            GameMessage::Input { conn_id, input } => {
                if let Some(_conn) = self.connections.get_mut(&conn_id) {
                    self.handle_input(conn_id, input).await?;
                }
            }
            
            GameMessage::Disconnect { conn_id } => {
                info!("Connection {} disconnected", conn_id);
                
                // Extract character ID first
                let char_to_save = self.connections.get(&conn_id)
                    .and_then(|conn| conn.character.as_ref())
                    .map(|ch| ch.clone());
                
                // Remove connection
                if let Some(conn) = self.connections.remove(&conn_id) {
                    if let Some(ch) = &conn.character {
                        let ch_id = ch.read().id;
                        self.world.write().remove_character(ch_id);
                    }
                }
                
                // Save character after connection is removed
                if let Some(ch) = char_to_save {
                    // Snapshot character state (stripping !Send Weak refs) before spawning.
                    let (ch_for_save, is_npc, ch_id, name) = {
                        let ch_guard = ch.read();
                        (
                            ch_guard.clone_for_save(),
                            ch_guard.is_npc,
                            ch_guard.id,
                            ch_guard.get_name().to_string(),
                        )
                    };

                    if !is_npc && ch_id > 0 {
                        let db = self.database.clone();
                        tokio::spawn(async move {
                            if let Err(e) = db.save_player(&ch_for_save).await {
                                warn!("Failed to save player {}: {}", name, e);
                            } else {
                                info!("Saved player {} on disconnect", name);
                            }
                        });
                    }
                }
            }
        }
        Ok(())
    }
    
    async fn handle_input(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        match conn.state {
            ConnectionState::GetName => {
                self.handle_get_name(conn_id, input).await?;
            }
            ConnectionState::GetOldPassword => {
                self.handle_get_password(conn_id, input).await?;
            }
            ConnectionState::GetNewPassword => {
                self.handle_new_password(conn_id, input).await?;
            }
            ConnectionState::ConfirmPassword => {
                self.handle_confirm_password(conn_id, input).await?;
            }
            ConnectionState::GetSex => {
                self.handle_get_sex(conn_id, input).await?;
            }
            ConnectionState::GetClass => {
                self.handle_get_class(conn_id, input).await?;
            }
            ConnectionState::GetRace => {
                self.handle_get_race(conn_id, input).await?;
            }
            ConnectionState::ReadMotd => {
                self.handle_read_motd(conn_id, input).await?;
            }
            ConnectionState::Menu => {
                self.handle_menu(conn_id, input).await?;
            }
            ConnectionState::Playing => {
                self.handle_command(conn_id, input).await?;
            }
            _ => {}
        }
        
        // Send next prompt
        if let Some(conn) = self.connections.get(&conn_id) {
            conn.send_prompt().await?;
        }
        
        Ok(())
    }
    
    async fn handle_get_name(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        if input.is_empty() {
            conn.close();
            return Ok(());
        }
        
        let name = input.trim().to_string();
        
        // Check if name is valid
        if name.len() < 3 || name.len() > 12 {
            conn.send_line("Names must be 3-12 characters long.").await?;
            return Ok(());
        }
        
        // Check if already playing
        if self.world.read().find_character_by_name(&name).is_some() {
            conn.send_line("That character is already playing!").await?;
            return Ok(());
        }
        
        // Check database for existing player
        let exists = self.database.player_exists(&name).await?;
        
        conn.temp_name = Some(name);
        
        if exists {
            conn.state = ConnectionState::GetOldPassword;
        } else {
            conn.state = ConnectionState::GetNewPassword;
        }
        
        Ok(())
    }
    
    async fn handle_get_password(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        let name = conn.temp_name.clone().unwrap();
        
        // Verify password
        let valid = self.database.verify_password(&name, &input).await?;
        
        if !valid {
            conn.send_line("Wrong password.").await?;
            conn.state = ConnectionState::GetName;
            conn.temp_name = None;
            conn.temp_password = None;
            return Ok(());
        }
        
        // Load character from database
        match self.database.load_player(&name).await {
            Ok(character) => {
                let ch_arc = self.world.write().create_character(character);
                conn.character = Some(ch_arc);
                conn.state = ConnectionState::Menu;
            }
            Err(e) => {
                error!("Failed to load player {}: {}", name, e);
                conn.send_line("Error loading character. Please try again.").await?;
                conn.state = ConnectionState::GetName;
                conn.temp_name = None;
            }
        }
        
        Ok(())
    }
    
    async fn handle_new_password(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        if input.len() < 3 {
            conn.send_line("Password must be at least 3 characters.").await?;
            conn.state = ConnectionState::GetNewPassword;
            return Ok(());
        }
        
        conn.temp_password = Some(input);
        conn.state = ConnectionState::ConfirmPassword;
        Ok(())
    }
    
    async fn handle_confirm_password(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        if Some(&input) != conn.temp_password.as_ref() {
            conn.send_line("Passwords don't match! Start over.").await?;
            conn.state = ConnectionState::GetNewPassword;
            conn.temp_password = None;
            return Ok(());
        }
        
        conn.state = ConnectionState::GetSex;
        Ok(())
    }
    
    async fn handle_get_sex(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        let sex = match input.to_lowercase().chars().next() {
            Some('m') => Gender::Male,
            Some('f') => Gender::Female,
            _ => {
                conn.send_line("That's not a sex! What IS your sex (M/F)?").await?;
                return Ok(());
            }
        };
        
        // Create character with basic info
        let name = conn.temp_name.clone().unwrap();
        let mut ch = Character::new_player(name, Class::Warrior, Race::Human);
        ch.player.sex = sex;
        
        let ch_arc = self.world.write().create_character(ch);
        conn.character = Some(ch_arc);
        conn.state = ConnectionState::GetClass;
        Ok(())
    }
    
    async fn handle_get_class(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        let class = match input.to_lowercase().chars().next() {
            Some('w') => Class::Warrior,
            Some('c') => Class::Cleric,
            Some('t') => Class::Thief,
            Some('m') => Class::MagicUser,
            Some('a') => Class::Artisan,
            _ => {
                conn.send_line("That's not a class!").await?;
                return Ok(());
            }
        };
        
        if let Some(ch) = &conn.character {
            ch.write().player.class = class;
        }
        
        conn.state = ConnectionState::GetRace;
        Ok(())
    }
    
    async fn handle_get_race(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        let race = match input.to_lowercase().chars().next() {
            Some('h') => Race::Human,
            Some('e') => Race::Elf,
            Some('d') => Race::Dwarf,
            Some('g') => Race::Gnome,
            _ => {
                conn.send_line("That's not a race!").await?;
                return Ok(());
            }
        };
        
        if let Some(ch) = &conn.character {
            ch.write().player.race = race;
        }

        conn.send_line(&self.motd).await?;
        conn.send_line("").await?;
        conn.send_line("&YPress ENTER to continue...&n").await?;
        conn.state = ConnectionState::ReadMotd;
        Ok(())
    }

    async fn handle_read_motd(&mut self, conn_id: u64, _input: String) -> Result<()> {
        if let Some(conn) = self.connections.get_mut(&conn_id) {
            conn.state = ConnectionState::Menu;
            conn.send_line("").await?;
            conn.send_line("&cMenu&n").await?;
            conn.send_line("  &Y0&n) Quit").await?;
            conn.send_line("  &Y1&n) Enter the game").await?;
        }
        Ok(())
    }
    
    async fn handle_menu(&mut self, conn_id: u64, input: String) -> Result<()> {
        let conn = self.connections.get_mut(&conn_id).unwrap();
        
        match input.chars().next() {
            Some('0') => {
                conn.send_line("Goodbye!").await?;
                conn.close();
            }
            Some('1') => {
                // Enter game
                if let Some(ch) = &conn.character {
                    let needs_create = ch.read().id == 0;
                    if needs_create {
                        let password = conn.temp_password.clone().unwrap_or_default();
                        let (ch_for_create, ch_name) = {
                            let ch_read = ch.read();
                            (ch_read.clone_for_save(), ch_read.get_name().to_string())
                        };
                        match self.database.create_player(&ch_for_create, &password).await {
                            Ok(new_id) => {
                                ch.write().id = new_id;
                                info!("Created new player {} with id {}", ch_name, new_id);
                            }
                            Err(e) => {
                                warn!("Failed to create player {}: {}", ch_name, e);
                                ch.write().id = self.next_conn_id.wrapping_add(1_000_000);
                            }
                        }
                    }

                    let start_room = ch.read().player.hometown;
                    self.world.read().move_character(ch.clone(), start_room)?;

                    conn.send_line("\r\n&YWelcome to DeltaMUD!&n\r\n").await?;
                    conn.state = ConnectionState::Playing;

                    // Look at room
                    self.do_look(conn_id, "".to_string()).await?;
                }
            }
            _ => {
                conn.send_line("That's not a menu choice!").await?;
            }
        }
        
        Ok(())
    }
    
    async fn handle_command(&mut self, conn_id: u64, input: String) -> Result<()> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(());
        }
        
        let command = parts[0].to_lowercase();
        let args = parts[1..].join(" ");
        
        // Get character for command execution
        let messages = if let Some(conn) = self.connections.get(&conn_id) {
            if let Some(ch) = &conn.character {
                let world = self.world.read();
                
                match command.as_str() {
                    // Movement
                    "look" | "l" => {
                        drop(world);
                        self.do_look(conn_id, args).await?;
                        return Ok(());
                    }
                    "north" | "n" => {
                        drop(world);
                        self.do_move(conn_id, NORTH).await?;
                        return Ok(());
                    }
                    "east" | "e" => {
                        drop(world);
                        self.do_move(conn_id, EAST).await?;
                        return Ok(());
                    }
                    "south" | "s" => {
                        drop(world);
                        self.do_move(conn_id, SOUTH).await?;
                        return Ok(());
                    }
                    "west" | "w" => {
                        drop(world);
                        self.do_move(conn_id, WEST).await?;
                        return Ok(());
                    }
                    "up" | "u" => {
                        drop(world);
                        self.do_move(conn_id, UP).await?;
                        return Ok(());
                    }
                    "down" | "d" => {
                        drop(world);
                        self.do_move(conn_id, DOWN).await?;
                        return Ok(());
                    }
                    
                    // Communication
                    "say" => {
                        drop(world);
                        self.do_say(conn_id, args).await?;
                        return Ok(());
                    }
                    "tell" => {
                        drop(world);
                        self.do_tell(conn_id, args).await?;
                        return Ok(());
                    }
                    "shout" => {
                        drop(world);
                        self.do_shout(conn_id, args).await?;
                        return Ok(());
                    }
                    
                    // Information
                    "who" => Commands::do_who(&ch.read(), &world, &args),
                    "score" | "sc" => Commands::do_score(&ch.read(), &world, &args),
                    "inventory" | "inv" | "i" => Commands::do_inventory(&ch.read(), &world, &args),
                    "equipment" | "eq" => Commands::do_equipment(&ch.read(), &world, &args),
                    
                    // Objects
                    "get" | "take" => Commands::do_get(&mut ch.write(), &world, &args),
                    "drop" => Commands::do_drop(&mut ch.write(), &world, &args),
                    "wear" => Commands::do_wear(&mut ch.write(), &world, &args),
                    "remove" => Commands::do_remove(&mut ch.write(), &world, &args),
                    
                    // Combat
                    "kill" | "k" | "hit" => {
                        drop(world);
                        Commands::do_kill(ch.clone(), &self.world.read(), &args)
                    }
                    "flee" => {
                        drop(world);
                        self.do_flee(conn_id).await?;
                        return Ok(());
                    }
                    
                    // Magic
                    "cast" | "c" => {
                        drop(world);
                        Commands::do_cast(ch.clone(), &self.world.read(), &args)
                    }
                    
                    // System
                    "quit" => {
                        drop(world);
                        self.do_quit(conn_id).await?;
                        return Ok(());
                    }
                    
                    _ => vec!["Huh?!?".to_string()],
                }
            } else {
                vec!["You must be logged in to use commands.".to_string()]
            }
        } else {
            vec![]
        };
        
        // Send messages to player
        if let Some(conn) = self.connections.get(&conn_id) {
            for msg in messages {
                conn.send_line(&msg).await?;
            }
        }
        
        Ok(())
    }
    
    async fn do_look(&mut self, conn_id: u64, _args: String) -> Result<()> {
        let conn = self.connections.get(&conn_id).unwrap();
        
        // Collect all the room data first, then send messages
        let room_data = if let Some(ch) = &conn.character {
            let ch = ch.read();
            if let Some(room_weak) = &ch.in_room {
                if let Some(room_arc) = room_weak.upgrade() {
                    let room = room_arc.read();
                    let ch_id = ch.id;
                    
                    // Collect room info
                    let room_name = room.name.clone();
                    let room_desc = room.description.clone();
                    
                    // Collect exits
                    let mut exits = Vec::new();
                    for (dir, exit) in room.exits.iter().enumerate() {
                        if exit.is_some() {
                            exits.push(match dir {
                                0 => "north",
                                1 => "east", 
                                2 => "south",
                                3 => "west",
                                4 => "up",
                                5 => "down",
                                _ => "unknown",
                            });
                        }
                    }
                    
                    // Collect people in room
                    let mut people = Vec::new();
                    for person_weak in &room.people {
                        if let Some(person) = person_weak.upgrade() {
                            let person = person.read();
                            if person.id != ch_id {
                                people.push(person.get_title());
                            }
                        }
                    }
                    
                    Some((room_name, room_desc, exits, people))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        
        // Now send messages without holding any locks
        if let Some((room_name, room_desc, exits, people)) = room_data {
            // Room name
            conn.send_line(&format!("&c{}&n", room_name)).await?;
            
            // Room description
            conn.send_line(&room_desc).await?;
            
            // Exits
            if !exits.is_empty() {
                conn.send_line(&format!("&g[ Exits: {} ]&n", exits.join(" "))).await?;
            }
            
            // People in room
            for person_title in people {
                conn.send_line(&format!("{} is here.", person_title)).await?;
            }
        }
        
        Ok(())
    }
    
    async fn do_say(&mut self, conn_id: u64, args: String) -> Result<()> {
        if args.is_empty() {
            if let Some(conn) = self.connections.get(&conn_id) {
                conn.send_line("Say what?").await?;
            }
            return Ok(());
        }

        let (ch_id, ch_name, room) = match self.connections.get(&conn_id).and_then(|c| c.character.as_ref()) {
            Some(ch) => {
                let ch_read = ch.read();
                let room = ch_read.in_room.as_ref().and_then(|w| w.upgrade());
                (ch_read.id, ch_read.get_name().to_string(), room)
            }
            None => return Ok(()),
        };

        if let Some(conn) = self.connections.get(&conn_id) {
            conn.send_line(&format!("You say, '{}'", args)).await?;
        }

        if let Some(room) = room {
            let msg = format!("{} says, '{}'", ch_name, args);
            self.act_to_room(&room, ch_id, &msg).await?;
        }

        Ok(())
    }

    async fn do_tell(&mut self, conn_id: u64, args: String) -> Result<()> {
        let mut parts = args.splitn(2, char::is_whitespace);
        let target_name = parts.next().unwrap_or("").trim().to_string();
        let message = parts.next().unwrap_or("").trim().to_string();

        if target_name.is_empty() || message.is_empty() {
            if let Some(conn) = self.connections.get(&conn_id) {
                conn.send_line("Tell whom what?").await?;
            }
            return Ok(());
        }

        let (speaker_id, speaker_name) = match self.connections.get(&conn_id).and_then(|c| c.character.as_ref()) {
            Some(ch) => {
                let ch = ch.read();
                (ch.id, ch.get_name().to_string())
            }
            None => return Ok(()),
        };

        let target = self.world.read().find_character_by_name(&target_name);
        match target {
            Some(target_ch) => {
                let target_id = target_ch.read().id;
                if target_id == speaker_id {
                    if let Some(conn) = self.connections.get(&conn_id) {
                        conn.send_line("You can't tell yourself anything.").await?;
                    }
                    return Ok(());
                }
                let display_name = target_ch.read().get_name().to_string();
                if let Some(conn) = self.connections.get(&conn_id) {
                    conn.send_line(&format!("You tell {}, '{}'", display_name, message)).await?;
                }
                self.send_to_char(target_id, &format!("{} tells you, '{}'", speaker_name, message)).await?;
            }
            None => {
                if let Some(conn) = self.connections.get(&conn_id) {
                    conn.send_line("No one by that name here.").await?;
                }
            }
        }
        Ok(())
    }

    async fn do_shout(&mut self, conn_id: u64, args: String) -> Result<()> {
        if args.trim().is_empty() {
            if let Some(conn) = self.connections.get(&conn_id) {
                conn.send_line("Shout what?").await?;
            }
            return Ok(());
        }

        let (speaker_id, speaker_name) = match self.connections.get(&conn_id).and_then(|c| c.character.as_ref()) {
            Some(ch) => {
                let ch = ch.read();
                (ch.id, ch.get_name().to_string())
            }
            None => return Ok(()),
        };

        if let Some(conn) = self.connections.get(&conn_id) {
            conn.send_line(&format!("You shout, '{}'", args)).await?;
        }

        let recipients: Vec<u64> = self.connections.values()
            .filter(|c| matches!(c.state, ConnectionState::Playing))
            .filter_map(|c| c.character.as_ref().map(|ch| ch.read().id))
            .filter(|id| *id != speaker_id)
            .collect();

        let msg = format!("{} shouts, '{}'", speaker_name, args);
        for id in recipients {
            self.send_to_char(id, &msg).await?;
        }
        Ok(())
    }

    async fn do_move(&mut self, conn_id: u64, direction: usize) -> Result<()> {
        let dir_name = match direction {
            NORTH => "north",
            EAST => "east",
            SOUTH => "south",
            WEST => "west",
            UP => "up",
            DOWN => "down",
            _ => return Ok(()),
        };
        let opposite_dir = match direction {
            NORTH => "south",
            EAST => "west",
            SOUTH => "north",
            WEST => "east",
            UP => "below",
            DOWN => "above",
            _ => "nowhere",
        };

        let (ch_arc, ch_id, ch_name, old_room) = {
            let conn = match self.connections.get(&conn_id) {
                Some(c) => c,
                None => return Ok(()),
            };
            let ch = match &conn.character {
                Some(ch) => ch.clone(),
                None => return Ok(()),
            };
            let (id, name, old_room) = {
                let ch_read = ch.read();
                let old_room = ch_read.in_room.as_ref().and_then(|w| w.upgrade());
                (ch_read.id, ch_read.get_name().to_string(), old_room)
            };
            (ch, id, name, old_room)
        };

        let to_room_vnum = match &old_room {
            Some(room) => match room.read().get_exit(direction) {
                Some(exit) => Some(exit.to_room),
                None => None,
            },
            None => None,
        };

        let to_room_vnum = match to_room_vnum {
            Some(v) => v,
            None => {
                if let Some(conn) = self.connections.get(&conn_id) {
                    conn.send_line("You can't go that way.").await?;
                }
                return Ok(());
            }
        };

        if let Some(room) = &old_room {
            let msg = format!("{} leaves {}.", ch_name, dir_name);
            self.act_to_room(room, ch_id, &msg).await?;
        }

        self.world.read().move_character(ch_arc.clone(), to_room_vnum)?;

        let new_room = ch_arc.read().in_room.as_ref().and_then(|w| w.upgrade());
        if let Some(room) = &new_room {
            let msg = format!("{} arrives from the {}.", ch_name, opposite_dir);
            self.act_to_room(room, ch_id, &msg).await?;
        }

        self.do_look(conn_id, String::new()).await?;
        Ok(())
    }

    async fn do_flee(&mut self, conn_id: u64) -> Result<()> {
        use rand::seq::SliceRandom;

        let (ch_arc, fighting, exits) = match self.connections.get(&conn_id).and_then(|c| c.character.as_ref()) {
            Some(ch) => {
                let ch_read = ch.read();
                let fighting = ch_read.fighting.is_some();
                let exits: Vec<usize> = ch_read.in_room.as_ref()
                    .and_then(|w| w.upgrade())
                    .map(|room| {
                        let room = room.read();
                        (0..room.exits.len())
                            .filter(|i| room.exits[*i].is_some())
                            .collect()
                    })
                    .unwrap_or_default();
                (ch.clone(), fighting, exits)
            }
            None => return Ok(()),
        };

        if !fighting {
            if let Some(conn) = self.connections.get(&conn_id) {
                conn.send_line("You aren't fighting anyone.").await?;
            }
            return Ok(());
        }

        let chosen = exits.choose(&mut rand::thread_rng()).copied();
        match chosen {
            Some(dir) => {
                Combat::stop_fighting(&mut ch_arc.write());
                if let Some(conn) = self.connections.get(&conn_id) {
                    conn.send_line("You flee in panic!").await?;
                }
                self.do_move(conn_id, dir).await?;
            }
            None => {
                if let Some(conn) = self.connections.get(&conn_id) {
                    conn.send_line("PANIC! You couldn't escape!").await?;
                }
            }
        }
        Ok(())
    }

    async fn do_quit(&mut self, conn_id: u64) -> Result<()> {
        if let Some(conn) = self.connections.get_mut(&conn_id) {
            conn.send_line("Goodbye!").await?;
            conn.close();
        }
        Ok(())
    }
    
    async fn game_tick(&mut self) -> Result<()> {
        self.violence_timer += 1;
        
        // Process combat every PULSE_VIOLENCE ticks
        if self.violence_timer >= PULSE_VIOLENCE {
            self.violence_timer = 0;
            self.process_combat().await?;
        }
        
        // Update affects every 10 seconds (100 ticks)
        static mut AFFECT_TIMER: u64 = 0;
        unsafe {
            AFFECT_TIMER += 1;
            if AFFECT_TIMER >= 100 {
                AFFECT_TIMER = 0;
                self.update_affects();
            }
        }
        
        // Regenerate HP/Mana/Move every 30 seconds (300 ticks)
        static mut REGEN_TIMER: u64 = 0;
        unsafe {
            REGEN_TIMER += 1;
            if REGEN_TIMER >= 300 {
                REGEN_TIMER = 0;
                self.regenerate_characters();
            }
        }
        
        // Zone resets every 15 minutes (9000 ticks)
        static mut ZONE_TIMER: u64 = 0;
        unsafe {
            ZONE_TIMER += 1;
            if ZONE_TIMER >= 9000 {
                ZONE_TIMER = 0;
                // TODO: Implement zone resets
            }
        }
        
        Ok(())
    }
    
    async fn process_combat(&mut self) -> Result<()> {
        // Collect combat messages in a separate scope to ensure world guard is dropped
        let combat_messages = {
            let world = self.world.read();
            let mut combat_messages = Vec::new();
            
            for (_, ch) in &world.characters {
                if ch.read().fighting.is_some() {
                    let messages = Combat::perform_violence(ch.clone());
                    combat_messages.push((ch.clone(), messages));
                }
            }
            
            combat_messages
        }; // world guard is dropped here
        
        // Send combat messages to appropriate players
        for (ch, messages) in combat_messages {
            let ch_id = ch.read().id;
            for msg in messages {
                self.send_to_char(ch_id, &msg).await?;
            }
        }

        Ok(())
    }
    
    fn update_affects(&mut self) {
        let world = self.world.write();
        
        for (_, ch) in &world.characters {
            affect_update(&mut ch.write());
        }
    }
    
    fn regenerate_characters(&mut self) {
        let world = self.world.write();
        
        for (_, ch) in &world.characters {
            let mut ch = ch.write();
            
            // Skip if fighting or position is bad
            if ch.fighting.is_some() || ch.position < Position::Stunned {
                continue;
            }
            
            // Regeneration rates based on position
            let (hit_gain, mana_gain, move_gain) = match ch.position {
                Position::Sleeping => (2, 2, 2),
                Position::Resting => (1, 1, 1),
                Position::Sitting => (1, 1, 0),
                _ => (0, 0, 0),
            };
            
            // Add constitution bonus to hit regen
            let hit_bonus = ((ch.aff_abils.con - 10) / 3) as i32;
            let total_hit_gain = hit_gain + hit_bonus.max(0);
            
            // Add intelligence bonus to mana regen
            let mana_bonus = ((ch.aff_abils.int - 10) / 3) as i32;
            let total_mana_gain = mana_gain + mana_bonus.max(0);
            
            // Update values
            ch.points.hit = (ch.points.hit + total_hit_gain).min(ch.points.max_hit);
            ch.points.mana = (ch.points.mana + total_mana_gain).min(ch.points.max_mana);
            ch.points.move_points = (ch.points.move_points + move_gain).min(ch.points.max_move);
        }
    }
}