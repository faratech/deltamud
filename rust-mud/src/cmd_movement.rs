// cmd_movement.rs — port of CircleMUD/DeltaMUD `src/act.movement.c`:
// movement (do_move / perform_move / do_simple_move), door handling
// (do_gen_door: open/close/lock/unlock/pick/ram via subcmd), city enter/leave,
// the sleep/rest/sit/stand/wake/meditate position commands, follow, and the
// mount family (mount/dismount/buck/tame).
//
// Borrow discipline matches commands.rs: copy needed values into locals first,
// then mutate; never hold a &Character/&Object across a send/act. Entities are
// looked up by id every time.
//
// Contract gaps (see manifest `helpers_needed`/`notes`): the Rust Room has no
// `linkrnum`/`linkmapnum`, `ROOM_WALL`/`ROOM_IMPROOM`, weather, or map-mv
// plumbing. ROOM_ATRIUM (C bit 1<<13) IS honoured for house gating via raw bits
// — the Rust RoomFlags bitfield mislabels that bit as NO_SUMMON, so it is tested
// with room_is_atrium() rather than the named variant (see house.rs header for
// the collision). Pieces with no representable backing are handled the way the
// surrounding C code degrades (e.g. an absent door keyword -> "door").

use crate::act::{act, ActArg, To};
use crate::constants;
use crate::flags::*;
use crate::handler::isname;
use crate::interpreter::{one_argument, search_block};
use crate::object::{ObjLoc, ObjectType};
use crate::room::{EX_CLOSED, EX_HIDDEN, EX_ISDOOR, EX_LOCKED, EX_PICKPROOF};
use crate::room::{RoomFlags, SectorType};
use crate::state::GameState;
use crate::types::*;

// AFF_WATERWALK is bit 6 in DeltaMUD (constants::AFFECTED_BITS index 6). It is
// not surfaced as a named const in flags.rs, so define it locally.
const AFF_WATERWALK: i64 = 1 << 6;
const AFF_TAMED: i64 = 1 << 16;
const AFF_CHAINED: i64 = 1 << 24;

// PRF2_INTANGIBLE (structs.h): ghosting immortals don't burn move (1 << 9).
const PRF2_INTANGIBLE: i64 = 1 << 9;

// ROOM_ATRIUM (structs.h, C bit 1<<13 — "the door to a house"). The Rust
// RoomFlags bitfield names this bit NO_SUMMON, so the house-gating code tests it
// raw via room_is_atrium() to stay C-accurate (see house.rs header).
const ROOM_ATRIUM: u32 = 1 << 13;

// CONT_* container value-1 bits (structs.h), used for container doors.
const CONT_CLOSEABLE: i32 = 1 << 0;
const CONT_PICKPROOF: i32 = 1 << 1;
const CONT_CLOSED: i32 = 1 << 2;
const CONT_LOCKED: i32 = 1 << 3;

// SCMD_* door subcommands (interpreter.h), matching command_table.rs.
const SCMD_OPEN: i32 = 0;
const SCMD_CLOSE: i32 = 1;
const SCMD_UNLOCK: i32 = 2;
const SCMD_LOCK: i32 = 3;
const SCMD_PICK: i32 = 4;
const SCMD_RAM: i32 = 5;

// config.c string constants (the exact bytes the C MUD sends).
const OK: &str = "&YOkay.&n\r\n";
const NOPERSON: &str = "&CNo-one by that name here.&n\r\n";

// flags_door[] requirement bits (act.movement.c).
const NEED_OPEN: i32 = 1;
const NEED_CLOSED: i32 = 2;
const NEED_UNLOCKED: i32 = 4;
const NEED_LOCKED: i32 = 8;

/// cmd_door[] — verb per subcmd.
const CMD_DOOR: [&str; 6] = ["open", "close", "unlock", "lock", "pick", "ram"];

/// flags_door[] — requirements per subcmd.
const FLAGS_DOOR: [i32; 6] = [
    NEED_CLOSED | NEED_UNLOCKED, // open
    NEED_OPEN,                   // close
    NEED_CLOSED | NEED_LOCKED,   // unlock
    NEED_CLOSED | NEED_UNLOCKED, // lock
    NEED_CLOSED | NEED_LOCKED,   // pick
    NEED_CLOSED | NEED_LOCKED,   // ram
];

// ---------------------------------------------------------------------------
// Movement
// ---------------------------------------------------------------------------

/// has_boat: can the char walk on no-swim water? (AFF_WATERWALK, immortal, or
/// an ITEM_BOAT in inventory / worn). DeltaMUD also allows non-wearable boats.
/// True if the room at `rnum` carries ROOM_ATRIUM (raw bit; the named RoomFlags
/// variant for this bit is NO_SUMMON, hence the raw test).
fn room_is_atrium(g: &GameState, rnum: RoomRnum) -> bool {
    g.room_opt(rnum).map(|r| r.room_flags.bits() & ROOM_ATRIUM != 0).unwrap_or(false)
}

fn has_boat(g: &GameState, ch: CharId) -> bool {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return false,
    };
    if c.affect_flags & AFF_WATERWALK != 0 || (!c.is_npc && c.player.level >= LVL_IMMORT) {
        return true;
    }
    // A boat in inventory or equipment.
    let mut boats: Vec<ObjId> = c.carrying.clone();
    boats.extend(c.equipment.iter().flatten().copied());
    for oid in boats {
        if g.get_obj(oid).map(|o| o.obj_type == ObjectType::Other && is_boat(o)).unwrap_or(false) {
            return true;
        }
    }
    false
}

// The Rust ObjectType enum has no Boat variant; DeltaMUD's ITEM_BOAT is type
// 22. Detect it via the prototype value if it lands as `Other`. Without the
// numeric type we cannot tell, so this is a best-effort no-op stub returning
// false — boats then fall back to AFF_WATERWALK / immortal. (See notes.)
fn is_boat(_o: &crate::object::Object) -> bool {
    false
}

pub fn do_move(g: &mut GameState, ch: CharId, _arg: &str, subcmd: i32) {
    // cmd numbers 1..6 (SCMD_NORTH..SCMD_DOWN) map to direction indices 0..5.
    perform_move(g, ch, (subcmd - 1) as i32, false);
}

/// perform_move: validate the exit, then do_simple_move, dragging followers.
/// Mirrors act.movement.c perform_move (mind: returns bool there; here we keep
/// the same control flow but ignore the return at the do_move entry point).
pub fn perform_move(g: &mut GameState, ch: CharId, dir: i32, need_specials_check: bool) -> bool {
    if dir < 0 || dir >= NUM_OF_DIRS as i32 {
        return false;
    }
    let dir = dir as usize;
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };

    let exit = g.room(rnum).exits[dir].clone();
    let exit = match exit {
        Some(e) if g.real_room(e.to_room).is_some() => e,
        _ => {
            g.send_to_char(ch, "Alas, you cannot go that way...\r\n");
            return false;
        }
    };

    if exit.exit_info & EX_CLOSED != 0 {
        match &exit.keyword {
            Some(kw) => {
                let msg = format!("The {} seems to be closed.\r\n", fname(kw));
                g.send_to_char(ch, &msg);
            }
            None => g.send_to_char(ch, "It seems to be closed.\r\n"),
        }
        return false;
    }

    // No followers: just move.
    let followers = g.get_char(ch).map(|c| c.followers.clone()).unwrap_or_default();
    if followers.is_empty() {
        return do_simple_move(g, ch, dir, need_specials_check);
    }

    let was_in = rnum;
    if !do_simple_move(g, ch, dir, need_specials_check) {
        return false;
    }

    for k in followers {
        let (krnum, kpos) = match g.get_char(k) {
            Some(c) => (c.in_room, c.position),
            None => continue,
        };
        if krnum == Some(was_in) && kpos >= Position::Standing {
            act(g, "You follow $N.", false, k, None, ActArg::Char(ch), To::Char);
            perform_move(g, k, dir as i32, true);
        }
    }
    true
}

