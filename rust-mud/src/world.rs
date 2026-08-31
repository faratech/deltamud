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
    // `load_chance` is DeltaMUD's per-command probability gate (reset_zone:
    // `number(1,100) >= arg4`, or arg3 for G). 0 => always loads (legacy zones).
    LoadMob {
        if_flag: bool,
        mob_vnum: MobVnum,
        max_count: i32,
        room_vnum: RoomVnum,
        load_chance: i32,
    },
    LoadObjInRoom {
        if_flag: bool,
        obj_vnum: ObjVnum,
        max_count: i32,
        room_vnum: RoomVnum,
        load_chance: i32,
    },
    GiveObjToMob {
        if_flag: bool,
        obj_vnum: ObjVnum,
        max_count: i32,
        load_chance: i32,
    },
    EquipMob {
        if_flag: bool,
        obj_vnum: ObjVnum,
        max_count: i32,
        wear_pos: usize,
        load_chance: i32,
    },
    PutObjInObj {
        if_flag: bool,
        obj_vnum: ObjVnum,
        max_count: i32,
        container_vnum: ObjVnum,
        load_chance: i32,
    },
    RemoveObj {
        if_flag: bool,
        room_vnum: RoomVnum,
        obj_vnum: ObjVnum,
    },
    Door {
        if_flag: bool,
        room_vnum: RoomVnum,
        direction: usize,
        state: i32,
    },
}

/// C db.c:1873 `#define ZO_DEAD 999` - a queued zone's age marker.
pub const ZONE_DEAD: i32 = 999;

#[derive(Debug, Clone)]
pub struct Zone {
    pub number: i32,
    pub name: String,
    /// Builder credit line (DeltaMUD `Z.builders`, second tilde-string).
    pub builders: String,
    pub lifespan: i32,
    pub age: i32,
    pub top: RoomVnum,
    pub reset_mode: i32,
    pub min_level: Level,
    pub max_level: Level,
    /// DeltaMUD zone-status flag from the optional `lvl1 lvl2 status_mode` line.
    pub status_mode: i32,
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
    /// C db.c stores the HP dice parts in the proto (hit=nodice, mana=
    /// sizedice, move=bonus) and read_mobile rolls max_hit = dice(nd, sd) +
    /// bonus (db.c:1790-1798). Loader-parsed protos carry the parsed triple;
    /// constructors that set hitpoints directly use (0, 0, hitpoints) so the
    /// roll degenerates to hitpoints. (issue #230)
    pub hit_dice: (i32, i32, i32),
    pub experience: Experience,
    pub gold: Gold,
    pub position: Position,
    pub default_pos: Position,
    pub sex: Gender,
    pub alignment: i32,
    /// MOB action flags (ACT_*/MOB_*) from file field 1, with MOB_ISNPC set.
    pub act_flags: i64,
    /// MOB affect flags (AFF_*) from file field 2.
    pub affect_flags: i64,
    pub armor: i32,
    pub hitroll: i16,
    pub damroll: i16,
    pub damnodice: i32,
    pub damsizedice: i32,
    // DeltaMUD extended combat stats (the `X` stats-line variant).
    pub power: i16,
    pub mpower: i16,
    pub defense: i16,
    pub mdefense: i16,
    pub technique: i16,
    // Espec (enhanced 'E'-format) ability scores; `None` => use NPC defaults.
    pub abilities: Option<crate::character::Abilities>,
    // BareHandAttack from the espec block (mob_specials.attack_type).
    pub attack_type: i32,
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
    pub curr_slots: i32,
    pub total_slots: i32,
    pub obj_class: i32,
    pub min_level: i32,
    pub bitvector: i64,
    /// The 4th tilde-string (action_description). The loader reads it and
    /// oedit_save_to_disk writes it back, so it must be carried on the proto or
    /// it is silently stripped from disk on any zone save.
    pub action_description: String,
    /// Stat applies (`A` blocks): location/modifier pairs, up to MAX_OBJ_AFFECT.
    pub affects: Vec<crate::object::ObjectAffect>,
    /// Extra descriptions (`E` blocks): (keyword, description).
    pub ex_descriptions: Vec<(String, String)>,
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
        // Espec (enhanced 'E'-format) abilities override the NPC defaults
        // (CircleMUD mobs otherwise default to 11/13). C applies these in
        // interpret_espec then copies real_abils -> aff_abils.
        mob.real_abils = proto.abilities.unwrap_or(crate::character::Abilities {
            str: 13,
            str_add: 0,
            intel: 13,
            wis: 13,
            dex: 13,
            con: 13,
            cha: 13,
        });
        mob.aff_abils = mob.real_abils;
        mob.alignment = proto.alignment;
        // Action + affect flags from the .mob file (MOB_ISNPC already set by the
        // loader). Drives mobact (SPEC/SENTINEL/SCAVENGER/AGGRESSIVE/HELPER) and
        // mob AFF_* state, none of which fired while these were left at 0.
        mob.act_flags = proto.act_flags;
        mob.affect_flags = proto.affect_flags;
        mob.position = proto.position;
        // C read_mobile (db.c:1790-1798): max_hit is rolled from the proto's
        // HP dice, max_hit = dice(nodice, sizedice) + bonus, then hit is set
        // to max_hit. Loader protos carry the parsed 'Hd+H' triple (#230).
        let (hp_nd, hp_sd, hp_bonus) = proto.hit_dice;
        let max_hit = self.rng.dice(hp_nd, hp_sd) + hp_bonus;
        mob.points.hit = max_hit;
        mob.points.max_hit = max_hit;
        mob.points.mana = 100;
        mob.points.max_mana = 100;
        mob.points.move_points = 80;
        mob.points.max_move = 80;
        // CircleMUD stores AC*10; an unarmored mob is AC 10 -> stored 100.
        mob.points.armor = if proto.armor == 0 {
            100
        } else {
            proto.armor as ArmorClass
        };
        mob.points.hitroll = proto.hitroll;
        mob.points.damroll = proto.damroll;
        // DeltaMUD extended combat stats (the `X` stats-line variant).
        mob.points.power = proto.power;
        mob.points.mpower = proto.mpower;
        mob.points.defense = proto.defense;
        mob.points.mdefense = proto.mdefense;
        mob.points.technique = proto.technique;
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
        obj.curr_slots = proto.curr_slots;
        obj.total_slots = proto.total_slots;
        obj.obj_class = proto.obj_class;
        obj.min_level = proto.min_level;
        obj.level = proto.min_level as Level;
        obj.bitvector = proto.bitvector;
        obj.affects = proto.affects.clone();
        obj.ex_descriptions = proto.ex_descriptions.clone();
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

