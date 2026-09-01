// cmd_offensive.rs — player-level offensive commands (CircleMUD/DeltaMUD
// act.offensive.c), ported 1:1 to the id-indexed GameState. Covers assist,
// hit / kill / murder / deathblow, target, backstab, order, flee, chain
// footing, bash, rescue, kick, berserk, disarm, trip, camouflage, blanket.
//
// Output strings track the C source byte-for-byte. The actual swings go
// through crate::combat::{set_fighting, damage, hit}; skill chances roll
// g.rng.number(1, N) against GET_SKILL(ch, SKILL) (which reads 0 until the
// skill is learned — the same degrade C exhibits for an unlearned skill).
//
// Fidelity notes (systems not yet ported elsewhere — they degrade exactly as
// C would with empty data, and are documented in the manifest):
//   * WAIT_STATE: wired — the offensive skills below call g.set_wait_state and
//     the heartbeat drains queued player input through the descriptor wait gate.
//   * Arena flee: wired via arena.rs. do_flee uses minlevel=1 for arena
//     combatants (else 15) and calls arena::arena_flee_start, which mirrors C's
//     IS_ARENACOMBATANT branch (LASTFIGHTING / GET_ARENAFLEETIMER, flee-recall
//     timer, no exp loss). match_over / trans_to_preproom are commented out in
//     the C source too, so they are intentionally absent. Non-arena fleers take
//     the gain_exp(-loss) branch as in C.
//   * pk gating: pk_allowed is false (config), so PC-vs-PC requires murder, as
//     in C. check_killer (combat::check_killer) is wired at the murder sites and
//     inside set_fighting/damage, flagging PLR_KILLER in jurisdicted areas.
//   * command_interpreter is reused for `order` (charmed pets execute orders).

use crate::act::{ActArg, To, act};
use crate::character::Affect;
use crate::combat::{self, chance, check_killer, damage_type, hit, hit_type, set_fighting};
use crate::flags::*;
use crate::handler::check_perm_duration;
use crate::object::ObjectType;
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Local constants mirroring C #defines not present in the Rust contract.
// ---------------------------------------------------------------------------

// SKILL_* numbers (spells.h).
const SKILL_CHAIN_FOOTING: u16 = 50;
const SKILL_BACKSTAB: u16 = 501;
const SKILL_BASH: u16 = 502;
const SKILL_KICK: u16 = 504;
const SKILL_RESCUE: u16 = 507;
const SKILL_BERSERK: u16 = 517;
const SKILL_CAMOUFLAGE: u16 = 518;
const SKILL_BLANKET: u16 = 519;
const SKILL_TRIP: u16 = 537;
const SKILL_DISARM: u16 = 538;
const SKILL_TARGET: u16 = 539;

// Attack type sentinels (spells.h). Backstab keys off (PIERCE - HIT).
const TYPE_HIT: i32 = 1100;
const TYPE_PIERCE: i32 = 1111;

// do_hit subcmds (interpreter.h).
const SCMD_MURDER: i32 = 1;
const SCMD_DEATHBLOW: i32 = 2;

// MOB act flags (structs.h) not in flags.rs.
const MOB_AWARE: i64 = 1 << 4; // can't be backstabbed
const MOB_NOBASH: i64 = 1 << 16; // can't be bashed / tripped

// AFF flag for chained feet (structs.h AFF_CHAINED).
const AFF_CHAINED: i64 = 1 << 24;

// Class ids (structs.h).
const CLASS_WARRIOR: Class = Class::Warrior;
const CLASS_THIEF: Class = Class::Thief;

// config.c — PvP gating.
// config.c — pk_victim_min: a PC below this level can't be flagged/attacked.
const PK_VICTIM_MIN: Level = 10;

// Standard C messages (interpreter.c).
const NOPERSON: &str = "&CNo-one by that name here.&n\r\n";
const OK: &str = "&YOkay.&n\r\n";

// ---------------------------------------------------------------------------
// Small accessors mirroring the utils.h macros against the live state.
// ---------------------------------------------------------------------------

fn fighting(g: &GameState, ch: CharId) -> Option<CharId> {
    g.get_char(ch).and_then(|c| c.fighting)
}
fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}
fn get_level(g: &GameState, ch: CharId) -> Level {
    g.get_char(ch).map(|c| c.player.level).unwrap_or(0)
}
fn get_class(g: &GameState, ch: CharId) -> Class {
    g.get_char(ch)
        .map(|c| c.player.class)
        .unwrap_or(Class::Warrior)
}
fn get_pos(g: &GameState, ch: CharId) -> Position {
    g.get_char(ch)
        .map(|c| c.position)
        .unwrap_or(Position::Standing)
}
fn get_hit(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.hit).unwrap_or(0)
}
fn get_max_hit(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.max_hit).unwrap_or(1)
}
fn get_move(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.move_points).unwrap_or(0)
}
fn get_master(g: &GameState, ch: CharId) -> Option<CharId> {
    g.get_char(ch).and_then(|c| c.master)
}
/// GET_SKILL(ch, num) — proficiency 0..100 (0 until learned).
fn get_skill(g: &GameState, ch: CharId, num: u16) -> i32 {
    g.get_char(ch).map(|c| c.skill(num) as i32).unwrap_or(0)
}
/// IS_AFFECTED(ch, bit) — affect bitvector test.
fn is_affected(g: &GameState, ch: CharId, bit: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.affect_flags & bit != 0)
        .unwrap_or(false)
}
/// MOB_FLAGGED(ch, flag) — NPC act-flag test.
fn mob_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.is_npc && (c.act_flags & flag != 0))
        .unwrap_or(false)
}
/// AWAKE(ch) — position above sleeping.
fn awake(g: &GameState, ch: CharId) -> bool {
    get_pos(g, ch) > Position::Sleeping
}
/// CAN_SEE wrapper.
fn can_see(g: &GameState, viewer: CharId, target: CharId) -> bool {
    g.can_see(viewer, target)
}
/// ROOM_FLAGGED(IN_ROOM(ch), PEACEFUL).
fn in_peaceful_room(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .and_then(|c| c.in_room)
        .map(|r| g.room(r).room_flags.contains(RoomFlags::PEACEFUL))
        .unwrap_or(false)
}
/// GET_EQ(ch, WEAR_WIELD).
fn wielded(g: &GameState, ch: CharId) -> Option<ObjId> {
    g.get_char(ch).and_then(|c| c.equipment[WEAR_WIELD])
}

