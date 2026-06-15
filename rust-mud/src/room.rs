// Room entity — id-indexed. Occupants and ground items are stored as ids.

use crate::types::*;

#[derive(Debug, Clone)]
pub struct Exit {
    pub description: Option<String>,
    pub keyword: Option<String>,
    pub exit_info: i32, // door flags: bit0=ISDOOR, plus CLOSED/LOCKED
    pub key: ObjVnum,
    pub to_room: RoomVnum, // destination vnum (resolved to rnum at lookup)
}

// Exit door-state bits (structs.h EX_*).
pub const EX_ISDOOR: i32 = 1 << 0;
pub const EX_CLOSED: i32 = 1 << 1;
pub const EX_LOCKED: i32 = 1 << 2;
pub const EX_PICKPROOF: i32 = 1 << 3;

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

impl SectorType {
    pub fn from_i32(v: i32) -> SectorType {
        match v {
            1 => SectorType::City,
            2 => SectorType::Field,
            3 => SectorType::Forest,
            4 => SectorType::Hills,
            5 => SectorType::Mountain,
            6 => SectorType::WaterSwim,
            7 => SectorType::WaterNoSwim,
            8 => SectorType::Flying,
            9 => SectorType::Underwater,
            10 => SectorType::Ice,
            _ => SectorType::Inside,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug)]
pub struct Room {
    pub number: RoomVnum, // virtual number (builder-facing)
    pub zone: i32,
    pub sector_type: SectorType,
    pub name: String,
    pub description: String,
    pub extra_descriptions: Vec<(String, String)>,
    pub exits: [Option<Exit>; NUM_OF_DIRS],
    pub room_flags: RoomFlags,
    pub light: u8,
    pub blood: u8,
    pub snow: u8,
    pub map_x: Option<i32>,
    pub map_y: Option<i32>,

    // Occupants & ground items, by id. Order matters for parity with the
    // C linked lists; handler code controls insertion order explicitly.
    pub people: Vec<CharId>,
    pub contents: Vec<ObjId>,
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
}
