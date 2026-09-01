// dg_scripts.rs — the DG-Scripts virtual machine (port of dg_scripts.c).
//
// This is the script_driver core: it walks a trigger's command list, handling
// if/elseif/else/end, while/done, switch/case/default/break, the set/eval/
// unset/global/remote/context/extract/makeuid/return/wait/halt directives, the
// `%var%` and `%var.field(subfield)%` substitution, and C's idiosyncratic
// left-to-right operator evaluation (eval_lhs_op_rhs). Everything else (the
// per-event fire hooks, e.g. greet_mtrigger / speech_mtrigger / command_*) is
// the public surface other modules call to drive scripts.
//
// Object identity: C uses GET_ID(go) — a unique long per char/obj, plus
// ROOM_ID_BASE+rnum for rooms. We reproduce that numbering in `id_of` so UID
// strings round-trip through find_char/find_obj/find_room exactly. Char ids
// are CharId.0 (offset into MOBOBJ range), obj ids likewise; rooms use
// ROOM_ID_BASE+rnum.
//
// Borrow discipline: the trigger arena + script tables live in dg_handler
// behind mutexes; the VM pulls a *clone* of the running trigger's cmdlist and
// reads vars by id, never holding a lock across a command_interpreter call.

use crate::dg_event::{WaitEvent, add_event};
use crate::dg_handler::{
    self, MAX_SCRIPT_DEPTH, MOB_TRIGGER, MTRIG_GLOBAL, MTRIG_RANDOM, OBJ_TRIGGER, OTRIG_RANDOM,
    ROOM_ID_BASE, ScriptKey, TRIG_NEW, TRIG_RESTART, TrigId, WLD_TRIGGER, WTRIG_GLOBAL,
    WTRIG_RANDOM, add_var_in, remove_var_in,
};
use crate::object::{ObjLoc, ObjectGraphOrder, walk_object_graph};
use crate::state::GameState;
use crate::types::*;
use std::cell::Cell;

pub const UID_CHAR: char = '\x1b';
const MAX_DEPTH_RECURSE: i32 = MAX_SCRIPT_DEPTH;

thread_local! {
    // C static `depth` in script_driver, and dg_owner_purged.
    static SCRIPT_DEPTH: Cell<i32> = const { Cell::new(0) };
    static OWNER_PURGED: Cell<bool> = const { Cell::new(false) };
}

// ---------------------------------------------------------------------------
// GoRef — the "void *go" the trigger runs on, with its trig_type.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoRef {
    Mob(CharId),
    Obj(ObjId),
    Room(RoomRnum),
}

impl GoRef {
    pub fn key(self) -> ScriptKey {
        match self {
            GoRef::Mob(c) => ScriptKey::Mob(c),
            GoRef::Obj(o) => ScriptKey::Obj(o),
            GoRef::Room(r) => ScriptKey::Room(r),
        }
    }
    pub fn trig_type(self) -> i32 {
        match self {
            GoRef::Mob(_) => MOB_TRIGGER,
            GoRef::Obj(_) => OBJ_TRIGGER,
            GoRef::Room(_) => WLD_TRIGGER,
        }
    }
    fn exists(self, g: &GameState) -> bool {
        match self {
            GoRef::Mob(c) => g.char_exists(c),
            GoRef::Obj(o) => g.get_obj(o).is_some(),
            GoRef::Room(r) => g.rooms.get(r).is_some(),
        }
    }
}

// GET_ID(go): the DG UID number for a char/obj/room. These MUST match the
// encoding the sibling dg_mobcmd / dg_objcmd / dg_wldcmd modules decode:
//   * char  UID = raw CharId.0          (decoded `CharId(id)`)
//   * obj   UID = raw ObjId.0           (decoded `ObjId(id)`)
//   * room  UID = ROOM_ID_BASE + vnum   (decoded via real_room(id-base))
// (MOBOBJ_ID_BASE is retained only for the dg_scripts.h constant surface.)
fn char_id(c: CharId) -> i64 {
    c.0 as i64
}
fn obj_id(o: ObjId) -> i64 {
    o.0 as i64
}
#[allow(dead_code)]
fn room_id(g: &GameState, r: RoomRnum) -> i64 {
    ROOM_ID_BASE + g.rooms.get(r).map(|room| room.number as i64).unwrap_or(0)
}

/// script_log: SCRIPT ERR: msg (mudlog NRM LVL_GOD).
pub fn script_log(msg: &str) {
    log::warn!("SCRIPT ERR: {}", msg);
}

// ---------------------------------------------------------------------------
// find_char / find_obj / find_room by UID number (search-by-number routines).
// ---------------------------------------------------------------------------
fn find_char_by_uid(g: &GameState, n: i64) -> Option<CharId> {
    if n <= 0 {
        return None;
    }
    let cid = CharId(n as u64);
    g.get_char(cid).map(|_| cid)
}
fn find_obj_by_uid(g: &GameState, n: i64) -> Option<ObjId> {
    if n <= 0 {
        return None;
    }
    let oid = ObjId(n as u64);
    g.get_obj(oid).map(|_| oid)
}
fn find_room_by_uid(g: &GameState, n: i64) -> Option<RoomRnum> {
    // Room UID = ROOM_ID_BASE + vnum (matches dg_wldcmd/dg_objcmd). Tolerate a
    // bare vnum encoding too.
    let vnum: RoomVnum = if n >= ROOM_ID_BASE {
        (n - ROOM_ID_BASE) as RoomVnum
    } else {
        n as RoomVnum
    };
    g.real_room(vnum)
}

// ---- generic name searches (get_char / get_obj / get_room by name) --------

fn parse_uid(name: &str) -> Option<i64> {
    let mut chars = name.chars();
    if chars.next() == Some(UID_CHAR) {
        chars.as_str().trim().parse::<i64>().ok()
    } else {
        None
    }
}

/// get_char(name): UID or world-wide name (visible). For DG we ignore
/// invis-level nuance (mobs have no invis); name match is isname.
fn get_char(g: &GameState, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        return find_char_by_uid(g, id);
    }
    g.chars.keys().copied().find(|&c| {
        g.get_char(c)
            .map(|x| crate::handler::isname(name, &x.player.name))
            .unwrap_or(false)
    })
}

fn get_obj(g: &GameState, name: &str) -> Option<ObjId> {
    if let Some(id) = parse_uid(name) {
        return find_obj_by_uid(g, id);
    }
    g.objs.keys().copied().find(|&o| {
        g.get_obj(o)
            .map(|x| crate::handler::isname(name, &x.name))
            .unwrap_or(false)
    })
}

/// get_room(name): UID, else a room vnum string.
fn get_room(g: &GameState, name: &str) -> Option<RoomRnum> {
    if let Some(id) = parse_uid(name) {
        return find_room_by_uid(g, id);
    }
    let vnum: RoomVnum = name.trim().parse().ok()?;
    g.real_room(vnum)
}

fn get_char_room(g: &GameState, name: &str, rnum: RoomRnum) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        return find_char_by_uid(g, id);
    }
    let room = g.rooms.get(rnum)?;
    room.people.iter().copied().find(|&c| {
        g.get_char(c)
            .map(|x| crate::handler::isname(name, &x.player.name))
            .unwrap_or(false)
    })
}

fn get_obj_in_list(g: &GameState, name: &str, list: &[ObjId]) -> Option<ObjId> {
    if let Some(id) = parse_uid(name) {
        return list.iter().copied().find(|&o| obj_id(o) == id);
    }
    list.iter().copied().find(|&o| {
        g.get_obj(o)
            .map(|x| crate::handler::isname(name, &x.name))
            .unwrap_or(false)
    })
}

fn get_object_in_equip(g: &GameState, ch: CharId, name: &str) -> Option<ObjId> {
    let c = g.get_char(ch)?;
    if let Some(id) = parse_uid(name) {
        return c
            .equipment
            .iter()
            .flatten()
            .copied()
            .find(|&o| obj_id(o) == id);
    }
    let (_n, kw) = crate::handler::get_number(name);
    c.equipment.iter().flatten().copied().find(|&o| {
        g.get_obj(o)
            .map(|x| crate::handler::isname(&kw, &x.name))
            .unwrap_or(false)
    })
}

fn obj_room(g: &GameState, o: ObjId) -> Option<RoomRnum> {
    let walk = walk_object_graph([o], ObjectGraphOrder::Preorder, "DG obj_room", |id| {
        g.get_obj(id).map(|object| match object.loc {
            ObjLoc::Contained(parent) => vec![parent],
            _ => Vec::new(),
        })
    });
    for visit in walk.visits {
        match g.get_obj(visit.id).map(|object| object.loc) {
            Some(ObjLoc::Room(room)) => return Some(room),
            Some(ObjLoc::Carried(ch) | ObjLoc::Worn(ch, _)) => {
                return g.get_char(ch).and_then(|character| character.in_room);
            }
            Some(ObjLoc::Contained(_)) => {}
            Some(ObjLoc::Nowhere) | None => return None,
        }
    }
    None
}

