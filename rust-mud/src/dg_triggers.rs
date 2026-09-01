// dg_triggers.rs — the DG-script FIRE POINTS (DeltaMUD/CircleMUD dg_triggers.c).
//
// This module is the layer between the game engine and the script VM. Every
// game event that can fire a trigger (a mob is greeted, a player speaks, an
// object is dropped, a room is entered, a pulse ticks, …) has a `*_mtrigger` /
// `*_otrigger` / `*_wtrigger` function here. Each one:
//   1. checks the entity actually has a script of the relevant type
//      (SCRIPT_CHECK → dg_handler::script_check),
//   2. walks that entity's attached trigger list, type/narg-checking each
//      (TRIGGER_CHECK + the random number(1,100) <= narg gate),
//   3. injects the contextual %variables% (actor, victim, object, amount,
//      direction, speech, arg, cmd …) onto the trigger,
//   4. invokes the VM via `dg_scripts::script_driver`, and
//   5. returns the C `int` result (here as `bool`/`i32`): for the gate hooks,
//      false/0 = BLOCK the default action, true/nonzero = allow it. The exact
//      return conventions per hook are preserved from the C verbatim.
//
// ─── Storage model ─────────────────────────────────────────────────────────
// Character / Object / Room cannot carry a SCRIPT_DATA pointer (project rule),
// so per-entity script state lives in dg_handler's module-static tables keyed
// by a `ScriptKey` (Mob(CharId)/Obj(ObjId)/Room(RoomRnum)). This file reads
// those tables through dg_handler's accessors and drives the VM in dg_scripts:
//   * dg_handler::script_check(g, key,ty)  = SCRIPT_CHECK
//   * dg_handler::trigger_ids(g, key)      = TRIGGERS(SCRIPT(go)) walk
//   * dg_handler::with_trig(g, id, |t| …)  = GET_TRIG_* field reads
//   * dg_handler::with_trig_mut + add_var_in = add_var / ADD_UID_VAR
//   * dg_handler::memory_for / forget   = SCRIPT_MEM walk / delete
//   * dg_scripts::script_driver(g, go, trig, type, mode) -> i32 = the VM
//   * dg_scripts::script_log(msg)       = SYSERR logging
//
// number(1,100) is g.rng.number(1,100) — C's utils.c number(). The non-standard
// left-to-right precedence of the C expressions is preserved by writing the
// comparisons exactly as the C does.

use crate::dg_handler::{self as dh, ScriptKey, TrigId};
use crate::dg_scripts::{self as vm, GoRef};
use crate::flags::AFF_CHARM;
use crate::state::GameState;
use crate::types::*;

// Re-export the trigger-type bit constants from dg_handler so callers and the
// conformance tests reference one canonical set. (dg_handler is the owner.)
pub use crate::dg_handler::{
    MOB_TRIGGER, OBJ_TRIGGER, OCMD_EQUIP, OCMD_INVEN, OCMD_ROOM, TRIG_NEW, WLD_TRIGGER,
};
pub use crate::dg_handler::{
    MTRIG_ACT, MTRIG_BRIBE, MTRIG_COMMAND, MTRIG_DEATH, MTRIG_ENTRY, MTRIG_FIGHT, MTRIG_GREET,
    MTRIG_GREET_ALL, MTRIG_HITPRCNT, MTRIG_LOAD, MTRIG_MEMORY, MTRIG_RANDOM, MTRIG_RECEIVE,
    MTRIG_SPEECH,
};
pub use crate::dg_handler::{
    OTRIG_COMMAND, OTRIG_DROP, OTRIG_FIGHT, OTRIG_GET, OTRIG_GIVE, OTRIG_LOAD, OTRIG_RANDOM,
    OTRIG_REMOVE, OTRIG_TIMER, OTRIG_WEAR,
};
pub use crate::dg_handler::{
    WTRIG_COMMAND, WTRIG_DROP, WTRIG_ENTER, WTRIG_RANDOM, WTRIG_RESET, WTRIG_SPEECH,
};

/// DG UID encoding: ESC byte then the entity's raw id (dg_scripts.h UID_CHAR).
const UID_CHAR: char = '\x1b';

// ── glue over the dg_handler storage layer ─────────────────────────────────

#[inline]
fn is_set(flags: i64, bit: i64) -> bool {
    flags & bit != 0
}

/// AFF_FLAGGED(ch, AFF_CHARM).
#[inline]
fn charmed(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_CHARM != 0)
        .unwrap_or(false)
}

/// AWAKE(ch): position > SLEEPING.
#[inline]
fn awake(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.position > Position::Sleeping)
        .unwrap_or(false)
}

/// FIGHTING(ch).
#[inline]
fn fighting(g: &GameState, ch: CharId) -> Option<CharId> {
    g.get_char(ch).and_then(|c| c.fighting)
}

/// CAN_SEE(ch, actor).
#[inline]
fn can_see(g: &GameState, ch: CharId, actor: CharId) -> bool {
    g.can_see(ch, actor)
}

/// number(1,100) — utils.c, via the owned PRNG. Centralised so the random gate
/// reads exactly like the C `(number(1, 100) <= GET_TRIG_NARG(t))`.
#[inline]
fn pct(g: &mut GameState) -> i32 {
    g.rng.number(1, 100)
}

// --- GET_TRIG_* field reads (snapshot, to avoid holding the handler lock) ---