/// do_simple_move: assumes the direction exists and is open. Charges movement
/// by sector loss, applies the leave/arrive broadcasts (suppressed by sneak),
/// relocates the char, and shows the new room. Returns true on success.
fn do_simple_move(g: &mut GameState, ch: CharId, dir: usize, need_specials_check: bool) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let exit = match g.room(rnum).exits[dir].clone() {
        Some(e) => e,
        None => return false,
    };
    let to_rnum = match g.real_room(exit.to_room) {
        Some(r) => r,
        None => return false,
    };

    // Mount state (RIDING/RIDDEN_BY). same_room: the mount/rider shares our room.
    let (aff, master, is_npc, level, riding, ridden_by) = match g.get_char(ch) {
        Some(c) => (c.affect_flags, c.master, c.is_npc, c.player.level, c.riding, c.ridden_by),
        None => return false,
    };
    let same_room = if let Some(m) = riding {
        g.get_char(m).and_then(|c| c.in_room) == Some(rnum)
    } else if let Some(r) = ridden_by {
        g.get_char(r).and_then(|c| c.in_room) == Some(rnum)
    } else {
        false
    };

    // Tamed mobiles being ridden cannot wander off (DAK).
    if ridden_by.is_some() && same_room && aff & AFF_TAMED != 0 {
        g.send_to_char(ch, "You've been tamed.  Now act it!\r\n");
        return false;
    }

    if aff & AFF_CHARM != 0 {
        if let Some(m) = master {
            let same = g.get_char(m).and_then(|c| c.in_room) == Some(rnum);
            if same {
                g.send_to_char(ch, "The thought of leaving your master makes you weep.\r\n");
                act(g, "$n bursts into tears.", false, ch, None, ActArg::None, To::Room);
                return false;
            }
        }
    }

    if aff & AFF_CHAINED != 0 {
        g.send_to_char(ch, "You try to move but find your feet are chained together!\r\n");
        return false;
    }

    // Boat requirement for no-swim water in either room. When riding, the mount
    // must have a boat too (C: (riding && !has_boat(RIDING(ch))) || !has_boat(ch)).
    let from_sect = g.room(rnum).sector_type;
    let to_sect = g.room(to_rnum).sector_type;
    if from_sect == SectorType::WaterNoSwim || to_sect == SectorType::WaterNoSwim {
        let mount_no_boat = riding.map(|m| !has_boat(g, m)).unwrap_or(false);
        if mount_no_boat || !has_boat(g, ch) {
            g.send_to_char(ch, "You need a boat to go there.\r\n");
            return false;
        }
    }

    // Movement cost: avg of source & destination sector loss; doubled in snow.
    let loss = |s: SectorType| {
        constants::MOVEMENT_LOSS.get(s as usize).copied().unwrap_or(1)
    };
    let mut need_movement = (loss(from_sect) + loss(to_sect)) / 2;
    if g.room(to_rnum).snow > 0 {
        need_movement *= 2;
    }

    // Riding skill check: a failed SKILL_RIDING roll rears the mount and throws
    // the rider (act.movement.c). C: GET_SKILL(ch,SKILL_RIDING) < number(1,91) -
    // number(-4, need_movement).
    if let Some(mount) = riding {
        let ride_skill = g.get_char(ch).map(|c| c.skill(SKILL_RIDING)).unwrap_or(0) as i32;
        if ride_skill < g.rng.number(1, 91) - g.rng.number(-4, need_movement) {
            act(g, "$N rears backwards, throwing you to the ground.", false, ch, None, ActArg::Char(mount), To::Char);
            act(g, "You rear backwards, throwing $n to the ground.", false, ch, None, ActArg::Char(mount), To::Vict);
            act(g, "$N rears backwards, throwing $n to the ground.", false, ch, None, ActArg::Char(mount), To::NotVict);
            dismount_char(g, ch);
            let d = g.rng.dice(1, 6);
            crate::combat::damage(g, ch, ch, d);
            return false;
        }
    }

    // Godroom gating (LVL_GRGOD): mortals/low gods can't pass.
    if (level as u8) < LVL_GRGOD && g.room(to_rnum).room_flags.contains(RoomFlags::GODROOM) {
        g.send_to_char(ch, "You aren't godly enough to use that room!\r\n");
        return false;
    }

    // Atrium / private-house gating (act.movement.c): when leaving a ROOM_ATRIUM
    // (the public approach to a house), a non-owner / non-guest cannot step into
    // the private house room. House_can_enter consults the house-control table.
    // The ATRIUM bit (C 1<<13) is the bit the Rust RoomFlags bitfield names
    // NO_SUMMON, so it is tested raw here (see house.rs header for the collision).
    if room_is_atrium(g, rnum) && !crate::house::house_can_enter(g, ch, exit.to_room) {
        g.send_to_char(ch, "That's private property -- no trespassing!\r\n");
        return false;
    }

    // Tunnel rooms: no room while mounted; otherwise hold a single PC.
    if (riding.is_some() || ridden_by.is_some()) && g.room(to_rnum).room_flags.contains(RoomFlags::TUNNEL) {
        g.send_to_char(ch, "There isn't enough room there, while mounted.\r\n");
        return false;
    }
    if g.room(to_rnum).room_flags.contains(RoomFlags::TUNNEL) && num_pc_in_room(g, to_rnum) > 1 {
        g.send_to_char(ch, "There isn't enough room there for more than one person!\r\n");
        return false;
    }

    // NOTE: the destination death-trap path (ROOM_DEATH) is intentionally not
    // pre-blocked here. In C the actual DT (dt_effect) fires AFTER relocation;
    // the only pre-move branch is the high-WIS+INT "you spot the trap" notice,
    // which needs MAX_PLAYER_STAT + the equipment-strip + death-cry plumbing
    // owned by the death/combat batch. Until then, movement into a DT room
    // proceeds (the post-move dt_effect lands with that batch).

    // Exhaustion check. When riding, the mount's move is spent instead.
    let is_imm = !is_npc && (level as u8) >= LVL_IMMORT;
    if let Some(mount) = riding {
        let mount_move = g.get_char(mount).map(|c| c.points.move_points).unwrap_or(0);
        if mount_move < need_movement {
            g.send_to_char(ch, "Your mount is too exhausted.\r\n");
            return false;
        }
    } else {
        let cur_move = g.get_char(ch).map(|c| c.points.move_points).unwrap_or(0);
        if cur_move < need_movement && !is_npc {
            if need_specials_check && master.is_some() {
                g.send_to_char(ch, "You are too exhausted to follow.\r\n");
            } else {
                g.send_to_char(ch, "You are too exhausted.\r\n");
            }
            return false;
        }
    }

    // Deduct movement: an unmounted mortal pays from self; a rider's mount pays;
    // a ridden mob's rider pays (C: PRF2_INTANGIBLE-exempt, immortals exempt).
    let intangible = g.get_char(ch).map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0).unwrap_or(false);
    if !is_imm && !intangible && !is_npc && riding.is_none() && ridden_by.is_none() {
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= need_movement;
        }
    } else if let Some(mount) = riding {
        if let Some(c) = g.get_char_mut(mount) {
            c.points.move_points -= need_movement;
        }
    } else if let Some(rider) = ridden_by {
        if let Some(c) = g.get_char_mut(rider) {
            c.points.move_points -= need_movement;
        }
    }

    // Leave broadcast. When riding, "$n rides $N <dir>" (unless either sneaks).
    let mount_sneak = riding.map(|m| g.get_char(m).map(|c| c.affect_flags & AFF_SNEAK != 0).unwrap_or(false));
    let rider_sneak = ridden_by.map(|r| g.get_char(r).map(|c| c.affect_flags & AFF_SNEAK != 0).unwrap_or(false));
    if let Some(mount) = riding {
        if !mount_sneak.unwrap_or(false) {
            if aff & AFF_SNEAK != 0 {
                let msg = format!("$n leaves {}.", DIR_NAMES[dir]);
                act(g, &msg, true, mount, None, ActArg::None, To::Room);
            } else {
                let msg = format!("$n rides $N {}.", DIR_NAMES[dir]);
                act(g, &msg, true, ch, None, ActArg::Char(mount), To::NotVict);
            }
        }
    } else if let Some(rider) = ridden_by {
        if aff & AFF_SNEAK == 0 {
            if rider_sneak.unwrap_or(false) {
                let msg = format!("$n leaves {}.", DIR_NAMES[dir]);
                act(g, &msg, true, ch, None, ActArg::None, To::Room);
            } else {
                let msg = format!("$n rides $N {}.", DIR_NAMES[dir]);
                act(g, &msg, true, rider, None, ActArg::Char(ch), To::NotVict);
            }
        }
    } else if aff & AFF_SNEAK == 0 {
        let msg = format!("$n leaves {}.", DIR_NAMES[dir]);
        act(g, &msg, true, ch, None, ActArg::None, To::Room);
    }

    // Relocate.
    g.char_from_room(ch);
    g.char_to_room(ch, to_rnum);

    // Drag the mount/rider along if it shared our origin room.
    if let Some(mount) = riding {
        if same_room && g.get_char(mount).and_then(|c| c.in_room) != Some(to_rnum) {
            g.char_from_room(mount);
            g.char_to_room(mount, to_rnum);
        }
    } else if let Some(rider) = ridden_by {
        if same_room && g.get_char(rider).and_then(|c| c.in_room) != Some(to_rnum) {
            g.char_from_room(rider);
            g.char_to_room(rider, to_rnum);
        }
    }

    // DG greet triggers: mobs/room react to the arriving actor.
    crate::dg_triggers::greet_mtrigger(g, ch, dir as i32);
    crate::dg_triggers::entry_mtrigger(g, ch);

    // Arrival broadcast (suppressed under sneak), with mount/rider phrasing.
    if aff & AFF_SNEAK == 0 {
        let from = arrival_from(dir);
        if riding.is_some() && same_room && !mount_sneak.unwrap_or(false) {
            let mount = riding.unwrap();
            let msg = format!("$n arrives from {}, riding $N.", from);
            act(g, &msg, true, ch, None, ActArg::Char(mount), To::Room);
        } else if ridden_by.is_some() && same_room && !rider_sneak.unwrap_or(false) {
            let rider = ridden_by.unwrap();
            let msg = format!("$n arrives from {}, ridden by $N.", from);
            act(g, &msg, true, ch, None, ActArg::Char(rider), To::Room);
        } else if riding.is_none() || (riding.is_some() && !same_room) {
            let msg = format!("$n arrives from {}.", from);
            act(g, &msg, true, ch, None, ActArg::None, To::Room);
        }
    }

    // Show the destination to the mover.
    if g.get_char(ch).and_then(|c| c.desc).is_some() {
        crate::commands::look_at_room(g, ch, false);
    }

    true
}

