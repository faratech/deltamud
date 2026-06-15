// dg_wldcmd.rs — the w* world/room script commands (DeltaMUD/CircleMUD
// dg_wldcmd.c).
//
// These are the verbs a running *room* trigger may execute (`wecho`, `wsend`,
// `wload`, `wpurge`, `wteleport`, `wforce`, `wdamage`, `wdoor`, `wzoneecho`,
// `wat`, `wasound`, …). The DG VM (dg_scripts.rs) substitutes %variables% in a
// script line and, for non-control verbs, calls `dispatch_wld_command(g, rnum,
// line)` here — the analogue of C's wld_command_interpreter(room, cmd).
//
// The room is identified by its real number (RoomRnum == index into
// GameState::rooms), matching C's `room_data *` which the VM passes as `go`.
//
// Faithfulness notes:
//   * Output uses act() for the room-wide `wecho`/`wasound` (C act_to_room —
//     a plain FALSE-hide act sent TO_ROOM *and* TO_CHAR for the room's first
//     occupant), and dg_comm::sub_write for the targeted `wsend`/`wechoaround`.
//   * `wdoor` edits the *destination* room's exit in the given direction:
//     purge / description / flags / key / name / room. Flag parsing reuses the
//     CircleMUD asciiflag_conv letter encoding.
//   * `wat` re-enters the world command interpreter with `room` switched to the
//     vnum-named room (recursive dispatch_wld_command).
//   * `wdamage` shares the exact damage/death body with dg_objcmd (no killer).
//
// Borrow discipline: handlers take `&mut GameState` + the RoomRnum; all entity
// access is through GameState's id-indexed finders.

use crate::act::{act, ActArg, To};
use crate::dg_comm::{send_to_zone, sub_write, TO_CHAR, TO_ROOM};
use crate::dg_handler::ROOM_ID_BASE;
use crate::interpreter::{command_interpreter, is_abbrev, search_block};
use crate::object::ObjLoc;
use crate::room::Exit;
use crate::state::GameState;
use crate::types::*;

// do_wsend subcmd selectors (dg_wldcmd.c).
const SCMD_WSEND: i32 = 0;
const SCMD_WECHOAROUND: i32 = 1;

const UID_CHAR: char = '\x1b';

// ---------------------------------------------------------------------------
// Logging (wld_log): C prefixes the room number and forwards to script_log.
// ---------------------------------------------------------------------------
fn wld_log(g: &GameState, room: RoomRnum, msg: &str) {
    let vnum = g.room_opt(room).map(|r| r.number).unwrap_or(NOWHERE);
    log::warn!("Wld (room {}): {}", vnum, msg);
}

// ---------------------------------------------------------------------------
// Argument tokenisers (any_one_arg semantics — DG never wants fill-word
// skipping in these positions, so a bare keyword/UID token survives intact).
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
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let mut chars = s.chars().peekable();
    let mut sign = 1i64;
    if let Some(&c) = chars.peek() {
        if c == '-' {
            sign = -1;
            chars.next();
        } else if c == '+' {
            chars.next();
        }
    }
    let mut val: i64 = 0;
    for c in chars {
        if let Some(d) = c.to_digit(10) {
            val = val * 10 + d as i64;
        } else {
            break;
        }
    }
    (sign * val) as i32
}

fn parse_uid(name: &str) -> Option<i64> {
    let mut chars = name.chars();
    if chars.next()? == UID_CHAR {
        chars.as_str().trim().parse::<i64>().ok()
    } else {
        None
    }
}

fn visible_target(g: &GameState, cid: CharId) -> bool {
    match g.get_char(cid) {
        Some(c) => c.is_npc || c.invis_level == 0,
        None => false,
    }
}

