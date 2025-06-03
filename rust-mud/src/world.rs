use crate::types::*;
use crate::room::{Room, Exit};
use crate::object::{Object, ObjectType, WearFlags, ExtraFlags};
use crate::character::Character;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use anyhow::{Result, anyhow};

// Zone structure
#[derive(Debug, Clone)]
pub struct Zone {
    pub number: i32,
    pub name: String,
    pub lifespan: i32,
    pub age: i32,
    pub top: RoomVnum,
    pub reset_mode: i32,
    pub min_level: Level,
    pub max_level: Level,
    pub map_x: Option<i32>,
    pub map_y: Option<i32>,
}

// Mobile (NPC) prototype
#[derive(Debug, Clone)]
pub struct MobileProto {
    pub vnum: MobVnum,
    pub name: String,
    pub short_desc: String,
    pub long_desc: String,
    pub description: String,
    pub level: Level,
    pub hitpoints: i32,
    pub experience: Experience,
    pub gold: Gold,
    pub position: Position,
    pub default_pos: Position,
    pub sex: Gender,
}

// Object prototype
#[derive(Debug, Clone)]
pub struct ObjectProto {
    pub vnum: ObjVnum,
    pub name: String,
    pub short_desc: String,
    pub description: String,
    pub obj_type: ObjectType,
    pub wear_flags: WearFlags,
    pub extra_flags: ExtraFlags,
    pub weight: i32,
    pub cost: i32,
    pub rent: i32,
    pub values: [i32; 4],
}

// Main world structure
pub struct World {
    // Room storage
    pub rooms: HashMap<RoomVnum, Arc<RwLock<Room>>>,
    pub room_index: Vec<RoomVnum>,  // For fast iteration
    
    // Zone storage
    pub zones: Vec<Zone>,
    
    // Prototypes
    pub mob_protos: HashMap<MobVnum, MobileProto>,
    pub obj_protos: HashMap<ObjVnum, ObjectProto>,
    
    // Active entities
    pub characters: HashMap<u64, Arc<RwLock<Character>>>,
    pub objects: HashMap<u64, Arc<RwLock<Object>>>,
    
    // ID generation
    next_char_id: u64,
    next_obj_id: u64,
}

impl World {
    pub fn new() -> Self {
        World {
            rooms: HashMap::new(),
            room_index: Vec::new(),
            zones: Vec::new(),
            mob_protos: HashMap::new(),
            obj_protos: HashMap::new(),
            characters: HashMap::new(),
            objects: HashMap::new(),
            next_char_id: 1,
            next_obj_id: 1,
        }
    }
    
    pub fn get_room(&self, vnum: RoomVnum) -> Option<Arc<RwLock<Room>>> {
        self.rooms.get(&vnum).cloned()
    }
    
    pub fn add_room(&mut self, room: Room) {
        let vnum = room.number;
        self.rooms.insert(vnum, Arc::new(RwLock::new(room)));
        self.room_index.push(vnum);
    }
    
    pub fn create_character(&mut self, ch: Character) -> Arc<RwLock<Character>> {
        let mut ch = ch;
        ch.id = self.next_char_id;
        self.next_char_id += 1;
        
        let ch_arc = Arc::new(RwLock::new(ch));
        self.characters.insert(ch_arc.read().id, ch_arc.clone());
        ch_arc
    }
    
    pub fn remove_character(&mut self, id: u64) {
        self.characters.remove(&id);
    }
    
    pub fn create_object(&mut self, obj: Object) -> Arc<RwLock<Object>> {
        let mut obj = obj;
        obj.id = self.next_obj_id;
        self.next_obj_id += 1;
        
        let obj_arc = Arc::new(RwLock::new(obj));
        self.objects.insert(obj_arc.read().id, obj_arc.clone());
        obj_arc
    }
    
    pub fn remove_object(&mut self, id: u64) {
        self.objects.remove(&id);
    }
    
