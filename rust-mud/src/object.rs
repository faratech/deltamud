// Object (item) entity — id-indexed. Location and containment are expressed
// as ids into the GameState arenas rather than locked pointers.

use crate::types::*;

// Object types (structs.h ITEM_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    // Discriminants MUST equal DeltaMUD structs.h ITEM_* numbers: they are the
    // value read from .obj files (file_loader) and the index into ITEM_TYPES
    // (constants.rs, C item_types[]). A previous, sequential numbering silently
    // mis-typed every non-weapon item (FOOD→OTHER, CONTAINER→FOOD, DRINKCON→
    // FOUNTAIN, ...) — masked for immortals, who bypass the item-type checks.
    Light = 1,
    Scroll = 2,
    Wand = 3,
    Staff = 4,
    Weapon = 5,
    FireWeapon = 6,
    Missile = 7,
    Treasure = 8,
    Armor = 9,
    Potion = 10,
    Worn = 11,
    Other = 12,
    Trash = 13,
    Trap = 14,
    Container = 15,
    Note = 16,
    LiqContainer = 17, // ITEM_DRINKCON
    Key = 18,
    Food = 19,
    Money = 20,
    Pen = 21,
    Boat = 22,
    Fountain = 23,
    Portal = 24,
    HpRegen = 25,
    MpRegen = 26,
    MvRegen = 27,
    Atm = 28,
}

impl ObjectType {
    pub fn from_i32(v: i32) -> ObjectType {
        use ObjectType::*;
        match v {
            1 => Light,
            2 => Scroll,
            3 => Wand,
            4 => Staff,
            5 => Weapon,
            6 => FireWeapon,
            7 => Missile,
            8 => Treasure,
            9 => Armor,
            10 => Potion,
            11 => Worn,
            13 => Trash,
            14 => Trap,
            15 => Container,
            16 => Note,
            17 => LiqContainer,
            18 => Key,
            19 => Food,
            20 => Money,
            21 => Pen,
            22 => Boat,
            23 => Fountain,
            24 => Portal,
            25 => HpRegen,
            26 => MpRegen,
            27 => MvRegen,
            28 => Atm,
            _ => Other, // 12 and any unknown
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
        const ANTI_ARTISAN = 1 << 17;
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
    // DeltaMUD "slots" carried in value[4]/value[5] of the .obj values line.
    pub curr_slots: i32,
    pub total_slots: i32,
    // DeltaMUD per-object class restriction (`c` block, stored as class-1) and
    // minimum use level (`L` block).
    pub obj_class: i32,
    pub min_level: i32,
    // 32-bit affect bitvector (`BV` block).
    pub bitvector: i64,
    pub affects: Vec<ObjectAffect>,
    /// Extra descriptions (`E` blocks): (keyword, description).
    pub ex_descriptions: Vec<(String, String)>,
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
            curr_slots: 0,
            total_slots: 0,
            obj_class: -1,
            min_level: 0,
            bitvector: 0,
            affects: Vec::new(),
            ex_descriptions: Vec::new(),
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