/// numdisplay(): comma-group a signed integer ("1234567" -> "1,234,567").
fn numdisplay(val: i64) -> String {
    let neg = val < 0;
    let digits = val.unsigned_abs().to_string();
    let mut out = String::new();
    let bytes = digits.as_bytes();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg { format!("-{}", out) } else { out }
}

/// stop_fighting(ch) (fight.c): clear the fight target, drop out of POS_FIGHTING.
/// (combat::stop_fighting is private; this matches its behaviour.)
fn stop_fighting(g: &mut GameState, ch: CharId) {
    // Delegate to the canonical combat::stop_fighting (fight.c:279-281):
    // the local copy skipped update_pos, leaving sitters sitting through
    // rescue/camouflage/blanket disengages (#129).
    combat::stop_fighting(g, ch);
}

fn raw_kill(g: &mut GameState, victim: CharId, killer: CharId) {
    combat::raw_kill(g, victim, Some(killer));
}

fn deathblow(g: &mut GameState, ch: CharId, victim: CharId, dam: i32) {
    let attacktype = combat::get_attacktype(g, ch);
    combat::deathblow(g, ch, victim, dam, attacktype);
}

// ===========================================================================
// ASSIST
// ===========================================================================

pub fn do_assist(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if fighting(g, ch).is_some() {
        g.send_to_char(
            ch,
            "You're already fighting!  How can you assist someone else?\r\n",
        );
        return;
    }
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Whom do you wish to assist?\r\n");
        return;
    }
    let helpee = match g.get_char_room_vis(ch, &arg) {
        Some(h) => h,
        None => {
            g.send_to_char(ch, NOPERSON);
            return;
        }
    };
    if helpee == ch {
        g.send_to_char(ch, "You can't help yourself any more than this!\r\n");
        return;
    }

    // Find whoever in the room is fighting the helpee.
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    let opponent = people.into_iter().find(|&p| fighting(g, p) == Some(helpee));

    let opponent = match opponent {
        None => {
            act(
                g,
                "But nobody is fighting $M!",
                false,
                ch,
                None,
                ActArg::Char(helpee),
                To::Char,
            );
            return;
        }
        Some(o) => o,
    };
    if !can_see(g, ch, opponent) {
        act(
            g,
            "You can't see who is fighting $M!",
            false,
            ch,
            None,
            ActArg::Char(helpee),
            To::Char,
        );
        return;
    }
    if !g.pk_allowed && !is_npc(g, opponent) {
        act(
            g,
            "Use 'murder' if you really want to attack $N.",
            false,
            ch,
            None,
            ActArg::Char(opponent),
            To::Char,
        );
        return;
    }
    g.send_to_char(ch, "You join the fight!\r\n");
    act(
        g,
        "$N assists you!",
        false,
        helpee,
        None,
        ActArg::Char(ch),
        To::Char,
    );
    act(
        g,
        "$n assists $N.",
        false,
        ch,
        None,
        ActArg::Char(helpee),
        To::NotVict,
    );
    hit(g, ch, opponent);
}

// ===========================================================================
// HIT / MURDER / DEATHBLOW
// ===========================================================================

pub fn do_hit(g: &mut GameState, ch: CharId, argument: &str, mut subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Hit who?\r\n");
        return;
    }
    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "They don't seem to be here.\r\n");
            return;
        }
    };
    if vict == ch {
        g.send_to_char(ch, "You hit yourself...OUCH!.\r\n");
        act(
            g,
            "$n hits $mself, and says OUCH!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Room,
        );
        return;
    }
    // Charmed: can't hit your own master.
    if is_affected(g, ch, AFF_CHARM) && get_master(g, ch) == Some(vict) {
        act(
            g,
            "$N is just such a good friend, you simply can't hit $M.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        return;
    }

    if !g.pk_allowed {
        if !is_npc(g, vict) && !is_npc(g, ch) {
            // C act.offensive.c:106-113: two arena combatants may 'hit' each
            // other - the command is rewritten to SCMD_MURDER (#126).
            if crate::arena::is_arena_combatant(ch) && crate::arena::is_arena_combatant(vict) {
                subcmd = SCMD_MURDER;
            }
            if subcmd != SCMD_MURDER && subcmd != SCMD_DEATHBLOW {
                g.send_to_char(ch, "Use 'murder' to hit another player.\r\n");
                return;
            }
            // Flag the attacker as a PLAYER KILLER (unless either party is a
            // newbie below pk_victim_min). Mirrors act.offensive.c do_hit.
            let newbie = get_level(g, vict) < PK_VICTIM_MIN || get_level(g, ch) < PK_VICTIM_MIN;
            if !newbie {
                check_killer(g, ch, vict);
            }
        }
        // You can't order a charmed pet to attack a player.
        if is_affected(g, ch, AFF_CHARM) {
            let master_pc = get_master(g, ch).map(|m| !is_npc(g, m)).unwrap_or(false);
            if master_pc && !is_npc(g, vict) {
                return;
            }
        }
    }

    if get_pos(g, ch) == Position::Standing && fighting(g, ch) != Some(vict) {
        if subcmd == SCMD_DEATHBLOW {
            if get_hit(g, vict) < 0 && get_pos(g, vict) < Position::Sleeping {
                deathblow(g, ch, vict, 100);
            } else {
                g.send_to_char(ch, "They aren't near death!\r\n");
            }
        } else {
            hit(g, ch, vict);
            g.set_wait_state(ch, PULSE_VIOLENCE as i32 + 2);
        }
    } else {
        g.send_to_char(ch, "You do the best you can!\r\n");
    }
}

pub fn do_kill(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    // Only an authenticated player principal with persisted Implementor trust
    // receives the raw-kill path.  A high display level is cosmetic authority,
    // and a switched body must inherit its original player's trust rather than
    // the body's level. Ordinary descriptorless NPCs still route to do_hit.
    let can_instakill = crate::interpreter::authenticated_input_authority(g, ch)
        .is_some_and(|principal| principal.authority >= i32::from(LVL_IMPL));
    if !can_instakill {
        do_hit(g, ch, argument, subcmd);
        return;
    }
    let (arg, _) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "Kill who?\r\n");
        return;
    }
    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "They aren't here.\r\n");
            return;
        }
    };
    if ch == vict {
        g.send_to_char(ch, "Your mother would be so sad.. :(\r\n");
        return;
    }
    act(
        g,
        "You chop $M to pieces!  Ah!  The blood!",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::Char,
    );
    act(
        g,
        "$N chops you to pieces!",
        false,
        vict,
        None,
        ActArg::Char(ch),
        To::Char,
    );
    act(
        g,
        "$n brutally slays $N!",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::NotVict,
    );
    raw_kill(g, vict, ch);
}

