// Core type definitions for DeltaMUD

pub type RoomVnum = i32;
pub type ObjVnum = i32;
pub type MobVnum = i32;
pub type RoomRnum = usize;
pub type ObjRnum = usize;
pub type MobRnum = usize;

pub type Level = u8;
pub type Hitroll = i16;
pub type Damroll = i16;
pub type ArmorClass = i16;
pub type Gold = i32;
pub type Experience = i64;

// Direction constants
pub const NORTH: usize = 0;
pub const EAST: usize = 1;
pub const SOUTH: usize = 2;
pub const WEST: usize = 3;
pub const UP: usize = 4;
pub const DOWN: usize = 5;
pub const NUM_OF_DIRS: usize = 6;

// Class constants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    MagicUser = 0,
    Cleric = 1,
    Thief = 2,
    Warrior = 3,
    Artisan = 4,
}

// Race constants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Race {
    Human = 0,
    Elf = 1,
    Gnome = 2,
    Dwarf = 3,
    Troll = 4,
    Orc = 5,
    HalfElf = 6,
    Kender = 7,
    Minotaur = 8,
    Vampire = 9,
    Ogre = 10,
    HalfOrc = 11,
}

// Position states
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Position {
    Dead = 0,
    MortalllyWounded = 1,
    Incapacitated = 2,
    Stunned = 3,
    Sleeping = 4,
    Meditating = 5,
    Resting = 6,
    Sitting = 7,
    Fighting = 8,
    Standing = 9,
}

// Gender
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Gender {
    Neutral = 0,
    Male = 1,
    Female = 2,
}

// Equipment positions
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
pub const WEAR_FLOAT: usize = 18;
pub const WEAR_FACE: usize = 19;
pub const NUM_WEARS: usize = 20;