// Core type definitions for DeltaMUD (idiomatic Rust port).
//
// The port follows CircleMUD's actually-single-threaded heartbeat model:
// a single `GameState` owns the world and every entity lives in an
// id-indexed arena. Entities reference each other by small `Copy` ids
// (`CharId`, `ObjId`, room real-numbers) instead of locked pointers, which
// mirrors C's `char_data *` semantics with safe indices and lets the borrow
// checker — not hand-managed lock ordering — enforce correctness.

use std::fmt;

// ---------------------------------------------------------------------------
// Virtual-number aliases (vnums are the builder-facing numbers from the
// world files; rnums are dense indices into the loaded arrays).
// ---------------------------------------------------------------------------
pub type RoomVnum = i32;
pub type ObjVnum = i32;
pub type MobVnum = i32;
pub type ZoneVnum = i32;

/// Real room number: a dense index into `GameState::rooms`.
pub type RoomRnum = usize;

pub type Level = u8;
pub type Hitroll = i16;
pub type Damroll = i16;
pub type ArmorClass = i16;
pub type Gold = i32;
pub type Experience = i64;

// ---------------------------------------------------------------------------
// Entity ids. Newtypes over u64 so they can never be confused with each
// other or with raw counts. A lookup that misses (e.g. after the entity is
// extracted) simply returns `None`, mirroring C's null-pointer checks.
// ---------------------------------------------------------------------------
macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u64);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}
id_newtype!(CharId);
id_newtype!(ObjId);
id_newtype!(ConnId);

// Sentinel "nil" references, matching CircleMUD's structs.h.
pub const NOWHERE: RoomVnum = -1;
pub const NOTHING: ObjVnum = -1;
pub const NOBODY: MobVnum = -1;

// ---------------------------------------------------------------------------
// Directions (structs.h: NUM_OF_DIRS = 6, order n/e/s/w/u/d).
// ---------------------------------------------------------------------------
pub const NORTH: usize = 0;
pub const EAST: usize = 1;
pub const SOUTH: usize = 2;
pub const WEST: usize = 3;
pub const UP: usize = 4;
pub const DOWN: usize = 5;
pub const NUM_OF_DIRS: usize = 6;

// ---------------------------------------------------------------------------
// Classes (DeltaMUD: 5 player classes).
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    MagicUser = 0,
    Cleric = 1,
    Thief = 2,
    Warrior = 3,
    Artisan = 4,
}

