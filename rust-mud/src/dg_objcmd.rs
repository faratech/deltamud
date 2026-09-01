// dg_objcmd.rs — the o* object script commands (DeltaMUD/CircleMUD dg_objcmd.c).
//
// These are the verbs a running object trigger may execute (`oecho`, `osend`,
// `oload`, `opurge`, `oteleport`, `oforce`, `odamage`, `otimer`, `otransform`,
// …). The DG VM (dg_scripts.rs) tokenises a script line, performs %variable%
// substitution, and — when the first word is not a control keyword — calls
// `dispatch_obj_command(g, obj, line)` here, exactly as C's script_driver()
// dispatches to obj_command_interpreter().
//
// Faithfulness notes:
//   * The command table is matched by *prefix* (C strncmp over the arg length),
//     so "oech" resolves to "oecho" and "oechoa" to "oechoaround" — the first
//     table entry that the typed word is a prefix of wins. The table order is
//     preserved from C so abbreviation resolution is identical.
//   * Echo/send output goes through dg_comm::sub_write (the script-side act()),
//     which renders the ~ @ ^ * (char) and ` (obj) tokens per recipient.
//   * `oecho` uses sub_write with TO_ROOM|TO_CHAR on the first person in the
//     room (matching C: sub_write(arg, world[room].people, TRUE, TO_ROOM|TO_CHAR)).
//   * Entity name lookups honour the UID convention: a name beginning with the
//     UID byte (\x1b) is "<base+id>" and resolves by unique id (find_char/obj);
//     otherwise it is a keyword resolved over the object's carrier/room/world.
//   * `opurge` purging the trigger's own object sets the module-static
//     OWNER_PURGED flag; the VM checks dg_owner_purged() after each command and
//     aborts the script if set (C's `if (dg_owner_purged) return`).
//
// Borrow discipline: every handler takes `&mut GameState` and the owning ObjId.
// All lookups go through GameState's id-indexed finders; nothing holds a borrow
// across a mutation.

use crate::act::{ActArg, To, act};
use crate::dg_comm::{TO_CHAR, TO_ROOM, sub_write};
use crate::dg_handler::ROOM_ID_BASE;
use crate::interpreter::{command_interpreter, indirect_command_target_is_forceable, is_abbrev};
use crate::object::{ObjLoc, ObjectGraphOrder, ObjectType, walk_object_graph};
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;
use std::sync::atomic::{AtomicBool, Ordering};

// do_osend subcmd selectors (dg_objcmd.c).
const SCMD_OSEND: i32 = 0;
const SCMD_OECHOAROUND: i32 = 1;

// UID prefix byte (dg_scripts.h UID_CHAR == '\x1b'). A script var that expands
// to a char/obj is rendered as "<UID_CHAR><id>"; the id is base + GET_ID.
const UID_CHAR: char = '\x1b';

// ---------------------------------------------------------------------------
// dg_owner_purged: set when opurge destroys the object that owns the running
// trigger (C global int dg_owner_purged, defined in dg_scripts.c and set here).
// The VM resets it before each command and checks it after; we expose accessors
// so the VM (dg_scripts.rs) can wire the same protocol without a struct field.
// ---------------------------------------------------------------------------
static OWNER_PURGED: AtomicBool = AtomicBool::new(false);

/// VM hook: clear the self-purge flag before running a command (C zeroes the
/// dg_owner_purged global at the top of the dispatch loop). Mirrors the peer
/// dg_mobcmd.rs API so the VM wires every command module identically.
pub fn clear_owner_purged() {
    OWNER_PURGED.store(false, Ordering::Relaxed);
}