#[inline]
fn trig_type(g: &GameState, t: TrigId) -> i64 {
    dh::with_trig(g, t, |tr| tr.trigger_type).unwrap_or(0)
}
#[inline]
fn trig_narg(g: &GameState, t: TrigId) -> i32 {
    dh::with_trig(g, t, |tr| tr.narg).unwrap_or(0)
}
#[inline]
fn trig_depth(g: &GameState, t: TrigId) -> i32 {
    dh::with_trig(g, t, |tr| tr.depth).unwrap_or(0)
}
#[inline]
fn trig_vnum(g: &GameState, t: TrigId) -> i32 {
    dh::with_trig(g, t, |tr| tr.vnum).unwrap_or(0)
}
#[inline]
fn trig_arg(g: &GameState, t: TrigId) -> String {
    dh::with_trig(g, t, |tr| tr.arglist.clone()).unwrap_or_default()
}

/// TRIGGER_CHECK(t, type): type bit set AND depth == 0 (not already running).
#[inline]
fn trigger_check(g: &GameState, t: TrigId, mask: i64) -> bool {
    is_set(trig_type(g, t), mask) && trig_depth(g, t) == 0
}

// --- add_var / ADD_UID_VAR onto a trigger's local var list -------------------

/// add_var(g, &GET_TRIG_VARS(t), name, value, context).
fn add_var(g: &mut GameState, t: TrigId, name: &str, value: &str) {
    dh::with_trig_mut(g, t, |tr| dh::add_var_in(&mut tr.var_list, name, value, 0));
}

/// ADD_UID_VAR(buf, t, ch, name, 0): set %name% to a character's UID token.
fn add_uid_var_char(g: &mut GameState, t: TrigId, name: &str, ch: CharId) {
    let buf = format!("{}{}", UID_CHAR, ch.0);
    add_var(g, t, name, &buf);
}

/// ADD_UID_VAR(buf, t, obj, name, 0): set %name% to an object's UID token.
fn add_uid_var_obj(g: &mut GameState, t: TrigId, name: &str, obj: ObjId) {
    let buf = format!("{}{}", UID_CHAR, obj.0);
    add_var(g, t, name, &buf);
}

// --- script_driver dispatch (GoRef-typed VM call) ----------------------------

#[inline]
fn drive_mob(g: &mut GameState, ch: CharId, t: TrigId) -> i32 {
    vm::script_driver(g, GoRef::Mob(ch), t, MOB_TRIGGER, TRIG_NEW)
}
#[inline]
fn drive_obj(g: &mut GameState, obj: ObjId, t: TrigId) -> i32 {
    vm::script_driver(g, GoRef::Obj(obj), t, OBJ_TRIGGER, TRIG_NEW)
}
#[inline]
fn drive_wld(g: &mut GameState, room: RoomRnum, t: TrigId) -> i32 {
    vm::script_driver(g, GoRef::Room(room), t, WLD_TRIGGER, TRIG_NEW)
}

// ═══════════════════════════════════════════════════════════════════════════
//  MOB TRIGGERS
// ═══════════════════════════════════════════════════════════════════════════

/// random_mtrigger: one pulse-driven random trigger on a mob.
/// Fired from: mobact::mobile_activity (per mob, each PULSE_MOBILE) — the C
/// `random_mtrigger(ch)` call inside the mobile_activity loop.
pub fn random_mtrigger(g: &mut GameState, ch: CharId) {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_RANDOM) || charmed(g, ch) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_RANDOM) && pct(g) <= trig_narg(g, t) {
            drive_mob(g, ch, t);
            break;
        }
    }
}

/// bribe_mtrigger: coins given to a mob.
/// Fired from: cmd_item give-gold path (do_give / perform_give_gold), with the
/// gold `amount`, after the mob pockets the coins (C act.item.c ordering).
pub fn bribe_mtrigger(g: &mut GameState, ch: CharId, actor: CharId, amount: i32) {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_BRIBE) || charmed(g, ch) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_BRIBE) && amount >= trig_narg(g, t) {
            add_var(g, t, "amount", &amount.to_string());
            add_uid_var_char(g, t, "actor", actor);
            drive_mob(g, ch, t);
            break;
        }
    }
}

/// greet_memory_mtrigger: mobs in the room that *remember* `actor` react.
/// Fired from: movement, right after a character is placed in the room (the C
/// greet_memory_mtrigger(actor) call after char_to_room in do_simple_move).
pub fn greet_memory_mtrigger(g: &mut GameState, actor: CharId) {
    let rnum = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    for ch in people {
        let key = ScriptKey::Mob(ch);
        if !dh::script_check(g, key, MTRIG_MEMORY)
            || dh::memory_for(g, ch).is_empty()
            || !awake(g, ch)
            || fighting(g, ch).is_some()
            || ch == actor
            || charmed(g, ch)
        {
            continue;
        }
        for t in dh::trigger_ids(g, key) {
            if !is_set(trig_type(g, t), MTRIG_MEMORY)
                || !can_see(g, ch, actor)
                || trig_depth(g, t) != 0
                || !(pct(g) <= trig_narg(g, t))
            {
                continue;
            }
            // Walk the mob's memory; if it remembers `actor`, react then forget.
            for mem in dh::memory_for(g, ch) {
                if mem.id != actor.0 as i64 {
                    continue;
                }
                match mem.cmd {
                    Some(c) => crate::interpreter::command_interpreter(g, ch, &c),
                    None => {
                        add_uid_var_char(g, t, "actor", actor);
                        drive_mob(g, ch, t);
                    }
                }
                dh::forget(g, ch, actor.0 as i64);
            }
        }
    }
}