        // Mirror db.c reset_zone's persistent state. `last_mob`/`last_obj`
        // are the C `mob`/`obj` pointers — they persist across iterations and
        // are only cleared by the 'R' command (obj=NULL); they are NOT reset on
        // a non-conditional command. The separate `mob_load`/`obj_load` booleans
        // ARE reset on a non-conditional command and gate `if_flag` chaining.
        let mut last_cmd = false;
        let mut last_mob: Option<CharId> = None;
        let mut last_obj: Option<ObjId> = None;
        let mut mob_load = false;
        let mut obj_load = false;
        // DeltaMUD: objects loaded by a reset of a non-savable ("default") zone
        // get ITEM_NORENT set (db.c ~2132: `if (!zone_table[zone].status_mode && obj)`).
        let no_rent_zone = self
            .zones
            .iter()
            .find(|z| z.number == zone_number)
            .map(|z| z.status_mode == 0)
            .unwrap_or(false);
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
            // C: `if (ZCMD.if_flag && !last_cmd && !mob_load && !obj_load) continue;`
            if if_flag && !last_cmd && !mob_load && !obj_load {
                continue;
            }
            // C: `if (!ZCMD.if_flag) { mob_load = FALSE; obj_load = FALSE; }`
            if !if_flag {
                mob_load = false;
                obj_load = false;
            }

