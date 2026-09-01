// mobact.rs — Mobile AI, a faithful port of DeltaMUD/CircleMUD `src/mobact.c`
// (the `mobile_activity` pulse) plus the mob-memory routines that live there
// (`remember` / `forget` / `clearMemory`). The hunting driver (`hunt_victim`,
// built on `find_first_step`'s BFS) lives in `src/graph.c` and so is owned by
// the sibling `graph.rs` module; it is re-exported here as `mobact::hunt_victim`
// so the assignment name resolves to that single implementation.
//
// `mobile_activity` runs once every PULSE_MOBILE (10s) and walks every NPC in
// the world (in `char_list` order — newest first, matching C's
// `character_list` traversal). For each awake, non-fighting mob it does, in
// the exact C order:
//   1. MOB_SPEC pulse spec-proc call (consumes the mob's turn on a true return)
//   2. scavenger pickup of the most valuable gettable item on the ground
//   3. wandering (unless MOB_SENTINEL), respecting STAY_ZONE / NOMOB / DEATH
//   4. aggression (MOB_AGGRESSIVE + the MOB_AGGR_{EVIL,GOOD,NEUTRAL} variants),
//      honoring NOHASSLE, WIMPY, and MERCY
//   5. mob memory: attack a remembered attacker found in the room (MERCY-aware)
//   6. MOB_HELPER: jump into any NPC-vs-PC fight
//   7. the three MOB_PCHELPER_{GOOD,NEUT,EVIL} variants: side with PCs instead,
//      including the KILLER/THIEF "reverse_att" retargeting
//
// Mob memory in C is a `memory_rec *` linked list hanging off
// `mob_specials.memory`, storing the persistent player idnum of each attacker.
// The Rust `Character` carries no such field, so — following the established
// side-state pattern in `quest.rs`/`shop.rs` (`OnceLock<Mutex<HashMap<…>>>`) —
// memory lives in a module-static map keyed by the mob's runtime `CharId`.
// `CharId`s are never reused (the allocator only increments), so a stale entry
// for an extracted mob is harmless; `clear_memory` reaps it on death/extract.
//
// Borrow discipline mirrors the rest of the port: gather ids/values into owned
// locals before any send/act/hit, and re-look-up entities by id afterward,
// because act()/hit()/perform_move() all take `&mut GameState`.

use crate::act::{ActArg, To, act};
use crate::cmd_movement::perform_move;
use crate::combat::hit;
use crate::flags::*;
use crate::room::{EX_CLOSED, RoomFlags};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// Hunting (graph.c) lives in graph.rs; re-export so the `mobact::hunt_victim`
// name resolves to that single canonical implementation rather than a divergent
// copy. mobile_activity does not itself drive hunting (C's mobile_activity has
// no MOB_MEMORY-based pursuit beyond same-room recall); the hunt driver is
// invoked by callers that set a mob's `hunting` field.

// ---------------------------------------------------------------------------
// MOB_* act-flag bits not yet surfaced in flags.rs (structs.h). flags.rs only
// names the Tier-0 subset (SPEC/SENTINEL/SCAVENGER/AGGRESSIVE/WIMPY); the rest
// of the aggression / memory / helper bits are defined here, matching the C
// `#define MOB_* (1 << n)` values exactly.
// ---------------------------------------------------------------------------
const MOB_STAY_ZONE: i64 = 1 << 6; // mob shouldn't wander out of zone
const MOB_AGGR_EVIL: i64 = 1 << 8; // auto-attack evil PCs
const MOB_AGGR_GOOD: i64 = 1 << 9; // auto-attack good PCs
const MOB_AGGR_NEUTRAL: i64 = 1 << 10; // auto-attack neutral PCs
const MOB_MEMORY: i64 = 1 << 11; // remember attackers if attacked
const MOB_HELPER: i64 = 1 << 12; // attack PCs fighting other NPCs
const MOB_MERCY: i64 = 1 << 24; // won't finish off the dying
const MOB_PCHELPER_GOOD: i64 = 1 << 26; // assists good PCs instead of mobs
const MOB_PCHELPER_NEUT: i64 = 1 << 27; // assists neutral PCs instead of mobs
const MOB_PCHELPER_EVIL: i64 = 1 << 28; // assists evil PCs instead of mobs

/// MOB_AGGR_TO_ALIGN: the union of the three alignment-aggression bits
/// (mobact.c `#define MOB_AGGR_TO_ALIGN`).
const MOB_AGGR_TO_ALIGN: i64 = MOB_AGGR_EVIL | MOB_AGGR_NEUTRAL | MOB_AGGR_GOOD;