// ===========================================================================
// TARGET (switch fighting target)
// ===========================================================================

pub fn do_target(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if is_npc(g, ch) && is_affected(g, ch, AFF_CHARM) {
        g.send_to_char(ch, "You'd better leave this skill to players.\r\n");
        return;
    } else if !is_npc(g, ch) && get_skill(g, ch, SKILL_TARGET) == 0 {
        g.send_to_char(ch, "You don't know of that skill.\r\n");
        return;
    }

    if arg.is_empty() {
        g.send_to_char(ch, "Hit who?\r\n");
        return;
    }
    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "They don't seem to be here.\r\n");
            return;
        }
    };
    if vict == ch {
        g.send_to_char(ch, "You hit yourself...OUCH!.\r\n");
        act(
            g,
            "$n hits $mself, and says OUCH!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Room,
        );
        return;
    }
    if is_affected(g, ch, AFF_CHARM) && get_master(g, ch) == Some(vict) {
        act(
            g,
            "$N is just such a good friend, you simply can't hit $M.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        return;
    }
    if !g.pk_allowed {
        if !is_npc(g, vict) && !is_npc(g, ch) && subcmd != SCMD_MURDER {
            g.send_to_char(ch, "Use 'murder' to hit another player.\r\n");
            return;
        }
        if is_affected(g, ch, AFF_CHARM) {
            let master_pc = get_master(g, ch).map(|m| !is_npc(g, m)).unwrap_or(false);
            if master_pc && !is_npc(g, vict) {
                return;
            }
        }
    }

    let percent = g.rng.number(1, 401); // 101% (>401) impossible -> always rolls
    let mut prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_TARGET)
    };
    prob += chance(g, ch, vict, 0) * 3;

    if percent > prob {
        act(
            g,
            "You try to switch target, but your current opponent keep you busy!",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n makes some desperate attempts to attack another opponent!",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
        return;
    }
    act(
        g,
        "You start fighting $N!",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::Char,
    );
    act(
        g,
        "$n starts fighting $N!",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::NotVict,
    );
    act(
        g,
        "$n starts fighting you!",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::Vict,
    );
    hit(g, ch, vict);
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

// ===========================================================================
// BACKSTAB
// ===========================================================================

pub fn do_backstab(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (buf, _) = crate::interpreter::one_argument(argument);

    let vict = match g.get_char_room_vis(ch, &buf) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "Backstab who?\r\n");
            return;
        }
    };
    if vict == ch {
        g.send_to_char(ch, "How can you sneak up on yourself?\r\n");
        return;
    }
    let weapon = match wielded(g, ch) {
        Some(w) => w,
        None => {
            g.send_to_char(ch, "You need to wield a weapon to make it a success.\r\n");
            return;
        }
    };
    // value[3] must be the piercing attack type.
    let attacktype = g.get_obj(weapon).map(|o| o.values[3]).unwrap_or(0);
    if attacktype != TYPE_PIERCE - TYPE_HIT {
        g.send_to_char(
            ch,
            "Only piercing weapons can be used for backstabbing.\r\n",
        );
        return;
    }
    if fighting(g, vict).is_some() {
        g.send_to_char(
            ch,
            "You can't backstab a fighting person -- they're too alert!\r\n",
        );
        return;
    }

    if mob_flagged(g, vict, MOB_AWARE) {
        act(
            g,
            "You notice $N lunging at you!",
            false,
            vict,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$e notices you lunging at $m!",
            false,
            vict,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        act(
            g,
            "$n notices $N lunging at $m!",
            false,
            vict,
            None,
            ActArg::Char(ch),
            To::NotVict,
        );
        hit(g, vict, ch);
        return;
    }

    let percent = g.rng.number(1, 101);
    let prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_BACKSTAB)
    };

    if awake(g, vict) && percent > prob {
        // Missed backstab: a 0-damage SKILL_BACKSTAB hit (act.offensive.c).
        damage_type(g, ch, vict, 0, SKILL_BACKSTAB as i32);
    } else {
        // Landed backstab: hit() with SKILL_BACKSTAB applies backstab_mult().
        hit_type(g, ch, vict, SKILL_BACKSTAB as i32);
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

// ===========================================================================
// ORDER
// ===========================================================================

pub fn do_order(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (name, message) = crate::interpreter::half_chop(argument);

    if name.is_empty() || message.is_empty() {
        g.send_to_char(ch, "Order who to do what?\r\n");
        return;
    }
    let vict = g.get_char_room_vis(ch, &name);
    if vict.is_none() && !crate::interpreter::is_abbrev(&name, "followers") {
        g.send_to_char(ch, "That person isn't here.\r\n");
        return;
    }
    if let Some(v) = vict {
        if ch == v {
            g.send_to_char(ch, "You obviously suffer from skitzofrenia.\r\n");
            return;
        }
    }

    if is_affected(g, ch, AFF_CHARM) {
        g.send_to_char(
            ch,
            "Your superior would not aprove of you giving orders.\r\n",
        );
        return;
    }

    if let Some(vict) = vict {
        let buf = format!("$N orders you to '{}'", message);
        act(g, &buf, false, vict, None, ActArg::Char(ch), To::Char);
        act(
            g,
            "$n gives $N an order.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Room,
        );

        let obeys = get_master(g, vict) == Some(ch)
            && is_affected(g, vict, AFF_CHARM)
            && crate::interpreter::indirect_command_target_is_forceable(g, vict);
        if !obeys {
            act(
                g,
                "$n has an indifferent look.",
                false,
                vict,
                None,
                ActArg::None,
                To::Room,
            );
        } else {
            g.send_to_char(ch, OK);
            crate::interpreter::command_interpreter(g, vict, &message);
        }
    } else {
        // order "followers"
        let buf = format!("$n issues the order '{}'.", message);
        act(g, &buf, false, ch, None, ActArg::None, To::Room);

        let org_room = g.get_char(ch).and_then(|c| c.in_room);
        let followers = g
            .get_char(ch)
            .map(|c| c.followers.clone())
            .unwrap_or_default();
        let mut found = false;
        for k in followers {
            let same_room = g.get_char(k).and_then(|c| c.in_room) == org_room;
            if same_room
                && is_affected(g, k, AFF_CHARM)
                && crate::interpreter::indirect_command_target_is_forceable(g, k)
            {
                found = true;
                crate::interpreter::command_interpreter(g, k, &message);
            }
        }
        if found {
            g.send_to_char(ch, OK);
        } else {
            g.send_to_char(ch, "Nobody here is a loyal subject of yours!\r\n");
        }
    }
}

// ===========================================================================
// FLEE
// ===========================================================================

pub fn do_flee(g: &mut GameState, ch: CharId, _argument: &str, _subcmd: i32) {
    if get_pos(g, ch) < Position::Fighting {
        g.send_to_char(ch, "You are in pretty bad shape, unable to flee!\r\n");
        return;
    }

    let was_fighting = fighting(g, ch);
    // IS_ARENACOMBATANT(ch) ? minlevel = 1 : minlevel = 15 (act.offensive.c).
    let minlevel: Level = if crate::arena::is_arena_combatant(ch) {
        1
    } else {
        15
    };

    for _ in 0..6 {
        let attempt = g.rng.number(0, (NUM_OF_DIRS - 1) as i32) as usize;
        // C act.offensive.c:383-385: candidate directions are gated on
        // CAN_GO (utils.h:486-489: exit exists, resolved, NOT closed, and
        // not an impassible map cell) and the destination is not a DEATH
        // room. The port skipped the CAN_GO half, so players and pets fled
        // through closed doors (#105).
        if !crate::graph::can_go(g, ch, attempt) {
            continue;
        }
        let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let to_death = {
            let exit = g.room(rnum).exits[attempt].as_ref();
            match exit.and_then(|e| g.real_room(e.to_room)) {
                Some(dest) => g.room(dest).room_flags.contains(RoomFlags::DEATH),
                None => continue, // CAN_GO false (no exit / unresolved)
            }
        };
        if to_death {
            continue;
        }

        act(
            g,
            "$n panics, and attempts to flee!",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        // C act.offensive.c:388 calls do_simple_move(ch, attempt, TRUE), so
        // fleeing pays movement points, respects chained/sneak/spec/DG gates
        // and can land you in a death trap exactly like a normal move (#127).
        if crate::cmd_movement::do_simple_move(g, ch, attempt, true) {
            g.send_to_char(ch, "You flee head over heels.\r\n");
            if was_fighting.is_some() && !is_npc(g, ch) && get_level(g, ch) >= minlevel {
                if let Some(wf) = was_fighting {
                    // Arena branch: arena_flee_start mirrors C's IS_ARENACOMBATANT
                    // path (flee-recall timer, no exp loss; match_over/
                    // trans_to_preproom are #commented-out in C too). It returns
                    // false for a non-arena fleer, who takes the gain_exp path.
                    if !crate::arena::arena_flee_start(g, ch, wf) {
                        let loss =
                            (get_max_hit(g, wf) - get_hit(g, wf)) as i64 * get_level(g, wf) as i64;
                        crate::limits::gain_exp(g, ch, -loss);
                        let msg = format!(
                            "&RYou lost {} experience points for fleeing!&n\r\n",
                            numdisplay(loss)
                        );
                        g.send_to_char(ch, &msg);
                    }
                }
            }
        } else {
            act(
                g,
                "$n tries to flee, but can't!",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        return;
    }
    g.send_to_char(ch, "PANIC!  You couldn't escape!\r\n");
}

// ===========================================================================
// CHAIN FOOTING
// ===========================================================================

pub fn do_chain_footing(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if get_class(g, ch) != CLASS_WARRIOR && get_level(g, ch) < LVL_IMMORT {
        g.send_to_char(
            ch,
            "You'd better leave all the martial arts to fighters.\r\n",
        );
        return;
    }
    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => match fighting(g, ch) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, "Chain who's feet?\r\n");
                return;
            }
        },
    };
    if fighting(g, ch).is_some() && fighting(g, ch) != Some(vict) {
        g.send_to_char(
            ch,
            "You're too busy fighting to throw yourself at ANOTHER person!\r\n",
        );
        return;
    }
    if vict == ch {
        g.send_to_char(ch, "You chain your own feet! Wait... AHHHHHHh\r\n");
        return;
    }
    if in_peaceful_room(g, ch) {
        g.send_to_char(
            ch,
            "This room just has such a peaceful, easy feeling...\r\n",
        );
        return;
    }

    let percent = g.rng.number(1, 401);
    let mut prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_CHAIN_FOOTING)
    };
    prob += chance(g, ch, vict, 0) * 3;

    // Already chained?
    let already = g
        .get_char(vict)
        .map(|c| {
            c.affected
                .iter()
                .any(|a| a.spell_type == SKILL_CHAIN_FOOTING as i32)
        })
        .unwrap_or(false);
    if already {
        g.send_to_char(ch, "Whoops, they're already chained. Doh.\r\n");
        return;
    }

    if get_move(g, ch) < 25 && get_level(g, ch) < LVL_IMMORT {
        g.send_to_char(ch, "You're too exhausted for such action!\r\n");
        return;
    } else if get_level(g, ch) < LVL_IMMORT {
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= 25;
        }
    }

    if percent > prob {
        act(
            g,
            "$N lunges at you, but you step aside and laugh at $e.",
            false,
            vict,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$n lunges at $N, but gets a face full of dirt instead!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        act(
            g,
            "You lunge at $N, but get a face full of dirt instead!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        if let Some(c) = g.get_char_mut(ch) {
            c.position = Position::Sitting;
        }
    } else {
        act(
            g,
            "$N lunges at you and skillfully chains your feet together!",
            false,
            vict,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$n lunges at $N and skillfully chains $S feet together!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        act(
            g,
            "You lunge at $N and skillfully chain $S feet together!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        let af = Affect {
            spell_type: SKILL_CHAIN_FOOTING as i32,
            duration: 2,
            modifier: 0,
            location: APPLY_NONE,
            bitvector: AFF_CHAINED,
            caster: Some(ch),
        };
        if let Some(c) = g.get_char_mut(vict) {
            c.affected.push(af);
        }
        g.affect_total(vict);
        if let Some(c) = g.get_char_mut(vict) {
            c.affect_flags |= AFF_CHAINED;
        }
    }
    if fighting(g, ch).is_none() {
        set_fighting(g, ch, vict);
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

// ===========================================================================
// BASH
// ===========================================================================

pub fn do_bash(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if get_class(g, ch) != CLASS_WARRIOR && get_level(g, ch) < LVL_IMMORT {
        g.send_to_char(
            ch,
            "You'd better leave all the martial arts to fighters.\r\n",
        );
        return;
    }
    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => match fighting(g, ch) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, "Bash who?\r\n");
                return;
            }
        },
    };
    if vict == ch {
        g.send_to_char(ch, "Aren't we funny today...\r\n");
        return;
    }
    if wielded(g, ch).is_none() {
        g.send_to_char(ch, "You need to wield a weapon to make it a success.\r\n");
        return;
    }
    if in_peaceful_room(g, ch) {
        g.send_to_char(
            ch,
            "This room just has such a peaceful, easy feeling...\r\n",
        );
        return;
    }

    let mut percent = g.rng.number(1, 401);
    let mut prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_BASH)
    };
    prob += chance(g, ch, vict, 0) * 3;

    if mob_flagged(g, vict, MOB_NOBASH) {
        percent = 401;
    }

    // SKILL_BASH attack type (act.offensive.c). The skill_message table
    // (lib/misc/messages) is not yet ported, so the type renders no flavour
    // text for now — a separate documented gap — but the type is correct.
    if percent > prob {
        damage_type(g, ch, vict, 0, SKILL_BASH as i32);
        if let Some(c) = g.get_char_mut(ch) {
            c.position = Position::Sitting;
        }
    } else {
        damage_type(g, ch, vict, 1, SKILL_BASH as i32);
        if let Some(c) = g.get_char_mut(vict) {
            c.position = Position::Sitting;
        }
        g.set_wait_state(vict, PULSE_VIOLENCE as i32);
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

// ===========================================================================
// RESCUE
// ===========================================================================

pub fn do_rescue(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "Whom do you want to rescue?\r\n");
            return;
        }
    };
    if vict == ch {
        g.send_to_char(ch, "What about fleeing instead?\r\n");
        return;
    }
    if fighting(g, ch) == Some(vict) {
        g.send_to_char(ch, "How can you rescue someone you are trying to kill?\r\n");
        return;
    }

    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    let tmp_ch = people.into_iter().find(|&p| fighting(g, p) == Some(vict));

    let tmp_ch = match tmp_ch {
        None => {
            act(
                g,
                "But nobody is fighting $M!",
                false,
                ch,
                None,
                ActArg::Char(vict),
                To::Char,
            );
            return;
        }
        Some(t) => t,
    };

    if get_class(g, ch) != CLASS_WARRIOR && get_level(g, ch) < LVL_IMMORT {
        g.send_to_char(ch, "But only true warriors can do this!");
        return;
    }

    let percent = g.rng.number(1, 101);
    let prob = get_skill(g, ch, SKILL_RESCUE);

    if percent > prob {
        g.send_to_char(ch, "You fail the rescue!\r\n");
        return;
    }
    g.send_to_char(ch, "Banzai!!  To the rescue!!!\r\n");
    act(
        g,
        "You are rescued by $N, leaving you confused.",
        false,
        vict,
        None,
        ActArg::Char(ch),
        To::Char,
    );
    act(
        g,
        "$n heroically rescues $N!",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::NotVict,
    );

    if fighting(g, vict) == Some(tmp_ch) {
        stop_fighting(g, vict);
    }
    if fighting(g, tmp_ch).is_some() {
        stop_fighting(g, tmp_ch);
    }
    if fighting(g, ch).is_some() {
        stop_fighting(g, ch);
    }

    set_fighting(g, ch, tmp_ch);
    set_fighting(g, tmp_ch, ch);
    g.set_wait_state(vict, 2 * PULSE_VIOLENCE as i32);
}