/// greet_mtrigger: mobs greet a character that just entered from `dir`.
/// Fired from: movement (do_simple_move), after the actor is in the new room.
/// Returns false (→0) if any greet trigger blocked the move; the mover's
/// entrance is cancelled in that case (C returns `final`).
/// `dir` is the direction the actor MOVED in (0..5); the trigger exposes the
/// *reverse* direction as %direction% (the way the actor came from).
pub fn greet_mtrigger(g: &mut GameState, actor: CharId, dir: i32) -> bool {
    let rnum = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return true,
    };
    let mut final_ok = true;
    let people = g.room(rnum).people.clone();
    for ch in people {
        let key = ScriptKey::Mob(ch);
        if !(dh::script_check(g, key, MTRIG_GREET | MTRIG_GREET_ALL))
            || !awake(g, ch)
            || fighting(g, ch).is_some()
            || ch == actor
            || charmed(g, ch)
        {
            continue;
        }
        for t in dh::trigger_ids(g, key) {
            let ty = trig_type(g, t);
            let greet_seen = is_set(ty, MTRIG_GREET) && can_see(g, ch, actor);
            let greet_all = is_set(ty, MTRIG_GREET_ALL);
            if (greet_seen || greet_all) && trig_depth(g, t) == 0 && pct(g) <= trig_narg(g, t) {
                if dir >= 0 && (dir as usize) < NUM_OF_DIRS {
                    add_var(g, t, "direction", DIR_NAMES[REV_DIR[dir as usize]]);
                }
                add_uid_var_char(g, t, "actor", actor);
                let intermediate = drive_mob(g, ch, t);
                if intermediate == 0 {
                    final_ok = false;
                }
                continue;
            }
        }
    }
    final_ok
}

/// entry_memory_mtrigger: when a *scripted* mob enters a room, react to any
/// remembered occupants. Fired from: movement, after the MOB itself moves.
pub fn entry_memory_mtrigger(g: &mut GameState, ch: CharId) {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_MEMORY) || charmed(g, ch) {
        return;
    }
    if dh::memory_for(g, ch).is_empty() {
        return;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    for actor in people {
        if actor == ch || dh::memory_for(g, ch).is_empty() {
            continue;
        }
        for mem in dh::memory_for(g, ch) {
            if mem.id != actor.0 as i64 {
                continue;
            }
            match mem.cmd {
                Some(c) => crate::interpreter::command_interpreter(g, ch, &c),
                None => {
                    for t in dh::trigger_ids(g, key) {
                        if trigger_check(g, t, MTRIG_MEMORY) && pct(g) <= trig_narg(g, t) {
                            add_uid_var_char(g, t, "actor", actor);
                            drive_mob(g, ch, t);
                            break;
                        }
                    }
                }
            }
            dh::forget(g, ch, actor.0 as i64);
        }
    }
}

/// entry_mtrigger: a scripted mob fires when *it* enters a room.
/// Fired from: movement (do_simple_move), after the mob is placed in the new
/// room. Returns 0/false to BLOCK the mob's entry (rare), else true.
pub fn entry_mtrigger(g: &mut GameState, ch: CharId) -> bool {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_ENTRY) || charmed(g, ch) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_ENTRY) && pct(g) <= trig_narg(g, t) {
            return drive_mob(g, ch, t) != 0;
        }
    }
    true
}

/// command_mtrigger: a mob in the actor's room intercepts a typed command.
/// Fired from: interpreter::run_command, BEFORE normal dispatch. Returns true
/// if a mob script consumed the command (interpreter must then stop).
pub fn command_mtrigger(g: &mut GameState, actor: CharId, cmd: &str, argument: &str) -> bool {
    let rnum = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let people = g.room(rnum).people.clone();
    for ch in people {
        let key = ScriptKey::Mob(ch);
        if !(dh::script_check(g, key, MTRIG_COMMAND) && !charmed(g, ch) && actor != ch) {
            continue;
        }
        for t in dh::trigger_ids(g, key) {
            if !is_set(trig_type(g, t), MTRIG_COMMAND) || trig_depth(g, t) != 0 {
                continue;
            }
            let targ = trig_arg(g, t);
            if targ.is_empty() {
                vm::script_log(&format!(
                    "SYSERR: Command Trigger #{} has no text argument!",
                    trig_vnum(g, t)
                ));
                continue;
            }
            if targ.starts_with('*') || cmd_prefix_match(&targ, cmd) {
                add_uid_var_char(g, t, "actor", actor);
                add_var(g, t, "arg", argument.trim_start());
                add_var(g, t, "cmd", cmd.trim_start());
                if drive_mob(g, ch, t) != 0 {
                    return true;
                }
            }
        }
    }
    false
}

/// strn_cmp(arg, cmd, strlen(arg)) == 0 → the typed `cmd` begins with the
/// trigger's arg word (case-insensitive prefix), matching C's command match.
fn cmd_prefix_match(trig_arg: &str, cmd: &str) -> bool {
    let n = trig_arg.len();
    cmd.len() >= n && cmd.as_bytes()[..n].eq_ignore_ascii_case(trig_arg.as_bytes())
}

/// speech_mtrigger: a mob reacts to something the actor said.
/// Fired from: cmd_comm::do_say (and the other speech commands), AFTER the say
/// is broadcast. `str_` is the spoken text.
pub fn speech_mtrigger(g: &mut GameState, actor: CharId, str_: &str) {
    let rnum = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    for ch in people {
        let key = ScriptKey::Mob(ch);
        if !(dh::script_check(g, key, MTRIG_SPEECH)
            && awake(g, ch)
            && !charmed(g, ch)
            && actor != ch)
        {
            continue;
        }
        for t in dh::trigger_ids(g, key) {
            if !is_set(trig_type(g, t), MTRIG_SPEECH) || trig_depth(g, t) != 0 {
                continue;
            }
            let targ = trig_arg(g, t);
            if targ.is_empty() {
                vm::script_log(&format!(
                    "SYSERR: Speech Trigger #{} has no text argument!",
                    trig_vnum(g, t)
                ));
                continue;
            }
            let narg = trig_narg(g, t);
            if (narg != 0 && word_check(str_, &targ)) || (narg == 0 && is_substring(&targ, str_)) {
                add_uid_var_char(g, t, "actor", actor);
                add_var(g, t, "speech", str_);
                drive_mob(g, ch, t);
                break;
            }
        }
    }
}

