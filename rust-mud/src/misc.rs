// misc.rs — the six leftover ACMD commands that live scattered across the C
// files whose bulk is already ported elsewhere:
//
//   do_listen / do_speed      — spells.c (DeltaMUD utility skills)
//   do_reboot  / do_pfileclean — db.c    (immortal admin: text-file reload /
//                                          stale-player prune)
//   do_attach  / do_detach     — dg_scripts.c (immortal DG: bind/unbind a
//                                          trigger vnum to a mob/obj/room)
//
// Only these six are ported here; the rest of spells.c / db.c / dg_scripts.c
// already live in their own modules (spell_parser, magic, database, dg_*). The
// usage / message strings are copied verbatim from the C source.
//
// House style (see cmd_informative.rs): read needed values into locals first,
// then mutate / send; re-look entities up by id; broadcast via act(); never
// hold a borrow across a send. The DG trigger plumbing comes from
// dg_db_scripts (real_trigger/read_trigger) and dg_handler (add_trigger /
// remove_trigger / extract_script / trigger_ids / ScriptKey).
//
// Contract gaps (called out in the manifest): the Rust port only tracks the
// `motd` text file in GameState (game.rs load_text_files reads lib/text/motd),
// so do_reboot reloads that one synchronously and acknowledges the remaining
// keywords — there is no wizlist/news/credits/help-table state on GameState to
// re-read. do_pfileclean logs + reports as in C, then queues the async database
// cleanup for game.rs to drain and reflect into player_table.

use crate::act::{ActArg, To, act};
use crate::constants::DIRS;
use crate::dg_db_scripts::{
    add_proto_trigger, clear_proto_triggers, read_trigger, real_trigger, remove_proto_trigger,
};
use crate::dg_handler::{
    self, ScriptKey, add_trigger, extract_script, remove_trigger, trigger_ids, with_trig,
};
use crate::interpreter::{is_abbrev, one_argument};
use crate::object::{ObjLoc, ObjectAffect};
use crate::room::EX_CLOSED;
use crate::spell_parser::{SKILL_LISTEN, SKILL_SPEED, get_char_world_vis, get_obj_world_vis};
use crate::state::GameState;
use crate::types::*;
use crate::world::zone_vnum_bounds;

const OK: &str = "Ok.\r\n";

// ===========================================================================
// do_speed (spells.c) — full-mana muscle-flex that refreshes movement.
// ===========================================================================
pub fn do_speed(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // GET_SKILL(ch, SKILL_SPEED) == 0 -> doesn't know how.
    if g.get_char(ch)
        .map(|c| c.skill(SKILL_SPEED as u16))
        .unwrap_or(0)
        == 0
    {
        g.send_to_char(ch, "You have no idea how to speed.\r\n");
        return;
    }

    let (mana, max_mana) = match g.get_char(ch) {
        Some(c) => (c.points.mana, c.points.max_mana),
        None => return,
    };

    if mana == max_mana {
        g.send_to_char(ch, "You retract and flex your muscles with strength.\r\n");
        if let Some(c) = g.get_char_mut(ch) {
            c.points.mana = 0;
            c.points.move_points = c.points.max_move;
        }
        g.send_to_char(ch, "You feel revived and ready to move again.\r\n");
    } else {
        g.send_to_char(ch, "You must have full mana in order to speed!\r\n");
    }
}

// ===========================================================================
// do_listen (spells.c) — SKILL_LISTEN check, then either survey the current
// room for unseen creatures or eavesdrop through a cardinal exit.
// ===========================================================================

/// CAN_LISTEN_BEHIND_DOOR(ch, dir): a thief may listen through a *closed* door
/// (the C macro requires CLASS_THIEF, an exit, a real destination, and the
/// EX_CLOSED bit set).
fn can_listen_behind_door(g: &GameState, ch: CharId, dir: usize) -> bool {
    let is_thief = g
        .get_char(ch)
        .map(|c| c.player.class == Class::Thief)
        .unwrap_or(false);
    if !is_thief {
        return false;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    match g.room(rnum).exits[dir].as_ref() {
        Some(e) => g.real_room(e.to_room).is_some() && (e.exit_info & EX_CLOSED) != 0,
        None => false,
    }
}

/// CAN_GO(ch, dir): exit exists, destination is real, and not closed.
fn can_go(g: &GameState, ch: CharId, dir: usize) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    match g.room(rnum).exits[dir].as_ref() {
        Some(e) => g.real_room(e.to_room).is_some() && (e.exit_info & EX_CLOSED) == 0,
        None => false,
    }
}