// PLR_* bits used by the PCHELPER reverse-attack logic (structs.h). PLR_FROZEN
// is the only PLR bit named in flags.rs.
const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;

// ---------------------------------------------------------------------------
// Mob memory side-state: mob CharId -> remembered player idnums (newest first,
// matching the C list which prepends). Mirrors `mob_specials.memory`.
// ---------------------------------------------------------------------------
static MEMORY: OnceLock<Mutex<HashMap<CharId, Vec<i64>>>> = OnceLock::new();

fn memory() -> &'static Mutex<HashMap<CharId, Vec<i64>>> {
    MEMORY.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Small predicate helpers (local copies of the utils.h macros, kept private to
// this module exactly like cmd_offensive.rs / shop.rs do).
// ---------------------------------------------------------------------------

fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}

/// IS_MOB(ch): a live NPC (C also checks GET_MOB_RNUM > -1; every NPC here has
/// a prototype vnum in `nr`, so `is_npc` is the faithful test).
fn is_mob(g: &GameState, ch: CharId) -> bool {
    is_npc(g, ch)
}

/// MOB_FLAGGED(ch, flag) — NPC act-flag test.
fn mob_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.is_npc && (c.act_flags & flag != 0))
        .unwrap_or(false)
}

/// PLR_FLAGGED(ch, flag) — PC act-flag test (act_flags holds PLR_* for PCs).
fn plr_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch)
        .map(|c| !c.is_npc && (c.act_flags & flag != 0))
        .unwrap_or(false)
}

/// PRF_FLAGGED(ch, flag) — preference test.
fn prf_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.prf_flags & flag != 0)
        .unwrap_or(false)
}

/// AWAKE(ch): position above Sleeping.
fn awake(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.position > Position::Sleeping)
        .unwrap_or(false)
}

fn fighting(g: &GameState, ch: CharId) -> Option<CharId> {
    g.get_char(ch).and_then(|c| c.fighting)
}

fn position(g: &GameState, ch: CharId) -> Position {
    g.get_char(ch).map(|c| c.position).unwrap_or(Position::Dead)
}

fn in_room(g: &GameState, ch: CharId) -> Option<RoomRnum> {
    g.get_char(ch).and_then(|c| c.in_room)
}

fn alignment(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.alignment).unwrap_or(0)
}

/// GET_IDNUM(ch) — the persistent player id (mobs are -1).
fn idnum(g: &GameState, ch: CharId) -> i64 {
    g.get_char(ch).map(|c| c.idnum).unwrap_or(-1)
}

/// Alignment predicates (utils.h thresholds: good >= 350, evil <= -350).
fn is_good(g: &GameState, ch: CharId) -> bool {
    alignment(g, ch) >= 350
}
fn is_evil(g: &GameState, ch: CharId) -> bool {
    alignment(g, ch) <= -350
}
fn is_neutral(g: &GameState, ch: CharId) -> bool {
    !is_good(g, ch) && !is_evil(g, ch)
}

fn get_name(g: &GameState, ch: CharId) -> String {
    g.get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// CAN_GET_OBJ(ch, obj): TAKE-flag, carry weight/count limits, and visibility.
// (utils.h: CAN_WEAR(obj, ITEM_WEAR_TAKE) && CAN_CARRY_OBJ && CAN_SEE_OBJ.)
// ---------------------------------------------------------------------------

/// str_app[].carry_w column (constants.c), indexed by STRENGTH_APPLY_INDEX —
/// identical to the table in cmd_item.rs (kept local to avoid a cross-module
/// private dependency).
const STR_APP_CARRY_W: [i32; 31] = [
    0, 3, 3, 10, 25, 55, 80, 90, 100, 100, 115, 115, 140, 140, 170, 170, 195, 220, 255, 640, 700,
    810, 970, 1130, 1440, 1750, 280, 305, 330, 380, 480,
];

fn strength_apply_index(g: &GameState, ch: CharId) -> usize {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let str_ = c.aff_abils.str as i32;
    let add = c.aff_abils.str_add as i32;
    if add == 0 || str_ != 18 {
        str_.clamp(0, 30) as usize
    } else if add <= 50 {
        26
    } else if add <= 75 {
        27
    } else if add <= 90 {
        28
    } else if add <= 99 {
        29
    } else {
        30
    }
}

fn can_carry_w(g: &GameState, ch: CharId) -> i32 {
    STR_APP_CARRY_W[strength_apply_index(g, ch)]
}

fn can_carry_n(g: &GameState, ch: CharId) -> i32 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    5 + (c.aff_abils.dex as i32 >> 1) + (c.player.level as i32 >> 1)
}

