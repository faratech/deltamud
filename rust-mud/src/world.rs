// World prototypes (zones, mob/obj protos, reset commands) and the
// zone-reset state machine, ported to the id-indexed GameState. The reset
// logic mirrors CircleMUD reset_zone (db.c) including if_flag chaining.

use crate::character::Character;
use crate::object::Object;
use crate::room::EX_CLOSED;
use crate::state::GameState;
use crate::types::*;

/// A parsed zone reset command (CircleMUD reset_com), each variant carrying
/// only its meaningful args. `if_flag` drives the M→E→G chaining rule.
#[derive(Debug, Clone)]
pub enum ResetCmd {
    LoadMob { if_flag: bool, mob_vnum: MobVnum, max_count: i32, room_vnum: RoomVnum },
    LoadObjInRoom { if_flag: bool, obj_vnum: ObjVnum, max_count: i32, room_vnum: RoomVnum },
    GiveObjToMob { if_flag: bool, obj_vnum: ObjVnum, max_count: i32 },
    EquipMob { if_flag: bool, obj_vnum: ObjVnum, max_count: i32, wear_pos: usize },
    PutObjInObj { if_flag: bool, obj_vnum: ObjVnum, max_count: i32, container_vnum: ObjVnum },
    RemoveObj { if_flag: bool, room_vnum: RoomVnum, obj_vnum: ObjVnum },
    Door { if_flag: bool, room_vnum: RoomVnum, direction: usize, state: i32 },
}

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
    pub armor: i32,
    pub hitroll: i16,
    pub damroll: i16,
    pub damnodice: i32,
    pub damsizedice: i32,
}

#[derive(Debug, Clone)]
pub struct ObjectProto {
    pub vnum: ObjVnum,
    pub name: String,
    pub short_desc: String,
    pub description: String,
    pub obj_type: crate::object::ObjectType,
    pub wear_flags: crate::object::WearFlags,
    pub extra_flags: crate::object::ExtraFlags,
    pub weight: i32,
    pub cost: i32,
    pub rent: i32,
    pub values: [i32; 4],
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResetSummary {
    pub mobs_spawned: u32,
    pub objs_spawned: u32,
    pub objs_removed: u32,
    pub doors_set: u32,
}

impl GameState {
    /// Instantiate a mob prototype into the world (unplaced). Returns its id.
    pub fn load_mobile(&mut self, vnum: MobVnum) -> Option<CharId> {
        let proto = self.mob_protos.get(&vnum)?.clone();
        let mut mob = Character::new_npc(vnum);
        mob.player.name = proto.name.clone();
        mob.player.level = proto.level;
        mob.player.sex = proto.sex;
        // Default NPC ability scores (CircleMUD mobs default to 11/13; exact
        // per-mob stats from the .mob enhanced block land in Batch 5).
        mob.real_abils = crate::character::Abilities {
            str: 13, str_add: 0, intel: 13, wis: 13, dex: 13, con: 13, cha: 13,
        };
        mob.aff_abils = mob.real_abils;
        mob.position = proto.position;
        mob.points.hit = proto.hitpoints.max(1);
        mob.points.max_hit = mob.points.hit;
        mob.points.mana = 100;
        mob.points.max_mana = 100;
        mob.points.move_points = 80;
        mob.points.max_move = 80;
        // CircleMUD stores AC*10; an unarmored mob is AC 10 -> stored 100.
        mob.points.armor = if proto.armor == 0 { 100 } else { proto.armor as ArmorClass };
        mob.points.hitroll = proto.hitroll;
        mob.points.damroll = proto.damroll;
        mob.points.gold = proto.gold;
        mob.points.exp = proto.experience;
        mob.short_desc = Some(proto.short_desc);
        mob.long_desc = Some(proto.long_desc);
        mob.npc_description = Some(proto.description);
        let id = self.create_char(mob);
        crate::dg_db_scripts::assign_triggers(crate::dg_handler::ScriptKey::Mob(id), vnum);
        Some(id)
    }

    /// Instantiate an object prototype into the world (unplaced). Returns id.
    pub fn load_object(&mut self, vnum: ObjVnum) -> Option<ObjId> {
        let proto = self.obj_protos.get(&vnum)?.clone();
        let mut obj = Object::new(vnum, proto.name, proto.short_desc);
        obj.description = proto.description;
        obj.obj_type = proto.obj_type;
        obj.wear_flags = proto.wear_flags;
        obj.extra_flags = proto.extra_flags;
        obj.weight = proto.weight;
        obj.cost = proto.cost;
        obj.rent = proto.rent;
        obj.values = proto.values;
        let id = self.create_obj(obj);
        crate::dg_db_scripts::assign_triggers(crate::dg_handler::ScriptKey::Obj(id), vnum);
        Some(id)
    }

    pub fn count_mobs_by_vnum(&self) -> std::collections::HashMap<MobVnum, i32> {
        let mut counts = std::collections::HashMap::new();
        for ch in self.chars.values() {
            if ch.is_npc {
                *counts.entry(ch.nr).or_insert(0) += 1;
            }
        }
        counts
    }

