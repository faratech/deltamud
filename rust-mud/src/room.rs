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

/// A room "special exit" (the `O` block — room_special_exit_data in
/// structs.h). Unlike the 6 cardinal directions, a special exit is a named
/// custom egress with its own leave message. Stored verbatim so a .wld file
/// round-trips; the runtime traversal is wired up by movement code.
#[derive(Debug, Clone)]
pub struct SpecialExit {
    pub general_description: Option<String>,
    pub keyword: Option<String>,
    pub ex_name: Option<String>,
    pub leave_msg: Option<String>,
    pub exit_info: i32,
    pub key: ObjVnum,
    pub to_room: RoomVnum,
}

// Exit door-state bits (structs.h EX_*).
pub const EX_ISDOOR: i32 = 1 << 0;
pub const EX_CLOSED: i32 = 1 << 1;
pub const EX_LOCKED: i32 = 1 << 2;
pub const EX_PICKPROOF: i32 = 1 << 3;
pub const EX_HIDDEN: i32 = 1 << 4;

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
    Underwater = 8,
    Flying = 9,
    Road = 10,
    Wall = 11,
    Swamp = 12,
    Desert = 13,
    Bridge = 14,
}

impl SectorType {
    pub fn from_i32(v: i32) -> SectorType {
        // C structs.h:89-106. The old enum had Flying=8/Underwater=9 swapped
        // and folded Road..Bridge into Inside, erasing Swamp rooms on redit
        // save (#259).
        match v {
            1 => SectorType::City,
            2 => SectorType::Field,
            3 => SectorType::Forest,
            4 => SectorType::Hills,
            5 => SectorType::Mountain,
            6 => SectorType::WaterSwim,
            7 => SectorType::WaterNoSwim,
            8 => SectorType::Underwater,
            9 => SectorType::Flying,
            10 => SectorType::Road,
            11 => SectorType::Wall,
            12 => SectorType::Swamp,
            13 => SectorType::Desert,
            14 => SectorType::Bridge,
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
        // NOTE: bits 12-15 below carry DeltaMUD-custom *names* that differ from
        // the C structs.h ROOM_* meanings (C has HOUSE_CRASH/ATRIUM/OLC/BFS_MARK
        // at 12-15). Callers that need the C semantics test these bits raw; see
        // cmd_movement.rs / house.rs headers. Do not "fix" the names here without
        // auditing every raw-bit workaround.
        const NO_RECALL = 1 << 12;
        const NO_SUMMON = 1 << 13;
        const NO_CLAN = 1 << 14;
        const ARENA = 1 << 15;
        // structs.h ROOM_IMPROOM (LVL_IMPL-only). MUST be defined or
        // RoomFlags::from_bits_truncate at load silently drops it, which both
        // strips it on any redit save AND makes the enter_game / goto IMPROOM
        // guards dead (the bit is never present in memory).
        const IMPROOM = 1 << 16;
        // structs.h ROOM_BAD_REGEN / ROOM_GOOD_REGEN (limits.c regen logic).
        const BAD_REGEN = 1 << 17;
        // structs.h ROOM_WALL (can't enter if on the surface map) /
        // ROOM_CLAN_ROOM (owned by a clan) — builder-settable, so they must
        // round-trip rather than be truncated at load.
        const WALL = 1 << 18;
        const CLAN_ROOM = 1 << 19;
        const GOOD_REGEN = 1 << 20;
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
    pub special_exit: Option<SpecialExit>,
    pub exits: [Option<Exit>; NUM_OF_DIRS],
    pub room_flags: RoomFlags,
    pub light: u8,
    pub blood: u8,
    pub snow: u8,
    pub map_x: Option<i32>,
    pub map_y: Option<i32>,
    pub mapmv: i32,
    /// C world[room].weather (maputils.c swc): the live storm type over this
    /// room (map cells and outdoor rooms of wzonecontrol zones); -1 = none
    /// (#190).
    pub weather: i32,

    // City-interior links (Map mods - Storm; structs.h linkmapnum/linkrnum).
    // linkrnum: real-room index of the city interior reached from a surface
    // map cell (do_enter). linkmapnum: real-room index of the surface map cell
    // reached from a city interior (do_leave). Both default to NOWHERE; C sets
    // them in maputils.c when the surface-map block is spliced in. The Rust
    // world loader does not splice the surface-map room block (see maputils.rs),
    // so these stay None here and do_enter/do_leave fall through to "no
    // visible entrance/exit", exactly as C does for an unlinked room (linkrnum<0
    // / linkmapnum==-1).
    pub linkrnum: Option<RoomRnum>,
    pub linkmapnum: Option<RoomRnum>,

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
            special_exit: None,
            exits: Default::default(),
            room_flags: RoomFlags::empty(),
            light: 0,
            blood: 0,
            snow: 0,
            map_x: None,
            map_y: None,
            mapmv: -1,
            weather: -1,
            linkrnum: None,
            linkmapnum: None,
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