/// CAN_SEE_OBJ for a mob: invisible objects are hidden unless the viewer has
/// detect-invis (mobs never carry PRF_HOLYLIGHT). Light gating is handled by
/// the room being lit enough to wander; mirror the practical effect of
/// MORT_CAN_SEE_OBJ here.
fn can_see_obj(g: &GameState, ch: CharId, oid: ObjId) -> bool {
    let obj = match g.get_obj(oid) {
        Some(o) => o,
        None => return false,
    };
    if obj
        .extra_flags
        .contains(crate::object::ExtraFlags::INVISIBLE)
    {
        let detect = g
            .get_char(ch)
            .map(|c| c.affect_flags & AFF_DETECT_INVIS != 0)
            .unwrap_or(false);
        return detect;
    }
    true
}

/// CAN_GET_OBJ(ch, obj).
fn can_get_obj(g: &GameState, ch: CharId, oid: ObjId) -> bool {
    let obj = match g.get_obj(oid) {
        Some(o) => o,
        None => return false,
    };
    if !obj.wear_flags.contains(crate::object::WearFlags::TAKE) {
        return false;
    }
    let weight = obj.weight;
    let carrying_w = g.get_char(ch).map(|c| c.carry_weight).unwrap_or(0);
    let carrying_n = g.get_char(ch).map(|c| c.carry_items as i32).unwrap_or(0);
    if carrying_w + weight > can_carry_w(g, ch) {
        return false;
    }
    if carrying_n + 1 > can_carry_n(g, ch) {
        return false;
    }
    can_see_obj(g, ch, oid)
}

// ---------------------------------------------------------------------------
// mobile_activity — the per-pulse driver.
// ---------------------------------------------------------------------------

/// CircleMUD/DeltaMUD `mobile_activity()`. Iterate every NPC and run the
/// default (or spec-proc) AI for this pulse.
pub fn mobile_activity(g: &mut GameState) {
    // Snapshot the char list so that mobs extracted mid-loop (deaths) or new
    // mobs don't disturb iteration — C walks `next_ch = ch->next` up front for
    // the same reason. We re-validate each id before acting on it.
    let mut ids: Vec<CharId> = g.char_ids();
    // C db.c PREPENDS to character_list, so mobile_activity walks
    // newest-first (LIFO). The IndexMap yields insertion order (oldest
    // first), which reversed matches C's sweep order - aggro order and the
    // shared RNG stream line up with the oracle (#182).
    ids.reverse();

    for ch in ids {
        // The char may have been extracted by an earlier iteration's combat.
        if !g.char_exists(ch) {
            continue;
        }
        if !is_mob(g, ch) {
            continue;
        }

        // ---- 1. MOB_SPEC special procedure (pulse call) ----------------
        // C looks up mob_index[rnum].func and calls it with cmd 0; a true
        // return ends the mob's turn. A genuinely missing func is a SYSERR in C
        // that clears MOB_SPEC. Shop keepers are resolved dynamically in this
        // port, so call_mob_spec preserves their flag separately from static
        // mob-spec misses.
        // `no_specials` (comm.c, set by the C `-s` flag) gates this exactly as
        // C does: mobact.c:51 `if (MOB_FLAGGED(ch, MOB_SPEC) && !no_specials)`.
        if !g.config.no_specials && mob_flagged(g, ch, MOB_SPEC) && call_mob_spec(g, ch) {
            continue; // spec consumed the mob's turn
        }

        // If still fighting or asleep, no default actions (C: FIGHTING || !AWAKE).
        if fighting(g, ch).is_some() || !awake(g, ch) {
            continue;
        }

        // ---- 1b. Mob hunting (stock CircleMUD mobact.c) -----------------
        // DeltaMUD's mobact.c dropped the stock `if (HUNTING(ch))
        // hunt_victim(ch);` driver, so DG `mhunt` set HUNTING and nothing ever
        // consumed it. Restored per the finish-the-game activation
        // (COMPATIBILITY.md register); stock places it right after the
        // awake/fighting gates, before scavenge/wander.
        if g.get_char(ch).map(|c| c.hunting.is_some()).unwrap_or(false) {
            crate::graph::hunt_victim(g, ch);
            // Stock C ends the mob's turn after hunting (the call site is
            // `if (HUNTING(ch)) { hunt_victim(ch); continue; }`): no wander,
            // scavenge or aggression on the same pulse.
            continue;
        }

        // ---- 1c. Living-world directed npcs (Deltania Breathes) ---------
        // Scheduled townsfolk and caravans are table-driven: town_life owns
        // their whole turn (commute step + occasional bark) and the generic
        // scavenge/wander/aggression AI below must not fight them.
        if crate::town_life::is_directed(g, ch) {
            crate::town_life::drive(g, ch);
            continue;
        }

        // ---- 2. Scavenger pickup ---------------------------------------
        if mob_flagged(g, ch, MOB_SCAVENGER) {
            scavenge(g, ch);
        }

        // ---- 3. Wandering ----------------------------------------------
        wander(g, ch);

        // The mob may have moved; re-validate before the room-scan branches.
        if !g.char_exists(ch) {
            continue;
        }

        // ---- 4. Aggression ---------------------------------------------
        if mob_flagged(g, ch, MOB_AGGRESSIVE | MOB_AGGR_TO_ALIGN) {
            // C mobact.c: the MERCY check `return`s from mobile_activity(),
            // pausing the ENTIRE world's mob AI for one pulse - not just this
            // mob's turn (#184).
            if aggro_attack(g, ch) {
                return;
            }
        }

        // C mobact.c has NO re-guard between the aggressive, memory and
        // helper blocks: a mob that just acquired a target still runs its
        // memory attack and helper assist in the same pulse (#183).
        if !g.char_exists(ch) {
            continue;
        }

        // ---- 5. Mob memory ---------------------------------------------
        if mob_flagged(g, ch, MOB_MEMORY) && has_memory(ch) {
            memory_attack(g, ch);
        }

        if !g.char_exists(ch) {
            continue;
        }

        // ---- 6. Helper mobs --------------------------------------------
        if mob_flagged(g, ch, MOB_HELPER) {
            helper_assist(g, ch);
        }

        // ---- 7. PC-helper variants -------------------------------------
        if mob_flagged(g, ch, MOB_PCHELPER_GOOD) && fighting(g, ch).is_none() {
            pchelper_good(g, ch);
        }
        if g.char_exists(ch) && mob_flagged(g, ch, MOB_PCHELPER_NEUT) && fighting(g, ch).is_none() {
            pchelper_neut(g, ch);
        }
        if g.char_exists(ch) && mob_flagged(g, ch, MOB_PCHELPER_EVIL) && fighting(g, ch).is_none() {
            pchelper_evil(g, ch);
        }
    }
}

