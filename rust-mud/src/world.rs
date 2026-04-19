use crate::types::*;
use crate::room::{Room, Exit};
use crate::object::{Object, ObjectType, WearFlags, ExtraFlags};
use crate::character::Character;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use anyhow::{Result, anyhow};

/// A single parsed zone reset command. Mirrors CircleMUD's `reset_com`
/// struct (see /web/deltamud/src/db.h:111-119) but uses a Rust enum
/// so each command carries only its meaningful arguments. The `if_flag`
/// field drives CircleMUD's chaining rule: when set, the command only
/// runs if the preceding one in this reset pass succeeded — this is
/// what lets M→E→E→E chains equip the freshly-loaded mob without
/// equipping stray mobs of the same vnum that happened to be in-world.
#[derive(Debug, Clone)]
pub enum ResetCmd {
    /// Load a mobile (NPC) into a room. Sets "last mob" for subsequent G/E.
    LoadMob { if_flag: bool, mob_vnum: MobVnum, max_count: i32, room_vnum: RoomVnum },
    /// Load an object into a room. Sets "last obj" for subsequent P.
    LoadObjInRoom { if_flag: bool, obj_vnum: ObjVnum, max_count: i32, room_vnum: RoomVnum },
    /// Give an object to the last-loaded mob.
    GiveObjToMob { if_flag: bool, obj_vnum: ObjVnum, max_count: i32 },
    /// Equip the last-loaded mob with an object at a wear position.
    EquipMob { if_flag: bool, obj_vnum: ObjVnum, max_count: i32, wear_pos: usize },
    /// Nest an object inside another object (by vnum of the container).
    PutObjInObj { if_flag: bool, obj_vnum: ObjVnum, max_count: i32, container_vnum: ObjVnum },
    /// Remove an object of a given vnum from a room if present.
    RemoveObj { if_flag: bool, room_vnum: RoomVnum, obj_vnum: ObjVnum },
    /// Force a door into open (0) / closed (1) / closed+locked (2) state.
    Door { if_flag: bool, room_vnum: RoomVnum, direction: usize, state: i32 },
}

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
    pub reset_commands: Vec<ResetCmd>,
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
        mob.points.hit = proto.hitpoints.max(1);
        mob.points.max_hit = mob.points.hit;
        // CircleMUD stores AC*10 internally. A mob with no AC data
        // effectively has AC 10 (unarmored); use 100 as the stored value
        // so the THAC0 formula lands near CircleMUD baseline. Without
        // this, mobs have AC 0 and level-1 PCs hit only on a natural 19+.
        if mob.points.armor == 0 {
            mob.points.armor = 100;
        }
        mob.points.gold = proto.gold;
        mob.points.exp = proto.experience;
        mob.position = proto.position;
        mob.short_desc = Some(proto.short_desc.clone());
        mob.long_desc = Some(proto.long_desc.clone());
        mob.npc_description = Some(proto.description.clone());

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

    /// Count live NPC instances by prototype vnum. Recomputed on each
    /// zone reset per advisor guidance — simpler than tracking create/
    /// destroy sites and the O(n) cost is negligible on a 15-min cadence.
    pub fn count_mobs_by_vnum(&self) -> HashMap<MobVnum, i32> {
        let mut counts = HashMap::new();
        for ch in self.characters.values() {
            let ch = ch.read();
            if ch.is_npc {
                *counts.entry(ch.nr).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Count live object instances by prototype vnum.
    pub fn count_objs_by_vnum(&self) -> HashMap<ObjVnum, i32> {
        let mut counts = HashMap::new();
        for obj in self.objects.values() {
            let obj = obj.read();
            *counts.entry(obj.item_number).or_insert(0) += 1;
        }
        counts
    }

    /// Execute a zone's reset_commands once. Mirrors CircleMUD's
    /// reset_zone() (see /web/deltamud/src/db.c:1973-2134). Returns
    /// a summary suitable for logging (count of mobs/objs/doors
    /// actually acted on).
    pub fn reset_zone(&mut self, zone_number: i32) -> ResetSummary {
        let commands: Vec<ResetCmd> = match self.zones.iter().find(|z| z.number == zone_number) {
            Some(z) => z.reset_commands.clone(),
            None => return ResetSummary::default(),
        };

        let mut mob_counts = self.count_mobs_by_vnum();
        let mut obj_counts = self.count_objs_by_vnum();

        let mut last_cmd: bool = false;
        let mut last_mob: Option<Arc<RwLock<Character>>> = None;
        let mut last_obj: Option<Arc<RwLock<Object>>> = None;
        let mut summary = ResetSummary::default();

        for cmd in &commands {
            // if_flag chaining: skip if this command is gated on the prior
            // command's success and the prior one failed (or was skipped).
            let (if_flag, _is_mob_start) = match cmd {
                ResetCmd::LoadMob { if_flag, .. } => (*if_flag, true),
                ResetCmd::LoadObjInRoom { if_flag, .. } => (*if_flag, false),
                ResetCmd::GiveObjToMob { if_flag, .. } => (*if_flag, false),
                ResetCmd::EquipMob { if_flag, .. } => (*if_flag, false),
                ResetCmd::PutObjInObj { if_flag, .. } => (*if_flag, false),
                ResetCmd::RemoveObj { if_flag, .. } => (*if_flag, false),
                ResetCmd::Door { if_flag, .. } => (*if_flag, false),
            };
            if if_flag && !last_cmd {
                continue;
            }
            // Reset chain state when starting a fresh (non-chained) command.
            if !if_flag {
                last_mob = None;
                last_obj = None;
            }

            match cmd {
                ResetCmd::LoadMob { mob_vnum, max_count, room_vnum, .. } => {
                    let live = mob_counts.get(mob_vnum).copied().unwrap_or(0);
                    if live >= *max_count {
                        last_cmd = false;
                        continue;
                    }
                    match self.load_mobile(*mob_vnum) {
                        Ok(mob_arc) => {
                            if let Some(room) = self.get_room(*room_vnum) {
                                room.write().add_character(Arc::downgrade(&mob_arc));
                                mob_arc.write().in_room = Some(Arc::downgrade(&room));
                                *mob_counts.entry(*mob_vnum).or_insert(0) += 1;
                                summary.mobs_spawned += 1;
                                last_mob = Some(mob_arc);
                                last_cmd = true;
                            } else {
                                self.remove_character(mob_arc.read().id);
                                last_cmd = false;
                            }
                        }
                        Err(_) => last_cmd = false,
                    }
                }
                ResetCmd::LoadObjInRoom { obj_vnum, max_count, room_vnum, .. } => {
                    let live = obj_counts.get(obj_vnum).copied().unwrap_or(0);
                    if live >= *max_count {
                        last_cmd = false;
                        continue;
                    }
                    match self.load_object(*obj_vnum) {
                        Ok(obj_arc) => {
                            if let Some(room) = self.get_room(*room_vnum) {
                                room.write().contents.push(obj_arc.clone());
                                obj_arc.write().in_room = Some(Arc::downgrade(&room));
                                *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                                summary.objs_spawned += 1;
                                last_obj = Some(obj_arc);
                                last_cmd = true;
                            } else {
                                self.remove_object(obj_arc.read().id);
                                last_cmd = false;
                            }
                        }
                        Err(_) => last_cmd = false,
                    }
                }
                ResetCmd::GiveObjToMob { obj_vnum, max_count, .. } => {
                    let live = obj_counts.get(obj_vnum).copied().unwrap_or(0);
                    if live >= *max_count || last_mob.is_none() {
                        last_cmd = false;
                        continue;
                    }
                    match self.load_object(*obj_vnum) {
                        Ok(obj_arc) => {
                            if let Some(mob) = &last_mob {
                                obj_arc.write().carried_by = Some(Arc::downgrade(mob));
                                mob.write().carrying.push(obj_arc.clone());
                                *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                                summary.objs_spawned += 1;
                                last_obj = Some(obj_arc);
                                last_cmd = true;
                            }
                        }
                        Err(_) => last_cmd = false,
                    }
                }
                ResetCmd::EquipMob { obj_vnum, max_count, wear_pos, .. } => {
                    let live = obj_counts.get(obj_vnum).copied().unwrap_or(0);
                    if live >= *max_count || last_mob.is_none() || *wear_pos >= NUM_WEARS {
                        last_cmd = false;
                        continue;
                    }
                    match self.load_object(*obj_vnum) {
                        Ok(obj_arc) => {
                            if let Some(mob) = &last_mob {
                                obj_arc.write().worn_by = Some(Arc::downgrade(mob));
                                obj_arc.write().worn_on = Some(*wear_pos);
                                mob.write().equipment[*wear_pos] = Some(obj_arc.clone());
                                *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                                summary.objs_spawned += 1;
                                last_obj = Some(obj_arc);
                                last_cmd = true;
                            }
                        }
                        Err(_) => last_cmd = false,
                    }
                }
                ResetCmd::PutObjInObj { obj_vnum, max_count, container_vnum, .. } => {
                    let live = obj_counts.get(obj_vnum).copied().unwrap_or(0);
                    if live >= *max_count {
                        last_cmd = false;
                        continue;
                    }
                    // Find an existing container instance by vnum.
                    let container = self.objects.values()
                        .find(|o| o.read().item_number == *container_vnum)
                        .cloned();
                    let container = match container {
                        Some(c) => c,
                        None => { last_cmd = false; continue; }
                    };
                    match self.load_object(*obj_vnum) {
                        Ok(obj_arc) => {
                            obj_arc.write().in_obj = Some(Arc::downgrade(&container));
                            container.write().contains.push(obj_arc.clone());
                            *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                            summary.objs_spawned += 1;
                            last_obj = Some(obj_arc);
                            last_cmd = true;
                        }
                        Err(_) => last_cmd = false,
                    }
                }
                ResetCmd::RemoveObj { room_vnum, obj_vnum, .. } => {
                    if let Some(room) = self.get_room(*room_vnum) {
                        let mut room = room.write();
                        let before = room.contents.len();
                        room.contents.retain(|o| o.read().item_number != *obj_vnum);
                        summary.objs_removed += (before - room.contents.len()) as u32;
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::Door { room_vnum, direction, state, .. } => {
                    if let Some(room) = self.get_room(*room_vnum) {
                        if let Some(exit) = room.write().exits.get_mut(*direction).and_then(|e| e.as_mut()) {
                            // exit_info bits: 1=CLOSED, 2=LOCKED (matches CircleMUD EX_*).
                            exit.exit_info = match *state {
                                0 => 0,          // open
                                1 => 1,          // closed
                                2 => 1 | 2,      // closed & locked
                                _ => exit.exit_info,
                            };
                            summary.doors_set += 1;
                            last_cmd = true;
                        } else {
                            last_cmd = false;
                        }
                    } else {
                        last_cmd = false;
                    }
                }
            }
        }

        if let Some(z) = self.zones.iter_mut().find(|z| z.number == zone_number) {
            z.age = 0;
        }

        summary
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResetSummary {
    pub mobs_spawned: u32,
    pub objs_spawned: u32,
    pub objs_removed: u32,
    pub doors_set: u32,
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