/// VM hook: read-and-clear whether the just-run o* command purged its owner.
pub fn take_owner_purged() -> bool {
    OWNER_PURGED.swap(false, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Logging (obj_log): C prefixes the object short_desc + vnum and forwards to
// script_log. We mirror the message format and route through script_log so
// every DG subsystem logs identically.
// ---------------------------------------------------------------------------
fn obj_log(g: &GameState, obj: ObjId, msg: &str) {
    let (short, vnum) = match g.get_obj(obj) {
        Some(o) => (o.short_description.clone(), o.item_number),
        None => ("<freed>".to_string(), NOTHING),
    };
    log::warn!("Obj ({}, VNum {}): {}", short, vnum, msg);
}

// ---------------------------------------------------------------------------
// Argument tokenisers matching the exact C helpers used in dg_objcmd.c.
// `one_argument`/`two_arguments` use any_one_arg semantics here (DG never wants
// fill-word skipping in these positions — C's one_argument in this file path is
// the punctuation-stopping any_one_name-free variant; we use any_one_arg so a
// bare keyword/UID token survives intact).
// ---------------------------------------------------------------------------
fn one_arg(argument: &str) -> (String, &str) {
    crate::interpreter::any_one_arg(argument)
}
fn two_args(argument: &str) -> (String, String, &str) {
    let (a, rest) = one_arg(argument);
    let (b, rest) = one_arg(rest);
    (a, b, rest)
}

fn is_number(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_digit())
        && crate::text::parse_i32_strict(s).is_ok()
}

/// atoi: leading optional sign + digits, C-style (stops at first non-digit,
/// returns 0 on no digits). Preserves the negative handling odamage relies on.
fn atoi(s: &str) -> Result<i32, crate::text::ParseIntError> {
    crate::text::parse_i32_atoi(s)
}

// ---------------------------------------------------------------------------
// obj_room (dg_objcmd.c): the real room the object — or its carrier/wearer, or
// the container it sits in — currently occupies. Recurses up the in_obj chain.
// ---------------------------------------------------------------------------
fn obj_room(g: &GameState, obj: ObjId) -> Option<RoomRnum> {
    let walk = walk_object_graph(
        [obj],
        ObjectGraphOrder::Preorder,
        "DG object command obj_room",
        |id| {
            g.get_obj(id).map(|object| match object.loc {
                ObjLoc::Contained(parent) => vec![parent],
                _ => Vec::new(),
            })
        },
    );
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

/// cycle_up: the outermost container of `obj` (C cycle_up — follows in_obj).
fn cycle_up(g: &GameState, obj: ObjId) -> ObjId {
    walk_object_graph(
        [obj],
        ObjectGraphOrder::Preorder,
        "DG object command cycle_up",
        |id| {
            g.get_obj(id).map(|object| match object.loc {
                ObjLoc::Contained(parent) => vec![parent],
                _ => Vec::new(),
            })
        },
    )
    .visits
    .last()
    .map(|visit| visit.id)
    .unwrap_or(obj)
}

// ---------------------------------------------------------------------------
// Entity-by-UID helpers (dg_scripts.c find_char/find_obj/find_room). A DG var
// that resolved to a live entity is rendered "<UID_CHAR><base+id>".
// ---------------------------------------------------------------------------
fn parse_uid(name: &str) -> Option<i64> {
    let mut chars = name.chars();
    if chars.next()? == UID_CHAR {
        chars.as_str().trim().parse::<i64>().ok()
    } else {
        None
    }
}

// A DG char/obj UID is the *raw arena id* (this port's GET_ID == CharId/ObjId),
// matching the peer dg_mobcmd.rs convention so the VM's UID strings resolve
// identically across all script-command modules.
fn find_char_by_id(g: &GameState, id: i64) -> Option<CharId> {
    if id <= 0 {
        return None;
    }
    let cid = CharId(id as u64);
    if g.char_exists(cid) { Some(cid) } else { None }
}

fn find_obj_by_id(g: &GameState, id: i64) -> Option<ObjId> {
    if id <= 0 {
        return None;
    }
    let oid = ObjId(id as u64);
    if g.get_obj(oid).is_some() {
        Some(oid)
    } else {
        None
    }
}

fn find_room_by_id(g: &GameState, id: i64) -> Option<RoomRnum> {
    // C get_room()/find_room(): a room UID is ROOM_ID_BASE + vnum. Decode the
    // vnum back out (and tolerate a bare vnum encoding for robustness).
    let vnum = if id >= ROOM_ID_BASE {
        (id - ROOM_ID_BASE) as RoomVnum
    } else {
        id as RoomVnum
    };
    g.real_room(vnum)
}

// ---------------------------------------------------------------------------
// get_char_by_obj (dg_scripts.c): find a character by name, starting with the
// object's carrier/wearer, then the whole character list. UID names resolve by
// id. The C "invis-lev" gate degrades to can_see-from-nobody semantics: NPCs
// always match; an immortal hidden by invis-level is skipped (here, players with
// a positive invis_level).
// ---------------------------------------------------------------------------
fn visible_target(g: &GameState, cid: CharId) -> bool {
    match g.get_char(cid) {
        Some(c) => c.is_npc || c.invis_level == 0,
        None => false,
    }
}

fn get_char_by_obj(g: &GameState, obj: ObjId, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        let cid = find_char_by_id(g, id)?;
        return if visible_target(g, cid) {
            Some(cid)
        } else {
            None
        };
    }

    // Carrier / wearer first.
    match g.get_obj(obj).map(|o| o.loc) {
        Some(ObjLoc::Carried(cid)) | Some(ObjLoc::Worn(cid, _)) => {
            if name_matches_char(g, cid, name) && visible_target(g, cid) {
                return Some(cid);
            }
        }
        _ => {}
    }

    // Whole world (character_list order).
    for cid in g.char_ids() {
        if name_matches_char(g, cid, name) && visible_target(g, cid) {
            return Some(cid);
        }
    }
    None
}

fn name_matches_char(g: &GameState, cid: CharId, name: &str) -> bool {
    g.get_char(cid)
        .map(|c| crate::handler::isname(name, &c.player.name))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// get_obj_by_obj (dg_scripts.c): find an object by name relative to `obj`:
// "self"/"me" -> obj; contents; container/worn/carried/room scope; then the
// whole object list. UID names resolve by id.
// ---------------------------------------------------------------------------
fn get_obj_by_obj(g: &GameState, obj: ObjId, name: &str) -> Option<ObjId> {
    if name.eq_ignore_ascii_case("self") || name.eq_ignore_ascii_case("me") {
        return Some(obj);
    }

    // Its own contents.
    let contains = g
        .get_obj(obj)
        .map(|o| o.contains.clone())
        .unwrap_or_default();
    if let Some(o) = obj_in_list(g, name, &contains) {
        return Some(o);
    }

    // Scope by current location (in_obj / worn_by / carried_by / room).
    match g.get_obj(obj).map(|o| o.loc) {
        Some(ObjLoc::Contained(container)) => {
            if let Some(id) = parse_uid(name) {
                if find_obj_by_id(g, id) == Some(container) {
                    return Some(container);
                }
            } else if name_matches_obj(g, container, name) {
                return Some(container);
            }
        }
        Some(ObjLoc::Worn(cid, _)) => {
            let equip: Vec<ObjId> = g
                .get_char(cid)
                .map(|c| c.equipment.iter().flatten().copied().collect())
                .unwrap_or_default();
            if let Some(o) = obj_in_list(g, name, &equip) {
                return Some(o);
            }
        }
        Some(ObjLoc::Carried(cid)) => {
            let carrying = g
                .get_char(cid)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            if let Some(o) = obj_in_list(g, name, &carrying) {
                return Some(o);
            }
        }
        _ => {
            if let Some(rnum) = obj_room(g, obj) {
                let contents = g.room(rnum).contents.clone();
                if let Some(o) = obj_in_list(g, name, &contents) {
                    return Some(o);
                }
            }
        }
    }

    // Whole object list (object_list order).
    if let Some(id) = parse_uid(name) {
        return find_obj_by_id(g, id);
    }
    for oid in g.obj_ids() {
        if name_matches_obj(g, oid, name) {
            return Some(oid);
        }
    }
    None
}

fn obj_in_list(g: &GameState, name: &str, list: &[ObjId]) -> Option<ObjId> {
    if let Some(id) = parse_uid(name) {
        return list
            .iter()
            .copied()
            .find(|&o| find_obj_by_id(g, id) == Some(o));
    }
    list.iter().copied().find(|&o| name_matches_obj(g, o, name))
}

fn name_matches_obj(g: &GameState, oid: ObjId, name: &str) -> bool {
    g.get_obj(oid)
        .map(|o| crate::handler::isname(name, &o.name))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// find_obj_target_room (dg_objcmd.c): resolve a teleport-destination spec into
// a real room, applying the permission gates (no GODROOM/HOUSE; PRIVATE only if
// empty or single-occupant). cdsr() (map-coordinate shortcut) is not modelled
// (returns NOWHERE), matching the cmd_wizard.rs note.
// ---------------------------------------------------------------------------
fn find_obj_target_room(g: &GameState, obj: ObjId, rawroomstr: &str) -> Option<RoomRnum> {
    let (roomstr, _) = one_arg(rawroomstr);
    if roomstr.is_empty() {
        return None;
    }

    let location: RoomRnum;
    if let Some(id) = parse_uid(&roomstr) {
        // UID room reference.
        location = find_room_by_id(g, id)?;
    } else if is_number(&roomstr) {
        location = g.real_room(atoi(&roomstr).ok()?)?;
    } else if let Some(cid) = get_char_by_obj(g, obj, &roomstr) {
        location = g.get_char(cid)?.in_room?;
    } else if let Some(tobj) = get_obj_by_obj(g, obj, &roomstr) {
        location = match g.get_obj(tobj)?.loc {
            ObjLoc::Room(r) => r,
            _ => return None,
        };
    } else {
        return None;
    }

    // Permission gates (GODROOM / HOUSE forbidden; PRIVATE if >1 occupant).
    let room = g.room_opt(location)?;
    if room.room_flags.contains(RoomFlags::GODROOM) || room.room_flags.contains(RoomFlags::HOUSE) {
        return None;
    }
    if room.room_flags.contains(RoomFlags::PRIVATE) && room.people.len() > 1 {
        return None;
    }
    Some(location)
}

// ===========================================================================
// Object command handlers
// ===========================================================================

/// oecho: echo a message to everyone in the object's room (sub_write).
fn do_oecho(g: &mut GameState, obj: ObjId, argument: &str) {
    let argument = argument.trim_start();
    if argument.is_empty() {
        obj_log(g, obj, "oecho called with no args");
        return;
    }
    match obj_room(g, obj) {
        Some(rnum) => {
            // C uses world[room].people (the first occupant) as the sub_write
            // anchor with TO_ROOM|TO_CHAR; with no occupants there is nothing
            // to send.
            if let Some(&anchor) = g.room(rnum).people.first() {
                sub_write(g, argument, anchor, true, TO_ROOM | TO_CHAR);
            }
        }
        None => obj_log(g, obj, "oecho called by object in NOWHERE"),
    }
}

/// oforce: make a target (or everyone in the room) execute a command.
fn do_oforce(g: &mut GameState, obj: ObjId, argument: &str) {
    let (arg1, line) = one_arg(argument);
    let line = line.trim_start();
    if arg1.is_empty() || line.is_empty() {
        obj_log(g, obj, "oforce called with too few args");
        return;
    }

    if arg1.eq_ignore_ascii_case("all") {
        match obj_room(g, obj) {
            None => obj_log(g, obj, "oforce called by object in NOWHERE"),
            Some(rnum) => {
                let people = g.room(rnum).people.clone();
                for ch in people {
                    if g.char_exists(ch) && !is_immortal(g, ch) {
                        command_interpreter(g, ch, line);
                    }
                }
            }
        }
    } else if let Some(ch) = get_char_by_obj(g, obj, &arg1) {
        if !is_immortal(g, ch) {
            command_interpreter(g, ch, line);
        }
    } else {
        obj_log(g, obj, "oforce: no target found");
    }
}

/// osend / oechoaround: send a message to a named character (or those around).
fn do_osend(g: &mut GameState, obj: ObjId, argument: &str, subcmd: i32) {
    let (buf, msg) = one_arg(argument);
    if buf.is_empty() {
        obj_log(g, obj, "osend called with no args");
        return;
    }
    let msg = msg.trim_start();
    if msg.is_empty() {
        obj_log(g, obj, "osend called without a message");
        return;
    }

    if let Some(ch) = get_char_by_obj(g, obj, &buf) {
        if subcmd == SCMD_OSEND {
            sub_write(g, msg, ch, true, TO_CHAR);
        } else if subcmd == SCMD_OECHOAROUND {
            sub_write(g, msg, ch, true, TO_ROOM);
        }
    } else {
        obj_log(g, obj, "no target found for osend");
    }
}

/// oexp: grant experience to a named character.
fn do_oexp(g: &mut GameState, obj: ObjId, argument: &str) {
    let (name, amount, _) = two_args(argument);
    if name.is_empty() || amount.is_empty() {
        obj_log(g, obj, "oexp: too few arguments");
        return;
    }
    if let Some(ch) = get_char_by_obj(g, obj, &name) {
        let amount = match atoi(&amount) {
            Ok(amount) => amount,
            Err(_) => {
                obj_log(g, obj, "oexp: amount outside supported range");
                return;
            }
        };
        crate::limits::gain_exp(g, ch, amount as i64);
    } else {
        obj_log(g, obj, "oexp: target not found");
    }
}

/// otimer: set the object's timer value.
fn do_otimer(g: &mut GameState, obj: ObjId, argument: &str) {
    let (arg, _) = one_arg(argument);
    if arg.is_empty() {
        obj_log(g, obj, "otimer: missing argument");
    } else if !arg
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        obj_log(g, obj, "otimer: bad argument");
    } else {
        let timer = match atoi(&arg) {
            Ok(timer) => timer,
            Err(_) => {
                obj_log(g, obj, "otimer: argument outside supported range");
                return;
            }
        };
        if let Some(o) = g.get_obj_mut(obj) {
            o.timer = timer;
        }
    }
}

/// otransform: morph the object into a different prototype, preserving its
/// location, id, and worn state (C do_otransform).
fn do_otransform(g: &mut GameState, obj: ObjId, argument: &str) {
    let (arg, _) = one_arg(argument);
    if arg.is_empty() {
        obj_log(g, obj, "otransform: missing argument");
        return;
    }
    if !arg
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        obj_log(g, obj, "otransform: bad argument");
        return;
    }

    let vnum = match atoi(&arg) {
        Ok(vnum) => vnum,
        Err(_) => {
            obj_log(g, obj, "otransform: argument outside supported range");
            return;
        }
    };
    // Load the prototype into a temporary, copy its prototype-derived fields
    // onto the existing object (keeping id/loc/contains/worn state), then drop
    // the temporary. read_object(vnum, VIRTUAL).
    let tmp = match g.load_object(vnum) {
        Some(t) => t,
        None => {
            obj_log(g, obj, "otransform: bad object vnum");
            return;
        }
    };

    // If worn, unequip first (C unequip_char), remember the slot/wearer.
    let worn_slot = match g.get_obj(obj).map(|o| o.loc) {
        Some(ObjLoc::Worn(cid, pos)) => {
            g.unequip_char(cid, pos);
            Some((cid, pos))
        }
        _ => None,
    };

    // Snapshot the prototype-derived fields from the temporary object.
    let proto = {
        let t = g.get_obj(tmp).expect("just loaded");
        ProtoSnapshot {
            item_number: t.item_number,
            name: t.name.clone(),
            description: t.description.clone(),
            short_description: t.short_description.clone(),
            action_description: t.action_description.clone(),
            obj_type: t.obj_type,
            wear_flags: t.wear_flags,
            extra_flags: t.extra_flags,
            weight: t.weight,
            cost: t.cost,
            rent: t.rent,
            level: t.level,
            timer: t.timer,
            values: t.values,
            affects: t.affects.clone(),
        }
    };

    // Overlay onto the existing object (preserve id, loc, contains).
    if let Some(o) = g.get_obj_mut(obj) {
        o.item_number = proto.item_number;
        o.name = proto.name;
        o.description = proto.description;
        o.short_description = proto.short_description;
        o.action_description = proto.action_description;
        o.obj_type = proto.obj_type;
        o.wear_flags = proto.wear_flags;
        o.extra_flags = proto.extra_flags;
        o.weight = proto.weight;
        o.cost = proto.cost;
        o.rent = proto.rent;
        o.level = proto.level;
        o.timer = proto.timer;
        o.values = proto.values;
        o.affects = proto.affects;
    }

    // Re-equip if it was worn.
    if let Some((cid, pos)) = worn_slot {
        g.equip_char(cid, obj, pos);
    }

    // Discard the temporary load.
    g.extract_obj(tmp);
}

struct ProtoSnapshot {
    item_number: ObjVnum,
    name: String,
    description: String,
    short_description: String,
    action_description: Option<String>,
    obj_type: ObjectType,
    wear_flags: crate::object::WearFlags,
    extra_flags: crate::object::ExtraFlags,
    weight: i32,
    cost: i32,
    rent: i32,
    level: Level,
    timer: i32,
    values: [i32; 4],
    affects: Vec<crate::object::ObjectAffect>,
}

/// opurge: purge all NPCs+objects in the room (no arg) or a single target.
fn do_opurge(g: &mut GameState, obj: ObjId, argument: &str) {
    let (arg, _) = one_arg(argument);

    if arg.is_empty() {
        if let Some(rnum) = obj_room(g, obj) {
            let people = g.room(rnum).people.clone();
            for ch in people {
                if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
                    crate::dg_handler::on_char_extracted(g, ch);
                    g.extract_char(ch);
                }
            }
            let contents = g.room(rnum).contents.clone();
            for o in contents {
                if o != obj {
                    crate::dg_handler::on_obj_extracted(g, o);
                    g.extract_obj(o);
                }
            }
        }
        return;
    }

    if let Some(ch) = get_char_by_obj(g, obj, &arg) {
        if !g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
            obj_log(g, obj, "opurge: purging a PC");
            return;
        }
        crate::dg_handler::on_char_extracted(g, ch);
        g.extract_char(ch);
    } else if let Some(o) = get_obj_by_obj(g, obj, &arg) {
        if o == obj {
            // The trigger purged its own object: flag it for the VM so it stops
            // running the script (C `dg_owner_purged = 1`). We set both our
            // module-static (read via take_owner_purged) and the VM's
            // thread-local (set_owner_purged), so the integration works whether
            // the VM polls our flag or relies on the shared one.
            OWNER_PURGED.store(true, Ordering::Relaxed);
            crate::dg_scripts::set_owner_purged();
        }
        crate::dg_handler::on_obj_extracted(g, o);
        g.extract_obj(o);
    } else {
        obj_log(g, obj, "opurge: bad argument");
    }
}