// ---------------------------------------------------------------------------
// Branch implementations.
// ---------------------------------------------------------------------------

/// Scavenger: 1-in-11 chance to grab the most valuable gettable item present.
fn scavenge(g: &mut GameState, ch: CharId) {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let contents = g.room(rnum).contents.clone();
    if contents.is_empty() {
        return;
    }
    if g.rng.number(0, 10) != 0 {
        return;
    }

    let mut max = 1;
    let mut best: Option<ObjId> = None;
    for oid in contents {
        if can_get_obj(g, ch, oid) {
            let cost = g.get_obj(oid).map(|o| o.cost).unwrap_or(0);
            if cost > max {
                best = Some(oid);
                max = cost;
            }
        }
    }
    if let Some(oid) = best {
        g.obj_from_anywhere(oid);
        g.obj_to_char(oid, ch);
        act(
            g,
            "$n gets $p.",
            false,
            ch,
            Some(oid),
            ActArg::None,
            To::Room,
        );
    }
}

/// Wandering: a non-sentinel standing mob has an 18-ish chance to pick a real
/// direction (door = number(0,18); only 0..NUM_OF_DIRS count) and walk through
/// it, respecting NOMOB/DEATH destinations and STAY_ZONE.
fn wander(g: &mut GameState, ch: CharId) {
    if mob_flagged(g, ch, MOB_SENTINEL) {
        return;
    }
    if position(g, ch) != Position::Standing {
        return;
    }
    let door = g.rng.number(0, 18);
    if door >= NUM_OF_DIRS as i32 {
        return;
    }
    let door = door as usize;

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };

    // CAN_GO(ch, door) (utils.h:486-489): exit exists, destination resolves,
    // NOT EX_CLOSED, and not an impassible (mapmv == -1) map cell. The port
    // only checked existence, so mobs wandered through closed doors and
    // across walls (#186).
    let exit = match g.room(rnum).exits[door].clone() {
        Some(e) => e,
        None => return,
    };
    if exit.exit_info & EX_CLOSED != 0 {
        return;
    }
    let to_rnum = match g.real_room(exit.to_room) {
        Some(r) => r,
        None => return,
    };
    if let Some(dest) = g.room_opt(to_rnum) {
        if dest.map_x.is_some() && dest.map_y.is_some() && dest.mapmv == -1 {
            return;
        }
    }

    // Destination must not be NOMOB / DEATH.
    let to_flags = g.room(to_rnum).room_flags;
    if to_flags.contains(RoomFlags::NO_MOB) || to_flags.contains(RoomFlags::DEATH) {
        return;
    }

    // STAY_ZONE: stay within the current zone.
    if mob_flagged(g, ch, MOB_STAY_ZONE) {
        let from_zone = g.room(rnum).zone;
        let to_zone = g.room(to_rnum).zone;
        if to_zone != from_zone {
            return;
        }
    }

    perform_move(g, ch, door as i32, true);
}