/// trgvar_in_room(vnum): number of people in room vnum, or -1 if no such room.
fn trgvar_in_room(g: &GameState, vnum: i32) -> i32 {
    match g.real_room(vnum) {
        Some(r) => g.rooms[r].people.len() as i32,
        None => {
            script_log("people.vnum: world[vnum] does not exist");
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Variable resolution. ResolvedTarget is what a var's value points at.
// ---------------------------------------------------------------------------
enum Resolved {
    Char(CharId),
    Obj(ObjId),
    Room(RoomRnum),
    None,
}

/// is_num(num): all digits (with optional leading '-'), trailing space ok.
fn is_num(s: &str) -> bool {
    let s = s.trim_end();
    if s.is_empty() {
        return true; // C returns 1 for empty (loop never sets non-digit)
    }
    let mut seen = false;
    for c in s.chars() {
        if c.is_ascii_digit() || c == '-' {
            seen = true;
        } else if c.is_whitespace() {
            break;
        } else {
            return false;
        }
    }
    seen || s.is_empty()
}

/// str_str: case-insensitive substring search (returns whether ct is in cs).
fn str_str(cs: &str, ct: &str) -> bool {
    if ct.is_empty() {
        return true;
    }
    cs.to_lowercase().contains(&ct.to_lowercase())
}

/// text_processed: strlen/trim/car/cdr pseudo-fields applied to a var's value.
fn text_processed(field: &str, value: &str) -> Option<String> {
    match field.to_lowercase().as_str() {
        "strlen" => Some(value.len().to_string()),
        "trim" => Some(value.trim().to_string()),
        "car" => Some(value.split_whitespace().next().unwrap_or("").to_string()),
        "cdr" => {
            let v = value.trim_start();
            match v.find(char::is_whitespace) {
                Some(p) => Some(v[p..].trim_start().to_string()),
                None => Some(String::new()),
            }
        }
        _ => None,
    }
}

fn class_name(c: Class) -> &'static str {
    match c {
        Class::MagicUser => "Magic User",
        Class::Cleric => "Cleric",
        Class::Thief => "Thief",
        Class::Warrior => "Warrior",
        Class::Artisan => "Artisan",
    }
}
fn race_name(r: Race) -> &'static str {
    match r {
        Race::Human => "Human",
        Race::Elf => "Elf",
        Race::Gnome => "Gnome",
        Race::Dwarf => "Dwarf",
        Race::Troll => "Troll",
        Race::Goblin => "Goblin",
        Race::Drow => "Drow",
        Race::Orc => "Orc",
        Race::Minotaur => "Minotaur",
        Race::HalfElf => "Half Elf",
        Race::Kender => "Kender",
        Race::Vampire => "Vampire",
        Race::Ogre => "Ogre",
        Race::HalfOrc => "Half Orc",
    }
}
fn gender_name(s: Gender) -> &'static str {
    match s {
        Gender::Neutral => "Neutral",
        Gender::Male => "Male",
        Gender::Female => "Female",
    }
}

/// find_replacement: sets the value of var.field(subfield) for the given trig.
fn find_replacement(
    g: &GameState,
    go: GoRef,
    trig: TrigId,
    var: &str,
    field: &str,
    subfield: &str,
) -> String {
    let key = go.key();

    // Look up var in trigger-local vars, then script globals (context-honoured).
    let local_val =
        dg_handler::with_trig(trig, |t| t.get_var(var).map(|v| v.value.clone())).flatten();
    let var_value = local_val.or_else(|| dg_handler::get_global_var(key, var));

    // No field: return the var's plain value, or "self"/"" specials.
    if field.is_empty() {
        return match var_value {
            Some(v) => v,
            None => {
                if var.eq_ignore_ascii_case("self") {
                    "self".to_string()
                } else {
                    String::new()
                }
            }
        };
    }

    // Resolve what the var points at.
    let resolved: Resolved = if let Some(name) = &var_value {
        resolve_named(g, go, name)
    } else if var.eq_ignore_ascii_case("self") {
        match go {
            GoRef::Mob(c) => Resolved::Char(c),
            GoRef::Obj(o) => Resolved::Obj(o),
            GoRef::Room(r) => Resolved::Room(r),
        }
    } else if var.eq_ignore_ascii_case("people") {
        let num = match crate::text::parse_i32_strict(field) {
            Ok(value) => value,
            Err(crate::text::ParseIntError::Overflow) => {
                script_log("people index outside supported 32-bit range");
                0
            }
            Err(_) => 0,
        };
        return if num > 0 {
            trgvar_in_room(g, num).to_string()
        } else {
            "0".to_string()
        };
    } else if var.eq_ignore_ascii_case("random") {
        return resolve_random(g, go, field);
    } else {
        Resolved::None
    };

    match resolved {
        Resolved::Char(c) => {
            resolve_char_field(g, go, trig, c, field, subfield, var_value.as_deref())
        }
        Resolved::Obj(o) => resolve_obj_field(g, trig, o, field, var_value.as_deref()),
        Resolved::Room(r) => resolve_room_field(g, trig, r, field, var_value.as_deref()),
        Resolved::None => String::new(),
    }
}

/// resolve a var's stored name into a char/obj/room, search order per trig type.
fn resolve_named(g: &GameState, go: GoRef, name: &str) -> Resolved {
    match go {
        GoRef::Mob(ch) => {
            if let Some(o) = get_object_in_equip(g, ch, name) {
                return Resolved::Obj(o);
            }
            let carry = g
                .get_char(ch)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            if let Some(o) = get_obj_in_list(g, name, &carry) {
                return Resolved::Obj(o);
            }
            if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
                if let Some(c) = get_char_room(g, name, rnum) {
                    return Resolved::Char(c);
                }
                let contents = g.rooms[rnum].contents.clone();
                if let Some(o) = get_obj_in_list(g, name, &contents) {
                    return Resolved::Obj(o);
                }
            }
            if let Some(c) = get_char(g, name) {
                return Resolved::Char(c);
            }
            if let Some(o) = get_obj(g, name) {
                return Resolved::Obj(o);
            }
            if let Some(r) = get_room(g, name) {
                return Resolved::Room(r);
            }
            Resolved::None
        }
        GoRef::Obj(obj) => {
            if let Some(c) = get_char_by_obj(g, obj, name) {
                return Resolved::Char(c);
            }
            if let Some(o) = get_obj_by_obj(g, obj, name) {
                return Resolved::Obj(o);
            }
            if let Some(r) = get_room(g, name) {
                return Resolved::Room(r);
            }
            Resolved::None
        }
        GoRef::Room(room) => {
            if let Some(c) = get_char_by_room(g, room, name) {
                return Resolved::Char(c);
            }
            if let Some(o) = get_obj_by_room(g, room, name) {
                return Resolved::Obj(o);
            }
            if let Some(r) = get_room(g, name) {
                return Resolved::Room(r);
            }
            Resolved::None
        }
    }
}

fn get_char_by_obj(g: &GameState, obj: ObjId, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        return find_char_by_uid(g, id);
    }
    if let Some(o) = g.get_obj(obj) {
        match o.loc {
            ObjLoc::Carried(c) | ObjLoc::Worn(c, _) => {
                if g.get_char(c)
                    .map(|x| crate::handler::isname(name, &x.player.name))
                    .unwrap_or(false)
                {
                    return Some(c);
                }
            }
            _ => {}
        }
    }
    get_char(g, name)
}

fn get_obj_by_obj(g: &GameState, obj: ObjId, name: &str) -> Option<ObjId> {
    if name.eq_ignore_ascii_case("self") || name.eq_ignore_ascii_case("me") {
        return Some(obj);
    }
    let o = g.get_obj(obj)?;
    if let Some(found) = get_obj_in_list(g, name, &o.contains) {
        return Some(found);
    }
    match o.loc {
        ObjLoc::Contained(parent) => {
            if let Some(id) = parse_uid(name) {
                if obj_id(parent) == id {
                    return Some(parent);
                }
            } else if g
                .get_obj(parent)
                .map(|p| crate::handler::isname(name, &p.name))
                .unwrap_or(false)
            {
                return Some(parent);
            }
        }
        ObjLoc::Worn(c, _) => {
            if let Some(found) = get_object_in_equip(g, c, name) {
                return Some(found);
            }
        }
        ObjLoc::Carried(c) => {
            let carry = g
                .get_char(c)
                .map(|x| x.carrying.clone())
                .unwrap_or_default();
            if let Some(found) = get_obj_in_list(g, name, &carry) {
                return Some(found);
            }
        }
        ObjLoc::Room(r) => {
            let contents = g.rooms[r].contents.clone();
            if let Some(found) = get_obj_in_list(g, name, &contents) {
                return Some(found);
            }
        }
        ObjLoc::Nowhere => {}
    }
    get_obj(g, name)
}

fn get_char_by_room(g: &GameState, room: RoomRnum, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        return find_char_by_uid(g, id);
    }
    if let Some(r) = g.rooms.get(room) {
        if let Some(c) = r.people.iter().copied().find(|&c| {
            g.get_char(c)
                .map(|x| crate::handler::isname(name, &x.player.name))
                .unwrap_or(false)
        }) {
            return Some(c);
        }
    }
    get_char(g, name)
}

fn get_obj_by_room(g: &GameState, room: RoomRnum, name: &str) -> Option<ObjId> {
    if let Some(r) = g.rooms.get(room) {
        let contents = r.contents.clone();
        if let Some(o) = get_obj_in_list(g, name, &contents) {
            return Some(o);
        }
    }
    get_obj(g, name)
}

/// %random.char% / %random.NUM% / %random.letter%
fn resolve_random(g: &GameState, go: GoRef, field: &str) -> String {
    if field.eq_ignore_ascii_case("char") {
        // pick a random person in the relevant room (excluding self for mobs).
        let (rnum, exclude) = match go {
            GoRef::Mob(c) => (g.get_char(c).and_then(|x| x.in_room), Some(c)),
            GoRef::Obj(o) => (obj_room(g, o), None),
            GoRef::Room(r) => (Some(r), None),
        };
        let Some(rnum) = rnum else {
            return String::new();
        };
        let people: Vec<CharId> = g
            .rooms
            .get(rnum)
            .map(|r| r.people.clone())
            .unwrap_or_default();
        // number(0,count) reservoir sampling, matching C exactly via rng.
        // C dg_scripts.c:1228-1259 candidate filters (#147): the MOB path
        // skips chars the scripter cannot see and PRF_NOHASSLE players; the
        // OBJ/WLD paths skip wizinvis immortals and NOHASSLE players.
        let mut rndm: Option<CharId> = None;
        let mut count = 0;
        let actor = match go {
            GoRef::Mob(c) => Some(c),
            _ => None,
        };
        for c in people {
            if Some(c) == exclude {
                continue;
            }
            if let Some(ch) = g.get_char(c) {
                let nohassle = ch.prf_flags & crate::flags::PRF_NOHASSLE != 0;
                let wizinvis = !ch.is_npc && ch.invis_level > 0;
                if nohassle {
                    continue;
                }
                if wizinvis {
                    continue;
                }
                if let Some(actor) = actor {
                    if !g.can_see(actor, c) {
                        continue;
                    }
                }
            }
            if rng_number(0, count) == 0 {
                rndm = Some(c);
            }
            count += 1;
        }
        match rndm {
            Some(c) => format!("{}{}", UID_CHAR, char_id(c)),
            None => String::new(),
        }
    } else if field.eq_ignore_ascii_case("letter") {
        let n = rng_number(1, 26);
        let ch = (96 + n) as u8 as char;
        ch.to_string()
    } else {
        let num = match crate::text::parse_i32_strict(field) {
            Ok(value) => value,
            Err(crate::text::ParseIntError::Overflow) => {
                script_log("random upper bound outside supported 32-bit range");
                0
            }
            Err(_) => 0,
        };
        if num > 0 {
            rng_number(1, num).to_string()
        } else {
            "0".to_string()
        }
    }
}