/// oteleport: move a named character (or everyone in the room) to a room.
fn do_oteleport(g: &mut GameState, obj: ObjId, argument: &str) {
    let (arg1, arg2, _) = two_args(argument);
    if arg1.is_empty() || arg2.is_empty() {
        obj_log(g, obj, "oteleport called with too few args");
        return;
    }

    let target = match find_obj_target_room(g, obj, &arg2) {
        Some(t) => t,
        None => {
            obj_log(g, obj, "oteleport target is an invalid room");
            return;
        }
    };

    if arg1.eq_ignore_ascii_case("all") {
        let rm = obj_room(g, obj);
        if rm == Some(target) {
            obj_log(g, obj, "oteleport target is itself");
        }
        // C iterates regardless of the self-target warning above.
        if let Some(rm) = rm {
            let people = g.room(rm).people.clone();
            for ch in people {
                g.char_from_room(ch);
                g.char_to_room(ch, target);
            }
        }
    } else if let Some(ch) = get_char_by_obj(g, obj, &arg1) {
        g.char_from_room(ch);
        g.char_to_room(ch, target);
    } else {
        obj_log(g, obj, "oteleport: no target found");
    }
}

/// oload: load a mob or object into the object's room (fires the LOAD trigger).
fn do_dgoload(g: &mut GameState, obj: ObjId, argument: &str) {
    let (arg1, arg2, _) = two_args(argument);
    if arg1.is_empty() || arg2.is_empty() || !is_number(&arg2) {
        obj_log(g, obj, "oload: bad syntax");
        return;
    }
    let number = match crate::text::parse_i32_strict(&arg2) {
        Ok(number) if number >= 0 => number,
        _ => {
            obj_log(g, obj, "oload: bad syntax");
            return;
        }
    };

    let room = match obj_room(g, obj) {
        Some(r) => r,
        None => {
            obj_log(g, obj, "oload: object in NOWHERE trying to load");
            return;
        }
    };

    if is_abbrev(&arg1, "mob") {
        match g.load_mobile(number) {
            Some(mob) => {
                g.char_to_room(mob, room);
                crate::dg_triggers::load_mtrigger(g, mob);
            }
            None => obj_log(g, obj, "oload: bad mob vnum"),
        }
    } else if is_abbrev(&arg1, "obj") {
        match g.load_object(number) {
            Some(object) => {
                g.obj_to_room(object, room);
                crate::dg_triggers::load_otrigger(g, object);
            }
            None => obj_log(g, obj, "oload: bad object vnum"),
        }
    } else {
        obj_log(g, obj, "oload: bad type");
    }
}