/// num_pc_in_room: count non-NPC occupants (CircleMUD num_pc_in_room).
fn num_pc_in_room(g: &GameState, rnum: RoomRnum) -> i32 {
    let people = match g.room_opt(rnum) {
        Some(r) => &r.people,
        None => return 0,
    };
    people
        .iter()
        .filter(|&&c| g.get_char(c).map(|x| !x.is_npc).unwrap_or(false))
        .count() as i32
}

/// Arrival phrasing: "the <revdir>" for cardinal dirs, "below" for up,
/// "above" for down (act.movement.c).
fn arrival_from(dir: usize) -> String {
    if dir == UP {
        "below".to_string()
    } else if dir == DOWN {
        "above".to_string()
    } else {
        format!("the {}", DIR_NAMES[REV_DIR[dir]])
    }
}

// ---------------------------------------------------------------------------
// Doors
// ---------------------------------------------------------------------------

/// A located door target: either a container object or an exit `door` index.
enum DoorTarget {
    Obj(ObjId),
    Door(usize),
}

/// find_door: locate an exit door by keyword and/or direction (act.movement.c).
fn find_door(g: &mut GameState, ch: CharId, dtype: &str, dir: &str, cmdname: &str) -> Option<usize> {
    let rnum = g.get_char(ch).and_then(|c| c.in_room)?;
    if !dir.is_empty() {
        // A direction was specified.
        let door = match search_block(dir, &constants::DIRS[..NUM_OF_DIRS]) {
            Some(d) => d,
            None => {
                g.send_to_char(ch, "That's not a direction.\r\n");
                return None;
            }
        };
        match g.room(rnum).exits[door].as_ref() {
            Some(e) => match &e.keyword {
                Some(kw) => {
                    if isname(dtype, kw) {
                        Some(door)
                    } else {
                        let msg = format!("I see no {} there.\r\n", dtype);
                        g.send_to_char(ch, &msg);
                        None
                    }
                }
                None => Some(door),
            },
            None => {
                g.send_to_char(ch, "I really don't see how you can close anything there.\r\n");
                None
            }
        }
    } else {
        // No direction: locate the keyword among all exits.
        if dtype.is_empty() {
            let msg = format!("What is it you want to {}?\r\n", cmdname);
            g.send_to_char(ch, &msg);
            return None;
        }
        for door in 0..NUM_OF_DIRS {
            if let Some(e) = g.room(rnum).exits[door].as_ref() {
                if let Some(kw) = &e.keyword {
                    if isname(dtype, kw) {
                        return Some(door);
                    }
                }
            }
        }
        let msg = format!("There doesn't seem to be {} {} here.\r\n", an(dtype), dtype);
        g.send_to_char(ch, &msg);
        None
    }
}

/// has_key: does ch carry / hold the key vnum? (act.movement.c)
fn has_key(g: &GameState, ch: CharId, key: ObjVnum) -> bool {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return false,
    };
    for &o in &c.carrying {
        if g.get_obj(o).map(|x| x.item_number == key).unwrap_or(false) {
            return true;
        }
    }
    if let Some(o) = c.equipment[WEAR_HOLD] {
        if g.get_obj(o).map(|x| x.item_number == key).unwrap_or(false) {
            return true;
        }
    }
    false
}

// ---- Door predicate helpers (the DOOR_IS_* macros) ----

fn door_is_openable(g: &GameState, target: &DoorTarget, ch: CharId, rnum: RoomRnum) -> bool {
    match target {
        DoorTarget::Obj(o) => {
            let obj = match g.get_obj(*o) {
                Some(o) => o,
                None => return false,
            };
            obj.obj_type == ObjectType::Container && (obj.values[1] & CONT_CLOSEABLE) != 0
        }
        DoorTarget::Door(d) => exit_info(g, ch, rnum, *d) & EX_ISDOOR != 0,
    }
}

fn door_is_open(g: &GameState, target: &DoorTarget, ch: CharId, rnum: RoomRnum) -> bool {
    match target {
        DoorTarget::Obj(o) => g
            .get_obj(*o)
            .map(|obj| obj.values[1] & CONT_CLOSED == 0)
            .unwrap_or(true),
        DoorTarget::Door(d) => exit_info(g, ch, rnum, *d) & EX_CLOSED == 0,
    }
}

fn door_is_unlocked(g: &GameState, target: &DoorTarget, ch: CharId, rnum: RoomRnum) -> bool {
    match target {
        DoorTarget::Obj(o) => g
            .get_obj(*o)
            .map(|obj| obj.values[1] & CONT_LOCKED == 0)
            .unwrap_or(true),
        DoorTarget::Door(d) => exit_info(g, ch, rnum, *d) & EX_LOCKED == 0,
    }
}

fn door_is_pickproof(g: &GameState, target: &DoorTarget, ch: CharId, rnum: RoomRnum) -> bool {
    match target {
        DoorTarget::Obj(o) => g
            .get_obj(*o)
            .map(|obj| obj.values[1] & CONT_PICKPROOF != 0)
            .unwrap_or(false),
        DoorTarget::Door(d) => exit_info(g, ch, rnum, *d) & EX_PICKPROOF != 0,
    }
}

fn door_key(g: &GameState, target: &DoorTarget, _ch: CharId, rnum: RoomRnum) -> ObjVnum {
    match target {
        DoorTarget::Obj(o) => g.get_obj(*o).map(|obj| obj.values[2]).unwrap_or(-1),
        DoorTarget::Door(d) => g
            .room_opt(rnum)
            .and_then(|r| r.exits[*d].as_ref())
            .map(|e| e.key)
            .unwrap_or(-1),
    }
}

fn exit_info(g: &GameState, _ch: CharId, rnum: RoomRnum, door: usize) -> i32 {
    g.room_opt(rnum)
        .and_then(|r| r.exits[door].as_ref())
        .map(|e| e.exit_info)
        .unwrap_or(0)
}

