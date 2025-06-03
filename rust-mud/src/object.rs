use crate::types::*;
use crate::character::Character;
use crate::room::Room;
use std::sync::{Arc, Weak};
use parking_lot::RwLock;

// Object types
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

// Object wear flags
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
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
        const FLOAT = 1 << 15;
        const FACE = 1 << 16;
    }
}

// Object extra flags
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
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

// Object affects
#[derive(Debug, Clone)]
pub struct ObjectAffect {
    pub location: i32,
    pub modifier: i32,
}

// Object values (interpretation depends on object type)
#[derive(Debug, Clone)]
pub struct ObjectValues {
    pub value: [i32; 4],
}

// Main object structure
#[derive(Debug)]
pub struct Object {
    pub id: u64,
    pub item_number: ObjVnum,
    
    // Location
    pub in_room: Option<Weak<RwLock<Room>>>,
    pub carried_by: Option<Weak<RwLock<Character>>>,
    pub worn_by: Option<Weak<RwLock<Character>>>,
    pub worn_on: Option<usize>,
    pub in_obj: Option<Weak<RwLock<Object>>>,
    
    // Descriptions
    pub name: String,          // Keywords
    pub description: String,   // Room description
    pub short_description: String,  // Inventory description
    pub action_description: Option<String>,  // Use message
    
    // Properties
    pub obj_type: ObjectType,
    pub wear_flags: WearFlags,
    pub extra_flags: ExtraFlags,
    pub weight: i32,
    pub cost: i32,
    pub rent: i32,
    pub level: Level,
    pub timer: i32,
    
    // Values (interpretation depends on type)
    pub values: ObjectValues,
    
    // Affects
    pub affects: Vec<ObjectAffect>,
    
    // Container contents
    pub contains: Vec<Arc<RwLock<Object>>>,
}

impl Object {
    pub fn new(vnum: ObjVnum, name: String, short_desc: String) -> Self {
        Object {
            id: 0,
            item_number: vnum,
            in_room: None,
            carried_by: None,
            worn_by: None,
            worn_on: None,
            in_obj: None,
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
            values: ObjectValues { value: [0; 4] },
            affects: Vec::new(),
            contains: Vec::new(),
        }
    }
    
    pub fn is_container(&self) -> bool {
        self.obj_type == ObjectType::Container
    }
    
    pub fn can_wear(&self, position: WearFlags) -> bool {
        self.wear_flags.contains(position)
    }
    
    pub fn add_to_container(&mut self, obj: Arc<RwLock<Object>>) {
        self.contains.push(obj);
    }
    
    pub fn remove_from_container(&mut self, obj_id: u64) {
        self.contains.retain(|obj| obj.read().id != obj_id);
    }
    
    pub fn get_total_weight(&self) -> i32 {
        let mut total = self.weight;
        for obj in &self.contains {
            total += obj.read().get_total_weight();
        }
        total
    }
    
    pub fn is_weapon(&self) -> bool {
        self.obj_type == ObjectType::Weapon
    }
    
    pub fn is_armor(&self) -> bool {
        self.obj_type == ObjectType::Armor
    }
    
    pub fn get_damage_dice(&self) -> Option<(i32, i32)> {
        if self.is_weapon() {
            Some((self.values.value[1], self.values.value[2]))
        } else {
            None
        }
    }
    
    pub fn get_armor_class(&self) -> i32 {
        if self.is_armor() {
            self.values.value[0]
        } else {
            0
        }
    }
}