/// odamage: deal (or heal, with a negative amount) HP to a target or to all in
/// the room of the object's outermost carrier/wearer (C do_odamage). On death,
/// the body is corpse'd and extracted/respawned exactly as a combat death.
fn do_odamage(g: &mut GameState, obj: ObjId, argument: &str) {
    let (name, amount, _) = two_args(argument);
    // Bad syntax: missing args, or a non-numeric amount that is not a "-digit".
    let amount_ok = {
        let bytes: Vec<char> = amount.chars().collect();
        let first = bytes.first().copied();
        first.map(|c| c.is_ascii_digit()).unwrap_or(false)
            || (first == Some('-') && bytes.get(1).map(|c| c.is_ascii_digit()).unwrap_or(false))
    };
    if name.is_empty() || amount.is_empty() || !amount_ok {
        obj_log(g, obj, "odamage: bad syntax");
        return;
    }
    let dam = match atoi(&amount) {
        Ok(dam) => dam,
        Err(_) => {
            obj_log(g, obj, "odamage: amount outside supported range");
            return;
        }
    };

    // Build the target set. "all" -> everyone in the room of the outermost
    // container's carrier/wearer; otherwise the single named target.
    // C dg_objcmd.c:502-510: for 'all', only an object that is carried or
    // worn has a room; an object LYING ON THE FLOOR resolves in_room == -1
    // and 'odamage all' does NOTHING (the Rust arm added ObjLoc::Room and
    // damaged the whole room instead - #150).
    let all = name.eq_ignore_ascii_case("all");
    let targets: Vec<CharId> = if all {
        let top = cycle_up(g, obj);
        let in_room = match g.get_obj(top).map(|o| o.loc) {
            Some(ObjLoc::Worn(cid, _)) | Some(ObjLoc::Carried(cid)) => {
                g.get_char(cid).and_then(|c| c.in_room)
            }
            _ => None,
        };
        match in_room {
            Some(rnum) => g.room(rnum).people.clone(),
            None => return,
        }
    } else {
        match get_char_by_obj(g, obj, &name) {
            Some(ch) => vec![ch],
            None => {
                obj_log(g, obj, "odamage: target not found");
                return;
            }
        }
    };

    // Killer credit: the object's carrier/wearer (the death log attribution).
    let killer = match g.get_obj(obj).map(|o| o.loc) {
        Some(ObjLoc::Carried(cid)) | Some(ObjLoc::Worn(cid, _)) => Some(cid),
        _ => None,
    };

    for ch in targets {
        if !g.char_exists(ch) {
            continue;
        }
        apply_script_damage(g, ch, dam, killer);
    }
}