/// do_doorcmd: perform the actual open/close/lock/unlock/pick/ram and notify
/// both rooms (act.movement.c do_doorcmd).
fn do_doorcmd(g: &mut GameState, ch: CharId, target: &DoorTarget, scmd: i32) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    // Determine the mirrored exit on the other side (for two-sided doors).
    let mut other: Option<(RoomRnum, usize)> = None;
    if let DoorTarget::Door(door) = target {
        let to_vnum = g.room(rnum).exits[*door].as_ref().map(|e| e.to_room);
        if let Some(to_vnum) = to_vnum {
            if let Some(other_rnum) = g.real_room(to_vnum) {
                let back_dir = REV_DIR[*door];
                // Does the reverse exit on the far side point back to us?
                let back_to = g
                    .room(other_rnum)
                    .exits[back_dir]
                    .as_ref()
                    .map(|e| e.to_room);
                let back_points_here =
                    back_to.and_then(|v| g.real_room(v)) == Some(rnum);
                if back_points_here {
                    other = Some((other_rnum, back_dir));
                }
            }
        }
    }

    // Build the room-notify verb prefix (e.g. "$n opens ").
    let verb = CMD_DOOR[scmd as usize];

    match scmd {
        SCMD_OPEN | SCMD_CLOSE => {
            toggle_open(g, target, rnum);
            if let Some((or, od)) = other {
                toggle_open_exit(g, or, od);
            }
            g.send_to_char(ch, OK);
        }
        SCMD_UNLOCK | SCMD_LOCK => {
            toggle_lock(g, target, rnum);
            if let Some((or, od)) = other {
                toggle_lock_exit(g, or, od);
            }
            g.send_to_char(ch, "*Click*\r\n");
        }
        SCMD_PICK => {
            toggle_lock(g, target, rnum);
            if let Some((or, od)) = other {
                toggle_lock_exit(g, or, od);
            }
            g.send_to_char(ch, "The lock quickly yields to your skills.\r\n");
        }
        SCMD_RAM => {
            toggle_lock(g, target, rnum);
            if let Some((or, od)) = other {
                toggle_lock_exit(g, or, od);
            }
            g.send_to_char(ch, "It gives under your mighty shove!\r\n");
            toggle_open(g, target, rnum);
            if let Some((or, od)) = other {
                toggle_open_exit(g, or, od);
            }
        }
        _ => {}
    }

    // Compose & deliver the room message.
    // C: "$n %ss " then either "$p." (obj) or "the $F."/"the door.".
    let prefix = match scmd {
        SCMD_PICK => "$n skillfully picks the lock on ".to_string(),
        SCMD_RAM => "$n uses his might and splits open ".to_string(),
        _ => format!("$n {}s ", verb),
    };
    match target {
        DoorTarget::Obj(o) => {
            // Only notify when the object is in the room (not a container the
            // mover is carrying — matches C `obj->in_room != NOWHERE`).
            let in_room = matches!(g.get_obj(*o).map(|x| x.loc), Some(ObjLoc::Room(_)));
            if in_room {
                let msg = format!("{}$p.", prefix);
                act(g, &msg, false, ch, Some(*o), ActArg::None, To::Room);
            }
        }
        DoorTarget::Door(door) => {
            let kw = g
                .room(rnum)
                .exits[*door]
                .as_ref()
                .and_then(|e| e.keyword.clone());
            match kw {
                Some(k) => {
                    let msg = format!("{}the $F.", prefix);
                    act(g, &msg, false, ch, None, ActArg::Str(k), To::Room);
                }
                None => {
                    let msg = format!("{}the door.", prefix);
                    act(g, &msg, false, ch, None, ActArg::None, To::Room);
                }
            }
        }
    }

    // Notify the other room for open/close on two-sided doors.
    if scmd == SCMD_OPEN || scmd == SCMD_CLOSE {
        if let (DoorTarget::Door(door), Some((other_rnum, back_dir))) = (target, other) {
            let _ = door;
            let back_kw = g
                .room(other_rnum)
                .exits[back_dir]
                .as_ref()
                .and_then(|e| e.keyword.clone());
            let name = back_kw.as_deref().map(fname).unwrap_or_else(|| "door".to_string());
            let suffix = if scmd == SCMD_CLOSE { "d" } else { "ed" };
            let msg = format!("The {} is {}{} from the other side.\r\n", name, verb, suffix);
            let people = g.room(other_rnum).people.clone();
            for pid in people {
                g.send_to_char(pid, &msg);
            }
        }
    }
}

fn toggle_open(g: &mut GameState, target: &DoorTarget, rnum: RoomRnum) {
    match target {
        DoorTarget::Obj(o) => {
            if let Some(obj) = g.get_obj_mut(*o) {
                obj.values[1] ^= CONT_CLOSED;
            }
        }
        DoorTarget::Door(d) => toggle_open_exit(g, rnum, *d),
    }
}

fn toggle_open_exit(g: &mut GameState, rnum: RoomRnum, door: usize) {
    if let Some(e) = g.room_mut(rnum).exits[door].as_mut() {
        e.exit_info ^= EX_CLOSED;
    }
}

fn toggle_lock(g: &mut GameState, target: &DoorTarget, rnum: RoomRnum) {
    match target {
        DoorTarget::Obj(o) => {
            if let Some(obj) = g.get_obj_mut(*o) {
                obj.values[1] ^= CONT_LOCKED;
            }
        }
        DoorTarget::Door(d) => toggle_lock_exit(g, rnum, *d),
    }
}

fn toggle_lock_exit(g: &mut GameState, rnum: RoomRnum, door: usize) {
    if let Some(e) = g.room_mut(rnum).exits[door].as_mut() {
        e.exit_info ^= EX_LOCKED;
    }
}

/// ok_pick: gate the pick/ram skill rolls. The skill numbers/damage are owned
/// by the skills/combat batch; here we model the keyhole and pickproof gates
/// faithfully and let the (yet-unwired) skill roll succeed.
fn ok_pick(g: &mut GameState, ch: CharId, keynum: ObjVnum, pickproof: bool, scmd: i32) -> bool {
    if scmd == SCMD_PICK {
        if keynum < 0 {
            g.send_to_char(ch, "Odd - you can't seem to find a keyhole.\r\n");
            return false;
        }
        if pickproof {
            g.send_to_char(ch, "It resists your attempts to pick it.\r\n");
            return false;
        }
        return true;
    }
    if scmd == SCMD_RAM {
        if keynum < 0 || pickproof {
            g.send_to_char(ch, "You ram it but it just won't budge.\r\n");
            return false;
        }
        return true;
    }
    true
}

pub fn do_gen_door(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let argument = arg.trim_start();
    if argument.is_empty() {
        let mut s = format!("{} what?\r\n", CMD_DOOR[subcmd as usize]);
        cap_first(&mut s);
        g.send_to_char(ch, &s);
        return;
    }

    // two_arguments: first two whitespace tokens (type, dir).
    let (dtype, rest) = one_argument(argument);
    let (dir, _) = one_argument(rest);

    // generic_find FIND_OBJ_INV | FIND_OBJ_ROOM: look for a container object
    // first (inventory, then room).
    let mut target: Option<DoorTarget> = None;
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    if let Some(o) = g.get_obj_in_list_vis(ch, &dtype, &inv) {
        target = Some(DoorTarget::Obj(o));
    } else if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
        let contents = g.room(rnum).contents.clone();
        if let Some(o) = g.get_obj_in_list_vis(ch, &dtype, &contents) {
            target = Some(DoorTarget::Obj(o));
        }
    }
    if target.is_none() {
        if let Some(d) = find_door(g, ch, &dtype, &dir, CMD_DOOR[subcmd as usize]) {
            target = Some(DoorTarget::Door(d));
        }
    }

    let target = match target {
        Some(t) => t,
        None => return, // find_door already emitted the error
    };

    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    let keynum = door_key(g, &target, ch, rnum);
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
    let flags = FLAGS_DOOR[subcmd as usize];

    if !door_is_openable(g, &target, ch, rnum) {
        let verb = CMD_DOOR[subcmd as usize].to_string();
        act(g, "You can't $F that!", false, ch, None, ActArg::Str(verb), To::Char);
    } else if door_is_open(g, &target, ch, rnum) && (flags & NEED_OPEN) != 0 {
        g.send_to_char(ch, "But it's already closed!\r\n");
    } else if !door_is_open(g, &target, ch, rnum) && (flags & NEED_CLOSED) != 0 {
        g.send_to_char(ch, "But it's currently open!\r\n");
    } else if door_is_unlocked(g, &target, ch, rnum) && (flags & NEED_LOCKED) != 0 {
        g.send_to_char(ch, "Oh.. it wasn't locked, after all..\r\n");
    } else if !door_is_unlocked(g, &target, ch, rnum) && (flags & NEED_UNLOCKED) != 0 {
        g.send_to_char(ch, "It seems to be locked.\r\n");
    } else if !has_key(g, ch, keynum)
        && (level as u8) < LVL_GOD
        && (subcmd == SCMD_LOCK || subcmd == SCMD_UNLOCK)
    {
        g.send_to_char(ch, "You don't seem to have the proper key.\r\n");
    } else if ok_pick(g, ch, keynum, door_is_pickproof(g, &target, ch, rnum), subcmd) {
        do_doorcmd(g, ch, &target, subcmd);
    }
}