/// act_mtrigger: a mob reacts to a line that passed through act() (a social,
/// emote or message it witnessed). Fired from: the act() integration — for each
/// NPC recipient of an act() message the integrator calls act_mtrigger(to, …).
/// `str_` is the rendered line; actor/victim/object/target/arg are the act()
/// parameters.
#[allow(clippy::too_many_arguments)]
pub fn act_mtrigger(
    g: &mut GameState,
    ch: CharId,
    str_: &str,
    actor: Option<CharId>,
    victim: Option<CharId>,
    object: Option<ObjId>,
    target: Option<ObjId>,
    arg: Option<&str>,
) {
    let key = ScriptKey::Mob(ch);
    if !(dh::script_check(g, key, MTRIG_ACT) && !charmed(g, ch) && actor != Some(ch)) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if !is_set(trig_type(g, t), MTRIG_ACT) || trig_depth(g, t) != 0 {
            continue;
        }
        let targ = trig_arg(g, t);
        if targ.is_empty() {
            vm::script_log(&format!(
                "SYSERR: Act Trigger #{} has no text argument!",
                trig_vnum(g, t)
            ));
            continue;
        }
        let narg = trig_narg(g, t);
        if (narg != 0 && word_check(str_, &targ)) || (narg == 0 && is_substring(&targ, str_)) {
            if let Some(a) = actor {
                add_uid_var_char(g, t, "actor", a);
            }
            if let Some(v) = victim {
                add_uid_var_char(g, t, "victim", v);
            }
            if let Some(o) = object {
                add_uid_var_obj(g, t, "object", o);
            }
            if let Some(tg) = target {
                add_uid_var_obj(g, t, "target", tg);
            }
            if let Some(a) = arg {
                add_var(g, t, "arg", a.trim_start());
            }
            drive_mob(g, ch, t);
            break;
        }
    }
}

/// fight_mtrigger: each violence pulse while the mob is fighting.
/// Fired from: combat violence pulse, per fighting mob.
pub fn fight_mtrigger(g: &mut GameState, ch: CharId) {
    let opponent = match fighting(g, ch) {
        Some(o) => o,
        None => return,
    };
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_FIGHT) || charmed(g, ch) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_FIGHT) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", opponent);
            drive_mob(g, ch, t);
            break;
        }
    }
}

/// hitprcnt_mtrigger: fires when a fighting mob's hp drops to/below narg %.
/// Fired from: combat violence pulse, per fighting mob (after fight_mtrigger).
pub fn hitprcnt_mtrigger(g: &mut GameState, ch: CharId) {
    let opponent = match fighting(g, ch) {
        Some(o) => o,
        None => return,
    };
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_HITPRCNT) || charmed(g, ch) {
        return;
    }
    let (hit, max_hit) = match g.get_char(ch) {
        Some(c) => (c.points.hit, c.points.max_hit),
        None => return,
    };
    if max_hit == 0 {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_HITPRCNT) && (hit * 100) / max_hit <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", opponent);
            drive_mob(g, ch, t);
            break;
        }
    }
}

/// receive_mtrigger: a mob is given an object.
/// Fired from: cmd_item perform_give, BEFORE the object changes hands. Returns
/// false/0 to BLOCK the give (the mob refuses), else true.
pub fn receive_mtrigger(g: &mut GameState, ch: CharId, actor: CharId, obj: ObjId) -> bool {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_RECEIVE) || charmed(g, ch) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_RECEIVE) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", actor);
            add_uid_var_obj(g, t, "object", obj);
            return drive_mob(g, ch, t) != 0;
        }
    }
    true
}

/// death_mtrigger: a scripted mob dies.
/// Fired from: combat::die / raw_kill, BEFORE the corpse is made. `actor` is the
/// killer (None for environmental death). Returns false/0 to BLOCK the default
/// death handling, else true.
pub fn death_mtrigger(g: &mut GameState, ch: CharId, actor: Option<CharId>) -> bool {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_DEATH) || charmed(g, ch) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_DEATH) {
            if let Some(a) = actor {
                add_uid_var_char(g, t, "actor", a);
            }
            return drive_mob(g, ch, t) != 0;
        }
    }
    true
}

/// load_mtrigger: a scripted mob is loaded into the world.
/// Fired from: mob instancing (read_mobile / zone reset 'M'), right after the
/// mob is placed.
pub fn load_mtrigger(g: &mut GameState, ch: CharId) {
    let key = ScriptKey::Mob(ch);
    if !dh::script_check(g, key, MTRIG_LOAD) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, MTRIG_LOAD) && pct(g) <= trig_narg(g, t) {
            drive_mob(g, ch, t);
            break;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  OBJECT TRIGGERS
// ═══════════════════════════════════════════════════════════════════════════

/// random_otrigger: pulse-driven random trigger on an object.
/// Fired from: the DG pulse (objects in world iterated each PULSE_DG_SCRIPT).
pub fn random_otrigger(g: &mut GameState, obj: ObjId) {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_RANDOM) {
        return;
    }
    let carrier = crate::dg_comm::obj_carrier(g, obj);
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_RANDOM) && pct(g) <= trig_narg(g, t) {
            if let Some(c) = carrier {
                add_uid_var_char(g, t, "actor", c);
            }
            drive_obj(g, obj, t);
            break;
        }
    }
}