    pub fn move_character(&self, ch: Arc<RwLock<Character>>, to_room: RoomVnum) -> Result<()> {
        let new_room = self.get_room(to_room)
            .ok_or_else(|| anyhow!("Room {} doesn't exist", to_room))?;
        
        let ch_weak = Arc::downgrade(&ch);
        let ch_id = ch.read().id;
        
        // Remove from old room
        if let Some(old_room_weak) = ch.read().in_room.clone() {
            if let Some(old_room) = old_room_weak.upgrade() {
                old_room.write().remove_character(ch_id);
            }
        }
        
        // Add to new room
        new_room.write().add_character(ch_weak.clone());
        ch.write().in_room = Some(Arc::downgrade(&new_room));
        
        Ok(())
    }
    
    pub fn load_mobile(&mut self, vnum: MobVnum) -> Result<Arc<RwLock<Character>>> {
        let proto = self.mob_protos.get(&vnum)
            .ok_or_else(|| anyhow!("Mobile {} doesn't exist", vnum))?;
        
        let mut mob = Character::new_npc(vnum);
        mob.player.name = proto.name.clone();
        mob.player.level = proto.level;
        mob.points.hit = proto.hitpoints;
        mob.points.max_hit = proto.hitpoints;
        mob.points.gold = proto.gold;
        mob.points.exp = proto.experience;
        mob.position = proto.position;
        
        Ok(self.create_character(mob))
    }
    
    pub fn load_object(&mut self, vnum: ObjVnum) -> Result<Arc<RwLock<Object>>> {
        let proto = self.obj_protos.get(&vnum)
            .ok_or_else(|| anyhow!("Object {} doesn't exist", vnum))?;
        
        let mut obj = Object::new(vnum, proto.name.clone(), proto.short_desc.clone());
        obj.description = proto.description.clone();
        obj.obj_type = proto.obj_type;
        obj.wear_flags = proto.wear_flags;
        obj.extra_flags = proto.extra_flags;
        obj.weight = proto.weight;
        obj.cost = proto.cost;
        obj.rent = proto.rent;
        obj.values.value = proto.values;
        
        Ok(self.create_object(obj))
    }
    
    pub fn find_character_by_name(&self, name: &str) -> Option<Arc<RwLock<Character>>> {
        let name_lower = name.to_lowercase();
        for (_, ch) in &self.characters {
            if ch.read().player.name.to_lowercase() == name_lower {
                return Some(ch.clone());
            }
        }
        None
    }
}

// World loading functions would go here
impl World {
    pub async fn load_world_files(&mut self) -> Result<()> {
        // TODO: Implement file loading
        // For now, create a few test rooms
        self.create_test_world();
        Ok(())
    }
    
    fn create_test_world(&mut self) {
        // Create the void
        let void = Room::new(
            0, 
            0, 
            "The Void".to_string(),
            "You are floating in nothingness.".to_string()
        );
        self.add_room(void);
        
        // Create temple
        let temple = Room::new(
            3001,
            30,
            "The Temple of Midgaard".to_string(),
            "You are in the southern end of the temple hall in the Temple of Midgaard.\r\n\
             The temple has been constructed from giant marble blocks, eternal in\r\n\
             appearance, and most of the walls are covered by ancient wall paintings\r\n\
             picturing Gods, Giants and peasants.".to_string()
        );
        self.add_room(temple);
        
        // Create temple square
        let square = Room::new(
            3005,
            30,
            "Temple Square".to_string(),
            "This is so-called temple square.  All around you can see the movement of\r\n\
             shops and bars along the roads.  Close by is the temple and the entrance\r\n\
             to the Clerics' Guild.".to_string()
        );
        self.add_room(square);
        
        // Link rooms
        if let Some(temple_room) = self.get_room(3001) {
            temple_room.write().set_exit(SOUTH, Exit {
                description: Some("The temple square lies to the south.".to_string()),
                keyword: None,
                exit_info: 0,
                key: -1,
                to_room: 3005,
            });
        }
        
        if let Some(square_room) = self.get_room(3005) {
            square_room.write().set_exit(NORTH, Exit {
                description: Some("The temple entrance is to the north.".to_string()),
                keyword: None,
                exit_info: 0,
                key: -1,
                to_room: 3001,
            });
        }
    }
}