/// dispatch_obj_command: the entry point the script VM calls (analogue of C
/// obj_command_interpreter). `argument` is the full command line for one trigger
/// step, with %variables% already expanded by the VM (entity refs survive as
/// UID strings). The leading verb is split off and prefix-matched against the
/// table in declaration order, exactly as C's strncmp loop does — so "oech"
/// resolves to "oecho".
///
/// Unlike mob commands, object commands have no fall-through to player commands:
/// an unknown verb is logged (C "Unknown object cmd"). Returns true if a
/// recognised o* verb ran (the VM ignores this for obj triggers and learns
/// about self-purges via take_owner_purged()).
pub fn dispatch_obj_command(g: &mut GameState, obj: ObjId, argument: &str) -> bool {
    let argument = argument.trim_start();
    if argument.is_empty() {
        return false;
    }
    let (cmd, line) = one_arg(argument);
    if cmd.is_empty() {
        return false;
    }
    // Prefix match against the table, in declaration order (C strncmp loop).
    match OBJ_CMDS.iter().find(|e| e.0.starts_with(cmd.as_str())) {
        Some(e) => {
            (e.1)(g, obj, line, e.2);
            true
        }
        None => {
            obj_log(g, obj, &format!("Unknown object cmd: '{}'", argument));
            false
        }
    }
}