/// timer_otrigger: an object's timer expired.
/// Fired from: limits::point_update (the C timer_otrigger call when obj->timer
/// reaches zero), BEFORE the default timer-expiry handling.
pub fn timer_otrigger(g: &mut GameState, obj: ObjId) {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_TIMER) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_TIMER) {
            drive_obj(g, obj, t);
        }
    }
}

/// get_otrigger: an object is picked up.
/// Fired from: cmd_item get/perform_get_from_room, BEFORE the get completes.
/// Returns false/0 to BLOCK the pickup, else true.
pub fn get_otrigger(g: &mut GameState, obj: ObjId, actor: CharId) -> bool {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_GET) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_GET) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", actor);
            return drive_obj(g, obj, t) != 0;
        }
    }
    true
}

/// cmd_otrig: command trigger on ONE object at placement `type_` (equip/inven/
/// room). Returns true if it consumed the command. Internal helper for
/// command_otrigger.
fn cmd_otrig(
    g: &mut GameState,
    obj: ObjId,
    actor: CharId,
    cmd: &str,
    argument: &str,
    type_: i32,
) -> bool {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_COMMAND) {
        return false;
    }
    for t in dh::trigger_ids(g, key) {
        if !is_set(trig_type(g, t), OTRIG_COMMAND) || trig_depth(g, t) != 0 {
            continue;
        }
        // narg here is the OCMD_ placement bitvector, not a percent.
        if (trig_narg(g, t) & type_) != 0 {
            let targ = trig_arg(g, t);
            if targ.is_empty() {
                vm::script_log(&format!(
                    "SYSERR: O-Command Trigger #{} has no text argument!",
                    trig_vnum(g, t)
                ));
                continue;
            }
            if targ.starts_with('*') || cmd_prefix_match(&targ, cmd) {
                add_uid_var_char(g, t, "actor", actor);
                add_var(g, t, "arg", argument.trim_start());
                add_var(g, t, "cmd", cmd.trim_start());
                if drive_obj(g, obj, t) != 0 {
                    return true;
                }
            }
        }
    }
    false
}

/// command_otrigger: scan the actor's equipment, then inventory, then the room
/// contents for an object command trigger matching `cmd`.
/// Fired from: interpreter::run_command, BEFORE normal dispatch (after the world
/// and mob command triggers). Returns true if consumed.
pub fn command_otrigger(g: &mut GameState, actor: CharId, cmd: &str, argument: &str) -> bool {
    // Equipment slots.
    let equip: Vec<ObjId> = match g.get_char(actor) {
        Some(c) => c.equipment.iter().flatten().copied().collect(),
        None => return false,
    };
    for oid in equip {
        if cmd_otrig(g, oid, actor, cmd, argument, OCMD_EQUIP) {
            return true;
        }
    }
    // Carried inventory.
    let carrying: Vec<ObjId> = g
        .get_char(actor)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    for oid in carrying {
        if cmd_otrig(g, oid, actor, cmd, argument, OCMD_INVEN) {
            return true;
        }
    }
    // Room contents.
    let room_contents: Vec<ObjId> = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(rnum) => g.room(rnum).contents.clone(),
        None => return false,
    };
    for oid in room_contents {
        if cmd_otrig(g, oid, actor, cmd, argument, OCMD_ROOM) {
            return true;
        }
    }
    false
}

/// fight_otrig: an object fight trigger (helper). Sets %actor% (the wearer's
/// opponent) and %owner% (the wearer).
fn fight_otrig(g: &mut GameState, ch: CharId, obj: ObjId) {
    let opponent = match fighting(g, ch) {
        Some(o) => o,
        None => return,
    };
    let key = ScriptKey::Obj(obj);
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_FIGHT) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", opponent);
            add_uid_var_char(g, t, "owner", ch);
            drive_obj(g, obj, t);
            break;
        }
    }
}

/// fight_otrigger: each violence pulse, fire fight triggers on the fighter's
/// equipped objects. Fired from: combat violence pulse, per fighting character.
pub fn fight_otrigger(g: &mut GameState, ch: CharId) {
    if fighting(g, ch).is_none() {
        return;
    }
    let equip: Vec<ObjId> = match g.get_char(ch) {
        Some(c) => c.equipment.iter().flatten().copied().collect(),
        None => return,
    };
    for oid in equip {
        if dh::script_check(g, ScriptKey::Obj(oid), OTRIG_FIGHT) {
            fight_otrig(g, ch, oid);
        }
    }
}

/// wear_otrigger: an object is worn. Fired from: cmd_item perform_wear, BEFORE
/// the wear completes. Returns false/0 to BLOCK the wear, else true.
pub fn wear_otrigger(g: &mut GameState, obj: ObjId, actor: CharId, _where: usize) -> bool {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_WEAR) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_WEAR) {
            add_uid_var_char(g, t, "actor", actor);
            return drive_obj(g, obj, t) != 0;
        }
    }
    true
}

/// remove_otrigger: an object is removed/unequipped. Fired from: cmd_item
/// perform_remove, BEFORE removal. Returns false/0 to BLOCK, else true.
pub fn remove_otrigger(g: &mut GameState, obj: ObjId, actor: CharId) -> bool {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_REMOVE) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_REMOVE) {
            add_uid_var_char(g, t, "actor", actor);
            return drive_obj(g, obj, t) != 0;
        }
    }
    true
}

/// drop_otrigger: an object is dropped. Fired from: cmd_item perform_drop,
/// BEFORE the drop. Returns false/0 to BLOCK the drop, else true.
pub fn drop_otrigger(g: &mut GameState, obj: ObjId, actor: CharId) -> bool {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_DROP) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_DROP) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", actor);
            return drive_obj(g, obj, t) != 0;
        }
    }
    true
}