// The VM needs number() during pure substitution (immutable &GameState). We
// route through a thread-local clone of the master rng that script_driver
// re-syncs each run, preserving CircleMUD number() semantics within a script.
thread_local! {
    static SCRIPT_RNG: std::cell::RefCell<crate::rng::Rng> =
        std::cell::RefCell::new(crate::rng::Rng::new(1));
}
fn rng_number(from: i32, to: i32) -> i32 {
    SCRIPT_RNG.with(|r| r.borrow_mut().number(from, to))
}
fn sync_script_rng(g: &mut GameState) {
    // Pull entropy from the master rng so script randomness advances the same
    // global stream (one draw seeds the script rng each driver entry).
    let seed = g.rng.circle_random() as u64;
    SCRIPT_RNG.with(|r| r.borrow_mut().srandom(seed));
}

#[allow(clippy::too_many_arguments)]
fn resolve_char_field(
    g: &GameState,
    go: GoRef,
    trig: TrigId,
    c: CharId,
    field: &str,
    subfield: &str,
    var_value: Option<&str>,
) -> String {
    if let Some(v) = var_value {
        if let Some(res) = text_processed(field, v) {
            return res;
        }
    }
    let ch = match g.get_char(c) {
        Some(x) => x,
        None => return String::new(),
    };
    let f = field.to_lowercase();
    match f.as_str() {
        "name" => ch
            .short_desc
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ch.player.name.clone()),
        "id" => char_id(c).to_string(),
        "alias" => ch.player.name.clone(),
        "level" => ch.player.level.to_string(),
        "align" => ch.alignment.to_string(),
        "gold" => ch.points.gold.to_string(),
        "sex" => gender_name(ch.player.sex).to_string(),
        "weight" => ch.player.weight.to_string(),
        "canbeseen" => {
            if let GoRef::Mob(viewer) = go {
                if !g.can_see(viewer, c) {
                    return "0".to_string();
                }
            }
            "1".to_string()
        }
        "class" => class_name(ch.player.class).to_string(),
        "race" => race_name(ch.player.race).to_string(),
        "fighting" => match ch.fighting {
            Some(v) => format!("{}{}", UID_CHAR, char_id(v)),
            None => String::new(),
        },
        // C dg_scripts.c:1325-1337: %actor.riding%/%actor.ridden_by% emit the
        // mount/rider UID; both fields exist on Character here (#156).
        "riding" => match ch.riding {
            Some(m) => format!("{}{}", UID_CHAR, char_id(m)),
            None => String::new(),
        },
        "ridden_by" => match ch.ridden_by {
            Some(r) => format!("{}{}", UID_CHAR, char_id(r)),
            None => String::new(),
        },
        "vnum" => if ch.is_npc { ch.nr } else { -1 }.to_string(),
        "str" => ch.aff_abils.str.to_string(),
        "stradd" => {
            if ch.aff_abils.str >= 18 {
                ch.aff_abils.str_add.to_string()
            } else {
                "0".to_string()
            }
        }
        "int" => ch.aff_abils.intel.to_string(),
        "wis" => ch.aff_abils.wis.to_string(),
        "dex" => ch.aff_abils.dex.to_string(),
        "con" => ch.aff_abils.con.to_string(),
        "cha" => ch.aff_abils.cha.to_string(),
        "room" => match ch.in_room {
            Some(r) => g.rooms[r].number.to_string(),
            None => "-1".to_string(),
        },
        "skill" => {
            let num = crate::spell_parser::find_skill_num(subfield);
            if num <= 0 {
                "unknown skill".to_string()
            } else {
                ch.skill(num as u16).to_string()
            }
        }
        "eq" => {
            let pos = find_eq_pos(subfield);
            match pos {
                Some(p) => match ch.equipment.get(p).copied().flatten() {
                    Some(o) => format!("{}{}", UID_CHAR, obj_id(o)),
                    None => String::new(),
                },
                None => String::new(),
            }
        }
        _ => {
            // unknown field: try the target's own script globals.
            if let Some(v) = dg_handler::get_global_var(ScriptKey::Mob(c), field) {
                return v;
            }
            log_unknown(trig, "char", field);
            String::new()
        }
    }
}

fn find_eq_pos(arg: &str) -> Option<usize> {
    // C dg_scripts.c:1370-1379 calls find_eq_pos (act.item.c:1474-1523):
    // a positional keyword table with !RESERVED! holes, matched by
    // search_block-style PREFIX match, THEN a numeric fallback (#154).
    const KEYWORDS: [[&str; 2]; 22] = [
        ["!RESERVED!", ""], // 0  light
        ["finger", ""],     // 1
        ["!RESERVED!", ""], // 2
        ["neck", ""],       // 3
        ["!RESERVED!", ""], // 4
        ["body", ""],       // 5
        ["head", ""],       // 6
        ["legs", ""],       // 7
        ["feet", ""],       // 8
        ["hands", ""],      // 9
        ["arms", ""],       // 10
        ["shield", ""],     // 11
        ["about", ""],      // 12
        ["waist", ""],      // 13
        ["wrist", ""],      // 14
        ["!RESERVED!", ""], // 15
        ["!RESERVED!", ""], // 16
        ["!RESERVED!", ""], // 17
        ["shoulders", ""],  // 18
        ["ankle", ""],      // 19
        ["face", ""],       // 20
        ["!RESERVED!", ""], // 21
    ];
    let a = arg.trim().to_lowercase();
    for (i, kw) in KEYWORDS.iter().enumerate() {
        // search_block(..., FALSE): `arg` is an abbreviation OF the keyword.
        if kw[0] != "!RESERVED!" && kw[0].starts_with(&a) {
            return Some(i);
        }
    }
    // numeric fallback (C: `if (pos == -1 && isdigit(*arg)) pos = atoi(arg)`).
    if let Ok(n) = a.parse::<usize>() {
        if n < NUM_WEARS {
            return Some(n);
        }
    }
    None
}

fn resolve_obj_field(
    g: &GameState,
    trig: TrigId,
    o: ObjId,
    field: &str,
    var_value: Option<&str>,
) -> String {
    if let Some(v) = var_value {
        if let Some(res) = text_processed(field, v) {
            return res;
        }
    }
    let obj = match g.get_obj(o) {
        Some(x) => x,
        None => return String::new(),
    };
    match field.to_lowercase().as_str() {
        "name" => obj.name.clone(),
        "id" => obj_id(o).to_string(),
        "shortdesc" => obj.short_description.clone(),
        "vnum" => obj.item_number.to_string(),
        "type" => crate::constants::ITEM_TYPES
            .get(obj.obj_type as usize)
            .copied()
            .unwrap_or("UNDEFINED")
            .to_string(),
        "timer" => obj.timer.to_string(),
        "val0" => obj.values[0].to_string(),
        "val1" => obj.values[1].to_string(),
        "val2" => obj.values[2].to_string(),
        "val3" => obj.values[3].to_string(),
        _ => {
            log_unknown(trig, "object", field);
            String::new()
        }
    }
}

fn resolve_room_field(
    g: &GameState,
    trig: TrigId,
    r: RoomRnum,
    field: &str,
    var_value: Option<&str>,
) -> String {
    if let Some(v) = var_value {
        if let Some(res) = text_processed(field, v) {
            return res;
        }
    }
    let room = match g.rooms.get(r) {
        Some(x) => x,
        None => return String::new(),
    };
    let dir = |d: usize| -> String {
        match &room.exits[d] {
            Some(e) => sprintbit(e.exit_info as i64, crate::constants::EXIT_BITS),
            None => String::new(),
        }
    };
    match field.to_lowercase().as_str() {
        "name" => room.name.clone(),
        "north" => dir(NORTH),
        "east" => dir(EAST),
        "south" => dir(SOUTH),
        "west" => dir(WEST),
        "up" => dir(UP),
        "down" => dir(DOWN),
        "vnum" => room.number.to_string(),
        _ => {
            log_unknown(trig, "room", field);
            String::new()
        }
    }
}

fn log_unknown(trig: TrigId, kind: &str, field: &str) {
    let (name, vnum) =
        dg_handler::with_trig(trig, |t| (t.name.clone(), t.vnum)).unwrap_or_default();
    script_log(&format!(
        "Trigger: {}, VNum {}. unknown {} field: '{}'",
        name, vnum, kind, field
    ));
}