pub fn do_listen(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    const HEARD_NOTHING: &str = "You don't hear anything unusual.\r\n";
    const ROOM_SPIEL: &str = "$n seems to listen intently for something.";

    let percent = g.rng.number(1, 101);
    let skill = g
        .get_char(ch)
        .map(|c| c.skill(SKILL_LISTEN as u16) as i32)
        .unwrap_or(0);
    if skill < percent {
        g.send_to_char(ch, HEARD_NOTHING);
        return;
    }

    let (word, _) = one_argument(arg);

    if word.is_empty() {
        // No argument: listen for hidden / invisible beings in the room.
        let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let people = g.room(rnum).people.clone();
        let mut found = 0;
        for tch in people {
            if tch == ch {
                continue;
            }
            let tlevel = g.get_char(tch).map(|c| c.player.level).unwrap_or(0);
            if !g.can_see(ch, tch) && tlevel < LVL_IMMORT {
                found += 1;
            }
        }

        if found > 0 {
            let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
            let msg = if level >= 15 {
                // Being a higher level is better (a little estimate noise).
                let est = (found + g.rng.number(0, 1) - g.rng.number(0, 1)).max(1);
                format!(
                    "You hear what might be {} creatures invisible, or hiding.\r\n",
                    est
                )
            } else {
                "You hear an odd rustling in the immediate area.\r\n".to_string()
            };
            g.send_to_char(ch, &msg);
        } else {
            g.send_to_char(ch, HEARD_NOTHING);
        }
        act(g, ROOM_SPIEL, true, ch, None, ActArg::None, To::Room);
        return;
    }

    // Argument must be one of the cardinal directions.
    let mut dir = NUM_OF_DIRS;
    for d in 0..NUM_OF_DIRS {
        // C: !strncmp(buf, dirs[dir], strlen(buf)) — prefix match on the arg.
        if DIRS[d].starts_with(&word) {
            dir = d;
            break;
        }
    }
    if dir == NUM_OF_DIRS {
        g.send_to_char(ch, "Listen where?\r\n");
        return;
    }

    if can_go(g, ch, dir) || can_listen_behind_door(g, ch, dir) {
        // Count everyone in the adjacent room.
        let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let to_vnum = g.room(rnum).exits[dir].as_ref().map(|e| e.to_room);
        let dest = to_vnum.and_then(|v| g.real_room(v));
        let found = match dest {
            Some(dr) => g.room(dr).people.len() as i32,
            None => 0,
        };

        if found > 0 {
            let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
            let msg = if level >= 15 {
                let est = (found + g.rng.number(0, 1) - g.rng.number(0, 1)).max(1);
                let (lead, tail) = dir_listen_phrase(dir);
                format!(
                    "You hear what might be {} creatures {}{}.\r\n",
                    est, lead, tail
                )
            } else {
                let (lead, tail) = match dir {
                    5 => ("below", ""),
                    4 => ("above", ""),
                    _ => ("the ", DIRS[dir]),
                };
                format!("You hear sounds from {}{}.\r\n", lead, tail)
            };
            g.send_to_char(ch, &msg);
        } else {
            g.send_to_char(ch, HEARD_NOTHING);
        }
        act(g, ROOM_SPIEL, true, ch, None, ActArg::None, To::Room);
    } else {
        g.send_to_char(ch, "You can't listen in that direction.\r\n");
    }
}

/// The high-level phrasing for "creatures <where>" (C: below / above / "to the
/// <dir>"). dir 5 = down, dir 4 = up.
fn dir_listen_phrase(dir: usize) -> (&'static str, &'static str) {
    match dir {
        5 => ("below", ""),
        4 => ("above", ""),
        _ => ("to the ", DIRS[dir]),
    }
}

// ===========================================================================
// do_reboot (db.c) — bound to the `reload` command. Reload server text files
// by keyword. The Rust port carries only the `motd` text on GameState, so the
// reachable files are reloaded synchronously and the remaining keywords are
// accepted (matching C, which simply re-reads each file and replies OK).
// ===========================================================================

/// All keywords the C do_reboot recognises (besides "all"/"*"). Used to
/// validate the argument so unknown options still hit the C error path.
const RELOAD_KEYWORDS: &[&str] = &[
    "wizlist",
    "immlist",
    "news",
    "credits",
    "circlemud",
    "motd",
    "imotd",
    "help",
    "info",
    "policy",
    "handbook",
    "background",
    "startup",
    "xhelp",
];

pub fn do_reboot(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (word, _) = one_argument(arg);

    // "all" or "*" reloads everything; in the Rust port that means the motd.
    if word == "all" || word == "*" {
        reload_motd(g);
        g.send_to_char(ch, OK);
        return;
    }

    if word == "motd" {
        reload_motd(g);
        g.send_to_char(ch, OK);
        return;
    }

    // Recognised but not separately backed on GameState: accept (C re-reads the
    // file and replies OK — there is no per-file state here to refresh).
    if RELOAD_KEYWORDS.contains(&word.as_str()) {
        g.send_to_char(ch, OK);
        return;
    }

    g.send_to_char(ch, "Unknown reload option.\r\n");
}

/// Re-read lib/text/motd into GameState.motd (the C MOTD_FILE = "text/motd",
/// mirrored by game.rs load_text_files). Silently keeps the old motd on error.
fn reload_motd(g: &mut GameState) {
    let path = std::path::Path::new(&g.config.lib_path)
        .join("text")
        .join("motd");
    if let Ok(s) = std::fs::read_to_string(&path) {
        g.motd = s;
    }
}