// A DG char/obj UID is the raw arena id (peer-consistent with dg_mobcmd.rs).
fn find_char_by_id(g: &GameState, id: i64) -> Option<CharId> {
    if id <= 0 {
        return None;
    }
    let cid = CharId(id as u64);
    if g.char_exists(cid) {
        Some(cid)
    } else {
        None
    }
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

fn name_matches_char(g: &GameState, cid: CharId, name: &str) -> bool {
    g.get_char(cid)
        .map(|c| crate::handler::isname(name, &c.player.name))
        .unwrap_or(false)
}
fn name_matches_obj(g: &GameState, oid: ObjId, name: &str) -> bool {
    g.get_obj(oid).map(|o| crate::handler::isname(name, &o.name)).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// get_char_by_room (dg_scripts.c): find a character by name, room first then
// the whole world. UID names resolve by id.
// ---------------------------------------------------------------------------
fn get_char_by_room(g: &GameState, room: RoomRnum, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        let cid = find_char_by_id(g, id)?;
        return if visible_target(g, cid) { Some(cid) } else { None };
    }
    // Room occupants first.
    if let Some(r) = g.room_opt(room) {
        for &cid in &r.people {
            if name_matches_char(g, cid, name) && visible_target(g, cid) {
                return Some(cid);
            }
        }
    }
    // Whole world.
    for &cid in &g.char_list {
        if name_matches_char(g, cid, name) && visible_target(g, cid) {
            return Some(cid);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// get_obj_by_room (dg_scripts.c): find an object by name, room contents first
// then the whole object list. UID names resolve by id.
// ---------------------------------------------------------------------------
fn get_obj_by_room(g: &GameState, room: RoomRnum, name: &str) -> Option<ObjId> {
    if let Some(id) = parse_uid(name) {
        if let Some(r) = g.room_opt(room) {
            for &oid in &r.contents {
                if find_obj_by_id(g, id) == Some(oid) {
                    return Some(oid);
                }
            }
        }
        return find_obj_by_id(g, id);
    }
    if let Some(r) = g.room_opt(room) {
        for &oid in &r.contents {
            if name_matches_obj(g, oid, name) {
                return Some(oid);
            }
        }
    }
    for &oid in &g.obj_list {
        if name_matches_obj(g, oid, name) {
            return Some(oid);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// get_room (dg_scripts.c): resolve a room spec (UID or vnum) into a real room.
// ---------------------------------------------------------------------------
fn get_room(g: &GameState, name: &str) -> Option<RoomRnum> {
    if let Some(id) = parse_uid(name) {
        return find_room_by_id(g, id);
    }
    g.real_room(atoi(name))
}

/// C get_room()/find_room(): a room UID is ROOM_ID_BASE + vnum (tolerating a
/// bare-vnum encoding for robustness).
fn find_room_by_id(g: &GameState, id: i64) -> Option<RoomRnum> {
    let vnum = if id >= ROOM_ID_BASE {
        (id - ROOM_ID_BASE) as RoomVnum
    } else {
        id as RoomVnum
    };
    g.real_room(vnum)
}

/// cdsr() (maputils.c) — "coordinate destination string -> rnum". The string is
/// a run of digits optionally split by a single lowercase 'x' separating an X
/// and Y map coordinate (e.g. "12x34"). A bare number is treated as a vnum.
/// Returns None (the C NOWHERE/-1) on any malformed string or out-of-range
/// coordinate. Coordinates are NOT wrapped here (the C rejects out-of-range),
/// unlike map_coords_to_rnum — but the bounds check below guarantees the wrap
/// loops in that helper are no-ops, so it resolves identically.
fn cdsr(g: &GameState, string: &str) -> Option<RoomRnum> {
    let mut xf = false;
    let mut x: i32 = -1;
    let mut tmp = String::new();
    for ch in string.chars() {
        if ch.is_ascii_digit() {
            tmp.push(ch);
        } else if ch == 'x' && !xf {
            if tmp.is_empty() {
                return None; // No X coordinate supplied.
            }
            xf = true;
            x = atoi(&tmp);
            tmp.clear();
        } else {
            return None;
        }
    }
    if tmp.is_empty() {
        return None; // No (Y) coordinate supplied.
    }
    if !xf {
        // Argument was entirely a number -> assume a vnum, return the rnum.
        return g.real_room(atoi(&tmp) as RoomVnum);
    }
    let y = atoi(&tmp);
    if x < 1 || x > g.max_map_x || y < 1 || y > g.max_map_y {
        return None;
    }
    g.map_coords_to_rnum(x, y)
}

// ---------------------------------------------------------------------------
// Whole-world visible-char / visible-obj finders (C get_char_vis(NULL, …) /
// get_obj_vis(NULL, …)) used by wteleport. NULL viewer => no visibility gate.
// ---------------------------------------------------------------------------
fn get_char_vis_world(g: &GameState, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        return find_char_by_id(g, id);
    }
    for &cid in &g.char_list {
        if name_matches_char(g, cid, name) && visible_target(g, cid) {
            return Some(cid);
        }
    }
    None
}
fn get_obj_vis_world(g: &GameState, name: &str) -> Option<ObjId> {
    if let Some(id) = parse_uid(name) {
        return find_obj_by_id(g, id);
    }
    for &oid in &g.obj_list {
        if name_matches_obj(g, oid, name) {
            return Some(oid);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// asciiflag_conv (db.c): a flag field is either a plain integer or a letter
// string (a=1, b=2, …, z=2^25, A=2^26, …). Used by wdoor for exit flags.
// ---------------------------------------------------------------------------
fn asciiflag_conv(flag: &str) -> i64 {
    let flag = flag.trim();
    // Plain integer form?
    if !flag.is_empty() && flag.chars().all(|c| c.is_ascii_digit()) {
        return flag.parse::<i64>().unwrap_or(0);
    }
    let mut value: i64 = 0;
    for c in flag.chars() {
        if c.is_ascii_lowercase() {
            value |= 1i64 << (c as u8 - b'a');
        } else if c.is_ascii_uppercase() {
            value |= 1i64 << (26 + (c as u8 - b'A'));
        }
    }
    value
}

// ---------------------------------------------------------------------------
// real_zone (dg_wldcmd.c): the zone (rnum) whose number*100..=top range covers
// the given room number. send_to_zone broadcasts using that zone rnum.
// ---------------------------------------------------------------------------
fn real_zone(g: &GameState, number: i32) -> Option<i32> {
    for (idx, z) in g.zones.iter().enumerate() {
        if number >= z.number * 100 && number <= z.top {
            return Some(idx as i32);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// act_to_room (dg_wldcmd.c): send `str` to everyone in the room. Because act()
// with TO_ROOM excludes the anchor, C sends it both TO_ROOM and TO_CHAR for the
// room's first occupant — so the anchor sees it too. We replicate exactly.
// ---------------------------------------------------------------------------
fn act_to_room(g: &mut GameState, msg: &str, room: RoomRnum) {
    let anchor = match g.room_opt(room).and_then(|r| r.people.first().copied()) {
        Some(a) => a,
        None => return, // no one in the room
    };
    act(g, msg, false, anchor, None, ActArg::None, To::Room);
    act(g, msg, false, anchor, None, ActArg::None, To::Char);
}

// ===========================================================================
// World command handlers
// ===========================================================================

/// wasound: echo a message into every adjacent room.
fn do_wasound(g: &mut GameState, room: RoomRnum, argument: &str) {
    let argument = argument.trim_start();
    if argument.is_empty() {
        wld_log(g, room, "wasound called with no argument");
        return;
    }
    // Collect neighbour rnums first (avoid borrow across the act calls).
    let mut neighbours: Vec<RoomRnum> = Vec::new();
    if let Some(r) = g.room_opt(room) {
        for door in 0..NUM_OF_DIRS {
            if let Some(exit) = r.get_exit(door) {
                if exit.to_room != NOWHERE {
                    if let Some(to) = g.real_room(exit.to_room) {
                        if to != room {
                            neighbours.push(to);
                        }
                    }
                }
            }
        }
    }
    for to in neighbours {
        act_to_room(g, argument, to);
    }
}

/// wecho: echo a message to everyone in the room.
fn do_wecho(g: &mut GameState, room: RoomRnum, argument: &str) {
    let argument = argument.trim_start();
    if argument.is_empty() {
        wld_log(g, room, "wecho called with no args");
    } else {
        act_to_room(g, argument, room);
    }
}

/// wsend / wechoaround: send a message to a named character (or those around).
fn do_wsend(g: &mut GameState, room: RoomRnum, argument: &str, subcmd: i32) {
    let (buf, msg) = one_arg(argument);
    if buf.is_empty() {
        wld_log(g, room, "wsend called with no args");
        return;
    }
    let msg = msg.trim_start();
    if msg.is_empty() {
        wld_log(g, room, "wsend called without a message");
        return;
    }
    if let Some(ch) = get_char_by_room(g, room, &buf) {
        if subcmd == SCMD_WSEND {
            sub_write(g, msg, ch, true, TO_CHAR);
        } else if subcmd == SCMD_WECHOAROUND {
            sub_write(g, msg, ch, true, TO_ROOM);
        }
    } else {
        wld_log(g, room, "no target found for wsend");
    }
}

/// wzoneecho: broadcast a line to every player in the zone containing a room.
fn do_wzoneecho(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (zone_name, msg) = one_arg(argument);
    let msg = msg.trim_start();
    if zone_name.is_empty() || msg.is_empty() {
        wld_log(g, room, "wzoneecho called with too few args");
        return;
    }
    match real_zone(g, atoi(&zone_name)) {
        None => wld_log(g, room, "wzoneecho called for nonexistant zone"),
        Some(zone) => {
            let line = format!("{}\r\n", msg);
            send_to_zone(g, &line, zone);
        }
    }
}

/// wdoor: create / edit / purge an exit on a target room in a direction.
fn do_wdoor(g: &mut GameState, room: RoomRnum, argument: &str) {
    const DOOR_FIELDS: &[&str] = &["purge", "description", "flags", "key", "name", "room"];

    let (target, direction, rest) = two_args(argument);
    let (field, value) = one_arg(rest);
    let value = value.trim_start();

    if target.is_empty() || direction.is_empty() || field.is_empty() {
        wld_log(g, room, "wdoor called with too few args");
        return;
    }

    let rm = match get_room(g, &target) {
        Some(r) => r,
        None => {
            wld_log(g, room, "wdoor: invalid target");
            return;
        }
    };

    let dir = match search_block(&direction, &DIR_NAMES) {
        Some(d) => d,
        None => {
            wld_log(g, room, "wdoor: invalid direction");
            return;
        }
    };

    let fd = match search_block(&field, DOOR_FIELDS) {
        Some(f) => f,
        None => {
            wld_log(g, room, "wdoor: invalid field");
            return;
        }
    };

    if fd == 0 {
        // purge the exit.
        if let Some(r) = g.rooms.get_mut(rm) {
            r.exits[dir] = None;
        }
        return;
    }

    // Ensure the exit exists (CREATE if absent), then edit the requested field.
    let exists = g.room_opt(rm).map(|r| r.exits[dir].is_some()).unwrap_or(false);
    if !exists {
        if let Some(r) = g.rooms.get_mut(rm) {
            r.exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: NOWHERE,
            });
        }
    }

    match fd {
        1 => {
            // description (append CRLF, matching C).
            let desc = format!("{}\r\n", value);
            if let Some(ex) = exit_mut(g, rm, dir) {
                ex.description = Some(desc);
            }
        }
        2 => {
            let flags = asciiflag_conv(value) as i32;
            if let Some(ex) = exit_mut(g, rm, dir) {
                ex.exit_info = flags;
            }
        }
        3 => {
            let key = atoi(value);
            if let Some(ex) = exit_mut(g, rm, dir) {
                ex.key = key;
            }
        }
        4 => {
            let kw = value.to_string();
            if let Some(ex) = exit_mut(g, rm, dir) {
                ex.keyword = Some(kw);
            }
        }
        5 => {
            // room: set destination vnum if it resolves (or explicit NOWHERE).
            let vnum = atoi(value);
            if g.real_room(vnum).is_some() || vnum == NOWHERE {
                if let Some(ex) = exit_mut(g, rm, dir) {
                    ex.to_room = vnum;
                }
            } else {
                wld_log(g, room, "wdoor: invalid door target");
            }
        }
        _ => {}
    }
}

fn exit_mut(g: &mut GameState, rm: RoomRnum, dir: usize) -> Option<&mut Exit> {
    g.rooms.get_mut(rm)?.exits[dir].as_mut()
}

/// wteleport: move a named character (or everyone in the room) to a room. The
/// destination may be a UID, a visible char/obj name (their room), or a vnum.
fn do_wteleport(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (arg1, arg2, _) = two_args(argument);
    if arg1.is_empty() || arg2.is_empty() {
        wld_log(g, room, "wteleport called with too few args");
        return;
    }

    // Resolve the destination. C tries cdsr() (coordinate/vnum shortcut) first.
    let target: Option<RoomRnum> = if let Some(r) = cdsr(g, &arg2) {
        Some(r)
    } else if let Some(id) = parse_uid(&arg2) {
        find_room_by_id(g, id)
    } else if let Some(ch) = get_char_vis_world(g, &arg2) {
        g.get_char(ch).and_then(|c| c.in_room)
    } else if let Some(tobj) = get_obj_vis_world(g, &arg2) {
        match g.get_obj(tobj).map(|o| o.loc) {
            Some(ObjLoc::Room(r)) => Some(r),
            _ => None,
        }
    } else {
        g.real_room(atoi(&arg2))
    };

    let target = match target {
        Some(t) => t,
        None => {
            wld_log(g, room, "wteleport target is an invalid room");
            return;
        }
    };

    if arg1.eq_ignore_ascii_case("all") {
        if target == room {
            wld_log(g, room, "wteleport all target is itself");
            return;
        }
        let people = g.room_opt(room).map(|r| r.people.clone()).unwrap_or_default();
        for ch in people {
            g.char_from_room(ch);
            g.char_to_room(ch, target);
        }
    } else if let Some(ch) = get_char_by_room(g, room, &arg1) {
        g.char_from_room(ch);
        g.char_to_room(ch, target);
    } else {
        wld_log(g, room, "wteleport: no target found");
    }
}

/// wforce: make a target (or everyone in the room) execute a command.
fn do_wforce(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (arg1, line) = one_arg(argument);
    let line = line.trim_start();
    if arg1.is_empty() || line.is_empty() {
        wld_log(g, room, "wforce called with too few args");
        return;
    }

    if arg1.eq_ignore_ascii_case("all") {
        let people = g.room_opt(room).map(|r| r.people.clone()).unwrap_or_default();
        for ch in people {
            if g.char_exists(ch) && !is_immortal(g, ch) {
                command_interpreter(g, ch, line);
            }
        }
    } else if let Some(ch) = get_char_by_room(g, room, &arg1) {
        if !is_immortal(g, ch) {
            command_interpreter(g, ch, line);
        }
    } else {
        wld_log(g, room, "wforce: no target found");
    }
}

/// wexp: grant experience to a named character.
fn do_wexp(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (name, amount, _) = two_args(argument);
    if name.is_empty() || amount.is_empty() {
        wld_log(g, room, "wexp: too few arguments");
        return;
    }
    if let Some(ch) = get_char_by_room(g, room, &name) {
        crate::limits::gain_exp(g, ch, atoi(&amount) as i64);
    } else {
        wld_log(g, room, "wexp: target not found");
    }
}

/// wpurge: purge all NPCs+objects in the room (no arg) or a single target.
fn do_wpurge(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (arg, _) = one_arg(argument);

    if arg.is_empty() {
        let people = g.room_opt(room).map(|r| r.people.clone()).unwrap_or_default();
        for ch in people {
            if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
                crate::dg_handler::on_char_extracted(g, ch);
                g.extract_char(ch);
            }
        }
        let contents = g.room_opt(room).map(|r| r.contents.clone()).unwrap_or_default();
        for o in contents {
            crate::dg_handler::on_obj_extracted(g, o);
            g.obj_from_anywhere(o);
            g.extract_obj(o);
        }
        return;
    }

    if let Some(ch) = get_char_by_room(g, room, &arg) {
        if !g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
            wld_log(g, room, "wpurge: purging a PC");
            return;
        }
        crate::dg_handler::on_char_extracted(g, ch);
        g.extract_char(ch);
    } else if let Some(o) = get_obj_by_room(g, room, &arg) {
        crate::dg_handler::on_obj_extracted(g, o);
        g.obj_from_anywhere(o);
        g.extract_obj(o);
    } else {
        wld_log(g, room, "wpurge: bad argument");
    }
}

/// wload: load a mob or object into the room (fires the LOAD trigger).
fn do_wload(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (arg1, arg2, _) = two_args(argument);
    if arg1.is_empty() || arg2.is_empty() || !is_number(&arg2) || atoi(&arg2) < 0 {
        wld_log(g, room, "wload: bad syntax");
        return;
    }
    let number = atoi(&arg2);

    if is_abbrev(&arg1, "mob") {
        match g.load_mobile(number) {
            Some(mob) => {
                g.char_to_room(mob, room);
                crate::dg_triggers::load_mtrigger(g, mob);
            }
            None => wld_log(g, room, "wload: bad mob vnum"),
        }
    } else if is_abbrev(&arg1, "obj") {
        match g.load_object(number) {
            Some(object) => {
                g.obj_to_room(object, room);
                crate::dg_triggers::load_otrigger(g, object);
            }
            None => wld_log(g, room, "wload: bad object vnum"),
        }
    } else {
        wld_log(g, room, "wload: bad type");
    }
}

/// wdamage: deal (positive) or heal (negative) HP to a target or all in the
/// room. C wdamage rejects a leading '-' in the syntax check but still atoi's
/// it; a negative amount heals and short-circuits with the rejuvenate message.
fn do_wdamage(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (name, amount, _) = two_args(argument);
    // C requires isdigit(*amount): a leading '-' fails the syntax gate.
    let amount_ok = amount.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
    if name.is_empty() || amount.is_empty() || !amount_ok {
        wld_log(g, room, "wdamage: bad syntax");
        return;
    }
    let dam = atoi(&amount);

    let all = name.eq_ignore_ascii_case("all");
    let targets: Vec<CharId> = if all {
        g.room_opt(room).map(|r| r.people.clone()).unwrap_or_default()
    } else {
        match get_char_by_room(g, room, &name) {
            Some(ch) => vec![ch],
            None => {
                wld_log(g, room, "wdamage: target not found");
                return;
            }
        }
    };

    for ch in targets {
        if !g.char_exists(ch) {
            continue;
        }
        // wdamage's negative-amount heal path (C: send "rejuvinated", return).
        if dam < 0 {
            if is_immortal(g, ch) {
                continue;
            }
            if let Some(c) = g.get_char_mut(ch) {
                c.points.hit -= dam; // dam<0 => heal
            }
            g.send_to_char(ch, "You feel rejuvinated.\r\n");
            if !all {
                return;
            }
            continue;
        }
        crate::dg_objcmd::apply_script_damage(g, ch, dam, None);
        if !all {
            return;
        }
    }
}

/// wat: execute a command as if from a different (vnum-named) room.
fn do_wat(g: &mut GameState, room: RoomRnum, argument: &str) {
    let (location, arg2) = crate::interpreter::half_chop(argument);
    let arg2 = arg2.trim();
    if location.is_empty()
        || arg2.is_empty()
        || !location.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
    {
        wld_log(g, room, "wat: bad syntax");
        return;
    }
    let vnum = atoi(&location);
    match g.real_room(vnum) {
        None => wld_log(g, room, "wat: location not found"),
        Some(r2) => {
            // Re-enter the world interpreter with the room switched (C wat).
            dispatch_wld_command(g, r2, arg2);
        }
    }
}

/// dispatch_wld_command: the entry point the script VM calls (analogue of C
/// wld_command_interpreter). `argument` is the full command line for one trigger
/// step, %variables% already expanded by the VM. The leading verb is split off
/// and prefix-matched against the table in declaration order (C strncmp).
///
/// Rooms have no fall-through to player commands: an unknown verb is logged.
/// Returns true if a recognised w* verb ran.
pub fn dispatch_wld_command(g: &mut GameState, room: RoomRnum, argument: &str) -> bool {
    let argument = argument.trim_start();
    if argument.is_empty() {
        return false;
    }
    let (cmd, line) = one_arg(argument);
    if cmd.is_empty() {
        return false;
    }
    match WLD_CMDS.iter().find(|e| e.0.starts_with(cmd.as_str())) {
        Some(e) => {
            (e.1)(g, room, line, e.2);
            true
        }
        None => {
            wld_log(g, room, &format!("Unknown world cmd: '{}'", argument));
            false
        }
    }
}

// Command table (preserves C wld_cmd_info order for prefix-match parity).
type WldFn = fn(&mut GameState, RoomRnum, &str, i32);
struct WldCmd(&'static str, WldFn, i32);
const WLD_CMDS: &[WldCmd] = &[
    WldCmd("wasound", |g, r, a, _| do_wasound(g, r, a), 0),
    WldCmd("wdoor", |g, r, a, _| do_wdoor(g, r, a), 0),
    WldCmd("wecho", |g, r, a, _| do_wecho(g, r, a), 0),
    WldCmd("wechoaround", |g, r, a, sc| do_wsend(g, r, a, sc), SCMD_WECHOAROUND),
    WldCmd("wexp", |g, r, a, _| do_wexp(g, r, a), 0),
    WldCmd("wforce", |g, r, a, _| do_wforce(g, r, a), 0),
    WldCmd("wload", |g, r, a, _| do_wload(g, r, a), 0),
    WldCmd("wpurge", |g, r, a, _| do_wpurge(g, r, a), 0),
    WldCmd("wsend", |g, r, a, sc| do_wsend(g, r, a, sc), SCMD_WSEND),
    WldCmd("wteleport", |g, r, a, _| do_wteleport(g, r, a), 0),
    WldCmd("wzoneecho", |g, r, a, _| do_wzoneecho(g, r, a), 0),
    WldCmd("wdamage", |g, r, a, _| do_wdamage(g, r, a), 0),
    WldCmd("wat", |g, r, a, _| do_wat(g, r, a), 0),
];

/// GET_LEVEL(ch) >= LVL_IMMORT.
fn is_immortal(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.player.level >= LVL_IMMORT).unwrap_or(false)
}