fn sprintbit(bits: i64, table: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in table.iter().enumerate() {
        if *name == "\n" {
            break;
        }
        if bits & (1 << i) != 0 {
            out.push_str(name);
            out.push(' ');
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// var_subst: substitute %var% / %var.field(subfield)% references in `line`.
// ---------------------------------------------------------------------------
fn var_subst(g: &GameState, go: GoRef, trig: TrigId, line: &str) -> String {
    if !line.contains('%') {
        return line.to_string();
    }
    let bytes: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        if bytes[i] != '%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // at '%'
        i += 1;
        if i < n && bytes[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // read var up to '%' or '.'
        let var_start = i;
        while i < n && bytes[i] != '%' && bytes[i] != '.' {
            i += 1;
        }
        let var: String = bytes[var_start..i].iter().collect();

        let mut field = String::new();
        let mut subfield = String::new();
        if i < n && bytes[i] == '.' {
            i += 1; // skip '.'
            let mut paren = 0;
            let field_start = i;
            let mut field_end = i;
            while i < n && (bytes[i] != '%' || paren > 0) {
                if bytes[i] == '(' {
                    if paren == 0 {
                        field_end = i;
                    }
                    paren += 1;
                    i += 1;
                } else if bytes[i] == ')' {
                    paren -= 1;
                    i += 1;
                } else if paren > 0 {
                    subfield.push(bytes[i]);
                    i += 1;
                } else {
                    i += 1;
                }
            }
            if field_end == field_start && paren == 0 {
                // no paren encountered; field runs field_start..i
                field_end = i;
            } else if subfield.is_empty() && field_end == field_start {
                field_end = i;
            }
            if field_end < field_start {
                field_end = i;
            }
            field = bytes[field_start..field_end.min(i)].iter().collect();
            // If there was no '(' the loop above leaves field_end == field_start;
            // recover the full field text.
            if field.is_empty() && subfield.is_empty() {
                field = bytes[field_start..i].iter().collect();
            }
        }
        // skip closing '%'
        if i < n && bytes[i] == '%' {
            i += 1;
        }

        let repl = find_replacement(g, go, trig, var.trim(), field.trim(), subfield.trim());
        out.push_str(&repl);
    }
    out
}

// ---------------------------------------------------------------------------
// Expression evaluation (C left-to-right, lowest priority first).
// ---------------------------------------------------------------------------
const OPS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "<", ">", "/=", "-", "+", "/", "*", "!",
];

fn atoi(s: &str) -> i64 {
    match crate::text::parse_i64_atoi(s) {
        Ok(value) => value,
        Err(crate::text::ParseIntError::Overflow) => {
            let clamped = if s.trim_start().starts_with('-') {
                i64::MIN
            } else {
                i64::MAX
            };
            script_log(&format!(
                "numeric value outside supported 64-bit range; clamped to {clamped}"
            ));
            clamped
        }
        Err(_) => unreachable!("parse_i64_atoi maps nonnumeric input to zero"),
    }
}

/// str_cmp returns 0 if equal (case-insensitive), used to mirror C signs.
fn str_cmp_sign(a: &str, b: &str) -> i32 {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    match a.cmp(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// eval_op(op, lhs, rhs): exactly the C semantics, including the str_cmp
/// branches and the `>=` C bug (uses `<= 0` for the string case).
fn eval_op(op: &str, lhs: &str, rhs: &str) -> String {
    let lhs = lhs.trim();
    let rhs = rhs.trim();
    match op {
        "||" => {
            let l = lhs.is_empty() || lhs.starts_with('0');
            let r = rhs.is_empty() || rhs.starts_with('0');
            if l && r { "0" } else { "1" }.to_string()
        }
        "&&" => {
            let l = lhs.is_empty() || lhs.starts_with('0');
            let r = rhs.is_empty() || rhs.starts_with('0');
            if l || r { "0" } else { "1" }.to_string()
        }
        "==" => {
            if is_num(lhs) && is_num(rhs) {
                ((atoi(lhs) == atoi(rhs)) as i32).to_string()
            } else {
                ((str_cmp_sign(lhs, rhs) == 0) as i32).to_string()
            }
        }
        "!=" => {
            if is_num(lhs) && is_num(rhs) {
                ((atoi(lhs) != atoi(rhs)) as i32).to_string()
            } else {
                ((str_cmp_sign(lhs, rhs) != 0) as i32).to_string()
            }
        }
        "<=" => {
            if is_num(lhs) && is_num(rhs) {
                ((atoi(lhs) <= atoi(rhs)) as i32).to_string()
            } else {
                ((str_cmp_sign(lhs, rhs) <= 0) as i32).to_string()
            }
        }
        ">=" => {
            if is_num(lhs) && is_num(rhs) {
                ((atoi(lhs) >= atoi(rhs)) as i32).to_string()
            } else {
                // NOTE: C uses `str_cmp(lhs,rhs) <= 0` here (a known DG quirk).
                ((str_cmp_sign(lhs, rhs) <= 0) as i32).to_string()
            }
        }
        "<" => {
            if is_num(lhs) && is_num(rhs) {
                ((atoi(lhs) < atoi(rhs)) as i32).to_string()
            } else {
                ((str_cmp_sign(lhs, rhs) < 0) as i32).to_string()
            }
        }
        ">" => {
            if is_num(lhs) && is_num(rhs) {
                ((atoi(lhs) > atoi(rhs)) as i32).to_string()
            } else {
                ((str_cmp_sign(lhs, rhs) > 0) as i32).to_string()
            }
        }
        "/=" => if str_str(lhs, rhs) { "1" } else { "0" }.to_string(),
        "*" => atoi(lhs).saturating_mul(atoi(rhs)).to_string(),
        "/" => {
            let n = atoi(rhs);
            if n != 0 {
                atoi(lhs).checked_div(n).unwrap_or(i64::MAX)
            } else {
                0
            }
            .to_string()
        }
        "+" => atoi(lhs).saturating_add(atoi(rhs)).to_string(),
        "-" => atoi(lhs).saturating_sub(atoi(rhs)).to_string(),
        "!" => {
            if is_num(rhs) {
                ((atoi(rhs) == 0) as i32).to_string()
            } else {
                (rhs.is_empty() as i32).to_string()
            }
        }
        _ => String::new(),
    }
}

/// matching_paren: index of the close paren matching the open at `start`.
fn matching_paren(s: &[char], start: usize) -> usize {
    let mut i = start + 1;
    let mut depth = 1;
    while i < s.len() && depth > 0 {
        match s[i] {
            '(' => depth += 1,
            ')' => depth -= 1,
            '"' => i = matching_quote(s, i),
            _ => {}
        }
        if depth == 0 {
            return i;
        }
        i += 1;
    }
    if i >= s.len() {
        s.len().saturating_sub(1)
    } else {
        i
    }
}
fn matching_quote(s: &[char], start: usize) -> usize {
    let mut i = start + 1;
    while i < s.len() && s[i] != '"' {
        if s[i] == '\\' {
            i += 1;
        }
        i += 1;
    }
    if i >= s.len() {
        s.len().saturating_sub(1)
    } else {
        i
    }
}

/// eval_expr: strip a leading paren group, else try lhs-op-rhs, else var_subst.
fn eval_expr(g: &GameState, line: &str, go: GoRef, trig: TrigId) -> String {
    let line = line.trim_start();
    if let Some(res) = eval_lhs_op_rhs(g, line, go, trig) {
        return res;
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.first() == Some(&'(') {
        let close = matching_paren(&chars, 0);
        let inner: String = chars[1..close].iter().collect();
        return eval_expr(g, &inner, go, trig);
    }
    var_subst(g, go, trig, line)
}

/// eval_lhs_op_rhs: find the lowest-priority operator at top level (respecting
/// parens/quotes), split there, recurse, and combine. Returns None if no op.
fn eval_lhs_op_rhs(g: &GameState, expr: &str, go: GoRef, trig: TrigId) -> Option<String> {
    let chars: Vec<char> = expr.chars().collect();
    // tokens: positions where an operator may begin (top-level only).
    let mut tokens: Vec<usize> = Vec::new();
    let mut p = 0;
    while p < chars.len() {
        tokens.push(p);
        if chars[p] == '(' {
            p = matching_paren(&chars, p) + 1;
        } else if chars[p] == '"' {
            p = matching_quote(&chars, p) + 1;
        } else if chars[p].is_alphanumeric() {
            p += 1;
            while p < chars.len() && (chars[p].is_alphanumeric() || chars[p].is_whitespace()) {
                p += 1;
            }
        } else {
            p += 1;
        }
    }

    for op in OPS {
        let oplen = op.chars().count();
        let opchars: Vec<char> = op.chars().collect();
        for &t in &tokens {
            if t + oplen <= chars.len() && chars[t..t + oplen] == opchars[..] {
                let lhs: String = chars[..t].iter().collect();
                let rhs: String = chars[t + oplen..].iter().collect();
                let lhr = eval_expr(g, &lhs, go, trig);
                let rhr = eval_expr(g, &rhs, go, trig);
                return Some(eval_op(op, &lhr, &rhr));
            }
        }
    }
    None
}

/// process_if(cond): true if the condition evaluates to a non-empty non-"0".
fn process_if(g: &GameState, cond: &str, go: GoRef, trig: TrigId) -> bool {
    let result = eval_expr(g, cond, go, trig);
    let p = result.trim_start();
    !(p.is_empty() || p.starts_with('0'))
}

// ---------------------------------------------------------------------------
// Control-flow scanners over the trigger's cmdlist (line indices).
// ---------------------------------------------------------------------------
fn leading(s: &str) -> &str {
    s.trim_start()
}

fn find_end(cmds: &[String], start: usize) -> usize {
    if start + 1 >= cmds.len() {
        return start;
    }
    let mut c = start + 1;
    while c + 1 < cmds.len() {
        let p = leading(&cmds[c]);
        if p.starts_with("if ") {
            c = find_end(cmds, c);
        } else if p.starts_with("end") {
            return c;
        }
        c += 1;
    }
    c.min(cmds.len() - 1)
}

/// find_else_end: returns (line, depth_inc). depth_inc==true means the caller
/// should ++depth (we entered an elseif/else body).
fn find_else_end(
    g: &GameState,
    cmds: &[String],
    start: usize,
    go: GoRef,
    trig: TrigId,
) -> (usize, bool) {
    if start + 1 >= cmds.len() {
        return (start, false);
    }
    let mut c = start + 1;
    while c + 1 < cmds.len() {
        let p = leading(&cmds[c]);
        if p.starts_with("if ") {
            c = find_end(cmds, c);
        } else if let Some(rest) = p.strip_prefix("elseif ") {
            if process_if(g, rest, go, trig) {
                return (c, true);
            }
        } else if p.starts_with("else") {
            return (c, true);
        } else if p.starts_with("end") {
            return (c, false);
        }
        c += 1;
    }
    (c.min(cmds.len() - 1), false)
}

fn find_done(cmds: &[String], start: usize) -> usize {
    if start + 1 >= cmds.len() {
        return start;
    }
    let mut c = start + 1;
    while c + 1 < cmds.len() {
        let p = leading(&cmds[c]);
        if p.starts_with("while ") || p.starts_with("switch ") {
            c = find_done(cmds, c);
        } else if p.starts_with("done") {
            return c;
        }
        c += 1;
    }
    c.min(cmds.len() - 1)
}

fn find_case(
    g: &GameState,
    cmds: &[String],
    start: usize,
    go: GoRef,
    trig: TrigId,
    cond: &str,
) -> usize {
    if start + 1 >= cmds.len() {
        return start;
    }
    let mut c = start + 1;
    while c + 1 < cmds.len() {
        let p = leading(&cmds[c]);
        if p.starts_with("while ") || p.starts_with("switch") {
            c = find_done(cmds, c);
        } else if let Some(rest) = p.strip_prefix("case ") {
            let buf = format!("({}) == ({})", cond, rest);
            if process_if(g, &buf, go, trig) {
                return c;
            }
        } else if p.starts_with("default") {
            return c;
        } else if p.starts_with("done") {
            return c;
        }
        c += 1;
    }
    c.min(cmds.len() - 1)
}

// ---------------------------------------------------------------------------
// Command processors that mutate trigger/script vars.
// ---------------------------------------------------------------------------
fn process_set(go: GoRef, trig: TrigId, cmd: &str) {
    // "set name value..."
    let (name, value) = two_arg_rest(cmd);
    if name.is_empty() {
        log_cmd_err(trig, "set", cmd);
        return;
    }
    let ctx = dg_handler::get_context(go.key());
    dg_handler::with_trig_mut(trig, |t| {
        add_var_in(&mut t.var_list, &name, value.trim_start(), ctx);
    });
}

fn process_eval(g: &GameState, go: GoRef, trig: TrigId, cmd: &str) {
    let (name, expr) = two_arg_rest(cmd);
    if name.is_empty() {
        log_cmd_err(trig, "eval", cmd);
        return;
    }
    let result = eval_expr(g, expr.trim_start(), go, trig);
    let ctx = dg_handler::get_context(go.key());
    dg_handler::with_trig_mut(trig, |t| add_var_in(&mut t.var_list, &name, &result, ctx));
}

fn makeuid_var(g: &GameState, go: GoRef, trig: TrigId, cmd: &str) {
    let (varname, uid_p) = two_arg_rest(cmd);
    let uid_p = uid_p.trim();
    if varname.is_empty() {
        log_cmd_err(trig, "makeuid", cmd);
        return;
    }
    if uid_p.is_empty() || atoi(uid_p) == 0 {
        log_cmd_err(trig, "makeuid", cmd);
        return;
    }
    let result = eval_expr(g, uid_p, go, trig);
    let uid = format!("{}{}", UID_CHAR, result);
    let ctx = dg_handler::get_context(go.key());
    dg_handler::with_trig_mut(trig, |t| add_var_in(&mut t.var_list, &varname, &uid, ctx));
}

fn process_unset(go: GoRef, trig: TrigId, cmd: &str) {
    let var = one_arg_rest(cmd);
    let var = var.trim();
    if var.is_empty() {
        log_cmd_err(trig, "unset", cmd);
        return;
    }
    if !dg_handler::remove_global_var(go.key(), var) {
        dg_handler::with_trig_mut(trig, |t| {
            remove_var_in(&mut t.var_list, var);
        });
    }
}

fn process_global(go: GoRef, trig: TrigId, cmd: &str, id: i64) {
    let var = one_arg_rest(cmd);
    let var = var.trim();
    if var.is_empty() {
        log_cmd_err(trig, "global", cmd);
        return;
    }
    let val = dg_handler::with_trig(trig, |t| t.get_var(var).map(|v| v.value.clone())).flatten();
    match val {
        Some(v) => {
            dg_handler::add_global_var(go.key(), var, &v, id);
            dg_handler::with_trig_mut(trig, |t| {
                remove_var_in(&mut t.var_list, var);
            });
        }
        None => {
            log_cmd_err(trig, "global", &format!("local var '{}' not found", var));
        }
    }
}

fn process_context(go: GoRef, trig: TrigId, cmd: &str) {
    let var = one_arg_rest(cmd);
    let var = var.trim();
    if var.is_empty() {
        log_cmd_err(trig, "context", cmd);
        return;
    }
    dg_handler::set_context(go.key(), atoi(var));
}

fn process_remote(g: &GameState, go: GoRef, trig: TrigId, cmd: &str) {
    // "remote <var> <uid>"
    let rest = one_arg_rest(cmd); // drop "remote"
    let (var, uid_s) = two_arg(&rest);
    if var.is_empty() || uid_s.is_empty() {
        log_cmd_err(trig, "remote", cmd);
        return;
    }
    let ctx = dg_handler::get_context(go.key());
    // find local var, then context-global.
    let vd = dg_handler::with_trig(trig, |t| {
        t.get_var(&var)
            .map(|v| (v.name.clone(), v.value.clone(), v.context))
    })
    .flatten()
    .or_else(|| dg_handler::get_global_var(go.key(), &var).map(|v| (var.clone(), v, ctx)));
    let Some((name, value, vctx)) = vd else {
        log_cmd_err(trig, "remote", "local var not found");
        return;
    };
    let uid = atoi(&uid_s);
    // C (dg_scripts.c:2125) only rejects uid<=0 as illegal. Its second branch
    // (`uid < ROOM_ID_BASE` => "is a PC?") relied on C's id scheme where mob/obj
    // ids sat in MOBOBJ_ID_BASE..ROOM_ID_BASE; in THIS port mob UID = raw CharId
    // and obj UID = raw ObjId (small positive ints below ROOM_ID_BASE), so that
    // range gate would reject every valid mob/obj target. Drop it: remote may set
    // a global on any valid target (mob/obj/room).
    if uid <= 0 {
        log_cmd_err(trig, "remote", "illegal uid");
        return;
    }
    let target = if let Some(r) = find_room_by_uid(g, uid) {
        Some(ScriptKey::Room(r))
    } else if let Some(c) = find_char_by_uid(g, uid) {
        Some(ScriptKey::Mob(c))
    } else {
        find_obj_by_uid(g, uid).map(ScriptKey::Obj)
    };
    if let Some(key) = target {
        dg_handler::add_global_var(key, &name, &value, vctx);
    }
}

fn extract_value(go: GoRef, trig: TrigId, cmd: &str) {
    // "extract <to> <num> <space-separated source...>"
    // C (dg_scripts.c:2217) parses with half_chop, NOT two_arguments — so the
    // source argument keeps the whole line remainder. The previous `two_arg`
    // calls truncated the source to a single token, breaking any "extract var N
    // word1 word2 word3" that picks a token past the second word. half_chop ==
    // split_word here (first word + rest-of-line).
    let rest = one_arg_rest(cmd); // drop "extract"; rest = "<to> <num> <source...>"
    let (to, after_to) = split_word(&rest); // to = dest var, remainder = "<num> <source...>"
    let (num_s, source) = split_word(after_to); // num token + full source remainder
    let num = atoi(&num_s);
    if num < 1 {
        script_log("extract number < 1!");
        return;
    }
    // take the num-th whitespace token of `source` (1-based).
    let token = source
        .split_whitespace()
        .nth((num - 1) as usize)
        .unwrap_or("")
        .to_string();
    let ctx = dg_handler::get_context(go.key());
    dg_handler::with_trig_mut(trig, |t| add_var_in(&mut t.var_list, &to, &token, ctx));
}

fn process_return(trig: TrigId, cmd: &str) -> i32 {
    let (_a1, a2) = two_arg(&one_arg_rest_keep(cmd));
    if a2.is_empty() {
        log_cmd_err(trig, "return", cmd);
        return 1;
    }
    match crate::text::parse_i32_atoi(&a2) {
        Ok(value) => value,
        Err(crate::text::ParseIntError::Overflow) => {
            log_cmd_err(trig, "return", "value outside supported 32-bit range");
            0
        }
        Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
    }
}

/// process_wait: schedule a wait event and park the trigger at the next line.
fn process_wait(g: &mut GameState, go: GoRef, trig: TrigId, cmd: &str, cur_line: usize) {
    let arg = one_arg_rest(cmd);
    let arg = arg.trim();
    if arg.is_empty() {
        log_cmd_err(trig, "wait", cmd);
        return;
    }

    let time: i64 = if let Some(rest) = arg.strip_prefix("until ") {
        // "until HH:MM" or "until HHMM"
        let rest = rest.trim();
        let (hr, min) = if let Some((h, m)) = rest.split_once(':') {
            (atoi(h), atoi(m))
        } else {
            let v = atoi(rest);
            (v / 100, v % 100)
        };
        wait_until_delay(g, hr, min)
    } else {
        // "<n>" or "<n> t" (mud-hours) or "<n> s" (seconds)
        let mut it = arg.split_whitespace();
        let n = atoi(it.next().unwrap_or("0"));
        match it.next() {
            Some("t") => n.saturating_mul(75).saturating_mul(PASSES_PER_SEC as i64),
            Some("s") => n.saturating_mul(PASSES_PER_SEC as i64),
            _ => n,
        }
    };

    let ev = add_event(
        time,
        WaitEvent {
            go,
            trig,
            trig_type: go.trig_type(),
        },
    );
    dg_handler::with_trig_mut(trig, |t| {
        t.wait_event = Some(ev);
        t.curr_line = cur_line + 1;
    });
}

fn wait_until_delay(g: &GameState, hr: i64, min: i64) -> i64 {
    wait_until_delay_from(g.pulse, crate::weather::mud_minute_of_day(), hr, min)
}

fn wait_until_delay_from(pulse: u64, current_mud_min: i64, hr: i64, min: i64) -> i64 {
    let target_min = min.saturating_add(hr.saturating_mul(60));
    let pulses_per_hour = 75 * PASSES_PER_SEC as i64; // SECS_PER_MUD_HOUR=75
    let ntime = target_min.saturating_mul(pulses_per_hour) / 60;
    let now = (pulse as i64 % pulses_per_hour)
        .saturating_add(current_mud_min.saturating_mul(pulses_per_hour) / 60);
    if now >= ntime {
        pulses_per_hour
            .saturating_mul(24)
            .saturating_sub(now)
            .saturating_add(ntime)
    } else {
        ntime.saturating_sub(now)
    }
}

fn log_cmd_err(trig: TrigId, what: &str, cmd: &str) {
    let (name, vnum) =
        dg_handler::with_trig(trig, |t| (t.name.clone(), t.vnum)).unwrap_or_default();
    script_log(&format!(
        "Trigger: {}, VNum {}. {}: '{}'",
        name, vnum, what, cmd
    ));
}

// ---- small arg helpers mirroring two_arguments/any_one_arg --------------
fn two_arg_rest(s: &str) -> (String, String) {
    // returns (2nd word lowercased? no — C two_arguments lowercases? It uses
    // one_argument which lowercases the *word* via str). For set/eval the name
    // keeps case via two_arguments(any_one_arg). We keep original case for the
    // name token (CircleMUD two_arguments uses any_one_arg -> no lowercase).
    let (_first, r1) = split_word(s); // drop the command word
    let (name, rest) = split_word(r1);
    (name, rest.to_string())
}
fn one_arg_rest(s: &str) -> String {
    let (_first, r1) = split_word(s);
    r1.trim_start().to_string()
}
fn one_arg_rest_keep(s: &str) -> String {
    // keep for process_return which wants two_arguments(cmd, a1, a2)
    s.to_string()
}
fn two_arg(s: &str) -> (String, String) {
    let (a, r) = split_word(s);
    let (b, _r2) = split_word(r);
    (a, b)
}
fn split_word(s: &str) -> (String, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(p) => (s[..p].to_string(), s[p..].trim_start()),
        None => (s.to_string(), ""),
    }
}

// ---------------------------------------------------------------------------
// script_driver — the core VM loop.
// ---------------------------------------------------------------------------

/// Resume a trigger after its wait event fires (trig_wait_event).
pub fn trig_wait_event(g: &mut GameState, ev: WaitEvent) {
    // Clear the parked event handle and resume.
    let still_attached = dg_handler::with_trig(ev.trig, |t| t.wait_event.is_some()).is_some();
    if !still_attached {
        return;
    }
    dg_handler::with_trig_mut(ev.trig, |t| t.wait_event = None);
    if ev.go.exists(g) {
        script_driver(g, ev.go, ev.trig, ev.trig_type, TRIG_RESTART);
    }
}

/// script_driver(go, trig, type, mode): run a trigger's command list. Returns
/// the script return value (default 1; a `return N` sets it).
pub fn script_driver(g: &mut GameState, go: GoRef, trig: TrigId, type_: i32, mode: i32) -> i32 {
    let depth = SCRIPT_DEPTH.with(|d| d.get());
    if depth > MAX_DEPTH_RECURSE {
        script_log("Triggers recursed beyond maximum allowed depth.");
        return 1;
    }
    SCRIPT_DEPTH.with(|d| d.set(depth + 1));
    sync_script_rng(g);

    // C comm.c: dg_act_check is cleared while the VM runs so %echo%-style
    // act() lines do not re-trigger act_mtrigger (#138).
    let prev_check = g.dg_act_check;
    g.dg_act_check = false;
    // The Game boundary contains command panics, so this layer must restore
    // every process/thread-local re-entry latch before the panic reaches it.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        script_driver_inner(g, go, trig, type_, mode)
    }));
    g.dg_act_check = prev_check;
    SCRIPT_DEPTH.with(|d| d.set(depth));
    match result {
        Ok(result) => result,
        Err(payload) => {
            OWNER_PURGED.with(|purged| purged.set(false));
            dg_handler::with_trig_mut(trig, |trigger| {
                trigger.depth = 0;
                trigger.curr_line = 0;
                trigger.var_list.clear();
            });
            std::panic::resume_unwind(payload)
        }
    }
}

fn script_driver_inner(g: &mut GameState, go: GoRef, trig: TrigId, type_: i32, mode: i32) -> i32 {
    let key = go.key();
    let mut ret_val = 1;

    // Snapshot the command list (immutable across the run; cmdlist is shared).
    let cmds = match dg_handler::with_trig(trig, |t| t.cmdlist.clone()) {
        Some(c) => c,
        None => return 1,
    };

    if mode == TRIG_NEW {
        dg_handler::with_trig_mut(trig, |t| {
            t.depth = 1;
            t.loops = 0;
            t.loop_origin.clear();
        });
        dg_handler::set_context(key, 0);
    }

    OWNER_PURGED.with(|p| p.set(false));

    let mut cl = if mode == TRIG_NEW {
        0
    } else {
        dg_handler::with_trig(trig, |t| t.curr_line).unwrap_or(0)
    };
    let mut loops: u32 = 0;

    while cl < cmds.len() {
        let cur_depth = dg_handler::with_trig(trig, |t| t.depth).unwrap_or(0);
        if cur_depth == 0 {
            break;
        }
        let raw = &cmds[cl];
        let p = leading(raw);

        if p.starts_with('*') {
            // comment
        } else if let Some(rest) = p.strip_prefix("if ") {
            if process_if(g, rest, go, trig) {
                dg_handler::with_trig_mut(trig, |t| t.depth += 1);
            } else {
                let (line, inc) = find_else_end(g, &cmds, cl, go, trig);
                cl = line;
                if inc {
                    dg_handler::with_trig_mut(trig, |t| t.depth += 1);
                }
            }
        } else if p.starts_with("elseif ") || p.starts_with("else") {
            cl = find_end(&cmds, cl);
            dg_handler::with_trig_mut(trig, |t| t.depth -= 1);
        } else if let Some(rest) = p.strip_prefix("while ") {
            let done = find_done(&cmds, cl);
            if process_if(g, rest, go, trig) {
                dg_handler::with_trig_mut(trig, |t| {
                    t.loop_origin.insert(done, cl);
                });
            } else {
                cl = done;
                loops = 0;
            }
        } else if let Some(rest) = p.strip_prefix("switch ") {
            cl = find_case(g, &cmds, cl, go, trig, rest);
        } else if p.starts_with("end") {
            dg_handler::with_trig_mut(trig, |t| t.depth -= 1);
        } else if p.starts_with("done") {
            let origin = dg_handler::with_trig(trig, |t| t.loop_origin.get(&cl).copied()).flatten();
            if let Some(while_line) = origin {
                let while_cond = leading(&cmds[while_line])
                    .strip_prefix("while ")
                    .unwrap_or("")
                    .to_string();
                if process_if(g, &while_cond, go, trig) {
                    cl = while_line;
                    loops += 1;
                    let total = dg_handler::with_trig_mut(trig, |t| {
                        t.loops += 1;
                        t.loops
                    })
                    .unwrap_or(0);
                    if loops == 30 {
                        process_wait(g, go, trig, "wait 1", cl);
                        return ret_val;
                    }
                    if total == 100 {
                        let vnum = dg_handler::with_trig(trig, |t| t.vnum).unwrap_or(0);
                        script_log(&format!("Trigger VNum {} has looped 100 times!!!", vnum));
                    }
                }
            }
        } else if p.starts_with("break") {
            cl = find_done(&cmds, cl);
        } else if p.starts_with("case") {
            // multiple cases to one body: no-op
        } else {
            // Substitute, then dispatch a directive or a real command.
            // C dg_scripts.c:1503-1563 var_subst writes into a
            // MAX_INPUT_LENGTH (256) buffer - long substitutions are clipped
            // (#160).
            let mut cmd = var_subst(g, go, trig, p);
            crate::text::truncate_utf8_bytes(&mut cmd, 256);
            let lc = cmd.trim_start();

            if let Some(rest) = strip_kw(lc, "eval ") {
                let _ = rest;
                process_eval(g, go, trig, &cmd);
            } else if strip_kw(lc, "extract ").is_some() {
                extract_value(go, trig, &cmd);
            } else if strip_kw(lc, "makeuid ").is_some() {
                makeuid_var(g, go, trig, &cmd);
            } else if lc.starts_with("halt") {
                break;
            } else if strip_kw(lc, "global ").is_some() {
                let ctx = dg_handler::get_context(key);
                process_global(go, trig, &cmd, ctx);
            } else if strip_kw(lc, "context ").is_some() {
                process_context(go, trig, &cmd);
            } else if strip_kw(lc, "remote ").is_some() {
                process_remote(g, go, trig, &cmd);
            } else if strip_kw(lc, "return ").is_some() {
                ret_val = process_return(trig, &cmd);
            } else if strip_kw(lc, "set ").is_some() {
                process_set(go, trig, &cmd);
            } else if strip_kw(lc, "unset ").is_some() {
                process_unset(go, trig, &cmd);
            } else if strip_kw(lc, "wait ").is_some() {
                process_wait(g, go, trig, &cmd, cl);
                return ret_val;
            } else if lc.starts_with("version") {
                // C dg_scripts.c:2378-2379: 'version' mudlogs the DG banner
                // to immortals (#161).
                crate::syslog::mudlog(
                    g,
                    "DG Scripts Version 0.99 Patch Level 5a    8/98",
                    crate::syslog::NRM,
                    LVL_GOD,
                );
            } else {
                // Real command — dispatch by trigger type. The DG command
                // modules (dg_mobcmd / dg_objcmd / dg_wldcmd) own the m*/o*/w*
                // verbs and set their owner-purged flag when a script purges the
                // entity it runs on; we honour the C protocol of clearing it
                // before and reading it after each command.
                let purged = run_one_command(g, go, type_, &cmd);
                if purged {
                    return ret_val;
                }
            }
        }

        cl += 1;
    }

    // End of trigger: clear local vars + running state.
    dg_handler::with_trig_mut(trig, |t| {
        t.var_list.clear();
        t.depth = 0;
        t.curr_line = 0;
    });
    ret_val
}

fn strip_kw<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    if s.get(..kw.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(kw))
    {
        s.get(kw.len()..)
    } else {
        None
    }
}