// ---------------------------------------------------------------------------
// Enter / leave (DeltaMUD city links)
// ---------------------------------------------------------------------------
//
// DeltaMUD's do_enter/do_leave move between an overworld surface-map room and a
// city interior via Room.linkrnum / Room.linkmapnum (act.movement.c). C sets
// those in maputils.c when the surface-map room block is spliced into world[].
// The Rust world loader does not splice that block (see maputils.rs), so the
// links stay None and both commands fall through to "no visible entrance/exit",
// exactly as C does for an unlinked room (linkrnum<0 / linkmapnum==-1). The
// relocate path below is fully wired and fires the moment a link is populated.

pub fn do_enter(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let toroom = g.get_char(ch).and_then(|c| c.in_room).and_then(|r| g.room(r).linkrnum);
    let toroom = match toroom {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "There is no visible entrance here.\r\n");
            return;
        }
    };
    act(g, "$n ventures into the city.", true, ch, None, ActArg::None, To::Room);
    g.send_to_char(ch, "You venture into the city.\r\n");
    g.char_from_room(ch);
    g.char_to_room(ch, toroom);
    act(g, "$n has arrived.", true, ch, None, ActArg::None, To::Room);
    if g.get_char(ch).and_then(|c| c.desc).is_some() {
        crate::commands::look_at_room(g, ch, false);
    }
}

pub fn do_leave(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let toroom = g.get_char(ch).and_then(|c| c.in_room).and_then(|r| g.room(r).linkmapnum);
    let toroom = match toroom {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "There is no visible exit here.\r\n");
            return;
        }
    };
    act(g, "$n leaves the city.", true, ch, None, ActArg::None, To::Room);
    g.send_to_char(ch, "You leave the city.\r\n");
    g.char_from_room(ch);
    g.char_to_room(ch, toroom);
    act(g, "$n has arrived.", true, ch, None, ActArg::None, To::Room);
    if g.get_char(ch).and_then(|c| c.desc).is_some() {
        crate::commands::look_at_room(g, ch, false);
    }
}

// ---------------------------------------------------------------------------
// Special exits (the `O` block)
// ---------------------------------------------------------------------------
//
// A room special exit is a named custom egress (act.movement.c). When a standing
// player types its ex_name, special_exit_command() intercepts the input before
// normal dispatch and moves them through. Ports do_special_move /
// perform_special_move / special_exit_command 1:1 (the dispatch hook lives in
// interpreter.rs run_command, mirroring interpreter.c:800).

/// special_exit_command (act.movement.c): if the current room has a special exit
/// and `cmd` prefix-matches its ex_name, perform the special move. Returns true
/// if it consumed the command.
pub fn special_exit_command(g: &mut GameState, ch: CharId, cmd: &str) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let ex_name = match g.room(rnum).special_exit.as_ref().and_then(|se| se.ex_name.clone()) {
        Some(n) if !n.is_empty() => n,
        _ => return false,
    };
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    // C: strn_cmp(cmd, ex_name, strlen(ex_name)) — the typed word must begin
    // with the full ex_name.
    if cmd.len() >= ex_name.len() && cmd[..ex_name.len()].eq_ignore_ascii_case(&ex_name) {
        perform_special_move(g, ch, true);
        return true;
    }
    false
}

/// perform_special_move (act.movement.c): validate the special exit (presence /
/// closed / hidden), then do_special_move, dragging followers.
fn perform_special_move(g: &mut GameState, ch: CharId, _need_specials_check: bool) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    if g.get_char(ch).map(|c| c.fighting.is_some()).unwrap_or(false) {
        return false;
    }
    let se = match g.room(rnum).special_exit.clone() {
        Some(se) => se,
        None => {
            g.send_to_char(ch, "Alas, you cannot go that way...\r\n");
            return false;
        }
    };
    if se.to_room == NOWHERE {
        g.send_to_char(ch, "Alas, you cannot go that way...\r\n");
        return false;
    }
    if se.exit_info & EX_CLOSED != 0 {
        if se.exit_info & EX_HIDDEN != 0 {
            g.send_to_char(ch, "Alas, you cannot go that way...\r\n");
        } else if let Some(kw) = &se.keyword {
            let msg = format!("The {} seems to be closed.\r\n", fname(kw));
            g.send_to_char(ch, &msg);
        } else {
            g.send_to_char(ch, "It seems to be closed.\r\n");
        }
        return false;
    }

    let followers = g.get_char(ch).map(|c| c.followers.clone()).unwrap_or_default();
    if followers.is_empty() {
        return do_special_move(g, ch);
    }

    let was_in = rnum;
    if !do_special_move(g, ch) {
        return false;
    }
    for k in followers {
        let (krnum, kpos) = match g.get_char(k) {
            Some(c) => (c.in_room, c.position),
            None => continue,
        };
        if krnum == Some(was_in) && kpos >= Position::Standing {
            act(g, "You follow $N.", false, k, None, ActArg::Char(ch), To::Char);
            perform_special_move(g, k, true);
        }
    }
    true
}

