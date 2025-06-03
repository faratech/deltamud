use crate::types::*;
use crate::room::Room;
use crate::object::Object;
use std::sync::{Arc, Weak};
use parking_lot::RwLock;
use chrono::{DateTime, Utc};

// Ability scores
#[derive(Debug, Clone, Default, Copy)]
pub struct Abilities {
    pub str: i8,
    pub dex: i8,
    pub int: i8,
    pub wis: i8,
    pub con: i8,
    pub cha: i8,
}

// Character vital statistics
#[derive(Debug, Clone, Default)]
pub struct CharPoints {
    pub hit: i32,
    pub max_hit: i32,
    pub mana: i32,
    pub max_mana: i32,
    pub move_points: i32,
    pub max_move: i32,
    pub armor: ArmorClass,
    pub gold: Gold,
    pub exp: Experience,
    pub hitroll: Hitroll,
    pub damroll: Damroll,
}

// Player-specific data
#[derive(Debug, Clone)]
pub struct PlayerData {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub sex: Gender,
    pub class: Class,
    pub race: Race,
    pub level: Level,
    pub hometown: RoomVnum,
    pub time_played: i64,
    pub weight: u8,
    pub height: u8,
}

// Affect structure for spells/skills
#[derive(Debug, Clone)]
pub struct Affect {
    pub spell_type: i32,
    pub duration: i32,
    pub modifier: i32,
    pub location: i32,
    pub bitvector: i64,
}

// Main character structure
#[derive(Debug)]
pub struct Character {
    pub id: u64,
    pub nr: MobVnum,  // -1 for players
    
    // Location
    pub in_room: Option<Weak<RwLock<Room>>>,
    pub was_in_room: Option<Weak<RwLock<Room>>>,
    
    // Core data
    pub player: PlayerData,
    pub real_abils: Abilities,
    pub aff_abils: Abilities,
    pub points: CharPoints,
    
    // Inventory and equipment
    pub carrying: Vec<Arc<RwLock<Object>>>,
    pub equipment: [Option<Arc<RwLock<Object>>>; NUM_WEARS],
    
    // Status
    pub position: Position,
    pub affected: Vec<Affect>,
    
    // Combat
    pub fighting: Option<Weak<RwLock<Character>>>,
    
    // Group/Follow
    pub master: Option<Weak<RwLock<Character>>>,
    pub followers: Vec<Weak<RwLock<Character>>>,
    
    // Flags
    pub is_npc: bool,
    pub act_flags: i64,
    pub affect_flags: i64,
    
    // Timestamps
    pub created_at: DateTime<Utc>,
    pub last_logon: DateTime<Utc>,
}

impl Character {
    pub fn new_player(name: String, class: Class, race: Race) -> Self {
        let now = Utc::now();
        Character {
            id: 0, // Will be assigned by database
            nr: -1,
            in_room: None,
            was_in_room: None,
            player: PlayerData {
                name,
                title: None,
                description: String::new(),
                sex: Gender::Neutral,
                class,
                race,
                level: 1,
                hometown: 3001, // Default starting room
                time_played: 0,
                weight: 150,
                height: 170,
            },
            real_abils: Abilities::default(),
            aff_abils: Abilities::default(),
            points: CharPoints {
                hit: 20,
                max_hit: 20,
                mana: 100,
                max_mana: 100,
                move_points: 80,
                max_move: 80,
                armor: 100,
                gold: 0,
                exp: 0,
                hitroll: 0,
                damroll: 0,
            },
            carrying: Vec::new(),
            equipment: Default::default(),
            position: Position::Standing,
            affected: Vec::new(),
            fighting: None,
            master: None,
            followers: Vec::new(),
            is_npc: false,
            act_flags: 0,
            affect_flags: 0,
            created_at: now,
            last_logon: now,
        }
    }
    
    pub fn new_npc(nr: MobVnum) -> Self {
        let now = Utc::now();
        Character {
            id: 0,
            nr,
            in_room: None,
            was_in_room: None,
            player: PlayerData {
                name: String::from("a mobile"),
                title: None,
                description: String::new(),
                sex: Gender::Neutral,
                class: Class::Warrior,
                race: Race::Human,
                level: 1,
                hometown: 0,
                time_played: 0,
                weight: 150,
                height: 170,
            },
            real_abils: Abilities::default(),
            aff_abils: Abilities::default(),
            points: CharPoints::default(),
            carrying: Vec::new(),
            equipment: Default::default(),
            position: Position::Standing,
            affected: Vec::new(),
            fighting: None,
            master: None,
            followers: Vec::new(),
            is_npc: true,
            act_flags: 0,
            affect_flags: 0,
            created_at: now,
            last_logon: now,
        }
    }
    
    pub fn get_name(&self) -> &str {
        &self.player.name
    }
    
    pub fn get_title(&self) -> String {
        match &self.player.title {
            Some(title) => format!("{} {}", self.player.name, title),
            None => self.player.name.clone(),
        }
    }
    
    pub fn is_immortal(&self) -> bool {
        self.player.level >= 31
    }
    
    pub fn can_see(&self, _target: &Character) -> bool {
        // Simplified visibility check
        if self.is_immortal() {
            return true;
        }
        
        // TODO: Add more visibility checks (light, invisibility, etc.)
        true
    }
    
    pub fn add_follower(&mut self, follower: Weak<RwLock<Character>>) {
        self.followers.push(follower);
    }
    
    pub fn remove_follower(&mut self, follower_id: u64) {
        self.followers.retain(|f| {
            if let Some(char) = f.upgrade() {
                char.read().id != follower_id
            } else {
                false
            }
        });
    }
    
    // Simple clone for database operations (without Weak references)
    pub fn clone_for_save(&self) -> Character {
        Character {
            id: self.id,
            nr: self.nr,
            in_room: None,
            was_in_room: None,
            player: self.player.clone(),
            real_abils: self.real_abils,
            aff_abils: self.aff_abils,
            points: self.points.clone(),
            carrying: Vec::new(),
            equipment: Default::default(),
            position: self.position,
            affected: self.affected.clone(),
            fighting: None,
            master: None,
            followers: Vec::new(),
            is_npc: self.is_npc,
            act_flags: self.act_flags,
            affect_flags: self.affect_flags,
            created_at: self.created_at,
            last_logon: self.last_logon,
        }
    }
}