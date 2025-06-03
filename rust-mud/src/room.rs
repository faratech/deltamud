use crate::types::*;
use crate::character::Character;
use crate::object::Object;
use std::sync::{Arc, Weak};
use parking_lot::RwLock;

// Room exit/direction data
#[derive(Debug, Clone)]
pub struct Exit {
    pub description: Option<String>,
    pub keyword: Option<String>,
    pub exit_info: i32,  // Door flags
    pub key: ObjVnum,
    pub to_room: RoomVnum,
}

// Sector types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SectorType {
    Inside = 0,
    City = 1,
    Field = 2,
    Forest = 3,
    Hills = 4,
    Mountain = 5,
    WaterSwim = 6,
    WaterNoSwim = 7,
    Flying = 8,
    Underwater = 9,
    Ice = 10,
}

// Room flags
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct RoomFlags: u32 {
        const DARK = 1 << 0;
        const DEATH = 1 << 1;
        const NO_MOB = 1 << 2;
        const INDOORS = 1 << 3;
        const PEACEFUL = 1 << 4;
        const SOUNDPROOF = 1 << 5;
        const NO_TRACK = 1 << 6;
        const NO_MAGIC = 1 << 7;
        const TUNNEL = 1 << 8;
        const PRIVATE = 1 << 9;
        const GODROOM = 1 << 10;
        const HOUSE = 1 << 11;
        const NO_RECALL = 1 << 12;
        const NO_SUMMON = 1 << 13;
        const NO_CLAN = 1 << 14;
        const ARENA = 1 << 15;
    }
}

// Main room structure
#[derive(Debug)]
pub struct Room {
    pub number: RoomVnum,
    pub zone: i32,
    pub sector_type: SectorType,
    pub name: String,
    pub description: String,
    pub extra_descriptions: Vec<(String, String)>,
    
    // Exits
    pub exits: [Option<Exit>; NUM_OF_DIRS],
    
    // Flags
    pub room_flags: RoomFlags,
    
    // Environmental
    pub light: u8,
    pub blood: u8,
    pub snow: u8,
    
    // Map coordinates
    pub map_x: Option<i32>,
    pub map_y: Option<i32>,
    
    // Contents
    pub people: Vec<Weak<RwLock<Character>>>,
    pub contents: Vec<Arc<RwLock<Object>>>,
}

impl Room {
    pub fn new(number: RoomVnum, zone: i32, name: String, description: String) -> Self {
        Room {
            number,
            zone,
            sector_type: SectorType::Inside,
            name,
            description,
            extra_descriptions: Vec::new(),
            exits: Default::default(),
            room_flags: RoomFlags::empty(),
            light: 0,
            blood: 0,
            snow: 0,
            map_x: None,
            map_y: None,
            people: Vec::new(),
            contents: Vec::new(),
        }
    }
    
    pub fn add_character(&mut self, character: Weak<RwLock<Character>>) {
        self.people.push(character);
    }
    
    pub fn remove_character(&mut self, char_id: u64) {
        self.people.retain(|ch| {
            if let Some(character) = ch.upgrade() {
                character.read().id != char_id
            } else {
                false
            }
        });
    }
    
    pub fn add_object(&mut self, object: Arc<RwLock<Object>>) {
        self.contents.push(object);
    }
    
    pub fn remove_object(&mut self, obj_id: u64) {
        self.contents.retain(|obj| obj.read().id != obj_id);
    }
    
    pub fn is_dark(&self) -> bool {
        self.room_flags.contains(RoomFlags::DARK) && self.light == 0
    }
    
    pub fn get_exit(&self, direction: usize) -> Option<&Exit> {
        if direction < NUM_OF_DIRS {
            self.exits[direction].as_ref()
        } else {
            None
        }
    }
    
    pub fn set_exit(&mut self, direction: usize, exit: Exit) {
        if direction < NUM_OF_DIRS {
            self.exits[direction] = Some(exit);
        }
    }
    
    pub fn send_to_room(&self, _message: &str) {
        // Send message to all characters in room
        for char_ref in &self.people {
            if let Some(character) = char_ref.upgrade() {
                // TODO: Send message through character's descriptor
                let _ch = character.read();
                // ch.send_message(message);
            }
        }
    }
    
    pub fn count_people(&self) -> usize {
        self.people.iter()
            .filter(|ch| ch.upgrade().is_some())
            .count()
    }
}