/// do_special_move (act.movement.c): the special-exit analogue of do_simple_move
/// (charm/chained/boat/movement/godroom/tunnel gates), then relocate via the
/// special exit and show the new room.
fn do_special_move(g: &mut GameState, ch: CharId) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let se = match g.room(rnum).special_exit.clone() {
        Some(se) => se,
        None => return false,
    };
    let to_rnum = match g.real_room(se.to_room) {
        Some(r) => r,
        None => return false,
    };

    let (aff, master, is_npc, level, riding, ridden_by) = match g.get_char(ch) {
        Some(c) => (c.affect_flags, c.master, c.is_npc, c.player.level, c.riding, c.ridden_by),
        None => return false,
    };

    if aff & AFF_CHARM != 0 {
        if let Some(m) = master {
            if g.get_char(m).and_then(|c| c.in_room) == Some(rnum) {
                g.send_to_char(ch, "The thought of leaving your master makes you weep.\r\n");
                act(g, "$n bursts into tears.", false, ch, None, ActArg::None, To::Room);
                return false;
            }
        }
    }
    if aff & AFF_CHAINED != 0 {
        g.send_to_char(ch, "You try to move but find your feet are chained together!\r\n");
        return false;
    }

    let from_sect = g.room(rnum).sector_type;
    let to_sect = g.room(to_rnum).sector_type;
    if (from_sect == SectorType::WaterNoSwim || to_sect == SectorType::WaterNoSwim) && !has_boat(g, ch) {
        g.send_to_char(ch, "You need a boat to go there.\r\n");
        return false;
    }

    let loss = |s: SectorType| constants::MOVEMENT_LOSS.get(s as usize).copied().unwrap_or(1);
    let need_movement = (loss(from_sect) + loss(to_sect)) / 2;

    let is_imm = !is_npc && (level as u8) >= LVL_IMMORT;
    if let Some(mount) = riding {
        if g.get_char(mount).map(|c| c.points.move_points).unwrap_or(0) < need_movement {
            g.send_to_char(ch, "Your mount is too exhausted.\r\n");
            return false;
        }
    } else if g.get_char(ch).map(|c| c.points.move_points).unwrap_or(0) < need_movement && !is_npc {
        g.send_to_char(ch, "You are too exhausted.\r\n");
        return false;
    }

    // NOTE: ROOM_WALL / ROOM_IMPROOM (C bits 18 / 16) gates are omitted to match
    // do_simple_move — the Rust RoomFlags bitfield does not model those bits
    // (see this module's header).
    //
    // Atrium / private-house gating (act.movement.c): leaving a ROOM_ATRIUM into
    // a private house is blocked for non-owners/non-guests. ATRIUM is C bit 1<<13
    // (named NO_SUMMON in the Rust RoomFlags bitfield), so it is tested raw.
    if room_is_atrium(g, rnum) && !crate::house::house_can_enter(g, ch, se.to_room) {
        g.send_to_char(ch, "That's private property -- no trespassing!\r\n");
        return false;
    }
    if (riding.is_some() || ridden_by.is_some()) && g.room(to_rnum).room_flags.contains(RoomFlags::TUNNEL) {
        g.send_to_char(ch, "There isn't enough room there, while mounted.\r\n");
        return false;
    }
    if g.room(to_rnum).room_flags.contains(RoomFlags::TUNNEL) && num_pc_in_room(g, to_rnum) > 1 {
        g.send_to_char(ch, "There isn't enough room there for more than one person!\r\n");
        return false;
    }
    if (level as u8) < LVL_GRGOD && g.room(to_rnum).room_flags.contains(RoomFlags::GODROOM) {
        g.send_to_char(ch, "You aren't godly enough to use that room!\r\n");
        return false;
    }

    let intangible = g.get_char(ch).map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0).unwrap_or(false);
    if !is_imm && !intangible && !is_npc {
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= need_movement;
        }
    } else if let Some(mount) = riding {
        if let Some(c) = g.get_char_mut(mount) {
            c.points.move_points -= need_movement;
        }
    } else if let Some(rider) = ridden_by {
        if let Some(c) = g.get_char_mut(rider) {
            c.points.move_points -= need_movement;
        }
    }

    if aff & AFF_SNEAK == 0 {
        match &se.leave_msg {
            Some(m) if !m.is_empty() => act(g, m, true, ch, None, ActArg::None, To::Room),
            _ => act(g, "$n leaves.", true, ch, None, ActArg::None, To::Room),
        }
    }

    g.char_from_room(ch);
    g.char_to_room(ch, to_rnum);

    if aff & AFF_SNEAK == 0 {
        act(g, "$n has arrived.", true, ch, None, ActArg::None, To::Room);
    }
    if g.get_char(ch).and_then(|c| c.desc).is_some() {
        crate::commands::look_at_room(g, ch, false);
    }
    true
}

// ---------------------------------------------------------------------------
// Position commands
// ---------------------------------------------------------------------------