/// Aggressive mobs: attack the first eligible PC in the room. Returns true if
/// a MERCY "decides not to kill" short-circuit fired (C `return`).
fn aggro_attack(g: &mut GameState, ch: CharId) -> bool {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let people = g.room(rnum).people.clone();
    let aggr_to_align = mob_flagged(g, ch, MOB_AGGR_TO_ALIGN);
    let wimpy = mob_flagged(g, ch, MOB_WIMPY);
    let mercy = mob_flagged(g, ch, MOB_MERCY);

    for vict in people {
        if vict == ch {
            continue;
        }
        if is_npc(g, vict) {
            continue;
        }
        if !g.can_see(ch, vict) {
            continue;
        }
        if prf_flagged(g, vict, PRF_NOHASSLE) {
            continue;
        }
        // MERCY: leave the dying alone (and, in C, abort the whole sweep).
        if position(g, vict) < Position::Sleeping && mercy {
            let line = format!(
                "{} decides not to kill {}, lying on the ground dying...\r\n",
                get_name(g, ch),
                get_name(g, vict)
            );
            g.send_to_room(rnum, &line, None);
            return true;
        }
        // WIMPY mobs only jump sleeping/unconscious victims.
        if wimpy && awake(g, vict) {
            continue;
        }
        // Plain MOB_AGGRESSIVE hits anyone; the AGGR_TO_ALIGN variants gate on
        // the victim's alignment.
        let attack = !aggr_to_align
            || (mob_flagged(g, ch, MOB_AGGR_EVIL) && is_evil(g, vict))
            || (mob_flagged(g, ch, MOB_AGGR_NEUTRAL) && is_neutral(g, vict))
            || (mob_flagged(g, ch, MOB_AGGR_GOOD) && is_good(g, vict));
        if attack {
            hit(g, ch, vict);
            return false; // found one; stop scanning (C `found = TRUE`)
        }
    }
    false
}