// Command table (preserves C obj_cmd_info order for prefix-match parity).
type ObjFn = fn(&mut GameState, ObjId, &str, i32);
struct ObjCmd(&'static str, ObjFn, i32);
const OBJ_CMDS: &[ObjCmd] = &[
    ObjCmd("oecho", |g, o, a, _| do_oecho(g, o, a), 0),
    ObjCmd(
        "oechoaround",
        |g, o, a, sc| do_osend(g, o, a, sc),
        SCMD_OECHOAROUND,
    ),
    ObjCmd("oexp", |g, o, a, _| do_oexp(g, o, a), 0),
    ObjCmd("oforce", |g, o, a, _| do_oforce(g, o, a), 0),
    ObjCmd("oload", |g, o, a, _| do_dgoload(g, o, a), 0),
    ObjCmd("opurge", |g, o, a, _| do_opurge(g, o, a), 0),
    ObjCmd("osend", |g, o, a, sc| do_osend(g, o, a, sc), SCMD_OSEND),
    ObjCmd("oteleport", |g, o, a, _| do_oteleport(g, o, a), 0),
    ObjCmd("odamage", |g, o, a, _| do_odamage(g, o, a), 0),
    ObjCmd("otimer", |g, o, a, _| do_otimer(g, o, a), 0),
    ObjCmd("otransform", |g, o, a, _| do_otransform(g, o, a), 0),
];

// ---------------------------------------------------------------------------
// Shared script-damage routine (the body of do_odamage / do_wdamage). Applies
// raw HP loss, recomputes position, emits the exact C position-change messages,
// and resolves death (corpse + extract / respawn). Immortals sidestep positive
// damage. A negative `dam` heals. This deliberately does NOT route through
// combat::damage (no to-hit, no retaliation, no "hits/misses" message) so it
// matches C do_odamage/do_wdamage exactly.
// ---------------------------------------------------------------------------
pub(crate) fn apply_script_damage(g: &mut GameState, ch: CharId, dam: i32, killer: Option<CharId>) {
    if is_immortal(g, ch) {
        if dam > 0 {
            g.send_to_char(
                ch,
                "Being the cool immortal you are, you sidestep a trap, obviously placed to kill you.\r\n",
            );
        }
        return;
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.points.hit -= dam;
    }
    update_pos(g, ch);

    let pos = g
        .get_char(ch)
        .map(|c| c.position)
        .unwrap_or(Position::Standing);
    match pos {
        Position::MortallyWounded => {
            act(
                g,
                "$n is mortally wounded, and will die soon, if not aided.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You are mortally wounded, and will die soon, if not aided.\r\n",
            );
        }
        Position::Incapacitated => {
            act(
                g,
                "$n is incapacitated and will slowly die, if not aided.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You are incapacitated an will slowly die, if not aided.\r\n",
            );
        }
        Position::Stunned => {
            act(
                g,
                "$n is stunned, but will probably regain consciousness again.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You're stunned, but will probably regain consciousness again.\r\n",
            );
        }
        Position::Dead => {
            act(
                g,
                "$n is dead!  R.I.P.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(ch, "You are dead!  Sorry...\r\n");
        }
        _ => {
            // >= POS_SLEEPING: the "really HURT" / "BLEEDING" feedback.
            let (hit, max_hit) = g
                .get_char(ch)
                .map(|c| (c.points.hit, c.points.max_hit))
                .unwrap_or((0, 0));
            if dam > (max_hit >> 2) {
                act(
                    g,
                    "That really did HURT!",
                    false,
                    ch,
                    None,
                    ActArg::None,
                    To::Char,
                );
            }
            if hit < (max_hit >> 2) {
                g.send_to_char(
                    ch,
                    "&RYou wish that your wounds would stop BLEEDING so much!&n\r\n",
                );
            }
        }
    }

    if g.get_char(ch)
        .map(|c| c.position == Position::Dead)
        .unwrap_or(false)
    {
        // mudlog the kill for PCs (best-effort; the C log line attributes the
        // object owner). NPC bodies are extracted; PCs respawn (env_die path).
        let is_pc = g.get_char(ch).map(|c| !c.is_npc).unwrap_or(false);
        if is_pc {
            let who = g
                .get_char(ch)
                .map(|c| c.player.name.clone())
                .unwrap_or_default();
            let killer_name = killer
                .and_then(|k| g.get_char(k).map(|c| c.player.name.clone()))
                .unwrap_or_else(|| "a trap".to_string());
            log::warn!("{} killed by {}", who, killer_name);
        }
        script_die(g, ch, killer);
    }
}

/// update_pos (fight.c): recompute a downed character's position from HP. Only
/// changes position when HP <= 0 or already below POS_STUNNED (matches C).
fn update_pos(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        let hp = c.points.hit;
        if hp > 0 && c.position > Position::Stunned {
            return;
        }
        c.position = if hp > 0 {
            Position::Standing
        } else if hp <= -11 {
            Position::Dead
        } else if hp <= -6 {
            Position::MortallyWounded
        } else if hp <= -3 {
            Position::Incapacitated
        } else {
            Position::Stunned
        };
    }
}

/// die(ch, killer) (fight.c): exp penalty + raw_kill. Mirrors limits::env_die's
/// no-killer path (the killer only matters for death triggers, which the body
/// extraction here already accounts for). Produces an identical corpse so DG and
/// combat deaths look the same in the world.
fn script_die(g: &mut GameState, ch: CharId, killer: Option<CharId>) {
    let (exp, level, is_npc) = match g.get_char(ch) {
        Some(c) => (c.points.exp, c.player.level as i32, c.is_npc),
        None => return,
    };
    // gain_exp(ch, -(GET_EXP - exp_to_level(level-1))/4).
    let penalty = -((exp - crate::limits::exp_to_level(level - 1)) / 4);
    crate::limits::gain_exp(g, ch, penalty);
    if !g.char_exists(ch) {
        return;
    }

    // C dg_objcmd.c:561-562 routes the kill through die(ch, carrier/wearer),
    // so a script-trap kill credits the object's carrier/wearer with XP and
    // the PK flag path. The port dropped the killer entirely (#150).
    if let (Some(k), true) = (killer, is_npc) {
        if k != ch {
            crate::combat::award_kill_experience(g, k, ch);
        }
    }

    if !is_npc {
        if let Some(c) = g.get_char_mut(ch) {
            const PLR_KILLER: i64 = 1 << 0;
            const PLR_THIEF: i64 = 1 << 1;
            c.act_flags &= !(PLR_KILLER | PLR_THIEF);
            c.conditions[FULL] = 0;
            c.conditions[THIRST] = 0;
            c.conditions[DRUNK] = 0;
        }
    }

    // raw_kill: stop fighting, strip affects, death cry, corpse, extract/respawn.
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
        c.affected.clear();
    }
    g.affect_total(ch);

    // death_cry: this room + a generic cry into every open-exit neighbour.
    crate::combat::death_cry(g, ch);

    let rnum = g.get_char(ch).and_then(|c| c.in_room);
    if let Some(rnum) = rnum {
        let name = g
            .get_char(ch)
            .map(|c| c.display_for_others())
            .unwrap_or_default();
        let corpse = make_corpse(g, &name, ch);
        let carried = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        for oid in carried {
            g.obj_from_anywhere(oid);
            g.obj_to_obj(oid, corpse);
        }
        let worn: Vec<usize> = (0..NUM_WEARS)
            .filter(|&p| {
                g.get_char(ch)
                    .map(|c| c.equipment[p].is_some())
                    .unwrap_or(false)
            })
            .collect();
        for p in worn {
            if let Some(oid) = g.unequip_char(ch, p) {
                g.obj_to_obj(oid, corpse);
            }
        }
        g.obj_to_room(corpse, rnum);
    }

    if is_npc {
        crate::dg_handler::on_char_extracted(g, ch);
        crate::mobact::clear_memory(ch);
        crate::arena::forget_char(ch);
        g.extract_char(ch);
    } else {
        respawn_pc(g, ch);
    }
}