/// Mark the owner purged from inside a DG command (set when *self is opurge'd).
pub fn set_owner_purged() {
    OWNER_PURGED.with(|p| p.set(true));
}

// ===========================================================================
// DG command dispatch for the running trigger line. The m*/o*/w* DG commands
// live in the sibling modules dg_mobcmd / dg_objcmd / dg_wldcmd; this routes a
// substituted command line to the right one, honouring the C dg_owner_purged
// protocol (clear before, read after) so a script that purges its own owner
// (mpurge/opurge/wpurge self) halts cleanly. For MOB triggers, an unrecognised
// m* verb falls through to the mob's normal command_interpreter exactly as
// dg_scripts.c does. Returns true if the owner was purged.
// ===========================================================================
fn run_one_command(g: &mut GameState, go: GoRef, type_: i32, cmd: &str) -> bool {
    #[cfg(test)]
    if cmd == "__panic_for_test" {
        panic!("injected DG script panic");
    }
    match type_ {
        MOB_TRIGGER => {
            if let GoRef::Mob(c) = go {
                if !g.char_exists(c) {
                    return false;
                }
                crate::dg_mobcmd::clear_owner_purged();
                let (verb, rest) = split_word(cmd);
                let handled = crate::dg_mobcmd::dispatch_mob_command(g, c, &verb, rest);
                if !handled && g.char_exists(c) {
                    crate::interpreter::command_interpreter(g, c, cmd);
                }
                let purged = crate::dg_mobcmd::take_owner_purged();
                if purged {
                    set_owner_purged();
                }
                purged
            } else {
                false
            }
        }
        OBJ_TRIGGER => {
            if let GoRef::Obj(o) = go {
                crate::dg_objcmd::clear_owner_purged();
                crate::dg_objcmd::dispatch_obj_command(g, o, cmd);
                let purged = crate::dg_objcmd::take_owner_purged();
                if purged {
                    set_owner_purged();
                }
                purged
            } else {
                false
            }
        }
        WLD_TRIGGER => {
            if let GoRef::Room(r) = go {
                crate::dg_wldcmd::dispatch_wld_command(g, r, cmd);
            }
            false
        }
        _ => false,
    }
}

