// Object (item) entity — id-indexed. Location and containment are expressed
// as ids into the GameState arenas rather than locked pointers.

use crate::types::*;

// Object types (structs.h ITEM_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    Light = 1,
    Scroll = 2,
    Wand = 3,
    Staff = 4,
    Weapon = 5,
    Treasure = 6,
    Armor = 7,
    Potion = 8,
    Other = 9,
    Trash = 10,
    Container = 11,
    Note = 12,
    LiqContainer = 13,
    Key = 14,
    Food = 15,
    Money = 16,
    Fountain = 17,
}

impl ObjectType {
    pub fn from_i32(v: i32) -> ObjectType {
        match v {
            1 => ObjectType::Light,
            2 => ObjectType::Scroll,
            3 => ObjectType::Wand,
            4 => ObjectType::Staff,
            5 => ObjectType::Weapon,
            6 => ObjectType::Treasure,
            7 => ObjectType::Armor,
            8 => ObjectType::Potion,
            10 => ObjectType::Trash,
            11 => ObjectType::Container,
            12 => ObjectType::Note,
            13 => ObjectType::LiqContainer,
            14 => ObjectType::Key,
            15 => ObjectType::Food,
            16 => ObjectType::Money,
            17 => ObjectType::Fountain,
            _ => ObjectType::Other,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct WearFlags: u32 {
        const TAKE = 1 << 0;
        const FINGER = 1 << 1;
        const NECK = 1 << 2;
        const BODY = 1 << 3;
        const HEAD = 1 << 4;
        const LEGS = 1 << 5;
        const FEET = 1 << 6;
        const HANDS = 1 << 7;
        const ARMS = 1 << 8;
        const SHIELD = 1 << 9;
        const ABOUT = 1 << 10;
        const WAIST = 1 << 11;
        const WRIST = 1 << 12;
        const WIELD = 1 << 13;
        const HOLD = 1 << 14;
        const SHOULDERS = 1 << 15;
        const ANKLE = 1 << 16;
        const FACE = 1 << 17;
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ExtraFlags: u64 {
        const GLOW = 1 << 0;
        const HUM = 1 << 1;
        const NO_RENT = 1 << 2;
        const NO_DONATE = 1 << 3;
        const NO_INVIS = 1 << 4;
        const INVISIBLE = 1 << 5;
        const MAGIC = 1 << 6;
        const NO_DROP = 1 << 7;
        const BLESS = 1 << 8;
        const ANTI_GOOD = 1 << 9;
        const ANTI_EVIL = 1 << 10;
        const ANTI_NEUTRAL = 1 << 11;
        const ANTI_MAGIC_USER = 1 << 12;
        const ANTI_CLERIC = 1 << 13;
        const ANTI_THIEF = 1 << 14;
        const ANTI_WARRIOR = 1 << 15;
        const NO_SELL = 1 << 16;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ObjectAffect {
    pub location: i32,
    pub modifier: i32,
}

/// Where an object currently lives. Exactly one of these holds at a time,
/// mirroring CircleMUD's mutually-exclusive in_room/carried_by/worn_by/in_obj.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjLoc {
    Nowhere,
    Room(RoomRnum),
    Carried(CharId),
    Worn(CharId, usize),
    Contained(ObjId),
}

#[derive(Debug)]
pub struct Object {
    pub id: ObjId,
    pub item_number: ObjVnum, // prototype vnum, or NOTHING for synthetic

    /// Current location (room / inventory / worn / inside container).
    pub loc: ObjLoc,
    /// Ids of objects this container holds (CircleMUD obj->contains list).
    pub contains: Vec<ObjId>,

    // Descriptions
    pub name: String,              // keyword list
    pub description: String,       // shown on the ground
    pub short_description: String, // inventory / action line
    pub action_description: Option<String>,

    // Properties
    pub obj_type: ObjectType,
    pub wear_flags: WearFlags,
    pub extra_flags: ExtraFlags,
    pub weight: i32,
    pub cost: i32,
    pub rent: i32,
    pub level: Level,
    pub timer: i32,
    pub values: [i32; 4],
    pub affects: Vec<ObjectAffect>,
}

impl Object {
    pub fn new(vnum: ObjVnum, name: String, short_desc: String) -> Self {
        Object {
            id: ObjId(0),
            item_number: vnum,
            loc: ObjLoc::Nowhere,
            contains: Vec::new(),
            name,
            description: String::new(),
            short_description: short_desc,
            action_description: None,
            obj_type: ObjectType::Other,
            wear_flags: WearFlags::TAKE,
            extra_flags: ExtraFlags::empty(),
            weight: 1,
            cost: 0,
            rent: 0,
            level: 0,
            timer: -1,
            values: [0; 4],
            affects: Vec::new(),
        }
    }

    pub fn is_container(&self) -> bool {
        self.obj_type == ObjectType::Container
    }
    pub fn is_weapon(&self) -> bool {
        self.obj_type == ObjectType::Weapon
    }
    pub fn is_armor(&self) -> bool {
        self.obj_type == ObjectType::Armor
    }
    pub fn can_wear(&self, position: WearFlags) -> bool {
        self.wear_flags.contains(position)
    }

    /// Weapon damage dice (num, size) from values[1]/values[2].
    pub fn damage_dice(&self) -> Option<(i32, i32)> {
        if self.is_weapon() {
            Some((self.values[1], self.values[2]))
        } else {
            None
        }
    }

    pub fn armor_class(&self) -> i32 {
        if self.is_armor() {
            self.values[0]
        } else {
            0
        }
    }
}