/// make_corpse: a corpse container holding the victim's effects (identical to
/// the limits.rs / combat.rs corpse so all deaths look the same).
fn make_corpse(g: &mut GameState, who: &str, victim: CharId) -> ObjId {
    use crate::object::Object;
    let mut obj = Object::new(
        NOTHING,
        format!("corpse {}", who),
        format!("the corpse of {}", who),
    );
    obj.description = format!("The corpse of {} is lying here.", who);
    obj.obj_type = ObjectType::Container;
    crate::combat::apply_corpse_metadata(&mut obj, g, victim);
    // C fight.c:315-318: GET_OBJ_TIMER(corpse) = IS_NPC(ch) ?
    // max_npc_corpse_time (5) : max_pc_corpse_time (10) (config.c:120-121),
    // decremented once per mud hour by point_update. The flat 60 made
    // corpses persist 6-12x longer than C (#102).
    obj.timer = if g.get_char(victim).map(|c| c.is_npc).unwrap_or(true) {
        5
    } else {
        10
    };
    obj.values = [0, 0, 0, 1]; // values[3] != 0 marks it a corpse for decay.
    obj.loc = ObjLoc::Nowhere;
    g.create_obj(obj)
}

/// respawn_pc: send a slain PC home at 1 HP (Tier-0 death handling).
fn respawn_pc(g: &mut GameState, victim: CharId) {
    g.char_from_room(victim);
    let home = g
        .get_char(victim)
        .map(|c| c.player.hometown)
        .unwrap_or(3001);
    let rnum = g.real_room(home).or_else(|| g.real_room(3001)).unwrap_or(0);
    if let Some(c) = g.get_char_mut(victim) {
        c.points.hit = 1;
        c.position = Position::Resting;
        c.fighting = None;
    }
    g.char_to_room(victim, rnum);
}