// ===========================================================================
// do_pfileclean (db.c) — prune PLR_DELETED player records.
//
// The C clean_pfile() walks player_main, calls delete_player_entry() for every
// row whose `act` column has PLR_DELETED set, then rebuilds the in-memory
// player index. The Rust command path is synchronous, so it queues the database
// work here; game.rs drains that request asynchronously and rebuilds
// GameState.player_table after the SQL cleanup. The password gate, the
// "Cleaning..." reply, and the mudlog line are copied verbatim from C.
// ===========================================================================

pub fn do_pfileclean(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // C: skip_spaces(&argument) then strcmp(argument, "OptimisePfile").
    let argument = arg.trim_start();

    if argument == "OptimisePfile" {
        g.send_to_char(ch, "Cleaning Player File Now.\r\n");
        let name = g
            .get_char(ch)
            .map(|c| c.get_name().to_string())
            .unwrap_or_default();
        // C: mudlog(buf, NRM, LVL_IMPL, TRUE).
        mudlog_imp(g, &format!("{} initiated playerfile clean.", name));
        g.queue_pfileclean();
    } else {
        g.send_to_char(ch, "Not unless you know the password.\r\n");
    }
}

/// mudlog(line, NRM, LVL_IMPL, TRUE) — delegate to the shared `syslog::mudlog`,
/// which writes the on-disk `<lib>/syslog` line and echoes it to online
/// implementor-level immortals (filtered by their PRF_LOG syslog level).
fn mudlog_imp(g: &mut GameState, line: &str) {
    crate::syslog::mudlog(g, line, crate::syslog::NRM, LVL_IMPL);
}

fn command_atoi(g: &mut GameState, ch: CharId, value: &str) -> Option<i32> {
    match crate::text::parse_i32_atoi(value) {
        Ok(value) => Some(value),
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(ch, "That number is outside the supported 32-bit range.\r\n");
            None
        }
        Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
    }
}

// ===========================================================================
// do_attach (dg_scripts.c) — bind trigger vnum -> mob/obj/room target.
//   attach { mtr | otr | wtr } { trigger } { name } [ location ]
// ===========================================================================
pub fn do_attach(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // C: argument = two_arguments(argument, arg, trig_name);
    //    two_arguments(argument, targ_name, loc_name);
    // two_arguments is just two repeated one_argument calls (per the contract),
    // so the whole line tokenises as kind, trig_name, targ_name, loc_name.
    let (kind, r1) = one_argument(arg);
    let (trig_name, r2) = one_argument(r1);
    let (targ_name, r3) = one_argument(r2);
    let (loc_name, _) = one_argument(r3);

    if kind.is_empty() || targ_name.is_empty() || trig_name.is_empty() {
        g.send_to_char(
            ch,
            "Usage: attach { mtr | otr | wtr } { trigger } { name } [ location ]\r\n",
        );
        return;
    }

    let Some(tn) = command_atoi(g, ch, &trig_name) else {
        return;
    };
    // loc = (*loc_name) ? atoi(loc_name) : -1;
    let loc: i32 = if loc_name.is_empty() {
        -1
    } else {
        let Some(location) = command_atoi(g, ch, &loc_name) else {
            return;
        };
        location
    };

    if is_abbrev(&kind, "mtr") {
        match get_char_world_vis(g, ch, &targ_name) {
            Some(victim) => {
                let is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
                if is_npc {
                    attach_to(
                        g,
                        ch,
                        ScriptKey::Mob(victim),
                        tn,
                        loc,
                        AttachDesc::Char(victim),
                    );
                } else {
                    g.send_to_char(ch, "Players can't have scripts.\r\n");
                }
            }
            None => g.send_to_char(ch, "That mob does not exist.\r\n"),
        }
    } else if is_abbrev(&kind, "otr") {
        match get_obj_world_vis(g, ch, &targ_name) {
            Some(object) => {
                attach_to(
                    g,
                    ch,
                    ScriptKey::Obj(object),
                    tn,
                    loc,
                    AttachDesc::Obj(object),
                );
            }
            None => g.send_to_char(ch, "That object does not exist.\r\n"),
        }
    } else if is_abbrev(&kind, "wtr") {
        // C: isdigit(*targ_name) && !strchr(targ_name, '.')
        let first_digit = targ_name
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
        if first_digit && !targ_name.contains('.') {
            // find_target_room reduces to a room-vnum lookup for digit input.
            let Some(vnum) = command_atoi(g, ch, &targ_name) else {
                return;
            };
            match g.real_room(vnum) {
                Some(room) => {
                    let rvnum = g.room(room).number;
                    attach_to(
                        g,
                        ch,
                        ScriptKey::Room(room),
                        tn,
                        loc,
                        AttachDesc::Room(rvnum),
                    );
                }
                None => {
                    // find_target_room messages "No room exists..." then the C
                    // branch falls through with NOWHERE (no further message).
                    g.send_to_char(ch, "No room exists with that number.\r\n");
                }
            }
        } else {
            g.send_to_char(ch, "You need to supply a room number.\r\n");
        }
    } else {
        g.send_to_char(ch, "Please specify 'mtr', otr', or 'wtr'.\r\n");
    }
}