pub fn do_stand(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    match g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing) {
        Position::Standing => {
            act(g, "You are already standing.", false, ch, None, ActArg::None, To::Char);
        }
        Position::Sitting => {
            act(g, "You stand up.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n clambers to $s feet.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Standing);
        }
        Position::Resting => {
            act(g, "You stop resting, and stand up.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops resting, and clambers on $s feet.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Standing);
        }
        Position::Meditating => {
            act(g, "You stop meditating, and stand up.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops meditating, and clambers on $s feet.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Standing);
        }
        Position::Sleeping => {
            act(g, "You have to wake up first!", false, ch, None, ActArg::None, To::Char);
        }
        Position::Fighting => {
            act(g, "Do you not consider fighting as standing?", false, ch, None, ActArg::None, To::Char);
        }
        Position::Stunned | Position::Incapacitated | Position::MortallyWounded | Position::Dead => {
            act(g, "Stand up!? In your physical state!? HA!", false, ch, None, ActArg::None, To::Char);
        }
    }
}

pub fn do_sit(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    match g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing) {
        Position::Standing => {
            act(g, "You sit down.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n sits down.", false, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sitting);
        }
        Position::Sitting => {
            g.send_to_char(ch, "You're sitting already.\r\n");
        }
        Position::Resting => {
            act(g, "You stop resting, and sit up.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops resting.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sitting);
        }
        Position::Meditating => {
            act(g, "You stop meditating, and open your eyes.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops meditating, and opens $s eyes.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sitting);
        }
        Position::Sleeping => {
            act(g, "You have to wake up first.", false, ch, None, ActArg::None, To::Char);
        }
        Position::Fighting => {
            act(g, "Sit down while fighting? are you MAD?", false, ch, None, ActArg::None, To::Char);
        }
        _ => {
            act(g, "You stop floating around, and sit down.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops floating around, and sits down.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sitting);
        }
    }
}

pub fn do_rest(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    match g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing) {
        Position::Standing => {
            act(g, "You sit down and rest your tired bones.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n sits down and rests.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Resting);
        }
        Position::Sitting => {
            act(g, "You rest your tired bones.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n rests.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Resting);
        }
        Position::Resting => {
            act(g, "You are already resting.", false, ch, None, ActArg::None, To::Char);
        }
        Position::Meditating => {
            act(g, "You stop meditating, and rest your tired bones.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops meditating, and rests.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Resting);
        }
        Position::Sleeping => {
            act(g, "You have to wake up first.", false, ch, None, ActArg::None, To::Char);
        }
        Position::Fighting => {
            act(g, "Rest while fighting?  Are you MAD?", false, ch, None, ActArg::None, To::Char);
        }
        _ => {
            act(g, "You stop floating around, and stop to rest your tired bones.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops floating around, and rests.", false, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sitting);
        }
    }
}

pub fn do_sleep(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    match g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing) {
        Position::Standing | Position::Sitting | Position::Resting => {
            g.send_to_char(ch, "You go to sleep.\r\n");
            act(g, "$n lies down and falls asleep.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sleeping);
        }
        Position::Sleeping => {
            g.send_to_char(ch, "You are already sound asleep.\r\n");
        }
        Position::Meditating => {
            act(g, "You stop meditating, and go to sleep.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops meditating, and goes to sleep.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sleeping);
        }
        Position::Fighting => {
            g.send_to_char(ch, "Sleep while fighting?  Are you MAD?\r\n");
        }
        _ => {
            act(g, "You stop floating around, and lie down to sleep.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops floating around, and lie down to sleep.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Sleeping);
        }
    }
}

pub fn do_meditate(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let (level, class, med_skill) = match g.get_char(ch) {
        Some(c) => (c.player.level, c.player.class, c.skill(SKILL_MEDITATE)),
        None => return,
    };

    if (level as u8) < LVL_IMMORT {
        if !(class == Class::MagicUser || class == Class::Cleric) {
            g.send_to_char(ch, "You've no idea how to meditate.\r\n");
            return;
        }
        if med_skill <= 10 {
            g.send_to_char(ch, "You've no idea how to meditate.\r\n");
            return;
        }
    }

    match g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing) {
        Position::Standing | Position::Sitting | Position::Resting => {
            g.send_to_char(ch, "You start to meditate.\r\n");
            act(g, "$n sits down and starts to meditate.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Meditating);
        }
        Position::Sleeping => {
            // C sends without CRLF here ("You have to wake up first.").
            g.send_to_char(ch, "You have to wake up first.");
        }
        Position::Meditating => {
            g.send_to_char(ch, "You are already meditating.\r\n");
        }
        Position::Fighting => {
            g.send_to_char(ch, "Meditate while fighting?  Are you MAD?\r\n");
        }
        _ => {
            act(g, "You stop floating around, and start to meditate.", false, ch, None, ActArg::None, To::Char);
            act(g, "$n stops floating around, and starts to meditate.", true, ch, None, ActArg::None, To::Room);
            set_pos(g, ch, Position::Meditating);
        }
    }
}

pub fn do_wake(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (word, _) = one_argument(arg);
    let mut self_wake = false;

    if !word.is_empty() {
        let my_pos = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing);
        if my_pos == Position::Sleeping {
            g.send_to_char(ch, "Maybe you should wake yourself up first.\r\n");
        } else if let Some(vict) = g.get_char_room_vis(ch, &word) {
            if vict == ch {
                self_wake = true;
            } else {
                let vpos = g.get_char(vict).map(|c| c.position).unwrap_or(Position::Standing);
                let vaff = g.get_char(vict).map(|c| c.affect_flags).unwrap_or(0);
                if vpos > Position::Sleeping {
                    act(g, "$E is already awake.", false, ch, None, ActArg::Char(vict), To::Char);
                } else if vaff & AFF_SLEEP != 0 {
                    act(g, "You can't wake $M up!", false, ch, None, ActArg::Char(vict), To::Char);
                } else if vpos < Position::Sleeping {
                    act(g, "$E's in pretty bad shape!", false, ch, None, ActArg::Char(vict), To::Char);
                } else {
                    act(g, "You wake $M up.", false, ch, None, ActArg::Char(vict), To::Char);
                    // TO_VICT | TO_SLEEP: deliver even though they're asleep.
                    crate::act::act_sleep(g, "You are awakened by $n.", false, ch, None, ActArg::Char(vict), To::Vict, true);
                    set_pos(g, vict, Position::Sitting);
                }
            }
        } else {
            g.send_to_char(ch, NOPERSON);
        }
        if !self_wake {
            return;
        }
    }

    // Wake self.
    let (aff, level, pos) = match g.get_char(ch) {
        Some(c) => (c.affect_flags, c.player.level, c.position),
        None => return,
    };
    if aff & AFF_SLEEP != 0 && (level as u8) < LVL_IMMORT {
        g.send_to_char(ch, "You can't wake up!\r\n");
    } else if pos > Position::Sleeping {
        g.send_to_char(ch, "You are already awake...\r\n");
    } else {
        g.send_to_char(ch, "You awaken, and sit up.\r\n");
        act(g, "$n awakens.", true, ch, None, ActArg::None, To::Room);
        set_pos(g, ch, Position::Sitting);
    }
}

// ---------------------------------------------------------------------------
// Follow
// ---------------------------------------------------------------------------

pub fn do_follow(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (word, _) = one_argument(arg);

    let leader = if !word.is_empty() {
        match g.get_char_room_vis(ch, &word) {
            Some(l) => l,
            None => {
                g.send_to_char(ch, NOPERSON);
                return;
            }
        }
    } else {
        g.send_to_char(ch, "Whom do you wish to follow?\r\n");
        return;
    };

    let (leader_is_npc, leader_level) = match g.get_char(leader) {
        Some(c) => (c.is_npc, c.player.level),
        None => return,
    };
    let (my_level, my_master, my_aff) = match g.get_char(ch) {
        Some(c) => (c.player.level, c.master, c.affect_flags),
        None => return,
    };

    // Can't follow an immortal as a mortal.
    if !leader_is_npc && (leader_level as u8) >= LVL_IMMORT && (my_level as u8) < LVL_IMMORT {
        g.send_to_char(ch, "You find yourself unable to.\r\n");
        return;
    }

    if my_master == Some(leader) {
        act(g, "You are already following $M.", false, ch, None, ActArg::Char(leader), To::Char);
        return;
    }

    if my_aff & AFF_CHARM != 0 && my_master.is_some() {
        let m = my_master.unwrap();
        act(g, "But you only feel like following $N!", false, ch, None, ActArg::Char(m), To::Char);
        return;
    }

    // Not charmed -> follow.
    if leader == ch {
        if my_master.is_none() {
            g.send_to_char(ch, "You are already following yourself.\r\n");
            return;
        }
        stop_follower(g, ch);
    } else {
        if circle_follow(g, ch, leader) {
            act(g, "Sorry, but following in loops is not allowed.", false, ch, None, ActArg::None, To::Char);
            return;
        }
        if my_master.is_some() {
            stop_follower(g, ch);
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.affect_flags &= !AFF_GROUP;
        }
        add_follower(g, ch, leader);
    }
}

/// circle_follow: would `ch` following `leader` create a loop? (CircleMUD)
fn circle_follow(g: &GameState, ch: CharId, leader: CharId) -> bool {
    let mut k = Some(leader);
    while let Some(cur) = k {
        if cur == ch {
            return true;
        }
        k = g.get_char(cur).and_then(|c| c.master);
    }
    false
}

/// add_follower: link ch -> leader and announce (CircleMUD add_follower).
fn add_follower(g: &mut GameState, ch: CharId, leader: CharId) {
    // Guard: ch must have no master (handled by caller via stop_follower).
    if g.get_char(ch).and_then(|c| c.master).is_some() {
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.master = Some(leader);
    }
    if let Some(l) = g.get_char_mut(leader) {
        if !l.followers.contains(&ch) {
            l.followers.push(ch);
        }
    }
    act(g, "You now follow $N.", false, ch, None, ActArg::Char(leader), To::Char);
    if g.can_see(leader, ch) {
        act(g, "$n starts following you.", true, ch, None, ActArg::Char(leader), To::Vict);
    }
    act(g, "$n starts to follow $N.", true, ch, None, ActArg::Char(leader), To::NotVict);
}

/// stop_follower: break ch's follow link and announce (CircleMUD stop_follower).
fn stop_follower(g: &mut GameState, ch: CharId) {
    let master = match g.get_char(ch).and_then(|c| c.master) {
        Some(m) => m,
        None => return,
    };
    let charmed = g.get_char(ch).map(|c| c.affect_flags & AFF_CHARM != 0).unwrap_or(false);

    if charmed {
        act(g, "You realize that $N is a jerk!", false, ch, None, ActArg::Char(master), To::Char);
        act(g, "$n realizes that $N is a jerk!", false, ch, None, ActArg::Char(master), To::NotVict);
        act(g, "$n hates your guts!", false, ch, None, ActArg::Char(master), To::Vict);
    } else {
        act(g, "You stop following $N.", false, ch, None, ActArg::Char(master), To::Char);
        act(g, "$n stops following $N.", true, ch, None, ActArg::Char(master), To::NotVict);
        act(g, "$n stops following you.", true, ch, None, ActArg::Char(master), To::Vict);
    }

    // Unlink from leader's follower list and clear master/group bit.
    if let Some(l) = g.get_char_mut(master) {
        l.followers.retain(|&f| f != ch);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.master = None;
        // C: REMOVE_BIT(AFF_FLAGS(ch), AFF_CHARM | AFF_GROUP)
        c.affect_flags &= !(AFF_CHARM | AFF_GROUP);
    }
}

// ---------------------------------------------------------------------------
// Mounts (mount / dismount / buck / tame)
// ---------------------------------------------------------------------------
//
// RIDING/RIDDEN_BY are modelled as Option<CharId> pointers on Character. These
// helpers and commands port handler.c mount_char/dismount_char and
// act.movement.c do_mount/do_dismount/do_buck/do_tame 1:1.

/// mount_char (handler.c): wire ch -> mount; no checks, no messages.
pub fn mount_char(g: &mut GameState, ch: CharId, mount: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.riding = Some(mount);
    }
    if let Some(m) = g.get_char_mut(mount) {
        m.ridden_by = Some(ch);
    }
}

/// dismount_char (handler.c): break the link from whichever end ch holds.
pub fn dismount_char(g: &mut GameState, ch: CharId) {
    let riding = g.get_char(ch).and_then(|c| c.riding);
    if let Some(mount) = riding {
        if let Some(m) = g.get_char_mut(mount) {
            m.ridden_by = None;
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.riding = None;
        }
    }
    let ridden_by = g.get_char(ch).and_then(|c| c.ridden_by);
    if let Some(rider) = ridden_by {
        if let Some(r) = g.get_char_mut(rider) {
            r.riding = None;
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.ridden_by = None;
        }
    }
}

pub fn do_mount(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (word, _) = one_argument(arg);
    if word.is_empty() {
        g.send_to_char(ch, "Mount who?\r\n");
        return;
    }
    let vict = match g.get_char_room_vis(ch, &word) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "There is no-one by that name here.\r\n");
            return;
        }
    };
    let (v_is_npc, v_mountable, v_mounted) = match g.get_char(vict) {
        Some(c) => (c.is_npc, c.act_flags & MOB_MOUNTABLE != 0, c.riding.is_some() || c.ridden_by.is_some()),
        None => return,
    };
    if !v_is_npc {
        g.send_to_char(ch, "Ehh... no.\r\n");
        return;
    }
    let ch_mounted = g
        .get_char(ch)
        .map(|c| c.riding.is_some() || c.ridden_by.is_some())
        .unwrap_or(false);
    if ch_mounted {
        g.send_to_char(ch, "You are already mounted.\r\n");
        return;
    }
    if v_mounted {
        g.send_to_char(ch, "It is already mounted.\r\n");
        return;
    }
    if v_is_npc && !v_mountable {
        g.send_to_char(ch, "You can't mount that!\r\n");
        return;
    }
    let skill = g.get_char(ch).map(|c| c.skill(SKILL_MOUNT)).unwrap_or(0);
    if skill == 0 {
        g.send_to_char(ch, "First you need to learn *how* to mount.\r\n");
        return;
    }
    if (skill as i32) <= g.rng.number(1, 101) {
        act(g, "You try to mount $N, but slip and fall off.", false, ch, None, ActArg::Char(vict), To::Char);
        act(g, "$n tries to mount you, but slips and falls off.", false, ch, None, ActArg::Char(vict), To::Vict);
        act(g, "$n tries to mount $N, but slips and falls off.", true, ch, None, ActArg::Char(vict), To::NotVict);
        let d = g.rng.dice(1, 2);
        crate::combat::damage(g, ch, ch, d);
        return;
    }

    act(g, "You mount $N.", false, ch, None, ActArg::Char(vict), To::Char);
    act(g, "$n mounts you.", false, ch, None, ActArg::Char(vict), To::Vict);
    act(g, "$n mounts $N.", true, ch, None, ActArg::Char(vict), To::NotVict);
    mount_char(g, ch, vict);

    // An untamed mob may buck a fresh rider (second skill roll).
    let v_tamed = g.get_char(vict).map(|c| c.affect_flags & AFF_TAMED != 0).unwrap_or(false);
    let skill2 = g.get_char(ch).map(|c| c.skill(SKILL_MOUNT)).unwrap_or(0);
    if v_is_npc && !v_tamed && (skill2 as i32) <= g.rng.number(1, 101) {
        act(g, "$N suddenly bucks upwards, throwing you violently to the ground!", false, ch, None, ActArg::Char(vict), To::Char);
        act(g, "$n is thrown to the ground as $N violently bucks!", true, ch, None, ActArg::Char(vict), To::NotVict);
        act(g, "You buck violently and throw $n to the ground.", false, ch, None, ActArg::Char(vict), To::Vict);
        dismount_char(g, ch);
        let d = g.rng.dice(1, 3);
        crate::combat::damage(g, vict, ch, d);
    }
}

pub fn do_dismount(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let riding = g.get_char(ch).and_then(|c| c.riding);
    let riding = match riding {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "You aren't even riding anything.\r\n");
            return;
        }
    };
    let in_water = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .map(|r| g.room(r).sector_type == SectorType::WaterNoSwim)
        .unwrap_or(false);
    if in_water && !has_boat(g, ch) {
        g.send_to_char(ch, "Yah, right, and then drown...\r\n");
        return;
    }

    act(g, "You dismount $N.", false, ch, None, ActArg::Char(riding), To::Char);
    act(g, "$n dismounts from you.", false, ch, None, ActArg::Char(riding), To::Vict);
    act(g, "$n dismounts $N.", true, ch, None, ActArg::Char(riding), To::NotVict);
    dismount_char(g, ch);
}

pub fn do_buck(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let ridden_by = g.get_char(ch).and_then(|c| c.ridden_by);
    let rider = match ridden_by {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "You're not even being ridden!\r\n");
            return;
        }
    };
    if g.get_char(ch).map(|c| c.affect_flags & AFF_TAMED != 0).unwrap_or(false) {
        g.send_to_char(ch, "But you're tamed!\r\n");
        return;
    }

    act(g, "You quickly buck, throwing $N to the ground.", false, ch, None, ActArg::Char(rider), To::Char);
    act(g, "$n quickly bucks, throwing you to the ground.", false, ch, None, ActArg::Char(rider), To::Vict);
    act(g, "$n quickly bucks, throwing $N to the ground.", false, ch, None, ActArg::Char(rider), To::NotVict);
    if let Some(r) = g.get_char_mut(rider) {
        r.position = Position::Sitting;
    }
    if g.rng.number(0, 4) != 0 {
        g.send_to_char(rider, "You hit the ground hard!\r\n");
        let d = g.rng.dice(2, 4);
        crate::combat::damage(g, rider, rider, d);
    }
    dismount_char(g, ch);
}