/// give_otrigger: an object is given to `victim`. Fired from: cmd_item
/// perform_give, BEFORE the give. Returns false/0 to BLOCK the give, else true.
pub fn give_otrigger(g: &mut GameState, obj: ObjId, actor: CharId, victim: CharId) -> bool {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_GIVE) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_GIVE) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", actor);
            add_uid_var_char(g, t, "victim", victim);
            return drive_obj(g, obj, t) != 0;
        }
    }
    true
}

/// load_otrigger: a scripted object is loaded. Fired from: object instancing
/// (read_object / zone reset 'O'/'G'/'E'/'P'), right after placement.
pub fn load_otrigger(g: &mut GameState, obj: ObjId) {
    let key = ScriptKey::Obj(obj);
    if !dh::script_check(g, key, OTRIG_LOAD) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, OTRIG_LOAD) && pct(g) <= trig_narg(g, t) {
            drive_obj(g, obj, t);
            break;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  WORLD (ROOM) TRIGGERS
// ═══════════════════════════════════════════════════════════════════════════

/// reset_wtrigger: a room's zone has been reset.
/// Fired from: db zone reset loop, once per reset of the room's zone.
pub fn reset_wtrigger(g: &mut GameState, room: RoomRnum) {
    let key = ScriptKey::Room(room);
    if !dh::script_check(g, key, WTRIG_RESET) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, WTRIG_RESET) && pct(g) <= trig_narg(g, t) {
            drive_wld(g, room, t);
            break;
        }
    }
}

/// random_wtrigger: pulse-driven random trigger on a room.
/// Fired from: the DG pulse (rooms iterated each PULSE_DG_SCRIPT).
pub fn random_wtrigger(g: &mut GameState, room: RoomRnum) {
    let key = ScriptKey::Room(room);
    if !dh::script_check(g, key, WTRIG_RANDOM) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, WTRIG_RANDOM) && pct(g) <= trig_narg(g, t) {
            drive_wld(g, room, t);
            break;
        }
    }
}

/// enter_wtrigger: a character enters `room` (moving in direction `dir`).
/// Fired from: movement (do_simple_move), BEFORE the actor is moved into the
/// room. Returns false/0 to BLOCK the move, else true. `dir` of -1 means a
/// non-walk entry (teleport) and suppresses the %direction% var.
pub fn enter_wtrigger(g: &mut GameState, room: RoomRnum, actor: CharId, dir: i32) -> bool {
    let key = ScriptKey::Room(room);
    if !dh::script_check(g, key, WTRIG_ENTER) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, WTRIG_ENTER) && pct(g) <= trig_narg(g, t) {
            if dir != -1 && (dir as usize) < NUM_OF_DIRS {
                add_var(g, t, "direction", DIR_NAMES[REV_DIR[dir as usize]]);
            }
            add_uid_var_char(g, t, "actor", actor);
            return drive_wld(g, room, t) != 0;
        }
    }
    true
}

/// command_wtrigger: the actor's room intercepts a typed command.
/// Fired from: interpreter::run_command, BEFORE normal dispatch (the FIRST of
/// the three command-trigger checks in C). Returns true if consumed.
pub fn command_wtrigger(g: &mut GameState, actor: CharId, cmd: &str, argument: &str) -> bool {
    let room = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let key = ScriptKey::Room(room);
    if !dh::script_check(g, key, WTRIG_COMMAND) {
        return false;
    }
    for t in dh::trigger_ids(g, key) {
        if !is_set(trig_type(g, t), WTRIG_COMMAND) || trig_depth(g, t) != 0 {
            continue;
        }
        let targ = trig_arg(g, t);
        if targ.is_empty() {
            vm::script_log(&format!(
                "SYSERR: W-Command Trigger #{} has no text argument!",
                trig_vnum(g, t)
            ));
            continue;
        }
        if targ.starts_with('*') || cmd_prefix_match(&targ, cmd) {
            add_uid_var_char(g, t, "actor", actor);
            add_var(g, t, "arg", argument.trim_start());
            add_var(g, t, "cmd", cmd.trim_start());
            return drive_wld(g, room, t) != 0;
        }
    }
    false
}

/// speech_wtrigger: the actor's room reacts to spoken text.
/// Fired from: cmd_comm::do_say (and the other speech commands), AFTER the say
/// broadcast (alongside speech_mtrigger).
pub fn speech_wtrigger(g: &mut GameState, actor: CharId, str_: &str) {
    let room = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let key = ScriptKey::Room(room);
    if !dh::script_check(g, key, WTRIG_SPEECH) {
        return;
    }
    for t in dh::trigger_ids(g, key) {
        if !is_set(trig_type(g, t), WTRIG_SPEECH) || trig_depth(g, t) != 0 {
            continue;
        }
        let targ = trig_arg(g, t);
        if targ.is_empty() {
            vm::script_log(&format!(
                "SYSERR: W-Speech Trigger #{} has no text argument!",
                trig_vnum(g, t)
            ));
            continue;
        }
        let narg = trig_narg(g, t);
        if (narg != 0 && word_check(str_, &targ)) || (narg == 0 && is_substring(&targ, str_)) {
            add_uid_var_char(g, t, "actor", actor);
            add_var(g, t, "speech", str_);
            drive_wld(g, room, t);
            break;
        }
    }
}