/// Describes the attach target for the success message.
enum AttachDesc {
    Char(CharId),
    Obj(ObjId),
    Room(RoomVnum),
}

/// Shared body of the three attach branches: real_trigger -> read_trigger ->
/// add_trigger, then print the C success / "trigger does not exist" message.
fn attach_to(g: &mut GameState, ch: CharId, key: ScriptKey, tn: i32, loc: i32, desc: AttachDesc) {
    let rn = real_trigger(tn);
    let tid = if rn >= 0 {
        read_trigger(rn as usize)
    } else {
        None
    };
    match tid {
        Some(tid) => {
            add_trigger(key, tid, loc);
            persist_proto_attach(g, key, tn);
            let trig_name = with_trig(tid, |t| t.name.clone()).unwrap_or_default();
            let msg = match desc {
                AttachDesc::Char(c) => {
                    let short = char_short(g, c);
                    format!("Trigger {} ({}) attached to {}.\r\n", tn, trig_name, short)
                }
                AttachDesc::Obj(o) => {
                    let short = obj_short(g, o);
                    format!("Trigger {} ({}) attached to {}.\r\n", tn, trig_name, short)
                }
                AttachDesc::Room(vnum) => {
                    format!(
                        "Trigger {} ({}) attached to room {}.\r\n",
                        tn, trig_name, vnum
                    )
                }
            };
            g.send_to_char(ch, &msg);
        }
        None => g.send_to_char(ch, "That trigger does not exist.\r\n"),
    }
}

// ===========================================================================
// do_detach (dg_scripts.c) — unbind a trigger (or 'all') from a target.
//   detach [ mob | object | room ] { target } { trigger | 'all' }
// ===========================================================================
pub fn do_detach(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // C: argument = two_arguments(argument, arg1, arg2); one_argument(argument, arg3);
    let (arg1, r1) = one_argument(arg);
    let (arg2, r2) = one_argument(r1);
    let (arg3, _) = one_argument(r2);

    if arg1.is_empty() || arg2.is_empty() {
        g.send_to_char(
            ch,
            "Usage: detach [ mob | object | room ] { target } { trigger | 'all' }\r\n",
        );
        return;
    }

    // --- room branch: operates on the immortal's current room. ---
    if arg1 == "room" {
        let room = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let key = ScriptKey::Room(room);
        if !dg_handler::has_script(key) {
            g.send_to_char(ch, "This room does not have any triggers.\r\n");
        } else if arg2 == "all" {
            extract_script(key);
            persist_proto_clear(g, key);
            g.send_to_char(ch, "All triggers removed from room.\r\n");
        } else if remove_trigger(key, &arg2) {
            persist_proto_remove(g, key, &arg2);
            g.send_to_char(ch, "Trigger removed.\r\n");
            // remove_trigger already drops the script container if it emptied
            // the trig_list; nothing else to do (matches C's TRIGGERS() check).
        } else {
            g.send_to_char(ch, "That trigger was not found.\r\n");
        }
        return;
    }

    // --- mob / object / fuzzy branches. ---
    let mut victim: Option<CharId> = None;
    let mut object: Option<ObjId> = None;
    let mut trigger: Option<String> = None;

    if is_abbrev(&arg1, "mob") {
        match get_char_world_vis(g, ch, &arg2) {
            None => g.send_to_char(ch, "No such mobile around.\r\n"),
            Some(v) => {
                if arg3.is_empty() {
                    g.send_to_char(ch, "You must specify a trigger to remove.\r\n");
                } else {
                    victim = Some(v);
                    trigger = Some(arg3.clone());
                }
            }
        }
    } else if is_abbrev(&arg1, "object") {
        match get_obj_world_vis(g, ch, &arg2) {
            None => g.send_to_char(ch, "No such object around.\r\n"),
            Some(o) => {
                if arg3.is_empty() {
                    g.send_to_char(ch, "You must specify a trigger to remove.\r\n");
                } else {
                    object = Some(o);
                    trigger = Some(arg3.clone());
                }
            }
        }
    } else {
        // Fuzzy: equip, then inventory, then room creature, then room ground,
        // then world creature, then world object — first hit wins (C order).
        if let Some(o) = obj_in_equip(g, ch, &arg1) {
            object = Some(o);
        } else if let Some(o) = obj_in_carrying(g, ch, &arg1) {
            object = Some(o);
        } else if let Some(v) = g.get_char_room_vis(ch, &arg1) {
            victim = Some(v);
        } else if let Some(o) = obj_in_room(g, ch, &arg1) {
            object = Some(o);
        } else if let Some(v) = get_char_world_vis(g, ch, &arg1) {
            victim = Some(v);
        } else if let Some(o) = get_obj_world_vis(g, ch, &arg1) {
            object = Some(o);
        } else {
            g.send_to_char(ch, "Nothing around by that name.\r\n");
        }
        // C: trigger = arg2; (the second word is the trigger here).
        trigger = Some(arg2.clone());
    }

    if let Some(v) = victim {
        let is_npc = g.get_char(v).map(|c| c.is_npc).unwrap_or(false);
        let key = ScriptKey::Mob(v);
        let trigger_all = trigger.as_deref() == Some("all");
        if !is_npc {
            g.send_to_char(ch, "Players don't have triggers.\r\n");
        } else if !dg_handler::has_script(key) {
            g.send_to_char(ch, "That mob doesn't have any triggers.\r\n");
        } else if trigger_all {
            extract_script(key);
            persist_proto_clear(g, key);
            let short = char_short(g, v);
            g.send_to_char(ch, &format!("All triggers removed from {}.\r\n", short));
        } else if matches!(&trigger, Some(t) if remove_trigger(key, t)) {
            if let Some(t) = &trigger {
                persist_proto_remove(g, key, t);
            }
            g.send_to_char(ch, "Trigger removed.\r\n");
        } else {
            g.send_to_char(ch, "That trigger was not found.\r\n");
        }
    } else if let Some(o) = object {
        let key = ScriptKey::Obj(o);
        let trigger_all = trigger.as_deref() == Some("all");
        if !dg_handler::has_script(key) {
            g.send_to_char(ch, "That object doesn't have any triggers.\r\n");
        } else if trigger_all {
            extract_script(key);
            persist_proto_clear(g, key);
            let short = obj_short(g, o);
            g.send_to_char(ch, &format!("All triggers removed from {}.\r\n", short));
        } else if matches!(&trigger, Some(t) if remove_trigger(key, t)) {
            if let Some(t) = &trigger {
                persist_proto_remove(g, key, t);
            }
            g.send_to_char(ch, "Trigger removed.\r\n");
        } else {
            g.send_to_char(ch, "That trigger was not found.\r\n");
        }
    }
}