/// Mob memory: attack a remembered attacker present in the room.
fn memory_attack(g: &mut GameState, ch: CharId) {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    let mercy = mob_flagged(g, ch, MOB_MERCY);

    for vict in people {
        if vict == ch {
            continue;
        }
        if is_npc(g, vict) || !g.can_see(ch, vict) || prf_flagged(g, vict, PRF_NOHASSLE) {
            continue;
        }
        let vid = idnum(g, vict);
        if !remembers(ch, vid) {
            continue;
        }
        // Found a remembered foe.
        if position(g, vict) < Position::Sleeping && mercy {
            let line = format!(
                "{} looks down in triumph at {}, dying in agony...\r\n",
                get_name(g, ch),
                get_name(g, vict)
            );
            g.send_to_room(rnum, &line, None);
        } else {
            act(
                g,
                "'Hey!  You're the fiend that attacked me!!!', exclaims $n.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            hit(g, ch, vict);
        }
        return; // C: found = TRUE -> stop after the first match
    }
}

/// Helper mob: jump into any fight where an NPC is fighting a PC.
fn helper_assist(g: &mut GameState, ch: CharId) {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();

    for vict in people {
        if vict == ch {
            continue;
        }
        if !is_npc(g, vict) {
            continue;
        }
        let target = match fighting(g, vict) {
            Some(t) => t,
            None => continue,
        };
        // The NPC must be fighting a PC, and not us.
        if is_npc(g, target) || ch == target {
            continue;
        }
        act(
            g,
            "$n jumps to the aid of $N!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Room,
        );
        hit(g, ch, target);
        return; // C: found = TRUE
    }
}

/// MOB_PCHELPER_GOOD: side with good PCs against their attackers, with the
/// KILLER/THIEF "reverse_att" twist (help the victim of a flagged aggressor).
fn pchelper_good(g: &mut GameState, ch: CharId) {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();

    for vict in people {
        if vict == ch {
            continue;
        }
        let vfight = match fighting(g, vict) {
            Some(t) => t,
            None => continue,
        };
        if is_npc(g, vict) || !is_good(g, vict) {
            continue;
        }

        // reverse_att: if the "good" PC is themselves a killer/thief and their
        // opponent is a clean PC, defend the opponent instead.
        let mut reverse_att = false;
        if (plr_flagged(g, vict, PLR_KILLER) || plr_flagged(g, vict, PLR_THIEF))
            && !is_npc(g, vfight)
            && !(plr_flagged(g, vfight, PLR_KILLER) || plr_flagged(g, vfight, PLR_THIEF))
        {
            reverse_att = true;
        }

        let befriend = if reverse_att { vfight } else { vict };
        let target = if reverse_att { vict } else { vfight };

        act(
            g,
            "$n flashes you a brave smile.",
            false,
            ch,
            None,
            ActArg::Char(befriend),
            To::Vict,
        );
        act(
            g,
            "$n exclaims, 'BANZAII!!!'",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        act(
            g,
            "$n jumps to your aid!",
            false,
            ch,
            None,
            ActArg::Char(befriend),
            To::Vict,
        );
        act(
            g,
            "$n jumps to the aid of $N!",
            false,
            ch,
            None,
            ActArg::Char(befriend),
            To::NotVict,
        );
        hit(g, ch, target);
        return; // C `break`
    }
}

/// MOB_PCHELPER_NEUT: side with neutral PCs against their attackers.
fn pchelper_neut(g: &mut GameState, ch: CharId) {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();

    for vict in people {
        if vict == ch {
            continue;
        }
        let vfight = match fighting(g, vict) {
            Some(t) => t,
            None => continue,
        };
        if is_npc(g, vict) || !is_neutral(g, vict) {
            continue;
        }
        act(
            g,
            "$n jumps to your aid!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Vict,
        );
        act(
            g,
            "$n jumps to the aid of $N!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        hit(g, ch, vfight);
        return; // C `break`
    }
}

/// MOB_PCHELPER_EVIL: side with evil PCs against their attackers, with the
/// reverse twist (defend a clean PC's flagged-killer opponent).
fn pchelper_evil(g: &mut GameState, ch: CharId) {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();

    for vict in people {
        if vict == ch {
            continue;
        }
        let vfight = match fighting(g, vict) {
            Some(t) => t,
            None => continue,
        };
        if is_npc(g, vict) || !is_evil(g, vict) {
            continue;
        }

        // reverse_att: clean evil PC vs a killer/thief opponent -> snarl at the
        // (evil) victim instead (C's mirror of the good case).
        let mut reverse_att = false;
        if !(plr_flagged(g, vict, PLR_KILLER) || plr_flagged(g, vict, PLR_THIEF))
            && !is_npc(g, vfight)
            && (plr_flagged(g, vfight, PLR_KILLER) || plr_flagged(g, vfight, PLR_THIEF))
        {
            reverse_att = true;
        }

        let target = if reverse_att { vict } else { vfight };

        act(
            g,
            "$n snarls viciously at you!",
            false,
            ch,
            None,
            ActArg::Char(target),
            To::Vict,
        );
        act(
            g,
            "$n snarls viciously at $N!",
            false,
            ch,
            None,
            ActArg::Char(target),
            To::NotVict,
        );
        hit(g, ch, target);
        return; // C `break`
    }
}

// ---------------------------------------------------------------------------
// Mob spec-proc dispatch. The C MUD calls mob_index[rnum].func; the Rust port
// resolves the func by the mob's prototype vnum against the set of ported spec
// procs. The pulse call passes cmd="" — none of the ported procs (shop_keeper /
// postmaster, both reactive) act on a bare pulse, so this degrades to a no-op
// just as those C funcs would. New pulse-driven specs slot in here by vnum.
// ---------------------------------------------------------------------------

/// Run the mob's spec proc for this pulse (cmd=""). Returns true if it consumed
/// the mob's turn. Statically assigned specs are tried first, exactly as C's
/// mobile_activity calls `mob_index[rnum].func(mob, mob, 0, "")`. Shop keepers
/// are resolved dynamically by vnum. A MOB_SPEC mob with neither registration is
/// an invalid prototype flag: log the C SYSERR and strip the bit so later pulses
/// do not keep retrying it.
fn call_mob_spec(g: &mut GameState, ch: CharId) -> bool {
    let vnum = g.get_char(ch).map(|c| c.nr).unwrap_or(NOBODY);
    if let Some(func) = crate::spec_assign::get_mob_spec(g, vnum) {
        if func(g, ch, ch, "", "") {
            return true;
        }
        return false;
    }
    if crate::shop::is_shop_keeper_vnum(vnum) {
        return crate::shop::shop_keeper(g, ch, ch, "", "");
    }
    if g.get_char(ch)
        .map(|c| c.act_flags & MOB_CASTER != 0)
        .unwrap_or(false)
    {
        return crate::spec_procs::magic_user(g, ch, ch, "", "");
    }

    let name = get_name(g, ch);
    log::warn!(
        "SYSERR: {} (#{}): Attempting to call non-existing mob func",
        name,
        vnum
    );
    if let Some(c) = g.get_char_mut(ch) {
        c.act_flags &= !MOB_SPEC;
    }
    false
}

/// Combat pulse spec dispatch (fight.c perform_violence): if a fighting mob has
/// MOB_SPEC or MOB_CASTER and a registered proc, call it with cmd=0/arg="".
/// The return value is intentionally ignored by the combat loop.
pub fn combat_mob_spec_pulse(g: &mut GameState, ch: CharId) {
    if g.config.no_specials {
        return;
    }
    let Some(c) = g.get_char(ch) else {
        return;
    };
    if !c.is_npc || c.act_flags & (MOB_SPEC | MOB_CASTER) == 0 {
        return;
    }
    let vnum = c.nr;
    if c.act_flags & MOB_CASTER != 0
        || crate::spec_assign::get_mob_spec(g, vnum).is_some()
        || crate::shop::is_shop_keeper_vnum(vnum)
    {
        let _ = call_mob_spec(g, ch);
    }
}

// ---------------------------------------------------------------------------
// Mob memory routines (mobact.c remember / forget / clearMemory).
// ---------------------------------------------------------------------------

fn has_memory(ch: CharId) -> bool {
    memory()
        .lock()
        .ok()
        .map(|m| m.get(&ch).map(|v| !v.is_empty()).unwrap_or(false))
        .unwrap_or(false)
}

fn remembers(ch: CharId, id: i64) -> bool {
    if id < 0 {
        return false;
    }
    memory()
        .lock()
        .ok()
        .map(|m| m.get(&ch).map(|v| v.contains(&id)).unwrap_or(false))
        .unwrap_or(false)
}

/// `remember(ch, victim)` — make `ch` remember `victim` (mobact.c remember).
/// No-op unless `ch` is a mob and `victim` is a mortal PC; duplicates ignored.
pub fn remember(g: &GameState, ch: CharId, victim: CharId) {
    if !is_npc(g, ch) {
        return;
    }
    if is_npc(g, victim) {
        return;
    }
    // C skips immortal victims (GET_LEVEL(victim) >= LVL_IMMORT).
    let imm = g
        .get_char(victim)
        .map(|c| c.player.level >= LVL_IMMORT)
        .unwrap_or(true);
    if imm {
        return;
    }
    let id = idnum(g, victim);
    if id < 0 {
        return;
    }
    if let Ok(mut m) = memory().lock() {
        let entry = m.entry(ch).or_default();
        if !entry.contains(&id) {
            // C prepends (newest first).
            entry.insert(0, id);
        }
    }
}

/// `forget(ch, victim)` — drop `victim` from `ch`'s memory (mobact.c forget).
pub fn forget(g: &GameState, ch: CharId, victim: CharId) {
    let id = idnum(g, victim);
    if id < 0 {
        return;
    }
    if let Ok(mut m) = memory().lock() {
        if let Some(entry) = m.get_mut(&ch) {
            entry.retain(|&x| x != id);
            if entry.is_empty() {
                m.remove(&ch);
            }
        }
    }
}

/// `clear_memory(ch)` — erase all of `ch`'s memory (mobact.c clearMemory).
/// Call this when a mob is extracted to reap its side-state entry.
pub fn clear_memory(ch: CharId) {
    if let Ok(mut m) = memory().lock() {
        m.remove(&ch);
    }
}

// ---------------------------------------------------------------------------
// Hunting lives in graph.rs (graph.c home), re-exported above as
// `mobact::hunt_victim`. See the `pub use crate::graph::hunt_victim` near the
// top of this module. The BFS (`find_first_step`) and the do_track command are
// also in graph.rs; mobile_activity drives a mob's hunt by calling
// `crate::graph::hunt_victim(g, ch)` for any mob whose `hunting` field is set.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::room::Room;

    fn npc_with_spec(g: &mut GameState, vnum: MobVnum, room: RoomRnum) -> CharId {
        let mut mob = Character::new_npc(vnum);
        mob.act_flags |= MOB_SPEC;
        let id = g.create_char(mob);
        g.char_to_room(id, room);
        id
    }

    #[test]
    fn mobile_activity_strips_unregistered_mob_spec_flag() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let mob = npc_with_spec(&mut g, 987_654, room);

        mobile_activity(&mut g);

        assert_eq!(g.get_char(mob).unwrap().act_flags & MOB_SPEC, 0);
    }

    #[test]
    fn mobile_activity_preserves_mob_spec_when_specials_disabled() {
        let mut cfg = Config::default();
        cfg.no_specials = true;
        let mut g = GameState::new(cfg);
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let mob = npc_with_spec(&mut g, 987_655, room);

        mobile_activity(&mut g);

        assert_ne!(g.get_char(mob).unwrap().act_flags & MOB_SPEC, 0);
    }

    #[test]
    fn mobile_activity_preserves_registered_mob_spec_flag() {
        let mut g = GameState::new(Config::default());
        crate::spec_assign::assign_specs(&mut g);
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let mob = npc_with_spec(&mut g, 1, room);

        mobile_activity(&mut g);

        assert_ne!(g.get_char(mob).unwrap().act_flags & MOB_SPEC, 0);
    }

    #[test]
    fn mobile_activity_preserves_dynamic_caster_spec_flag() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let mut mob = Character::new_npc(987_656);
        mob.act_flags |= MOB_SPEC | MOB_CASTER;
        let mob = g.create_char(mob);
        g.char_to_room(mob, room);

        mobile_activity(&mut g);

        assert_ne!(g.get_char(mob).unwrap().act_flags & MOB_SPEC, 0);
    }

    /// Finish-the-game activation: a mob with HUNTING set closes distance to
    /// its prey over pulses (stock CircleMUD's mobile_activity driver that
    /// DeltaMUD dropped).
    #[test]
    fn hunting_mob_closes_distance_over_pulses() {
        use crate::character::Character;
        use crate::config::Config;
        use crate::room::Room;

        let mut g = GameState::new(Config::default());
        // Linear corridor: 300(north)<- 301 <- 302 <- 303 <- 304.
        let rooms: Vec<usize> = (0..5)
            .map(|i| {
                g.add_room(Room::new(
                    300 + i as i32,
                    3,
                    format!("R{}", i),
                    String::new(),
                ))
            })
            .collect();
        for w in rooms.windows(2) {
            let (s, n) = (w[0], w[1]);
            g.room_mut(s).exits[NORTH] = Some(crate::room::Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: -1,
                to_room: g.rooms[n].number,
            });
            g.room_mut(n).exits[SOUTH] = Some(crate::room::Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: -1,
                to_room: g.rooms[s].number,
            });
        }

        let mut mob = Character::new_npc(1600);
        mob.player.level = 10;
        mob.points.max_hit = 100;
        mob.points.hit = 100; // freshly-loaded mob: full health
        mob.points.max_move = 100;
        mob.points.move_points = 100; // freshly-loaded mob: full movement
        let mob_id = g.create_char(mob);
        g.char_to_room(mob_id, rooms[0]);

        let mut prey = Character::new_player("Prey".into(), Class::Warrior, Race::Human);
        prey.player.level = 10;
        prey.points.max_hit = 10000;
        prey.points.hit = 10000; // survives the strikes; we only assert pursuit
        let prey_id = g.create_char(prey);
        g.char_to_room(prey_id, rooms[4]);

        g.get_char_mut(mob_id).unwrap().hunting = Some(prey_id);
        println!(
            "pre-loop: pos={:?} hp={} mv={}",
            g.get_char(mob_id).unwrap().position,
            g.get_char(mob_id).unwrap().points.hit,
            g.get_char(mob_id).unwrap().points.move_points
        );

        // The hunter must pursue: track that its room advances toward the
        // prey across pulses (the pursuit itself is the restored behavior).
        let start = g.get_char(mob_id).and_then(|c| c.in_room);
        let mut best = start;
        for _ in 0..12 {
            mobile_activity(&mut g);
            let m = g.get_char(mob_id).and_then(|c| c.in_room);
            if m > best {
                best = m;
            }
        }
        let prey_room = g.get_char(prey_id).and_then(|c| c.in_room);
        assert_ne!(best, start, "hunter never moved");
        assert!(
            best >= prey_room || g.get_char(prey_id).is_none(),
            "hunter must close distance: start {:?}, best {:?}, prey {:?}",
            start,
            best,
            prey_room
        );
    }
}