            match cmd {
                ResetCmd::LoadMob {
                    mob_vnum,
                    max_count,
                    room_vnum,
                    load_chance,
                    ..
                } => {
                    if mob_counts.get(mob_vnum).copied().unwrap_or(0) >= *max_count
                        || self.rng.number(1, 100) < *load_chance
                    {
                        last_cmd = false;
                        continue;
                    }
                    let rnum = match self.real_room(*room_vnum) {
                        Some(r) => r,
                        None => {
                            last_cmd = false;
                            continue;
                        }
                    };
                    if let Some(mob) = self.load_mobile(*mob_vnum) {
                        self.char_to_room(mob, rnum);
                        crate::dg_triggers::load_mtrigger(self, mob);
                        *mob_counts.entry(*mob_vnum).or_insert(0) += 1;
                        summary.mobs_spawned += 1;
                        last_mob = Some(mob);
                        last_cmd = true;
                        mob_load = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::LoadObjInRoom {
                    obj_vnum,
                    max_count,
                    room_vnum,
                    load_chance,
                    ..
                } => {
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count
                        || self.rng.number(1, 100) < *load_chance
                    {
                        last_cmd = false;
                        continue;
                    }
                    // C db.c:2012-2030: a NEGATIVE room vnum creates the
                    // object unplaced (in_room = NOWHERE) and still counts as
                    // a successful command (#236).
                    let rnum = self.real_room(*room_vnum);
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        match rnum {
                            Some(r) => self.obj_to_room(obj, r),
                            None => {
                                self.get_obj_mut(obj).unwrap().loc = crate::object::ObjLoc::Nowhere;
                            }
                        }
                        crate::dg_triggers::load_otrigger(self, obj);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                        obj_load = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::GiveObjToMob {
                    obj_vnum,
                    max_count,
                    load_chance,
                    ..
                } => {
                    // C 'G': gated on mob_load (not just a live mob pointer).
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count
                        || last_mob.is_none()
                        || !mob_load
                        || self.rng.number(1, 100) < *load_chance
                    {
                        last_cmd = false;
                        continue;
                    }
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.obj_to_char(obj, last_mob.unwrap());
                        crate::dg_triggers::load_otrigger(self, obj);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::EquipMob {
                    obj_vnum,
                    max_count,
                    wear_pos,
                    load_chance,
                    ..
                } => {
                    // C 'E': gated on mob_load (not just a live mob pointer).
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count
                        || last_mob.is_none()
                        || !mob_load
                        || self.rng.number(1, 100) < *load_chance
                        || *wear_pos >= NUM_WEARS
                    {
                        last_cmd = false;
                        continue;
                    }
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.equip_char(last_mob.unwrap(), obj, *wear_pos);
                        crate::dg_triggers::load_otrigger(self, obj);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::PutObjInObj {
                    obj_vnum,
                    max_count,
                    container_vnum,
                    load_chance,
                    ..
                } => {
                    // C 'P': gated on obj_load (a prior 'O' must have loaded).
                    if obj_counts.get(obj_vnum).copied().unwrap_or(0) >= *max_count
                        || !obj_load
                        || self.rng.number(1, 100) < *load_chance
                    {
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
                        None => {
                            last_cmd = false;
                            continue;
                        }
                    };
                    if let Some(obj) = self.load_object(*obj_vnum) {
                        self.obj_to_obj(obj, container);
                        crate::dg_triggers::load_otrigger(self, obj);
                        *obj_counts.entry(*obj_vnum).or_insert(0) += 1;
                        summary.objs_spawned += 1;
                        last_obj = Some(obj);
                        last_cmd = true;
                    } else {
                        last_cmd = false;
                    }
                }
                ResetCmd::RemoveObj {
                    room_vnum,
                    obj_vnum,
                    ..
                } => {
                    // C 'R' (db.c ~2084): get_obj_in_list_num returns only the
                    // FIRST matching object in the room; extract it once. Always
                    // sets last_cmd=1 (even if no match). On a match, C sets obj=NULL,
                    // so the trailing NO_RENT bit does NOT fire this iteration.
                    // C db.c:2084-2092: last_cmd = 1 unconditionally (the
                    // fall-through sits outside the range check) - altering
                    // it changed if_flag chaining for malformed tables (#235).
                    if let Some(rnum) = self.real_room(*room_vnum) {
                        let found =
                            self.rooms[rnum].contents.iter().copied().find(|&o| {
                                self.objs.get(&o).map(|x| x.item_number) == Some(*obj_vnum)
                            });
                        if let Some(o) = found {
                            self.obj_from_anywhere(o);
                            self.extract_obj(o);
                            summary.objs_removed += 1;
                            last_obj = None;
                        }
                    }
                    last_cmd = true;
                }
                ResetCmd::Door {
                    room_vnum,
                    direction,
                    state,
                    ..
                } => {
                    // C 'D' (db.c ~2095): manipulate ONLY the EX_CLOSED/EX_LOCKED
                    // bits via REMOVE_BIT/SET_BIT, preserving EX_ISDOOR/EX_PICKPROOF/
                    // EX_HIDDEN. state 0 = open (clear CLOSED+LOCKED), 1 = closed
                    // (set CLOSED, clear LOCKED), 2 = closed+locked (set both). Any
                    // other state leaves the bits unchanged.
                    if let Some(rnum) = self.real_room(*room_vnum) {
                        if let Some(exit) = self.rooms[rnum]
                            .exits
                            .get_mut(*direction)
                            .and_then(|e| e.as_mut())
                        {
                            match *state {
                                0 => {
                                    exit.exit_info &= !crate::room::EX_LOCKED;
                                    exit.exit_info &= !EX_CLOSED;
                                }
                                1 => {
                                    exit.exit_info |= EX_CLOSED;
                                    exit.exit_info &= !crate::room::EX_LOCKED;
                                }
                                2 => {
                                    exit.exit_info |= crate::room::EX_LOCKED;
                                    exit.exit_info |= EX_CLOSED;
                                }
                                _ => {}
                            }
                            summary.doors_set += 1;
                        }
                        // C db.c:2095-2125: 'D' sets last_cmd = 1 AFTER the
                        // else, overwriting the ZONE_ERROR 0 - unconditional
                        // for missing rooms and exits alike (#235).
                        last_cmd = true;
                    }
                }
            }

            // C (db.c ~2132, after the switch, every iteration):
            //   if (!zone_table[zone].status_mode && obj)
            //     SET_BIT(obj->obj_flags.extra_flags, ITEM_NORENT);
            // `obj`/`last_obj` is the most-recently-loaded object pointer, which
            // persists across iterations and is cleared only by the 'R' command.
            if no_rent_zone {
                if let Some(oid) = last_obj {
                    if let Some(o) = self.objs.get_mut(&oid) {
                        o.extra_flags |= crate::object::ExtraFlags::NO_RENT;
                    }
                }
            }
        }

        if let Some(z) = self.zones.iter_mut().find(|z| z.number == zone_number) {
            z.age = 0;
        }

        // C db.c:2137-2142: reset_zone finishes by walking every room vnum in
        // the zone and firing reset_wtrigger - WTRIG_RESET scripts re-seal
        // doors and restore one-shot state on every repop (#144).
        if let Some(z) = self.zones.iter().find(|z| z.number == zone_number) {
            let start = z.number * 100;
            for vnum in start..=z.top {
                if let Some(rnum) = self.real_room(vnum) {
                    crate::dg_triggers::reset_wtrigger(self, rnum);
                }
            }
        }
        summary
    }
}

#[cfg(test)]
mod hit_dice_tests {
    use super::*;
    use crate::config::Config;

    fn bare_proto(hit_dice: (i32, i32, i32)) -> MobileProto {
        MobileProto {
            vnum: 2000,
            name: "test mob".into(),
            short_desc: "the test mob".into(),
            long_desc: "the test mob stands here.".into(),
            description: String::new(),
            level: 10,
            hitpoints: hit_dice.0,
            hit_dice,
            experience: 0,
            gold: 0,
            position: Position::Standing,
            default_pos: Position::Standing,
            sex: Gender::Male,
            alignment: 0,
            act_flags: 0,
            affect_flags: 0,
            armor: 100,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 2,
            power: 0,
            mpower: 0,
            defense: 0,
            mdefense: 0,
            technique: 0,
            abilities: None,
            attack_type: 0,
        }
    }

    #[test]
    fn mob_max_hit_is_rolled_from_hp_dice_not_the_dice_count() {
        // Issue #230 / C db.c:1790-1798: '1d1+665' must roll
        // dice(1, 1) + 665 = 666, not collapse to the dice count (1).
        let mut g = GameState::new(Config::default());
        g.rng.srandom(12345);
        let mut proto = bare_proto((1, 1, 665));
        proto.vnum = 2000;
        g.mob_protos.insert(2000, proto);
        let cid = g.load_mobile(2000).expect("mob loads");
        let mob = g.get_char(cid).unwrap();
        assert_eq!(mob.points.max_hit, 666);
        assert_eq!(mob.points.hit, 666);
    }

    #[test]
    fn classic_format_hp_dice_roll_nd_sd_plus_bonus() {
        // Classic 'Hd+H' e.g. 2d10+50 -> dice(2, 10) + 50.
        let mut g = GameState::new(Config::default());
        g.rng.srandom(999);
        let mut proto = bare_proto((2, 10, 50));
        proto.vnum = 2001;
        g.mob_protos.insert(2001, proto);
        let cid = g.load_mobile(2001).unwrap();
        let mob = g.get_char(cid).unwrap();
        let expected = {
            let mut r = crate::rng::Rng::new(999);
            r.dice(2, 10) + 50
        };
        assert_eq!(mob.points.max_hit, expected);
        assert!(mob.points.max_hit >= 52 && mob.points.max_hit <= 70);
    }
}