pub fn do_tame(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (word, _) = one_argument(arg);
    if word.is_empty() {
        g.send_to_char(ch, "Tame who?\r\n");
        return;
    }
    let vict = match g.get_char_room_vis(ch, &word) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "They're not here.\r\n");
            return;
        }
    };
    let (v_is_npc, v_mountable, v_aff) = match g.get_char(vict) {
        Some(c) => (c.is_npc, c.act_flags & MOB_MOUNTABLE != 0, c.affect_flags),
        None => return,
    };
    if v_is_npc && !v_mountable {
        g.send_to_char(ch, "You can't do that to them.\r\n");
        return;
    }
    if g.get_char(ch).map(|c| c.skill(SKILL_TAME)).unwrap_or(0) == 0 {
        g.send_to_char(ch, "You don't even know how to tame something.\r\n");
        return;
    }
    if !v_is_npc {
        g.send_to_char(ch, "You can't do that.\r\n");
        return;
    }
    if v_aff & AFF_TAMED != 0 {
        g.send_to_char(ch, "It is already tamed.\r\n");
        return;
    }
    // Tamer skill roll (act.movement.c: GET_SKILL(ch,SKILL_TAME) <= number(1,101)).
    if g.get_char(ch).map(|c| c.skill(SKILL_TAME)).unwrap_or(0) as i32 <= g.rng.number(1, 101) {
        g.send_to_char(ch, "You fail to tame it.\r\n");
        return;
    }

    // affect_join(vict, AFF_TAMED, dur 24): add a timed affect carrying the
    // tamed bit and recompute totals so AFF_TAMED lands on affect_flags.
    if let Some(c) = g.get_char_mut(vict) {
        // Replace any existing tame affect (same spell + APPLY_NONE).
        c.affected.retain(|a| !(a.spell_type == SKILL_TAME as i32 && a.location == 0));
        c.affected.push(crate::character::Affect {
            spell_type: SKILL_TAME as i32,
            duration: 24,
            modifier: 0,
            location: 0, // APPLY_NONE
            bitvector: AFF_TAMED,
            caster: Some(ch),
        });
    }
    g.affect_total(vict);
    act(g, "You tame $N. It will last 24 hours.", false, ch, None, ActArg::Char(vict), To::Char);
    act(g, "$n tames you.", false, ch, None, ActArg::Char(vict), To::Vict);
    act(g, "$n tames $N.", false, ch, None, ActArg::Char(vict), To::NotVict);
}

// ---------------------------------------------------------------------------
// Small local helpers (private to this module; not added to the contract)
// ---------------------------------------------------------------------------

fn set_pos(g: &mut GameState, ch: CharId, pos: Position) {
    if let Some(c) = g.get_char_mut(ch) {
        c.position = pos;
    }
}

/// First whitespace-delimited keyword of a namelist (CircleMUD fname).
fn fname(namelist: &str) -> String {
    namelist.split_whitespace().next().unwrap_or("").to_string()
}

/// "a"/"an" article for a word (CircleMUD AN()).
fn an(word: &str) -> &'static str {
    let first = word.trim_start().chars().next().unwrap_or('x').to_ascii_lowercase();
    if "aeiou".contains(first) {
        "an"
    } else {
        "a"
    }
}

/// Uppercase the first character in place (CircleMUD CAP).
fn cap_first(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            s.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
}

// Skill numbers referenced by movement commands. These are not defined in the
// Tier-0 contract yet; define the DeltaMUD spello/skill ids locally so the
// gates compile and read the right skill slot.
const SKILL_MEDITATE: u16 = 156;
// DeltaMUD spells.h: SKILL_MOUNT 521, SKILL_RIDING 522, SKILL_TAME 523.
const SKILL_MOUNT: u16 = 521;
const SKILL_RIDING: u16 = 522;
const SKILL_TAME: u16 = 523;

// MOB_MOUNTABLE is bit 20 in DeltaMUD's action_bits (constants::ACTION_BITS
// index 20). Not a named const in flags.rs.
const MOB_MOUNTABLE: i64 = 1 << 20;