// ===========================================================================
// Fire hooks live in the sibling dg_triggers module (it injects the contextual
// %variables% and drives this VM). dg_scripts owns only the VM + var resolver.
// The one pulse-driver that has no natural home in dg_triggers is the periodic
// random scan, kept here and delegating to dg_triggers' random hooks.
// ===========================================================================

// ---------------------------------------------------------------------------
// script_trigger_check — per-pulse random trigger scan (PULSE_DG_SCRIPT).
// Mirrors dg_scripts.c: iterate chars/objs/rooms, fire the random hooks when
// the entity's script-type bitvector has the RANDOM bit (and, for world/mob,
// the zone is non-empty unless the GLOBAL bit is set).
// ---------------------------------------------------------------------------
pub fn script_trigger_check(g: &mut GameState) {
    let chars: Vec<CharId> = g.char_ids();
    for c in chars {
        if !g.char_exists(c) {
            continue;
        }
        let ty = dg_handler::script_types(ScriptKey::Mob(c));
        if ty & MTRIG_RANDOM != 0 && ((ty & MTRIG_GLOBAL != 0) || !zone_empty_for_char(g, c)) {
            crate::dg_triggers::random_mtrigger(g, c);
        }
    }
    let objs: Vec<ObjId> = g.obj_ids();
    for o in objs {
        if g.get_obj(o).is_none() {
            continue;
        }
        if dg_handler::script_types(ScriptKey::Obj(o)) & OTRIG_RANDOM != 0 {
            crate::dg_triggers::random_otrigger(g, o);
        }
    }
    // Synthetic surface-map cells cannot carry WTRIGs. GameState maintains an
    // append-only index of ordinary rooms, including OLC additions made after
    // the map splice, so this hot path is O(real rooms), not O(map cells).
    // Snapshot it because a fired trigger mutably borrows the whole game.
    let rooms = g.non_map_room_rnums().to_vec();
    for r in rooms {
        let ty = dg_handler::script_types(ScriptKey::Room(r));
        if ty & WTRIG_RANDOM != 0 && ((ty & WTRIG_GLOBAL != 0) || !zone_empty_for_room(g, r)) {
            crate::dg_triggers::random_wtrigger(g, r);
        }
    }
}