/// drop_wtrigger: something is dropped in the actor's room.
/// Fired from: cmd_item perform_drop, BEFORE the drop completes (after the
/// object's own drop_otrigger). Returns false/0 to BLOCK the drop, else true.
pub fn drop_wtrigger(g: &mut GameState, obj: ObjId, actor: CharId) -> bool {
    let room = match g.get_char(actor).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return true,
    };
    let key = ScriptKey::Room(room);
    if !dh::script_check(g, key, WTRIG_DROP) {
        return true;
    }
    for t in dh::trigger_ids(g, key) {
        if trigger_check(g, t, WTRIG_DROP) && pct(g) <= trig_narg(g, t) {
            add_uid_var_char(g, t, "actor", actor);
            add_uid_var_obj(g, t, "object", obj);
            return drive_wld(g, room, t) != 0;
        }
    }
    true
}

// ═══════════════════════════════════════════════════════════════════════════
//  Text-matching helpers (dg_triggers.c) — speech/act phrase matching.
//  These preserve C's exact whole-word / phrase semantics.
// ═══════════════════════════════════════════════════════════════════════════

/// one_phrase (dg_triggers.c): pull the first phrase off `arg`. A phrase is a
/// double-quoted span ("two words") or a single whitespace-delimited word.
/// Returns (phrase, rest).
fn one_phrase(arg: &str) -> (String, &str) {
    let arg = arg.trim_start();
    if arg.is_empty() {
        return (String::new(), "");
    }
    if let Some(stripped) = arg.strip_prefix('"') {
        match stripped.find('"') {
            Some(end) => (stripped[..end].to_string(), &stripped[end + 1..]),
            None => (stripped.to_string(), ""),
        }
    } else {
        let mut end = arg.len();
        for (idx, c) in arg.char_indices() {
            if c.is_whitespace() || c == '"' {
                end = idx;
                break;
            }
        }
        (arg[..end].to_string(), &arg[end..])
    }
}

/// is_substring (dg_triggers.c): true if `sub` occurs in `string` as a whole
/// word/phrase — bounded by start/end or whitespace/punctuation on both sides.
/// Case-insensitive (str_str in C is case-insensitive).
fn is_substring(sub: &str, string: &str) -> bool {
    if sub.is_empty() {
        return false;
    }
    let hay = string.to_lowercase();
    let needle = sub.to_lowercase();
    let hb = hay.as_bytes();
    let nb = needle.as_bytes();
    // C dg_triggers.c:155-174: str_str finds the FIRST occurrence only and
    // the boundary check applies to that one match - a later occurrence of
    // the keyword never rescues the match (#151).
    let pos = match hay.find(&needle) {
        Some(p) => p,
        None => return false,
    };
    let before_ok = pos == 0 || {
        let prev = hb[pos - 1];
        prev.is_ascii_whitespace() || (prev as char).is_ascii_punctuation()
    };
    let end = pos + nb.len();
    let after_ok = end == hb.len() || {
        let next = hb[end];
        next.is_ascii_whitespace() || (next as char).is_ascii_punctuation()
    };
    before_ok && after_ok
}

/// word_check (dg_triggers.c): true if `str_` contains any word/phrase from the
/// `wordlist` (phrases in double quotes). A `*` wordlist matches everything.
fn word_check(str_: &str, wordlist: &str) -> bool {
    if wordlist.starts_with('*') {
        return true;
    }
    let mut rest = wordlist;
    loop {
        let (phrase, remainder) = one_phrase(rest);
        if phrase.is_empty() {
            break;
        }
        if is_substring(&phrase, str_) {
            return true;
        }
        rest = remainder;
    }
    false
}