/// Staff principals (including switched or quarantined sessions) are spared
/// script damage and forced commands regardless of display level.
fn is_immortal(g: &GameState, ch: CharId) -> bool {
    !indirect_command_target_is_forceable(g, ch)
}

#[cfg(test)]
mod object_ancestry_tests {
    use super::*;
    use crate::config::Config;
    use crate::object::Object;
    use crate::room::Room;

    #[test]
    fn dg_object_ancestry_is_cycle_safe_and_depth_bounded() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(700, 7, "DG room".into(), String::new()));

        let a = g.create_obj(Object::new(NOTHING, "a".into(), "a".into()));
        let b = g.create_obj(Object::new(NOTHING, "b".into(), "b".into()));
        g.get_obj_mut(a).unwrap().loc = ObjLoc::Contained(b);
        g.get_obj_mut(b).unwrap().loc = ObjLoc::Contained(a);
        assert_eq!(obj_room(&g, a), None);
        assert_eq!(cycle_up(&g, a), b);

        let chain: Vec<ObjId> = (0..crate::object::MAX_OBJECT_GRAPH_DEPTH + 5)
            .map(|index| {
                g.create_obj(Object::new(
                    NOTHING,
                    format!("deep {index}"),
                    format!("deep {index}"),
                ))
            })
            .collect();
        for pair in chain.windows(2) {
            g.get_obj_mut(pair[0]).unwrap().loc = ObjLoc::Contained(pair[1]);
        }
        g.get_obj_mut(*chain.last().unwrap()).unwrap().loc = ObjLoc::Room(room);

        // The room and outermost container lie beyond the shared safety bound.
        assert_eq!(obj_room(&g, chain[0]), None);
        assert_eq!(
            cycle_up(&g, chain[0]),
            chain[crate::object::MAX_OBJECT_GRAPH_DEPTH - 1]
        );
    }
}