// ===========================================================================
// KICK
// ===========================================================================

pub fn do_kick(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if get_class(g, ch) != CLASS_WARRIOR && get_level(g, ch) < LVL_IMMORT {
        g.send_to_char(
            ch,
            "You'd better leave all the martial arts to fighters.\r\n",
        );
        return;
    }
    let (arg, _) = crate::interpreter::one_argument(argument);

    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => match fighting(g, ch) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, "Kick who?\r\n");
                return;
            }
        },
    };
    if vict == ch {
        g.send_to_char(ch, "Aren't we funny today...\r\n");
        return;
    }
    let percent = g.rng.number(1, 401);
    let mut prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_KICK)
    };
    prob += chance(g, ch, vict, 0) * 3;

    // SKILL_KICK attack type (act.offensive.c); skill_message gap as above.
    if percent > prob {
        damage_type(g, ch, vict, 0, SKILL_KICK as i32);
    } else {
        let dmg = (get_level(g, ch) as i32) >> 1;
        damage_type(g, ch, vict, dmg, SKILL_KICK as i32);
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 3);
}

// ===========================================================================
// BERSERK
// ===========================================================================

pub fn do_berserk(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if get_class(g, ch) != CLASS_WARRIOR && get_level(g, ch) < LVL_IMMORT {
        g.send_to_char(ch, "But only true warriors can do this!");
        return;
    }
    if get_move(g, ch) < 50 {
        g.send_to_char(ch, "But you're out of breath!");
        return;
    }

    let (arg, _) = crate::interpreter::one_argument(argument);
    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => match fighting(g, ch) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, "Berserk attack who?\r\n");
                return;
            }
        },
    };
    if vict == ch {
        g.send_to_char(ch, "You can't possibly be that insane???\r\n");
        return;
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.points.move_points -= 50;
    }

    let mut prob = get_skill(g, ch, SKILL_BERSERK);
    let v_level = get_level(g, vict) as i32;
    let ch_level = get_level(g, ch) as i32;
    // C rolls the main number(...) first, drawing the divisors after it
    // (act.offensive.c:726-728) - keep the stream order identical (#136).
    let percent_roll = g.rng.number((prob as f32 * 0.4) as i32, 101);
    let r1 = g.rng.number(6, 12);
    let r2 = g.rng.number(4, 10);
    let mut percent = percent_roll + (v_level / r1) - (ch_level / r2);

    // prob scales with hp/maxhp (floor 0.6).
    let mut factor = get_hit(g, ch) as f32 / get_max_hit(g, ch) as f32;
    if factor < 0.6 {
        factor = 0.6;
    }
    prob = (prob as f32 * factor) as i32;

    if percent < 0 {
        percent = 0;
    }

    let mut dmg = 1;
    if let Some(w) = wielded(g, ch) {
        if g.get_obj(w).map(|o| o.obj_type) == Some(ObjectType::Weapon) {
            let (n, s) = g
                .get_obj(w)
                .map(|o| (o.values[1], o.values[2]))
                .unwrap_or((0, 0));
            dmg += g.rng.dice(n, s);
        }
    }
    let multiplier = g.rng.number(2, 5);
    dmg *= multiplier;
    if dmg > 200 {
        dmg = 250;
    }

    // SKILL_BERSERK attack type (act.offensive.c); skill_message gap as above.
    if percent > prob {
        damage_type(g, ch, vict, 0, SKILL_BERSERK as i32);
        g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 4);
    } else {
        damage_type(g, ch, vict, dmg, SKILL_BERSERK as i32);
        g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
    }
}