// ═══════════════════════════════════════════════════════════════════════════
//  Conformance self-check against the lib/world/trg/*.trg prototypes.
// ═══════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::dg_handler::{DG_TEST_LOCK, TrigData};
    use crate::object::Object;
    use crate::room::Room;
    use std::collections::HashMap;

    fn install_active_trigger(
        g: &mut GameState,
        attach_type: i32,
        trigger_type: i64,
        narg: i32,
    ) -> TrigId {
        dh::install_trig(
            g,
            TrigData {
                nr: 0,
                vnum: 9999,
                attach_type,
                name: "active reentry test".into(),
                trigger_type,
                narg,
                arglist: "*".into(),
                cmdlist: vec!["return 1".into()],
                curr_line: 0,
                depth: 1,
                loops: 0,
                wait_event: None,
                var_list: Vec::new(),
                purged: false,
                loop_origin: HashMap::new(),
            },
        )
    }

    fn install_command_trigger(g: &mut GameState, name: &str, commands: &[&str]) -> TrigId {
        dh::install_trig(
            g,
            TrigData {
                nr: 0,
                vnum: 10_001,
                attach_type: MOB_TRIGGER,
                name: name.into(),
                trigger_type: MTRIG_COMMAND,
                narg: 0,
                arglist: "*".into(),
                cmdlist: commands.iter().map(|command| (*command).into()).collect(),
                curr_line: 0,
                depth: 0,
                loops: 0,
                wait_event: None,
                var_list: Vec::new(),
                purged: false,
                loop_origin: HashMap::new(),
            },
        )
    }

    fn assert_still_active(g: &GameState, trigger: TrigId) {
        assert_eq!(dh::with_trig(g, trigger, |t| t.depth), Some(1));
    }

    #[test]
    fn substring_is_whole_word() {
        assert!(is_substring("hello", "well hello there"));
        assert!(is_substring("hello", "hello"));
        assert!(is_substring("hello", "say hello, friend"));
        assert!(!is_substring("ell", "hello"));
        assert!(!is_substring("hell", "hello"));
    }

    #[test]
    fn wordcheck_phrases_and_star() {
        assert!(word_check("anything at all", "*"));
        assert!(word_check("the magic word is xyzzy", "xyzzy"));
        assert!(word_check("open sesame please", "\"open sesame\""));
        assert!(!word_check("nope", "yes maybe"));
    }

    #[test]
    fn one_phrase_quoted_and_word() {
        let (p, rest) = one_phrase("\"two words\" tail");
        assert_eq!(p, "two words");
        assert_eq!(rest.trim_start(), "tail");
        let (p2, rest2) = one_phrase("single rest");
        assert_eq!(p2, "single");
        assert_eq!(rest2.trim_start(), "rest");
    }

    #[test]
    fn cmd_prefix_is_case_insensitive_prefix() {
        assert!(cmd_prefix_match("pull", "pull lever"));
        assert!(cmd_prefix_match("pu", "PULL"));
        assert!(!cmd_prefix_match("pull", "pu"));
    }

    // 14.trg / 15.trg are wld triggers ("2 g 100" / "2 g 14"): attach-type 2 =
    // WLD, flag letter 'g' = the 7th flag bit (a=0 … g=6) => 1<<6 = WTRIG_ENTER.
    #[test]
    fn wtrig_enter_bit_matches_trg_encoding() {
        assert_eq!(WTRIG_ENTER, 1 << 6);
        assert!(is_set(WTRIG_ENTER, WTRIG_ENTER));
    }

    #[test]
    fn ocmd_placement_bits_distinct() {
        assert_eq!(OCMD_EQUIP | OCMD_INVEN | OCMD_ROOM, 0b111);
        assert_ne!(OCMD_EQUIP, OCMD_INVEN);
        assert_ne!(OCMD_INVEN, OCMD_ROOM);
    }

    #[test]
    fn uid_var_encoding_is_esc_plus_id() {
        let buf = format!("{}{}", UID_CHAR, CharId(4242).0);
        assert_eq!(buf.as_bytes()[0], 0x1b);
        assert_eq!(&buf[1..], "4242");
    }

    #[test]
    fn active_command_speech_and_act_triggers_do_not_reenter() {
        let _lock = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = GameState::new(Config::default());

        dh::boot_handler(&mut g);
        let room = g.add_room(Room::new(
            3001,
            0,
            "Trigger Test Room".into(),
            "A trigger test room.".into(),
        ));
        let actor = g.create_char(Character::new_player(
            "Actor".into(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        ));
        let mob = g.create_char(Character::new_npc(2001));
        g.char_to_room(actor, room);
        g.char_to_room(mob, room);
        let obj = g.create_obj(Object::new(4001, "token".into(), "a token".into()));

        let mob_trigger = install_active_trigger(
            &mut g,
            MOB_TRIGGER,
            MTRIG_COMMAND | MTRIG_SPEECH | MTRIG_ACT,
            1,
        );
        dh::add_trigger(&mut g, ScriptKey::Mob(mob), mob_trigger, -1);
        assert!(!command_mtrigger(&mut g, actor, "look", ""));
        assert_still_active(&g, mob_trigger);
        speech_mtrigger(&mut g, actor, "hello");
        assert_still_active(&g, mob_trigger);
        act_mtrigger(&mut g, mob, "hello", Some(actor), None, None, None, None);
        assert_still_active(&g, mob_trigger);

        let obj_trigger = install_active_trigger(&mut g, OBJ_TRIGGER, OTRIG_COMMAND, OCMD_INVEN);
        dh::add_trigger(&mut g, ScriptKey::Obj(obj), obj_trigger, -1);
        assert!(!cmd_otrig(&mut g, obj, actor, "look", "", OCMD_INVEN));
        assert_still_active(&g, obj_trigger);

        let world_trigger =
            install_active_trigger(&mut g, WLD_TRIGGER, WTRIG_COMMAND | WTRIG_SPEECH, 1);
        dh::add_trigger(&mut g, ScriptKey::Room(room), world_trigger, -1);
        assert!(!command_wtrigger(&mut g, actor, "look", ""));
        assert_still_active(&g, world_trigger);
        speech_wtrigger(&mut g, actor, "hello");
        assert_still_active(&g, world_trigger);
    }

    #[test]
    fn self_caused_command_runs_once_while_a_different_trigger_may_nest() {
        let _lock = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut g = GameState::new(Config::default());

        dh::boot_handler(&mut g);
        let room = g.add_room(Room::new(
            3001,
            0,
            "Nested trigger room".into(),
            String::new(),
        ));
        let mut player = Character::new_player(
            "Actor".into(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        player.player.level = 10;
        let actor = g.create_char(player);
        let mut npc = Character::new_npc(2001);
        npc.player.level = 20;
        let mob = g.create_char(npc);
        g.char_to_room(actor, room);
        g.char_to_room(mob, room);

        let owner = ScriptKey::Mob(mob);
        dh::add_global_var(&mut g, owner, "outer_runs", "0", 0);
        dh::add_global_var(&mut g, owner, "nested_runs", "0", 0);
        let outer = install_command_trigger(
            &mut g,
            "self-causing outer",
            &[
                "eval outer_runs %outer_runs% + 1",
                "global outer_runs",
                "mforce %actor% look",
                "return 1",
            ],
        );
        let nested = install_command_trigger(
            &mut g,
            "independent nested",
            &[
                "eval nested_runs %nested_runs% + 1",
                "global nested_runs",
                "return 1",
            ],
        );
        dh::add_trigger(&mut g, owner, outer, -1);
        dh::add_trigger(&mut g, owner, nested, -1);

        assert!(command_mtrigger(&mut g, actor, "look", ""));
        assert_eq!(
            dh::get_global_var(&g, owner, "outer_runs").as_deref(),
            Some("1")
        );
        assert_eq!(
            dh::get_global_var(&g, owner, "nested_runs").as_deref(),
            Some("1")
        );
        assert_eq!(dh::with_trig(&g, outer, |trigger| trigger.depth), Some(0));
        assert_eq!(dh::with_trig(&g, nested, |trigger| trigger.depth), Some(0));
    }
}
