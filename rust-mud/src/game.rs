use crate::connection::{Connection, ConnectionState, GameMessage};
use crate::world::World;
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
use log::{info, error};

pub struct Game {
    world: Arc<RwLock<World>>,
    database: Arc<dyn DatabaseInterface>,
    connections: HashMap<u64, Connection>,
    next_conn_id: u64,
    violence_timer: u64,
}

impl Game {
    pub fn new(world: Arc<RwLock<World>>, database: Arc<dyn DatabaseInterface>) -> Self {
        Game {
            world,
            database,
            connections: HashMap::new(),
            next_conn_id: 1,
            violence_timer: 0,
        }
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
                    // Extract all needed data before any async operations
                    let (ch_id, is_npc, name) = {
                        let ch_guard = ch.read();
                        (ch_guard.id, ch_guard.is_npc, ch_guard.get_name().to_string())
                    };
                    
                    if !is_npc && ch_id > 0 {
                        // For now, just log that we would save the player
                        // In production, this should be queued for async save
                        info!("Player {} disconnected, would save to database", name);
                        // TODO: Implement proper async save without Send issues
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
        
        // TODO: Show MOTD
        conn.state = ConnectionState::Menu;
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
                    // Save new character to database if needed
                    let needs_save = ch.read().id == 0;
                    if needs_save {
                        let _password = conn.temp_password.clone().unwrap_or_default();
                        let ch_name = ch.read().get_name().to_string();
                        // For now, skip actual database save to avoid Send issues
                        // TODO: Implement proper async save
                        info!("Would create new player: {}", ch_name);
                        // Assign a temporary ID
                        ch.write().id = 1000; // Temporary ID for testing
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
                    "say" => Commands::do_say(&ch.read(), &world, &args),
                    "tell" => Commands::do_tell(&ch.read(), &world, &args),
                    "shout" => Commands::do_shout(&ch.read(), &world, &args),
                    
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
                    "flee" => Commands::do_flee(&mut ch.write(), &world, &args),
                    
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
        
        let conn = self.connections.get(&conn_id).unwrap();
        if let Some(ch) = &conn.character {
            let ch = ch.read();
            
            // Send to self
            conn.send_line(&format!("You say, '{}'", args)).await?;
            
            // Send to room
            if let Some(room_weak) = &ch.in_room {
                if let Some(room) = room_weak.upgrade() {
                    let room = room.read();
                    for other_weak in &room.people {
                        if let Some(other) = other_weak.upgrade() {
                            let other = other.read();
                            if other.id != ch.id {
                                // Find connection for other character
                                for (_, other_conn) in &self.connections {
                                    if let Some(other_ch) = &other_conn.character {
                                        if other_ch.read().id == other.id {
                                            other_conn.send_line(&format!("{} says, '{}'", 
                                                ch.get_name(), args)).await?;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
    
    async fn do_move(&mut self, conn_id: u64, direction: usize) -> Result<()> {
        let conn = self.connections.get(&conn_id).unwrap();
        
        if let Some(ch) = &conn.character {
            let ch_clone = ch.clone();
            
            // Check if movement is possible and get destination
            let move_result = {
                let ch = ch.read();
                if let Some(room_weak) = &ch.in_room {
                    if let Some(room) = room_weak.upgrade() {
                        let room = room.read();
                        if let Some(exit) = room.get_exit(direction) {
                            Some(exit.to_room)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            
            // Process movement result
            match move_result {
                Some(to_room) => {
                    self.world.read().move_character(ch_clone, to_room)?;
                    self.do_look(conn_id, "".to_string()).await?;
                }
                None => {
                    conn.send_line("You can't go that way.").await?;
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
            
            // Find connection for this character
            for (_, conn) in &self.connections {
                if let Some(conn_ch) = &conn.character {
                    if conn_ch.read().id == ch_id {
                        for msg in messages {
                            conn.send_line(&msg).await?;
                        }
                        break;
                    }
                }
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