fn persist_proto_attach(g: &mut GameState, key: ScriptKey, trig_vnum: i32) {
    let Some((kind, entity_vnum, save_kind)) = proto_target(g, key) else {
        return;
    };
    if add_proto_trigger(kind, entity_vnum, trig_vnum) {
        mark_proto_script_dirty(g, entity_vnum, save_kind);
    }
}

fn persist_proto_remove(g: &mut GameState, key: ScriptKey, trigger: &str) {
    let Some((kind, entity_vnum, save_kind)) = proto_target(g, key) else {
        return;
    };
    if remove_proto_trigger(kind, entity_vnum, trigger) {
        mark_proto_script_dirty(g, entity_vnum, save_kind);
    }
}

fn persist_proto_clear(g: &mut GameState, key: ScriptKey) {
    let Some((kind, entity_vnum, save_kind)) = proto_target(g, key) else {
        return;
    };
    if clear_proto_triggers(kind, entity_vnum) {
        mark_proto_script_dirty(g, entity_vnum, save_kind);
    }
}

fn proto_target(g: &GameState, key: ScriptKey) -> Option<(i32, i32, i32)> {
    match key {
        ScriptKey::Mob(ch) => {
            let vnum = g.get_char(ch).filter(|c| c.is_npc).map(|c| c.nr)?;
            if vnum < 0 {
                None
            } else {
                Some((key.trig_type(), vnum, crate::olc::OLC_SAVE_MOB))
            }
        }
        ScriptKey::Obj(obj) => {
            let vnum = g.get_obj(obj).map(|o| o.item_number)?;
            if vnum < 0 {
                None
            } else {
                Some((key.trig_type(), vnum, crate::olc::OLC_SAVE_OBJ))
            }
        }
        ScriptKey::Room(room) => {
            let vnum = g.rooms.get(room).map(|r| r.number)?;
            Some((key.trig_type(), vnum, crate::olc::OLC_SAVE_ROOM))
        }
    }
}

fn mark_proto_script_dirty(g: &mut GameState, entity_vnum: i32, save_kind: i32) {
    if let Some(zone_rnum) = crate::olc::real_zone(g, entity_vnum) {
        if let Some(zone_number) = g.zones.get(zone_rnum).map(|z| z.number) {
            crate::olc::olc_add_to_save_list(zone_number, save_kind);
        }
    }
}

// ---------------------------------------------------------------------------
// Local helpers
// ---------------------------------------------------------------------------

/// GET_SHORT(victim): NPC short_description, else the (display) name.
fn char_short(g: &GameState, cid: CharId) -> String {
    g.get_char(cid)
        .map(|c| {
            if let Some(s) = &c.short_desc {
                if !s.is_empty() {
                    return s.clone();
                }
            }
            c.player.name.clone()
        })
        .unwrap_or_else(|| "someone".to_string())
}

/// C: object->short_description ? short_description : object->name.
fn obj_short(g: &GameState, oid: ObjId) -> String {
    g.get_obj(oid)
        .map(|o| {
            if !o.short_description.is_empty() {
                o.short_description.clone()
            } else {
                o.name.clone()
            }
        })
        .unwrap_or_else(|| "something".to_string())
}

/// get_object_in_equip_vis: a worn item matching `name`.
fn obj_in_equip(g: &GameState, ch: CharId, name: &str) -> Option<ObjId> {
    let eq: Vec<ObjId> = g
        .get_char(ch)?
        .equipment
        .iter()
        .flatten()
        .copied()
        .collect();
    g.get_obj_in_list_vis(ch, name, &eq)
}