fn zone_empty_for_char(g: &GameState, c: CharId) -> bool {
    match g.get_char(c).and_then(|x| x.in_room) {
        Some(r) => zone_empty_for_room(g, r),
        None => true,
    }
}

/// is_empty(zone): true if no connected player is in that zone.
fn zone_empty_for_room(g: &GameState, r: RoomRnum) -> bool {
    let zone = match g.rooms.get(r) {
        Some(room) => room.zone,
        None => return true,
    };
    for cid in g.char_ids() {
        if let Some(ch) = g.get_char(cid) {
            if !ch.is_npc && ch.desc.is_some() {
                if let Some(rn) = ch.in_room {
                    if g.rooms[rn].zone == zone {
                        return false;
                    }
                }
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// boot — load prototypes, assign room triggers, clear runtime state.
// ---------------------------------------------------------------------------
pub fn boot_dg_scripts(g: &mut GameState, lib_path: &str) {
    crate::dg_event::boot_events();
    dg_handler::boot_handler();
    crate::dg_db_scripts::boot_triggers(g, lib_path);
}

// ---------------------------------------------------------------------------
// Unit tests for the pure expression engine — the C left-to-right operator
// semantics and the number/string comparison quirks scripts depend on.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod eval_tests {
    use super::*;

    #[test]
    fn atoi_leading_int() {
        assert_eq!(atoi("  12abc"), 12);
        assert_eq!(atoi("-5x"), -5);
        assert_eq!(atoi("abc"), 0);
        assert_eq!(atoi(""), 0);
    }

    #[test]
    fn is_num_matches_c() {
        assert!(is_num("123"));
        assert!(is_num("-7"));
        assert!(is_num("12 ")); // trailing space ok
        assert!(!is_num("12a"));
        assert!(!is_num("abc"));
    }

    #[test]
    fn eval_op_numeric_compares() {
        assert_eq!(eval_op("==", "5", "5"), "1");
        assert_eq!(eval_op("!=", "5", "6"), "1");
        assert_eq!(eval_op("<", "3", "5"), "1");
        assert_eq!(eval_op(">", "5", "3"), "1");
        assert_eq!(eval_op("+", "2", "3"), "5");
        assert_eq!(eval_op("-", "10", "4"), "6");
        assert_eq!(eval_op("*", "6", "7"), "42");
        assert_eq!(eval_op("/", "10", "3"), "3");
        assert_eq!(eval_op("/", "10", "0"), "0"); // div-by-zero => 0
    }

    #[test]
    fn eval_op_string_compares() {
        // Non-numeric == uses case-insensitive str_cmp.
        assert_eq!(eval_op("==", "Read", "read"), "1");
        assert_eq!(eval_op("==", "hut", "hat"), "0");
        // /= is substring (used by some scripts).
        assert_eq!(eval_op("/=", "the corpse", "corpse"), "1");
        assert_eq!(eval_op("/=", "sword", "axe"), "0");
    }

    #[test]
    fn eval_op_logical() {
        assert_eq!(eval_op("||", "0", "0"), "0");
        assert_eq!(eval_op("||", "1", "0"), "1");
        assert_eq!(eval_op("&&", "1", "1"), "1");
        assert_eq!(eval_op("&&", "1", "0"), "0");
        assert_eq!(eval_op("&&", "", "1"), "0"); // empty == false
    }

    #[test]
    fn str_str_case_insensitive() {
        assert!(str_str("Hello World", "world"));
        assert!(!str_str("Hello", "xyz"));
    }

    #[test]
    fn random_room_scan_includes_real_rooms_appended_after_map_cells() {
        use crate::config::Config;
        use crate::room::Room;

        let mut g = GameState::new(Config::default());
        let real_before = g.add_room(Room::new(100, 0, "Before".into(), "".into()));
        let mut map = Room::new(2_000_000, 0, "Map".into(), "".into());
        map.map_x = Some(1);
        map.map_y = Some(1);
        let map_room = g.add_room(map);
        g.map_start_rnum = Some(map_room);
        let real_after = g.add_room(Room::new(101, 0, "After".into(), "".into()));

        assert_eq!(g.non_map_room_rnums(), &[real_before, real_after]);
        assert!(!g.non_map_room_rnums().contains(&map_room));
    }

    #[test]
    fn random_room_scan_work_does_not_scale_with_surface_map_cells() {
        use crate::config::Config;
        use crate::room::Room;

        const MAP_CELLS: usize = 50_000;

        let mut g = GameState::new(Config::default());
        let real_before = g.add_room(Room::new(100, 0, String::new(), String::new()));
        for offset in 0..MAP_CELLS {
            let mut map = Room::new(2_000_000 + offset as i32, 0, String::new(), String::new());
            map.map_x = Some((offset + 1) as i32);
            map.map_y = Some(1);
            g.add_room(map);
        }
        let real_after = g.add_room(Room::new(101, 0, String::new(), String::new()));

        assert_eq!(g.rooms.len(), MAP_CELLS + 2);
        assert_eq!(g.non_map_room_rnums(), &[real_before, real_after]);
    }

    #[test]
    fn dg_variable_object_room_lookup_rejects_a_containment_cycle() {
        use crate::config::Config;
        use crate::object::{ObjLoc, Object};

        let mut g = GameState::new(Config::default());
        let first = g.create_obj(Object::new(NOTHING, "first".into(), "first".into()));
        let second = g.create_obj(Object::new(NOTHING, "second".into(), "second".into()));
        g.get_obj_mut(first).unwrap().loc = ObjLoc::Contained(second);
        g.get_obj_mut(second).unwrap().loc = ObjLoc::Contained(first);

        assert_eq!(obj_room(&g, first), None);
    }

    #[test]
    fn wait_until_uses_current_mud_hour_plus_hour_pulse_offset() {
        let pulses_per_hour = 75 * PASSES_PER_SEC as i64;
        let pulse_offset = pulses_per_hour / 2;

        assert_eq!(
            wait_until_delay_from(pulse_offset as u64, 12 * 60, 12, 45),
            pulses_per_hour / 4
        );
        assert_eq!(
            wait_until_delay_from(pulse_offset as u64, 12 * 60, 12, 15),
            (24 * pulses_per_hour) - pulse_offset + (15 * pulses_per_hour / 60)
        );
    }
}

// ---------------------------------------------------------------------------
// End-to-end VM test: build a minimal world + mob, attach a synthetic trigger
// that uses eval/if/set/while + %self.field% substitution, run it, and assert
// the resulting trigger-local variables. Exercises var_subst, eval_expr,
// eval_op, process_if/set/eval, and the control-flow scanners in one pass.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod vm_tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::dg_handler::{DG_TEST_LOCK, TrigData, add_trigger, install_trig};
    use crate::room::Room;
    use std::collections::HashMap;
    use std::sync::MutexGuard;

    /// Serialise the shared DG module-statics across parallel test threads and
    /// return a fresh world + mob. The returned guard must be kept alive for the
    /// test's duration (the caller binds it to `_lock`).
    fn world() -> (MutexGuard<'static, ()>, GameState, CharId) {
        let lock = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        let rn = g.add_room(Room::new(
            3001,
            0,
            "Test Room".into(),
            "A test room.".into(),
        ));
        let mut mob = Character::new_npc(2001);
        mob.player.name = "kobold guard".into();
        mob.short_desc = Some("a kobold guard".into());
        mob.player.level = 7;
        mob.alignment = 50;
        mob.aff_abils.cha = 16;
        let cid = g.create_char(mob);
        g.char_to_room(cid, rn);
        (lock, g, cid)
    }

    fn make_trig(cmds: &[&str]) -> TrigId {
        install_trig(TrigData {
            nr: 0,
            vnum: 9999,
            attach_type: MOB_TRIGGER,
            name: "test".into(),
            trigger_type: 1 << 1,
            narg: 100,
            arglist: String::new(),
            cmdlist: cmds.iter().map(|s| s.to_string()).collect(),
            curr_line: 0,
            depth: 0,
            loops: 0,
            wait_event: None,
            var_list: Vec::new(),
            purged: false,
            loop_origin: HashMap::new(),
        })
    }

    fn make_world_random_trig(global: bool, marker: &str) -> TrigId {
        install_trig(TrigData {
            nr: 0,
            vnum: 10_000,
            attach_type: WLD_TRIGGER,
            name: format!("random room {marker}"),
            trigger_type: WTRIG_RANDOM | if global { WTRIG_GLOBAL } else { 0 },
            narg: 100,
            arglist: String::new(),
            cmdlist: vec![
                format!("set {marker} yes"),
                format!("global {marker}"),
                "halt".into(),
            ],
            curr_line: 0,
            depth: 0,
            loops: 0,
            wait_event: None,
            var_list: Vec::new(),
            purged: false,
            loop_origin: HashMap::new(),
        })
    }

    // Local trigger vars are freed when the trigger finishes (C frees
    // var_list), so the tests promote the computed value to a *global* with
    // `global <var>` before halting, then read it back from the script's
    // persistent global table.
    fn read_global(g: &GameState, cid: CharId, name: &str) -> Option<String> {
        let _ = g;
        dg_handler::get_global_var(ScriptKey::Mob(cid), name)
    }

    #[test]
    fn eval_and_self_fields() {
        let (_lock, mut g, cid) = world();
        let trig = make_trig(&[
            "eval doubled %self.level% * 2",
            "if %self.align% > 0",
            "  set good yes",
            "else",
            "  set good no",
            "end",
            "global doubled",
            "global good",
            "halt",
        ]);
        add_trigger(ScriptKey::Mob(cid), trig, -1);
        script_driver(&mut g, GoRef::Mob(cid), trig, MOB_TRIGGER, TRIG_NEW);
        assert_eq!(read_global(&g, cid, "doubled").as_deref(), Some("14"));
        assert_eq!(read_global(&g, cid, "good").as_deref(), Some("yes"));
    }

    #[test]
    fn while_loop_counts() {
        let (_lock, mut g, cid) = world();
        let trig = make_trig(&[
            "set i 0",
            "while %i% < 3",
            "  eval i %i% + 1",
            "done",
            "global i",
            "halt",
        ]);
        add_trigger(ScriptKey::Mob(cid), trig, -1);
        script_driver(&mut g, GoRef::Mob(cid), trig, MOB_TRIGGER, TRIG_NEW);
        assert_eq!(read_global(&g, cid, "i").as_deref(), Some("3"));
    }

    #[test]
    fn panicking_trigger_restores_all_reentry_latches() {
        let (_lock, mut g, cid) = world();
        g.dg_act_check = true;
        let trig = make_trig(&["__panic_for_test"]);
        add_trigger(ScriptKey::Mob(cid), trig, -1);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            script_driver(&mut g, GoRef::Mob(cid), trig, MOB_TRIGGER, TRIG_NEW)
        }));
        assert!(result.is_err());
        assert!(g.dg_act_check, "act-trigger gate must be restored");
        assert_eq!(SCRIPT_DEPTH.with(|depth| depth.get()), 0);
        assert_eq!(dg_handler::with_trig(trig, |t| t.depth), Some(0));
        assert_eq!(dg_handler::with_trig(trig, |t| t.curr_line), Some(0));

        // A subsequent trigger on the same thread must execute normally.
        let followup = make_trig(&["set survived yes", "global survived", "halt"]);
        add_trigger(ScriptKey::Mob(cid), followup, -1);
        assert_eq!(
            script_driver(&mut g, GoRef::Mob(cid), followup, MOB_TRIGGER, TRIG_NEW),
            1
        );
        assert_eq!(read_global(&g, cid, "survived").as_deref(), Some("yes"));
    }

    #[test]
    fn heartbeat_fires_random_triggers_on_real_rooms_added_after_the_map() {
        let _lock = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        g.add_room(Room::new(100, 1, "Before map".into(), String::new()));
        let mut map = Room::new(2_000_000, 2, "Synthetic map".into(), String::new());
        map.map_x = Some(10);
        map.map_y = Some(20);
        let map_room = g.add_room(map);
        g.map_start_rnum = Some(map_room);
        let local_room = g.add_room(Room::new(101, 3, "OLC local".into(), String::new()));
        let global_room = g.add_room(Room::new(102, 4, "OLC global".into(), String::new()));

        let local = make_world_random_trig(false, "local_fired");
        let global = make_world_random_trig(true, "global_fired");
        let synthetic = make_world_random_trig(true, "map_fired");
        add_trigger(ScriptKey::Room(local_room), local, -1);
        add_trigger(ScriptKey::Room(global_room), global, -1);
        add_trigger(ScriptKey::Room(map_room), synthetic, -1);

        // With no connected player in either zone, only GLOBAL may fire.
        script_trigger_check(&mut g);
        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Room(global_room), "global_fired").as_deref(),
            Some("yes")
        );
        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Room(local_room), "local_fired"),
            None
        );
        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Room(map_room), "map_fired"),
            None
        );

        let mut player = Character::new_player("Watcher".into(), Class::Warrior, Race::Human);
        player.desc = Some(ConnId(77));
        let player = g.create_char(player);
        g.char_to_room(player, local_room);

        // The next eligible heartbeat includes the real post-map room.
        script_trigger_check(&mut g);
        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Room(local_room), "local_fired").as_deref(),
            Some("yes")
        );
        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Room(map_room), "map_fired"),
            None
        );
    }

    #[test]
    fn double_or_pattern_left_to_right() {
        // The `%x% == 1 || 2 || 3` idiom from 2190.trg: with C left-to-right
        // eval, the trailing non-zero literals make the whole thing truthy.
        let (_lock, mut g, cid) = world();
        let trig = make_trig(&[
            "set gib 5",
            "if %gib% == 1 || 2 || 3",
            "  set hit yes",
            "end",
            "global hit",
            "halt",
        ]);
        add_trigger(ScriptKey::Mob(cid), trig, -1);
        script_driver(&mut g, GoRef::Mob(cid), trig, MOB_TRIGGER, TRIG_NEW);
        assert_eq!(read_global(&g, cid, "hit").as_deref(), Some("yes"));
    }

    #[test]
    fn random_numeric_field_zero() {
        let (_lock, g, _cid) = world();
        let trig = make_trig(&["* noop"]);
        let s = var_subst(&g, GoRef::Mob(CharId(1)), trig, "value=%random.0%");
        assert_eq!(s, "value=0");
    }

    #[test]
    fn multibyte_substitution_is_clipped_on_a_char_boundary() {
        let (_lock, mut g, cid) = world();
        let expanded = "€".repeat(100);
        dg_handler::add_global_var(ScriptKey::Mob(cid), "source", &expanded, 0);
        let trig = make_trig(&["set payload %source%", "global payload", "halt"]);
        add_trigger(ScriptKey::Mob(cid), trig, -1);

        script_driver(&mut g, GoRef::Mob(cid), trig, MOB_TRIGGER, TRIG_NEW);

        let payload = read_global(&g, cid, "payload").expect("payload promoted to global");
        assert_eq!(payload, "€".repeat(81));
        assert_eq!(payload.len(), 243);
    }
}