// ===========================================================================
// DISARM
// ===========================================================================

pub fn do_disarm(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    // C does `if (!ch) return;` — ch is always valid here.

    let vict = match g.get_char_room_vis(ch, &arg).or_else(|| fighting(g, ch)) {
        Some(v) => v,
        None => {
            // get_char_room_vis failed AND not fighting.
            g.send_to_char(ch, "Disarm who?\r\n");
            return;
        }
    };

    // Charmed mobs can't be ordered to disarm.
    if is_npc(g, ch) && is_affected(g, ch, AFF_CHARM) {
        g.send_to_char(ch, "You'd better leave this skill to players.\r\n");
        return;
    } else if !is_npc(g, ch) && get_skill(g, ch, SKILL_DISARM) == 0 {
        // Doesn't know how: if wielding, you lose your own weapon.
        if g.get_char(ch)
            .and_then(|c| c.equipment[WEAR_WIELD])
            .is_some()
        {
            if let Some(obj) = g.unequip_char(ch, WEAR_WIELD) {
                let rnum = g.get_char(ch).and_then(|c| c.in_room);
                if let Some(rnum) = rnum {
                    g.obj_to_room(obj, rnum);
                }
            }
            act(
                g,
                "While trying to disarm $N, you lose your own weapon.",
                true,
                ch,
                None,
                ActArg::Char(vict),
                To::Char,
            );
            act(
                g,
                "While trying to disarm you, $n loses $s own weapon.",
                true,
                ch,
                None,
                ActArg::Char(vict),
                To::Vict,
            );
            act(
                g,
                "While trying to disarm $N, $n loses $s own weapon.",
                true,
                ch,
                None,
                ActArg::Char(vict),
                To::NotVict,
            );
        } else {
            g.send_to_char(ch, "You dont know of that skill.\r\n");
        }
        return;
    }

    if vict == ch {
        g.send_to_char(ch, "Aren't we funny today...\r\n");
        return;
    }

    // Victim isn't wielding.
    if g.get_char(vict)
        .and_then(|c| c.equipment[WEAR_WIELD])
        .is_none()
    {
        g.send_to_char(ch, "Disarm what weapon?!\r\n");
        return;
    }

    let percent = g.rng.number(1, 401);
    let mut prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_DISARM)
    };
    prob += chance(g, ch, vict, 0) * 3;

    if percent > prob && get_pos(g, vict) > Position::Sleeping {
        act(
            g,
            "$n tries to disarm $N but fails.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        act(
            g,
            "You try to disarm $N but fail.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        act(
            g,
            "$n tries to disarm you but fails.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Vict,
        );
        if fighting(g, vict).is_none() {
            hit(g, vict, ch);
        }
    } else {
        act(
            g,
            "$n makes $N drop his weapon to the ground with some fast moves.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        act(
            g,
            "With some fast moves you manage to make $N drop the weapon.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        act(
            g,
            "$n performs some fast moves, and makes you drop your weapon.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Vict,
        );
        if let Some(obj) = g.unequip_char(vict, WEAR_WIELD) {
            let rnum = g.get_char(vict).and_then(|c| c.in_room);
            if let Some(rnum) = rnum {
                g.obj_to_room(obj, rnum);
            }
        }
        if fighting(g, vict).is_none() {
            hit(g, vict, ch);
        }
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32);
}