/// get_obj_in_list_vis(ch, name, ch->carrying).
fn obj_in_carrying(g: &GameState, ch: CharId, name: &str) -> Option<ObjId> {
    let inv = g.get_char(ch)?.carrying.clone();
    g.get_obj_in_list_vis(ch, name, &inv)
}

/// get_obj_in_list_vis(ch, name, world[IN_ROOM(ch)].contents).
fn obj_in_room(g: &GameState, ch: CharId, name: &str) -> Option<ObjId> {
    let rnum = g.get_char(ch)?.in_room?;
    let contents = g.room(rnum).contents.clone();
    g.get_obj_in_list_vis(ch, name, &contents)
}

// Keep ObjLoc / trigger_ids referenced so the imports document the contract
// surface this module relies on (loc resolution / trigger snapshotting).
#[allow(dead_code)]
fn _contract_anchor(g: &GameState, oid: ObjId, key: ScriptKey) -> (bool, usize) {
    let in_room = matches!(g.get_obj(oid).map(|o| o.loc), Some(ObjLoc::Room(_)));
    (in_room, trigger_ids(key).len())
}

// ===========================================================================
// do_tlist / do_tstat (dg_scripts.c) — trigger prototype inspection.
// ===========================================================================
pub fn do_tlist(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, rest) = crate::interpreter::one_argument(arg);
    let (b, _) = crate::interpreter::one_argument(rest);
    if a.is_empty() {
        g.send_to_char(
            ch,
            "Usage: tlist <begining number or zone> [<ending number>]\r\n",
        );
        return;
    }
    let Some(parsed_first) = command_atoi(g, ch, &a) else {
        return;
    };
    let (first, last): (i32, i32) = if !b.is_empty() {
        let Some(last) = command_atoi(g, ch, &b) else {
            return;
        };
        (parsed_first, last)
    } else {
        let Some(first) = parsed_first.checked_mul(100) else {
            g.send_to_char(
                ch,
                "That trigger range is outside the supported 32-bit range.\r\n",
            );
            return;
        };
        let Some(last) = first.checked_add(99) else {
            g.send_to_char(
                ch,
                "That trigger range is outside the supported 32-bit range.\r\n",
            );
            return;
        };
        (first, last)
    };
    if first < 0 || last < 0 {
        g.send_to_char(
            ch,
            "Values must be between 0 and highest possible vnum.\n\r",
        );
        return;
    }
    if first >= last {
        g.send_to_char(ch, "Second value must be greater than first.\n\r");
        return;
    }
    let mut found = 0;
    let mut out = String::new();
    for nr in 0..crate::dg_db_scripts::top_of_trigt() {
        if let Some(tp) = crate::dg_db_scripts::trig_proto(nr) {
            if tp.vnum > last {
                break;
            }
            if tp.vnum >= first {
                found += 1;
                out.push_str(&format!("{:5}. [{:5}] {}\r\n", found, tp.vnum, tp.name));
            }
        }
    }
    if found == 0 {
        out.push_str("No triggers found in that range.\r\n");
    }
    // C dg_scripts.c:2407 do_tlist ends with page_string(ch->desc, ...).
    match ch_desc(g, ch) {
        Some(conn) => crate::modify::page_string(g, conn, &out),
        None => g.send_to_char(ch, &out),
    }
}

fn ch_desc(g: &GameState, ch: CharId) -> Option<crate::types::ConnId> {
    g.get_char(ch).and_then(|c| c.desc)
}

pub fn do_tstat(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (s, _) = crate::interpreter::half_chop(arg);
    if s.is_empty() {
        g.send_to_char(ch, "Usage: tstat <vnum>\r\n");
        return;
    }
    let Some(vnum) = command_atoi(g, ch, &s) else {
        return;
    };
    let rnum = crate::dg_db_scripts::real_trigger(vnum);
    if rnum < 0 {
        g.send_to_char(ch, "That vnum does not exist.\r\n");
        return;
    }
    if let Some(tp) = crate::dg_db_scripts::trig_proto(rnum as usize) {
        let kind = match tp.attach_type {
            0 => "Mobiles",
            1 => "Objects",
            _ => "Rooms",
        };
        let mut out = String::new();
        out.push_str(&format!(
            "Name: '{}',  VNum: [{:5}], RNum: [{:5}]\r\n",
            tp.name, tp.vnum, rnum
        ));
        out.push_str(&format!("Trigger Intended Assignment: {}\r\n", kind));
        out.push_str(&format!(
            "Trigger Type: {}, Numeric Arg: {}, Arg list: {}\r\n",
            tp.trigger_type, tp.narg, tp.arglist
        ));
        out.push_str("Commands:\r\n");
        for line in &tp.cmdlist {
            out.push_str(line);
            out.push_str("\r\n");
        }
        g.send_to_char(ch, &out);
    }
}