impl Class {
    pub fn from_u8(v: u8) -> Class {
        match v {
            0 => Class::MagicUser,
            1 => Class::Cleric,
            2 => Class::Thief,
            3 => Class::Warrior,
            _ => Class::Artisan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Race {
    Human = 0,
    Elf = 1,
    Gnome = 2,
    Dwarf = 3,
    Troll = 4,
    Goblin = 5,
    Drow = 6,
    Orc = 7,
    Minotaur = 8,
    // Non-C races kept beyond the playable C range (RACE_UNDEFINED=-1, 0..=8
    // are the structs.h indices persisted to player_main.race).
    HalfElf = 9,
    Kender = 10,
    Vampire = 11,
    Ogre = 12,
    HalfOrc = 13,
}

impl Race {
    pub fn from_u8(v: u8) -> Race {
        match v {
            0 => Race::Human,
            1 => Race::Elf,
            2 => Race::Gnome,
            3 => Race::Dwarf,
            4 => Race::Troll,
            5 => Race::Goblin,
            6 => Race::Drow,
            7 => Race::Orc,
            8 => Race::Minotaur,
            9 => Race::HalfElf,
            10 => Race::Kender,
            11 => Race::Vampire,
            12 => Race::Ogre,
            _ => Race::HalfOrc,
        }
    }
}

// ---------------------------------------------------------------------------
// Position. Numeric values MUST match structs.h POS_* exactly — they are
// stored in the player file / DB and used as min_position gates in the
// command table. (The prior port had Resting/Meditating swapped.)
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Position {
    Dead = 0,
    MortallyWounded = 1,
    Incapacitated = 2,
    Stunned = 3,
    Sleeping = 4,
    Resting = 5,
    Meditating = 6,
    Sitting = 7,
    Fighting = 8,
    Standing = 9,
}

impl Position {
    pub fn from_u8(v: u8) -> Position {
        match v {
            0 => Position::Dead,
            1 => Position::MortallyWounded,
            2 => Position::Incapacitated,
            3 => Position::Stunned,
            4 => Position::Sleeping,
            5 => Position::Resting,
            6 => Position::Meditating,
            7 => Position::Sitting,
            8 => Position::Fighting,
            _ => Position::Standing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Gender {
    Neutral = 0,
    Male = 1,
    Female = 2,
}

impl Gender {
    pub fn from_u8(v: u8) -> Gender {
        match v {
            1 => Gender::Male,
            2 => Gender::Female,
            _ => Gender::Neutral,
        }
    }
}

// ---------------------------------------------------------------------------
// Immortal level thresholds (structs.h LVL_*).
// ---------------------------------------------------------------------------
pub const LVL_IMPL: Level = 105;
pub const LVL_GRGOD: Level = 104;
pub const LVL_GOD: Level = 103;
pub const LVL_IMMORT: Level = 101;
pub const LVL_FREEZE: Level = LVL_GRGOD;

// ---------------------------------------------------------------------------
// Condition indices (structs.h): char->player_specials->conditions[].
// ---------------------------------------------------------------------------
pub const DRUNK: usize = 0;
pub const FULL: usize = 1;
pub const THIRST: usize = 2;
pub const NUM_CONDITIONS: usize = 3;

// Player-data fixed limits (structs.h — *DO*NOT*CHANGE* for save-compat).
pub const MAX_SKILLS: usize = 1000;
pub const MAX_TONGUE: usize = 3;
pub const MAX_AFFECT: usize = 32;

// ---------------------------------------------------------------------------
// Equipment wear positions (structs.h WEAR_*; NUM_WEARS = 20).
// ---------------------------------------------------------------------------
pub const WEAR_LIGHT: usize = 0;
pub const WEAR_FINGER_R: usize = 1;
pub const WEAR_FINGER_L: usize = 2;
pub const WEAR_NECK_1: usize = 3;
pub const WEAR_NECK_2: usize = 4;
pub const WEAR_BODY: usize = 5;
pub const WEAR_HEAD: usize = 6;
pub const WEAR_LEGS: usize = 7;
pub const WEAR_FEET: usize = 8;
pub const WEAR_HANDS: usize = 9;
pub const WEAR_ARMS: usize = 10;
pub const WEAR_SHIELD: usize = 11;
pub const WEAR_ABOUT: usize = 12;
pub const WEAR_WAIST: usize = 13;
pub const WEAR_WRIST_R: usize = 14;
pub const WEAR_WRIST_L: usize = 15;
pub const WEAR_WIELD: usize = 16;
pub const WEAR_HOLD: usize = 17;
pub const WEAR_SHOULDERS: usize = 18;
pub const WEAR_ANKLE_R: usize = 19;
pub const WEAR_ANKLE_L: usize = 20;
pub const WEAR_FACE: usize = 21;
pub const NUM_WEARS: usize = 22;

// ---------------------------------------------------------------------------
// Heartbeat timing. CircleMUD: OPT_USEC=100000 => PASSES_PER_SEC=10, so one
// pulse is 100ms. RL_SEC = *PASSES_PER_SEC. (comm.c game_loop.)
// ---------------------------------------------------------------------------
pub const PASSES_PER_SEC: u64 = 10;
pub const PULSE_VIOLENCE: u64 = 2 * PASSES_PER_SEC; // every 2s
pub const PULSE_MOBILE: u64 = 10 * PASSES_PER_SEC; // every 10s
pub const PULSE_ZONE: u64 = 10 * PASSES_PER_SEC; // every 10s

/// Direction names for output (CircleMUD `dirs[]`).
pub const DIR_NAMES: [&str; NUM_OF_DIRS] = ["north", "east", "south", "west", "up", "down"];

/// The direction you came *from* when arriving via `dir` (for arrival msgs).
pub const REV_DIR: [usize; NUM_OF_DIRS] = [SOUTH, WEST, NORTH, EAST, DOWN, UP];

#[cfg(test)]
mod race_index_tests {
    use super::*;

    #[test]
    fn race_ordinals_match_c_structs_h_indices() {
        // C structs.h:134-142 / races.c parse_race: the persisted player_main
        // .race value is the C index. Before #243 the enum had Orc=5,
        // HalfElf=6, Kender=7, so a Goblin ('f' -> 5) was stored, loaded and
        // displayed as Orc, a Drow as Half-Elf, an Orc as Kender.
        assert_eq!(Race::Goblin as u8, 5);
        assert_eq!(Race::Drow as u8, 6);
        assert_eq!(Race::Orc as u8, 7);
        assert_eq!(Race::Minotaur as u8, 8);
        for letter in ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i'] {
            let idx = crate::races::parse_race(letter);
            assert!(idx >= 0);
            assert_eq!(Race::from_u8(idx as u8) as u8, idx as u8);
        }
        assert_eq!(Race::from_u8(5), Race::Goblin);
        assert_eq!(Race::from_u8(7), Race::Orc);
    }
}