// ===========================================================================
// TRIP
// ===========================================================================

pub fn do_trip(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = crate::interpreter::one_argument(argument);

    if is_npc(g, ch) && is_affected(g, ch, AFF_CHARM) {
        g.send_to_char(ch, "You'd better leave this skill to players.\r\n");
        return;
    } else if !is_npc(g, ch) && get_skill(g, ch, SKILL_TRIP) == 0 {
        g.send_to_char(ch, "You dont know of that skill.\r\n");
        return;
    }

    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => match fighting(g, ch) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, "Trip who?\r\n");
                return;
            }
        },
    };
    if vict == ch {
        g.send_to_char(ch, "Aren't we funny today...\r\n");
        return;
    }

    if is_npc(g, vict) && mob_flagged(g, vict, MOB_NOBASH) {
        act(
            g,
            "You try to trip $N but can't.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        act(
            g,
            "$n tries to trip you, but you are unmovable.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Vict,
        );
        act(
            g,
            "$n tries to trip $N, but can't.",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        return;
    }

    let percent = g.rng.number(1, 501);
    let mut prob = if is_npc(g, ch) {
        90
    } else {
        get_skill(g, ch, SKILL_TRIP)
    };
    prob += chance(g, ch, vict, 0) * 3;

    // SKILL_TRIP attack type (act.offensive.c); skill_message gap as above.
    if percent > prob {
        damage_type(g, ch, vict, 0, SKILL_TRIP as i32);
    } else {
        damage_type(g, ch, vict, 2, SKILL_TRIP as i32);
        g.set_wait_state(vict, PULSE_VIOLENCE as i32 * 2);
        g.set_wait_state(ch, PULSE_VIOLENCE as i32);
        return;
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

fn remove_temporary_hide(g: &mut GameState, ch: CharId) {
    if is_affected(g, ch, AFF_HIDE) && !check_perm_duration(g, ch, AFF_HIDE) {
        if let Some(c) = g.get_char_mut(ch) {
            c.affect_flags &= !AFF_HIDE;
        }
    }
}

// ===========================================================================
// CAMOUFLAGE
// ===========================================================================

pub fn do_camouflage(g: &mut GameState, ch: CharId, _argument: &str, _subcmd: i32) {
    if (get_class(g, ch) != CLASS_THIEF || get_skill(g, ch, SKILL_CAMOUFLAGE) <= 0)
        && get_level(g, ch) < LVL_IMMORT
    {
        g.send_to_char(ch, "You don't seem to have the camouflage ability.\r\n");
        return;
    }

    if get_pos(g, ch) != Position::Fighting {
        g.send_to_char(ch, "You can only camouflage yourself when fighting.\r\n");
        return;
    }

    if get_move(g, ch) < 25 {
        g.send_to_char(ch, "You're too tired to do that!");
        return;
    }

    g.send_to_char(
        ch,
        "You attempt to camouflage yourself with your surroundings.\r\n",
    );
    if let Some(c) = g.get_char_mut(ch) {
        c.points.move_points -= 25;
    }

    remove_temporary_hide(g, ch);

    // prob = SKILL_CAMOUFLAGE + dex_app_skill[GET_DEX].hide.
    let dex = g.get_char(ch).map(|c| c.aff_abils.dex as i32).unwrap_or(0);
    let hide_bonus = crate::constants::DEX_APP_SKILL
        [crate::constants::app_index(dex, crate::constants::DEX_APP_SKILL.len())]
    .hide;
    let mut prob = get_skill(g, ch, SKILL_CAMOUFLAGE) + hide_bonus;
    // C act.offensive.c:950/1036 rolls number(prob*0.6, 101) BEFORE the
    // level divisor number(10, 15); matching the draw order keeps seeded
    // golden replays aligned with the C stream (#136).
    let mut percent = g.rng.number((prob as f32 * 0.6) as i32, 101);
    let r = g.rng.number(10, 15);
    percent -= get_level(g, ch) as i32 / r;
    // C stores the roll in a `byte` (unsigned char): a negative int narrows
    // to ~255, so the skill FAILS. The i32 clamp to 0 turned that rare
    // failure into a success (#134).
    if percent < 0 {
        percent &= 0xFF; // two's-complement (byte) narrowing
    }
    if percent <= 0 {
        percent = 0;
    }

    let mut factor = get_hit(g, ch) as f32 / get_max_hit(g, ch) as f32;
    if factor < 0.6 {
        factor = 0.6;
    }
    prob = (prob as f32 * factor) as i32;

    if percent > prob {
        return;
    }

    let name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let buf = format!("{} has disappeared from view!\r\n", name);
    act(g, &buf, true, ch, None, ActArg::None, To::Room);
    if let Some(vict) = fighting(g, ch) {
        stop_fighting(g, vict);
        stop_fighting(g, ch);
        let vname = g
            .get_char(vict)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        let line = format!(
            "{} is befuddled, having lost sight of his target.\r\n",
            vname
        );
        let rnum = g.get_char(ch).and_then(|c| c.in_room);
        if let Some(rnum) = rnum {
            g.send_to_room(rnum, &line, None);
        }
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.affect_flags |= AFF_HIDE;
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

// ===========================================================================
// BLANKET (group camouflage)
// ===========================================================================

pub fn do_blanket(g: &mut GameState, ch: CharId, _argument: &str, _subcmd: i32) {
    if (get_class(g, ch) != CLASS_THIEF || get_skill(g, ch, SKILL_BLANKET) <= 0)
        && get_level(g, ch) < LVL_IMMORT
    {
        g.send_to_char(
            ch,
            "You don't seem to have the blanket camouflage ability.\r\n",
        );
        return;
    }

    if get_pos(g, ch) != Position::Fighting {
        g.send_to_char(
            ch,
            "You can only blanket comouflage your group when fighting.\r\n",
        );
        return;
    }

    if get_move(g, ch) < 25 {
        g.send_to_char(ch, "You're too tired to do that!");
        return;
    }

    g.send_to_char(ch, "You attempt to blanket camouflage your group.\r\n");
    if let Some(c) = g.get_char_mut(ch) {
        c.points.move_points -= 25;
    }

    // The group leader k: master if any, else ch.
    let k = get_master(g, ch).unwrap_or(ch);
    let ch_room = g.get_char(ch).and_then(|c| c.in_room);

    // First un-hide the leader (if in this room) and grouped followers in-room.
    let k_in_room = g.get_char(k).and_then(|c| c.in_room) == ch_room;
    if k_in_room || k == ch {
        remove_temporary_hide(g, k);
    }

    let followers = g
        .get_char(k)
        .map(|c| c.followers.clone())
        .unwrap_or_default();
    let mut grpsize = 0;
    for f in followers.iter().copied() {
        if is_affected(g, f, AFF_GROUP) && g.get_char(f).and_then(|c| c.in_room) == ch_room {
            grpsize += 1;
            remove_temporary_hide(g, f);
        }
    }

    // prob = SKILL_CAMOUFLAGE + dex_app_skill[GET_DEX].hide - grpsize.
    let dex = g.get_char(ch).map(|c| c.aff_abils.dex as i32).unwrap_or(0);
    let hide_bonus = crate::constants::DEX_APP_SKILL
        [crate::constants::app_index(dex, crate::constants::DEX_APP_SKILL.len())]
    .hide;
    let mut prob = get_skill(g, ch, SKILL_CAMOUFLAGE) + hide_bonus - grpsize;
    // C act.offensive.c:950/1036 rolls number(prob*0.6, 101) BEFORE the
    // level divisor number(10, 15); matching the draw order keeps seeded
    // golden replays aligned with the C stream (#136).
    let mut percent = g.rng.number((prob as f32 * 0.6) as i32, 101);
    let r = g.rng.number(10, 15);
    percent -= get_level(g, ch) as i32 / r;
    // C stores the roll in a `byte` (unsigned char): a negative int narrows
    // to ~255, so the skill FAILS. The i32 clamp to 0 turned that rare
    // failure into a success (#134).
    if percent < 0 {
        percent &= 0xFF; // two's-complement (byte) narrowing
    }
    if percent <= 0 {
        percent = 0;
    }

    let mut factor = get_hit(g, ch) as f32 / get_max_hit(g, ch) as f32;
    if factor < 0.55 {
        factor = 0.55;
    }
    prob = (prob as f32 * factor) as i32;

    if percent > prob {
        return;
    }

    // Success: hide the leader (and the rest of the group).
    // C act.offensive.c:992 initialises befuddled = 1 and tests !befuddled,
    // so the "befuddled" room lines are dead code there; matching that by
    // starting true keeps the port from emitting the extra line (#135).
    let mut befuddled = true;
    if k_in_room || k == ch {
        let kname = g
            .get_char(k)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        let buf = format!("{} has disappeared from view!\r\n", kname);
        act(g, &buf, true, k, None, ActArg::None, To::Room);
        if let Some(vict) = fighting(g, k) {
            stop_fighting(g, vict);
            stop_fighting(g, k);
            if !befuddled {
                let vname = g
                    .get_char(vict)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default();
                let line = format!(
                    "{} is befuddled, having lost sight of his target.\r\n",
                    vname
                );
                if let Some(rnum) = ch_room {
                    g.send_to_room(rnum, &line, None);
                }
                befuddled = true;
            }
        }
        if let Some(c) = g.get_char_mut(k) {
            c.affect_flags |= AFF_HIDE;
        }
    }

    for f in followers.iter().copied() {
        if is_affected(g, f, AFF_GROUP) && g.get_char(f).and_then(|c| c.in_room) == ch_room {
            if let Some(vict) = fighting(g, f) {
                stop_fighting(g, vict);
                stop_fighting(g, f);
                let fname = g
                    .get_char(f)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default();
                let buf = format!("{} has disappeared from view!\r\n", fname);
                act(g, &buf, true, f, None, ActArg::None, To::Room);
                if !befuddled {
                    let vname = g
                        .get_char(vict)
                        .map(|c| c.player.name.clone())
                        .unwrap_or_default();
                    let line = format!(
                        "{} is befuddled, having lost sight of his target.\r\n",
                        vname
                    );
                    if let Some(rnum) = ch_room {
                        g.send_to_room(rnum, &line, None);
                    }
                    befuddled = true;
                }
            }
            if let Some(c) = g.get_char_mut(f) {
                c.affect_flags |= AFF_HIDE;
            }
        }
    }
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::room::Room;

    fn connected_player(
        g: &mut GameState,
        conn: ConnId,
        name: &str,
        level: Level,
        trust: i32,
    ) -> CharId {
        let mut character =
            crate::character::Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.idnum = conn.0 as i64;
        character.player.level = level;
        character.trust = trust;
        character.desc = Some(conn);
        let character = g.create_char(character);
        let mut descriptor = Descriptor::new(conn, "kill.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(character);
        g.descriptors.insert(conn, descriptor);
        character
    }

    fn durable_kill_target(g: &mut GameState, room: RoomRnum, name: &str) -> CharId {
        let mut victim = crate::character::Character::new_npc(7100);
        victim.player.name = name.to_string();
        victim.points.hit = 100_000;
        victim.points.max_hit = 100_000;
        victim.real_points = victim.points.clone();
        let victim = g.create_char(victim);
        g.char_to_room(victim, room);
        victim
    }

    fn hidden_player_with_affect(duration: Option<i32>) -> (GameState, CharId) {
        let mut g = GameState::new(Config::default());
        let mut ch = crate::character::Character::new_player(
            "Hidden".to_string(),
            Class::Thief,
            Race::Human,
        );
        ch.affect_flags |= AFF_HIDE;
        if let Some(duration) = duration {
            ch.affected.push(Affect {
                spell_type: -1,
                duration,
                modifier: 0,
                location: 0,
                bitvector: AFF_HIDE,
                caster: None,
            });
        }
        let ch_id = g.create_char(ch);
        (g, ch_id)
    }

    #[test]
    fn remove_temporary_hide_preserves_permanent_hide() {
        let (mut g, ch) = hidden_player_with_affect(Some(-1));

        remove_temporary_hide(&mut g, ch);

        assert!(is_affected(&g, ch, AFF_HIDE));
    }

    #[test]
    fn remove_temporary_hide_clears_non_permanent_hide() {
        let (mut g, ch) = hidden_player_with_affect(None);

        remove_temporary_hide(&mut g, ch);

        assert!(!is_affected(&g, ch, AFF_HIDE));
    }

    #[test]
    fn kill_instakill_uses_authenticated_trust_and_not_display_level() {
        let mut display_game = GameState::new(Config::default());
        let display_room =
            display_game.add_room(Room::new(71_001, 0, "Kill test".to_string(), String::new()));
        let display = connected_player(&mut display_game, ConnId(811), "Display", LVL_IMPL, 1);
        display_game.char_to_room(display, display_room);
        let display_victim = durable_kill_target(&mut display_game, display_room, "Displayvictim");

        crate::interpreter::run_authenticated_command(
            &mut display_game,
            display,
            "kill Displayvictim",
        );

        assert!(
            display_game.char_exists(display_victim),
            "display level 105 with trust 1 must take only the ordinary hit path"
        );

        let mut trust_game = GameState::new(Config::default());
        let trust_room =
            trust_game.add_room(Room::new(71_002, 0, "Kill test".to_string(), String::new()));
        let trusted = connected_player(
            &mut trust_game,
            ConnId(812),
            "Trusted",
            1,
            i32::from(LVL_IMPL),
        );
        trust_game.char_to_room(trusted, trust_room);
        let trust_victim = durable_kill_target(&mut trust_game, trust_room, "Trustvictim");

        crate::interpreter::run_authenticated_command(&mut trust_game, trusted, "kill Trustvictim");

        assert!(
            !trust_game.char_exists(trust_victim),
            "display level 1 with Implementor trust must receive the authenticated raw-kill path"
        );

        let quarantine_victim =
            durable_kill_target(&mut trust_game, trust_room, "Quarantinevictim");
        trust_game.authority_quarantine.insert(812);
        crate::interpreter::run_authenticated_command(
            &mut trust_game,
            trusted,
            "kill Quarantinevictim",
        );
        assert!(
            trust_game.char_exists(quarantine_victim),
            "a quarantined Implementor must receive only the ordinary combat path"
        );
    }

    #[test]
    fn kill_instakill_rejects_a_duplicate_switched_principal_alias() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(71_003, 0, "Kill test".to_string(), String::new()));
        let principal = connected_player(&mut g, ConnId(813), "Principal", 1, i32::from(LVL_IMPL));
        let mut body = crate::character::Character::new_npc(7101);
        body.player.name = "Avatar".to_string();
        body.player.level = LVL_IMPL;
        body.desc = Some(ConnId(813));
        let body = g.create_char(body);
        g.get_char_mut(principal).unwrap().desc = None;
        {
            let descriptor = g.descriptors.get_mut(&ConnId(813)).unwrap();
            descriptor.character = Some(body);
            descriptor.original = Some(principal);
        }
        g.char_to_room(body, room);

        let mut alias_body = crate::character::Character::new_npc(7102);
        alias_body.desc = Some(ConnId(814));
        let alias_body = g.create_char(alias_body);
        let mut duplicate = Descriptor::new(ConnId(814), "kill.test".to_string());
        duplicate.character = Some(alias_body);
        duplicate.original = Some(principal);
        g.descriptors.insert(ConnId(814), duplicate);

        let victim = durable_kill_target(&mut g, room, "Aliasvictim");
        crate::interpreter::run_authenticated_command(&mut g, body, "kill Aliasvictim");

        assert!(
            g.char_exists(victim),
            "a malformed switched alias must fail closed to ordinary combat"
        );
    }
}