// ===========================================================================
// do_rebalance (olc.c) — rebalance a zone's mob/obj stats.
// ===========================================================================
pub fn do_rebalance(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (what, rest) = crate::interpreter::one_argument(arg);
    let (znum_s, _) = crate::interpreter::one_argument(rest);
    if what.is_empty() || znum_s.is_empty() {
        g.send_to_char(ch, "Format: rebalance <mob/obj> <znum>\r\n");
        return;
    }
    let tobalance = if crate::interpreter::is_abbrev(&what, "obj") {
        1
    } else if crate::interpreter::is_abbrev(&what, "mob") {
        2
    } else {
        g.send_to_char(ch, "Balance mobs or objects?\r\n");
        return;
    };
    let znum = match crate::text::parse_i32_strict(&znum_s) {
        Ok(value) => value,
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
            return;
        }
        Err(_) => {
            g.send_to_char(ch, "Invalid zone number.\r\n");
            return;
        }
    };
    let zone_vnum = match zone_vnum_bounds(znum) {
        Some((value, _)) => value,
        None => {
            g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
            return;
        }
    };
    let zone_idx = match crate::olc::real_zone(g, zone_vnum) {
        Some(z) => z,
        None => {
            g.send_to_char(ch, "Invalid zone number.\r\n");
            return;
        }
    };
    if !crate::olc::can_edit_zone(g, ch, zone_idx) {
        g.send_to_char(ch, "You do not have permission to edit that zone.\r\n");
        return;
    }
    let zone_number = g.zones[zone_idx].number;
    let Some(zone_start) = g.zones[zone_idx].vnum_start() else {
        g.send_to_char(ch, "Invalid zone number.\r\n");
        return;
    };
    let zone_end = g.zones[zone_idx].top;
    if tobalance == 1 {
        for proto in g.obj_protos.values_mut() {
            if proto.vnum >= zone_start && proto.vnum <= zone_end {
                rebalance_object_proto(proto);
            }
        }
    } else {
        for proto in g.mob_protos.values_mut() {
            if proto.vnum >= zone_start && proto.vnum <= zone_end {
                rebalance_mobile_proto(proto);
            }
        }
    }
    g.send_to_char(
        ch,
        &format!(
            "Rebalancing {} in zone {}: You will have to save this zone for changes to be permanent.\r\n",
            if tobalance == 1 { "objects" } else { "mobiles" },
            zone_number
        ),
    );
    crate::olc::olc_add_to_save_list(zone_number, if tobalance == 1 { 1 } else { 3 });
}

fn rebalance_object_proto(proto: &mut crate::world::ObjectProto) {
    if proto.min_level == 0 {
        return;
    }
    const STRENGTHS: [[i32; 5]; 6] = [
        [3, 3, 3, 3, 3],
        [1, 5, 1, 5, 4],
        [2, 3, 4, 4, 3],
        [4, 1, 4, 2, 5],
        [5, 1, 5, 2, 3],
        [2, 5, 2, 2, 5],
    ];
    let row = if (0..5).contains(&proto.obj_class) {
        (proto.obj_class + 1) as usize
    } else {
        0
    };
    let row = STRENGTHS[row];
    let modnum =
        |num: usize| -> i32 { 750 * row[num] * proto.min_level / 5 / 100 / (NUM_WEARS as i32) };
    let applies = [
        (crate::flags::APPLY_POWER, modnum(0)),
        (crate::flags::APPLY_MPOWER, modnum(1)),
        (crate::flags::APPLY_DEFENSE, modnum(2)),
        (crate::flags::APPLY_MDEFENSE, modnum(3)),
        (crate::flags::APPLY_TECHNIQUE, modnum(4)),
    ];
    if proto.affects.len() < applies.len() {
        proto.affects.resize(
            applies.len(),
            ObjectAffect {
                location: crate::flags::APPLY_NONE,
                modifier: 0,
            },
        );
    }
    for (slot, (location, modifier)) in applies.into_iter().enumerate() {
        proto.affects[slot] = ObjectAffect { location, modifier };
    }
}