    pub fn count_objs_by_vnum(&self) -> std::collections::HashMap<ObjVnum, i32> {
        let mut counts = std::collections::HashMap::new();
        for obj in self.objs.values() {
            *counts.entry(obj.item_number).or_insert(0) += 1;
        }
        counts
    }

    /// Run an initial reset of every zone (CircleMUD boot behaviour).
    pub fn prime_zones(&mut self) -> (u32, u32) {
        let zone_numbers: Vec<i32> = self.zones.iter().map(|z| z.number).collect();
        let (mut mobs, mut objs) = (0u32, 0u32);
        for zn in zone_numbers {
            let s = self.reset_zone(zn);
            mobs += s.mobs_spawned;
            objs += s.objs_spawned;
        }
        (mobs, objs)
    }

    /// Execute a zone's reset commands once (CircleMUD reset_zone).
    pub fn reset_zone(&mut self, zone_number: i32) -> ResetSummary {
        let commands: Vec<ResetCmd> = match self.zones.iter().find(|z| z.number == zone_number) {
            Some(z) => z.reset_commands.clone(),
            None => return ResetSummary::default(),
        };

        let mut mob_counts = self.count_mobs_by_vnum();
        let mut obj_counts = self.count_objs_by_vnum();

        let mut last_cmd = false;
        let mut last_mob: Option<CharId> = None;
        let mut last_obj: Option<ObjId> = None;
        let mut summary = ResetSummary::default();

        for cmd in &commands {
            let if_flag = match cmd {
                ResetCmd::LoadMob { if_flag, .. }
                | ResetCmd::LoadObjInRoom { if_flag, .. }
                | ResetCmd::GiveObjToMob { if_flag, .. }
                | ResetCmd::EquipMob { if_flag, .. }
                | ResetCmd::PutObjInObj { if_flag, .. }
                | ResetCmd::RemoveObj { if_flag, .. }
                | ResetCmd::Door { if_flag, .. } => *if_flag,
            };
            if if_flag && !last_cmd {
                continue;
            }
            if !if_flag {
                last_mob = None;
                last_obj = None;
            }

            match cmd {
                ResetCmd::LoadMob { mob_vnum, max_count, room_vnum, .. } => {
                    if mob_counts.get(mob_vnum).copied().unwrap_or(0) >= *max_count {
                        last_cmd = false;
                        continue;
                    }
                    let rnum = match self.real_room(*room_vnum) {
                        Some(r) => r,
                        None => { last_cmd = false; continue; }
                    };
                    if let Some(mob) = self.load_mobile(*mob_vnum) {
                        self.char_to_room(mob, rnum);
                        *mob_counts.entry(*mob_vnum).or_insert(0) += 1;
                        summary.mobs_spawned += 1;
                        last_mob = Some(mob);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::LoadObjInRoom { obj_vnum, max_count, room_vnum, .. } => {
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count {
                        last_cmd = false;
                        continue;
                    }
                    let rnum = match self.real_room(*room_vnum) {
                        Some(r) => r,
                        None => { last_cmd = false; continue; }
                    };
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.obj_to_room(obj, rnum);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::GiveObjToMob { obj_vnum, max_count, .. } => {
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count || last_mob.is_none() {
                        last_cmd = false;
                        continue;
                    }
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.obj_to_char(obj, last_mob.unwrap());
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::EquipMob { obj_vnum, max_count, wear_pos, .. } => {
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count
                        || last_mob.is_none()
                        || *wear_pos >= NUM_WEARS
                    {
                        last_cmd = false;
                        continue;
                    }
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.equip_char(last_mob.unwrap(), obj, *wear_pos);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::PutObjInObj { obj_vnum, max_count, container_vnum, .. } => {
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count {
                        last_cmd = false;
                        continue;
                    }
                    let container = self
                        .objs
                        .iter()
                        .find(|(_, o)| o.item_number == *container_vnum)
                        .map(|(id, _)| *id);
                    let container = match container {
                        Some(c) => c,
                        None => { last_cmd = false; continue; }
                    };
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.obj_to_obj(obj, container);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::RemoveObj { room_vnum, obj_vnum, .. } => {
                    if let Some(rnum) = self.real_room(*room_vnum) {
                        let to_remove: Vec<ObjId> = self.rooms[rnum]
                            .contents
                            .iter()
                            .copied()
                            .filter(|&o| self.objs.get(&o).map(|x| x.item_number) == Some(*obj_vnum))
                            .collect();
                        for o in to_remove {
                            self.obj_from_anywhere(o);
                            self.extract_obj(o);
                            summary.objs_removed += 1;
                        }
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::Door { room_vnum, direction, state, .. } => {
                    if let Some(rnum) = self.real_room(*room_vnum) {
                        if let Some(exit) =
                            self.rooms[rnum].exits.get_mut(*direction).and_then(|e| e.as_mut())
                        {
                            exit.exit_info = match *state {
                                0 => 0,
                                1 => EX_CLOSED,
                                2 => EX_CLOSED | crate::room::EX_LOCKED,
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
        let _ = last_obj;
        summary
    }
}