fn rebalance_mobile_proto(proto: &mut crate::world::MobileProto) {
    let level = proto.level as f64;
    // Same medit.c:1442-1447 double-math fix as set_mob_stats (#265).
    proto.defense =
        ((level * 7.5 * 7.0 / 10.0) as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    proto.mdefense = proto.defense;
    proto.power =
        ((level * 7.5 * 8.0 / 10.0) as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    proto.mpower = proto.defense;
    proto.technique = proto.defense;
    proto.damnodice = 1;
    proto.damsizedice = 1 + ((proto.level as i32) - 1) * 3;
    proto.hitpoints = 20 + ((proto.level as i32) - 1) * 17;
    proto.gold = ((proto.level as i64) * 10) as Gold;
    let etl_now = crate::limits::exp_to_level(proto.level as i32);
    let etl_prev = crate::limits::exp_to_level(proto.level as i32 - 1);
    proto.experience = (((etl_now - etl_prev) / (19 + level as i64)) as f64 * 1.5) as Experience;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::object::{ExtraFlags, ObjectType, WearFlags};
    use crate::world::{MobileProto, ObjectProto, Zone};

    fn connected_player(g: &mut GameState, conn: ConnId, name: &str, level: Level) -> CharId {
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.player.level = level;
        let id = g.create_char(ch);
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.character = Some(id);
        }
        id
    }

    fn test_zone(number: i32, top: RoomVnum, builders: &str) -> Zone {
        Zone {
            number,
            name: format!("Zone {}", number),
            builders: builders.to_string(),
            lifespan: 30,
            age: 0,
            top,
            reset_mode: 2,
            min_level: 0,
            max_level: 60,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        }
    }

    fn object_proto(vnum: ObjVnum, short: &str) -> ObjectProto {
        ObjectProto {
            vnum,
            name: short.to_string(),
            short_desc: short.to_string(),
            description: format!("{} is here.", short),
            obj_type: ObjectType::Armor,
            wear_flags: WearFlags::TAKE,
            extra_flags: ExtraFlags::empty(),
            weight: 1,
            cost: 0,
            rent: 0,
            values: [0; 4],
            curr_slots: 0,
            total_slots: 0,
            obj_class: -1,
            min_level: 0,
            bitvector: 0,
            action_description: String::new(),
            affects: Vec::new(),
            ex_descriptions: Vec::new(),
        }
    }

    fn mobile_proto(vnum: MobVnum, short: &str, level: Level) -> MobileProto {
        MobileProto {
            vnum,
            name: short.to_string(),
            short_desc: short.to_string(),
            long_desc: format!("{} is here.\r\n", short),
            description: String::new(),
            level,
            hitpoints: 1,
            hit_dice: (0, 0, 1),
            experience: 0,
            gold: 0,
            position: Position::Standing,
            default_pos: Position::Standing,
            sex: Gender::Neutral,
            alignment: 0,
            act_flags: 0,
            affect_flags: 0,
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 1,
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
    fn rebalance_objects_updates_zone_proto_applies() {
        let mut g = GameState::new(Config::default());
        g.zones.push(test_zone(1, 199, "Imm"));
        let ch = connected_player(&mut g, ConnId(1), "Imm", LVL_IMMORT);
        let mut target = object_proto(150, "target");
        target.min_level = 10;
        g.obj_protos.insert(150, target);
        let mut outside = object_proto(250, "outside");
        outside.min_level = 10;
        g.obj_protos.insert(250, outside);

        do_rebalance(&mut g, ch, "obj 1", 0);

        let proto = g.obj_protos.get(&150).unwrap();
        assert_eq!(proto.affects[0].location, crate::flags::APPLY_POWER);
        assert_eq!(proto.affects[0].modifier, 2);
        assert_eq!(proto.affects[1].location, crate::flags::APPLY_MPOWER);
        assert_eq!(proto.affects[2].location, crate::flags::APPLY_DEFENSE);
        assert_eq!(proto.affects[3].location, crate::flags::APPLY_MDEFENSE);
        assert_eq!(proto.affects[4].location, crate::flags::APPLY_TECHNIQUE);
        assert!(g.obj_protos.get(&250).unwrap().affects.is_empty());
        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("Rebalancing objects in zone 1")
        );
        crate::olc::olc_remove_from_save_list(1, crate::olc::OLC_SAVE_OBJ);
    }

    #[test]
    fn rebalance_mobiles_updates_zone_proto_stats() {
        let mut g = GameState::new(Config::default());
        g.zones.push(test_zone(1, 199, "Imm"));
        let ch = connected_player(&mut g, ConnId(1), "Imm", LVL_IMMORT);
        g.mob_protos.insert(150, mobile_proto(150, "target", 10));
        g.mob_protos.insert(250, mobile_proto(250, "outside", 10));

        do_rebalance(&mut g, ch, "mob 1", 0);

        let proto = g.mob_protos.get(&150).unwrap();
        assert_eq!(proto.power, 60);
        assert_eq!(proto.defense, 52);
        assert_eq!(proto.mpower, 52);
        assert_eq!(proto.mdefense, 52);
        assert_eq!(proto.technique, 52);
        assert_eq!(proto.damnodice, 1);
        assert_eq!(proto.damsizedice, 28);
        assert_eq!(proto.hitpoints, 173);
        assert_eq!(proto.gold, 100);
        assert!(proto.experience > 0);
        assert_eq!(g.mob_protos.get(&250).unwrap().power, 0);
        crate::olc::olc_remove_from_save_list(1, crate::olc::OLC_SAVE_MOB);
    }

    #[test]
    fn dg_admin_commands_reject_numeric_overflow_before_lookup_or_mutation() {
        let mut g = GameState::new(Config::default());
        let ch = connected_player(&mut g, ConnId(77), "Imm", LVL_IMMORT);

        do_attach(&mut g, ch, "wtr 2147483648 100", 0);
        do_tlist(&mut g, ch, "2147483648", 0);
        do_tlist(&mut g, ch, "21474837", 0);
        do_tstat(&mut g, ch, "-2147483649", 0);
        do_rebalance(&mut g, ch, "mob 21474837", 0);

        let output = &g.descriptors[&ConnId(77)].outbuf;
        assert_eq!(
            output.matches("outside the supported 32-bit range").count(),
            4
        );
        assert_eq!(
            output
                .matches("zone number is outside the supported range")
                .count(),
            1
        );
    }
}
