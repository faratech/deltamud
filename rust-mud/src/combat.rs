// Combat (DeltaMUD fight.c / utils.c), id-based. Melee rounds in the
// heartbeat, the DeltaMUD probability/damage model (chance()/dam_multi()),
// damage + position/death, corpse drop, PK flagging (check_killer).
//
// DeltaMUD does NOT use stock CircleMUD THAC0. The to-hit probability,
// saving throws and damage scaling all come from chance()/dam_multi() in
// utils.c, driven by technique/dex/str/con/int/wis ability scores vs the
// power/defense/mpower/mdefense combat ratings. These live here as the
// canonical `pub fn`s; magic.rs and cmd_offensive.rs both call them (no fork).

use crate::act::{act, ActArg, To};
use crate::constants::ATTACK_HIT_TEXT;
use crate::object::{Object, ObjectType, ObjLoc};
use crate::room::{RoomFlags, SectorType};
use crate::spell_parser::{MAX_SPELLS, TYPE_UNDEFINED};
use crate::state::GameState;
use crate::types::*;

// Attack-type sentinels (spells.h). The weapon verb table is keyed by
// (attacktype - TYPE_HIT); SKILL_BACKSTAB pierces.
pub const TYPE_HIT: i32 = 1100;
pub const TYPE_STAB: i32 = 1114;
const NUM_ATTACK_TYPES: i32 = 15; // olc.h
const SKILL_BACKSTAB: u16 = 501;

// PLR_* act-flag bits for PCs (structs.h).
const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;

// Victim defensive skills (spells.h) and their shared difficulty scalar.
// fight.c rolls (number(1,100) * AVOID_FACTOR) <= GET_SKILL(victim, X).
const SKILL_DODGE: u16 = 532;
const SKILL_PARRY: u16 = 533;
const SKILL_AVOID: u16 = 534;
const SKILL_RIPOSTE: u16 = 535;
const AVOID_FACTOR: i32 = 20; // spells.h

// config.c — PvP gating (pk_allowed is false on this MUD).
const PK_ALLOWED: bool = false;
// Position multiplier hack (fight.c) keys off POS_FIGHTING's ordinal (8).
const POS_FIGHTING_ORD: i32 = Position::Fighting as i32;

/// May `ch` attack `victim`? (self / immortal / peaceful room.)
pub fn can_kill(g: &GameState, ch: CharId, victim: CharId) -> Result<(), String> {
    if ch == victim {
        return Err("You hit yourself...OUCH!.\r\n".to_string());
    }
    let v_imm = g.get_char(victim).map(|c| c.is_immortal()).unwrap_or(false);
    if v_imm {
        return Err("You cannot attack an immortal!\r\n".to_string());
    }
    let ch_imm = g.get_char(ch).map(|c| c.is_immortal()).unwrap_or(false);
    if !ch_imm {
        if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
            if g.room(rnum).room_flags.contains(RoomFlags::PEACEFUL) {
                return Err("This room just has such a peaceful, easy feeling...\r\n".to_string());
            }
        }
    }
    Ok(())
}

/// set_fighting: attacker begins targeting victim (fight.c set_fighting).
pub fn set_fighting(g: &mut GameState, ch: CharId, victim: CharId) {
    if ch == victim {
        return;
    }
    if g.get_char(ch).and_then(|c| c.fighting).is_some() {
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = Some(victim);
        c.position = Position::Fighting;
    }
    // PK bookkeeping: on a non-PK MUD, attacking another player in a
    // jurisdicted, non-peaceful area flags the attacker as a KILLER (fight.c).
    if !PK_ALLOWED {
        check_killer(g, ch, victim);
    }
}

fn stop_fighting(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
    }
}

/// One combat pulse: every fighter takes a swing (CircleMUD perform_violence).
pub fn perform_violence(g: &mut GameState) {
    let fighters: Vec<CharId> = g
        .char_list
        .iter()
        .copied()
        .filter(|&c| g.get_char(c).and_then(|x| x.fighting).is_some())
        .collect();

    for ch in fighters {
        let victim = match g.get_char(ch).and_then(|c| c.fighting) {
            Some(v) => v,
            None => continue,
        };
        // Victim must still exist, be in the same room, and be alive.
        let same_room = g.get_char(ch).and_then(|c| c.in_room)
            == g.get_char(victim).and_then(|c| c.in_room);
        if !g.char_exists(victim) || !same_room {
            stop_fighting(g, ch);
            continue;
        }
        if g.get_char(ch).map(|c| c.position != Position::Fighting).unwrap_or(true) {
            continue;
        }
        hit(g, ch, victim);
    }
}

/// chance(ch, vict, type) (utils.c): the percent chance ch lands a hit on
/// vict. type 0 = normal/weapon attack, type 1 = magical attack.
///   0   = total failure (always misses)
///   50  = even 50/50
///   100 = total success (always hits)
/// Transcribed term-for-term from the C; the same integer truncation applies.
pub fn chance(g: &GameState, ch: CharId, vict: CharId, ty: i32) -> i32 {
    let (c_tech, c_dex, c_int) = match g.get_char(ch) {
        Some(c) => (c.points.technique as i32, c.aff_abils.dex as i32, c.aff_abils.intel as i32),
        None => return 0,
    };
    let (v_tech, v_dex, v_wis) = match g.get_char(vict) {
        Some(c) => (c.points.technique as i32, c.aff_abils.dex as i32, c.aff_abils.wis as i32),
        None => return 0,
    };
    // sh_int p;  (C uses signed-short arithmetic, fits in i32 here)
    let mut p: i32 = match ty {
        0 => (c_tech - v_tech) + (c_dex - v_dex) * 10,
        1 => (c_tech - v_tech) + (c_int - v_wis) * 10,
        _ => return 0,
    };
    p /= 10; // widen the ranges (-1000..1000 -> -100..100)
    p = (p + 100) / 2; // 0%..100%
    p.clamp(0, 100)
}

/// dam_multi(ch, vict, type) (utils.c): the damage multiplier ch gets vs vict.
/// type 0 = normal attack (power/str vs defense/con), type 1 = magical
/// (mpower/int vs mdefense/wis).
///
/// NOTE: the negative branch reproduces the C exactly. In C, `2/300` is
/// *integer* division and equals 0, so `p = 1 - (2/300)*p` is always 1 for
/// p < 0. We keep that quirk: an attacker weaker than the defender never drops
/// below a 1.0 multiplier here (the final `< 0 ? 0` clamp can still apply).
pub fn dam_multi(g: &GameState, ch: CharId, vict: CharId, ty: i32) -> f32 {
    let (a_pow, a_mpow, a_str, a_int) = match g.get_char(ch) {
        Some(c) => (
            c.points.power as f32,
            c.points.mpower as f32,
            c.aff_abils.str as f32,
            c.aff_abils.intel as f32,
        ),
        None => return 1.0,
    };
    let (v_def, v_mdef, v_con, v_wis) = match g.get_char(vict) {
        Some(c) => (
            c.points.defense as f32,
            c.points.mdefense as f32,
            c.aff_abils.con as f32,
            c.aff_abils.wis as f32,
        ),
        None => return 1.0,
    };
    let mut p: f32 = match ty {
        0 => (a_pow - v_def) + (a_str - v_con) * 10.0,
        1 => (a_mpow - v_mdef) + (a_int - v_wis) * 10.0,
        _ => return 1.0,
    };
    p /= 10.0; // widen the ranges
    if p >= 0.0 {
        p = 1.0 + (2.0 * p / 100.0);
    } else {
        p *= -1.0;
        // C: p = 1 - (2/300)*p; 2/300 == 0 in integer math, so this is 1.
        p = 1.0 - (0.0 * p);
    }
    if p < 0.0 {
        0.0
    } else {
        p
    }
}

/// GET_ATTACKTYPE(ch) (utils.h): the weapon attack-type for ch — the wielded
/// weapon's value[3] (+TYPE_HIT), else the mob's BareHandAttack (+TYPE_HIT),
/// else plain TYPE_HIT.
pub fn get_attacktype(g: &GameState, ch: CharId) -> i32 {
    if let Some(w) = g.get_char(ch).and_then(|c| c.equipment[WEAR_WIELD]) {
        if let Some(o) = g.get_obj(w) {
            let v3 = o.values[3];
            if o.is_weapon() && v3 > -1 && v3 < NUM_ATTACK_TYPES {
                return v3 + TYPE_HIT;
            }
            return TYPE_HIT;
        }
    }
    // Bare-handed: NPCs use their prototype BareHandAttack; PCs use TYPE_HIT.
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return TYPE_HIT,
    };
    if c.is_npc {
        let at = g.mob_protos.get(&c.nr).map(|p| p.attack_type).unwrap_or(0);
        if at > -1 && at < NUM_ATTACK_TYPES {
            return at + TYPE_HIT;
        }
    }
    TYPE_HIT
}

/// Resolve one attack (fight.c hit()). DeltaMUD: `number(0,100) > chance()`
/// decides hit vs miss; on a hit, weapon/bare/mob dice + a position multiplier
/// build the raw damage, which `damage()` then scales by `dam_multi()`.
pub fn hit(g: &mut GameState, ch: CharId, victim: CharId) {
    // peaceful-room / same-room sanity is handled by callers and damage(); here
    // we just resolve the swing as fight.c hit() does.
    let attacktype = get_attacktype(g, ch);

    let diceroll = g.rng.number(0, 100);
    let awake = g.get_char(victim).map(|c| c.position > Position::Sleeping).unwrap_or(false);

    // Decide whether this is a hit or a miss.
    if diceroll > chance(g, ch, victim, 0) && awake {
        // The attacker missed the victim (0 damage -> miss message).
        damage_type(g, ch, victim, 0, attacktype);
        return;
    }

    // The victim has been hit; now calculate damage.
    let wield = g.get_char(ch).and_then(|c| c.equipment[WEAR_WIELD]);
    let mut dam: i32 = if let Some((n, s)) = wield.and_then(|w| g.get_obj(w)).and_then(|o| o.damage_dice()) {
        g.rng.dice(n, s)
    } else {
        // No weapon: NPCs use prototype damnodice/damsizedice, PCs roll 0..2.
        let is_npc = g.get_char(ch).map(|c| c.is_npc).unwrap_or(false);
        if is_npc {
            let nr = g.get_char(ch).map(|c| c.nr).unwrap_or(NOBODY);
            let (n, s) = g
                .mob_protos
                .get(&nr)
                .map(|p| (p.damnodice, p.damsizedice))
                .unwrap_or((0, 0));
            g.rng.dice(n, s)
        } else {
            g.rng.number(0, 2) // max 2 bare-hand damage for players
        }
    };

    // Position multiplier if the victim isn't ready to fight (fight.c hack):
    //   sitting 1.33x .. mortally 3.00x. Integer math keyed off POS_FIGHTING.
    let v_pos = g.get_char(victim).map(|c| c.position as i32).unwrap_or(POS_FIGHTING_ORD);
    if v_pos < POS_FIGHTING_ORD {
        dam *= 1 + (POS_FIGHTING_ORD - v_pos) / 3;
    }

    // At least 1 hp damage per hit.
    dam = dam.max(1);

    // Victim defensive skills (fight.c do_actual_damage, before HP is deducted):
    // riposte / avoid / parry / dodge. A successful defense consumes the swing.
    if try_defensive_skills(g, ch, victim, attacktype) {
        return;
    }

    damage_type(g, ch, victim, dam, attacktype);
}

/// Victim defensive-skill checks (fight.c do_actual_damage, ~lines 903-979),
/// run after a melee hit has landed but before any HP is deducted. Returns
/// `true` if the swing was consumed (the caller must not apply its damage):
///   * RIPOSTE — victim strikes `ch` back (weapon dice or 2 bare-hand).
///   * AVOID   — victim trips `ch` to POS_SITTING + a violence wait-state.
///   * PARRY   — full block, no damage, no position change.
///   * DODGE   — full evade, no damage.
/// Only fires for true weapon attacks (TYPE_HIT..=TYPE_STAB) when the attacker
/// is standing (`GET_POS(ch) > POS_STANDING-1`). Each roll is
/// `number(1,100) * AVOID_FACTOR <= GET_SKILL(victim, X)`, tried in C's order.
fn try_defensive_skills(g: &mut GameState, ch: CharId, victim: CharId, attacktype: i32) -> bool {
    // Guard: only real weapon attacks, and only while the attacker stands.
    if !(TYPE_HIT..=TYPE_STAB).contains(&attacktype) {
        return false;
    }
    let ch_standing = g
        .get_char(ch)
        .map(|c| (c.position as i32) > (Position::Standing as i32) - 1)
        .unwrap_or(false);
    if !ch_standing {
        return false;
    }

    let skill = |g: &GameState, num: u16| -> i32 {
        g.get_char(victim).map(|c| c.skill(num) as i32).unwrap_or(0)
    };

    // --- Riposte: counter-attack ----------------------------------------
    if g.rng.number(1, 100) * AVOID_FACTOR <= skill(g, SKILL_RIPOSTE) {
        act(g, "You anticipate $N's attack, avoiding it, and striking back!",
            true, victim, None, ActArg::Char(ch), To::Char);
        act(g, "$n anticipates your attack, and strikes back at you!",
            false, victim, None, ActArg::Char(ch), To::Vict);
        act(g, "$n anticipates $N's ameteur attack, and strikes back expertly.",
            false, victim, None, ActArg::Char(ch), To::NotVict);

        // Weapon dice (val1, val2) with the attacker's position multiplier,
        // else minimal bare-hand damage. damage_dice() yields Some only for a
        // wielded ITEM_WEAPON, exactly matching C's `tobj` / GET_OBJ_TYPE guard.
        let rip_dam = match g
            .get_char(victim)
            .and_then(|c| c.equipment[WEAR_WIELD])
            .and_then(|w| g.get_obj(w))
            .and_then(|o| o.damage_dice())
        {
            Some((n, s)) => {
                let mut d = g.rng.dice(n, s);
                let ch_pos = g.get_char(ch).map(|c| c.position as i32).unwrap_or(POS_FIGHTING_ORD);
                d *= 1 + (POS_FIGHTING_ORD - ch_pos) / 3;
                d
            }
            None => 2,
        };
        damage_type(g, victim, ch, rip_dam, SKILL_RIPOSTE as i32);
        return true;
    }

    // --- Avoid: trip the attacker ---------------------------------------
    if g.rng.number(1, 100) * AVOID_FACTOR <= skill(g, SKILL_AVOID) {
        act(g, "You avoid $N's attack, tossing $M to the ground.",
            true, victim, None, ActArg::Char(ch), To::Char);
        act(g, "$n avoids your attack, trips you, sending you to the ground.",
            false, victim, None, ActArg::Char(ch), To::Vict);
        act(g, "$n avoids $N's pathetic attack and sends $M sprawling.",
            false, victim, None, ActArg::Char(ch), To::NotVict);

        if let Some(c) = g.get_char_mut(ch) {
            c.position = Position::Sitting;
        }
        g.set_wait_state(ch, PULSE_VIOLENCE as i32);
        return true;
    }

    // --- Parry: full block ----------------------------------------------
    if g.rng.number(1, 100) * AVOID_FACTOR <= skill(g, SKILL_PARRY) {
        act(g, "You parry $N's viscious attack upon your person.",
            true, victim, None, ActArg::Char(ch), To::Char);
        act(g, "$n spoils your attack with a deft parrying move.",
            false, victim, None, ActArg::Char(ch), To::Vict);
        act(g, "$n parries $N's attack with a series of skillful maneuvers.",
            false, victim, None, ActArg::Char(ch), To::NotVict);
        return true;
    }

    // --- Dodge: full evade ----------------------------------------------
    if g.rng.number(1, 100) * AVOID_FACTOR <= skill(g, SKILL_DODGE) {
        act(g, "You narrowly dodge $N's masterful attack.",
            true, victim, None, ActArg::Char(ch), To::Char);
        act(g, "$n narrowly dodges your skillful attack, just avoiding your intended blow.",
            false, victim, None, ActArg::Char(ch), To::Vict);
        act(g, "$n narrowly dodges $N's strike.",
            false, victim, None, ActArg::Char(ch), To::NotVict);
        return true;
    }

    false
}

/// damage(ch, victim, dam) — back-compat entry for callers that don't carry an
/// attack type (dg scripts, environment damage). Routes to the typed path with
/// TYPE_UNDEFINED, exactly as C `damage(ch,victim,dam,TYPE_UNDEFINED)` would.
pub fn damage(g: &mut GameState, ch: CharId, victim: CharId, dmg: i32) {
    damage_type(g, ch, victim, dmg, TYPE_UNDEFINED);
}

/// do_actual_damage (fight.c): scale the raw damage by dam_multi(), apply it,
/// emit the weapon/skill damage message, update position, drive retaliation
/// and death. `attacktype` selects the verb table and the dam_multi flavour.
pub fn damage_type(g: &mut GameState, ch: CharId, victim: CharId, dmg: i32, attacktype: i32) {
    // PK flagging on a non-PK MUD (fight.c do_actual_damage).
    if !PK_ALLOWED {
        check_killer(g, ch, victim);
    }

    // Damage multiplier. Spells (1..=MAX_SPELLS) use the magical flavour;
    // everything else (weapons / skills / undefined) uses the physical one.
    let mt = if attacktype > 0 && attacktype <= MAX_SPELLS { 1 } else { 0 };
    let mut dmg = (dmg as f32 * dam_multi(g, ch, victim, mt)) as i32;

    // Clamp to the per-round window and subtract hp.
    dmg = dmg.clamp(0, 1000);
    if let Some(v) = g.get_char_mut(victim) {
        v.points.hit -= dmg;
    }

    // A mob remembers a PC who strikes it (mobact memory; fight.c).
    let ch_is_pc = g.get_char(ch).map(|c| !c.is_npc).unwrap_or(false);
    let vic_is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
    if ch_is_pc && vic_is_npc {
        crate::mobact::remember(g, victim, ch);
    }

    update_position(g, victim);

    // Damage / miss message (weapon verb table for IS_WEAPON attack types).
    if is_weapon_type(attacktype) {
        dam_message(g, dmg, ch, victim, attacktype);
    } else if attacktype == TYPE_UNDEFINED {
        // Generic strike with no weapon flavour (environment / dg damage):
        // fall back to the plain TYPE_HIT verb so onlookers still see a hit.
        dam_message(g, dmg, ch, victim, TYPE_HIT);
    }
    // (Spell/skill messages are emitted by the magic subsystem, as in C.)

    // Victim retaliates if not already fighting.
    if g.get_char(victim).and_then(|c| c.fighting).is_none()
        && g.get_char(victim).map(|c| c.position > Position::Stunned).unwrap_or(false)
    {
        set_fighting(g, victim, ch);
    }

    if g.get_char(victim).map(|c| c.position == Position::Dead).unwrap_or(false) {
        die(g, ch, victim);
    }
}

/// IS_WEAPON(type) (fight.c): TYPE_HIT..TYPE_STAB inclusive (the verb table).
fn is_weapon_type(attacktype: i32) -> bool {
    attacktype >= TYPE_HIT && attacktype <= TYPE_STAB
}

/// check_killer (fight.c): on a non-PK MUD, a player who initiates an attack on
/// another player in a jurisdicted, non-peaceful area becomes a PLAYER KILLER.
pub fn check_killer(g: &mut GameState, ch: CharId, vict: CharId) {
    if ch == vict {
        return;
    }
    let (ch_npc, ch_killer) = match g.get_char(ch) {
        Some(c) => (c.is_npc, !c.is_npc && c.act_flags & PLR_KILLER != 0),
        None => return,
    };
    let (v_npc, v_killer, v_thief) = match g.get_char(vict) {
        Some(c) => (
            c.is_npc,
            !c.is_npc && c.act_flags & PLR_KILLER != 0,
            !c.is_npc && c.act_flags & PLR_THIEF != 0,
        ),
        None => return,
    };
    // Only PC-on-PC, neither party already flagged, attacker not a KILLER.
    if v_killer || v_thief || ch_killer || ch_npc || v_npc {
        return;
    }
    let room = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    // IS_JURISDICTED(room): SECT_CITY || SECT_INSIDE. Skip peaceful rooms.
    let sect = g.room(room).sector_type;
    let jurisdicted = matches!(sect, SectorType::City | SectorType::Inside);
    if !jurisdicted || g.room(room).room_flags.contains(RoomFlags::PEACEFUL) {
        return;
    }

    // Flag the killer, drop alignment to the floor, notify.
    let ch_thief = g.get_char(ch).map(|c| c.act_flags & PLR_THIEF != 0).unwrap_or(false);
    let log = format!(
        "PC Killer bit set on {} for initiating attack on {} at {}.",
        g.get_char(ch).map(|c| c.get_name().to_string()).unwrap_or_default(),
        g.get_char(vict).map(|c| c.get_name().to_string()).unwrap_or_default(),
        g.room(room).name.clone(),
    );
    if !ch_thief {
        mudlog(g, &log, LVL_IMMORT);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.act_flags |= PLR_KILLER;
        c.alignment = -1000;
    }
    g.send_to_char(
        ch,
        "This is a jurisdicted area. If you want to be a PLAYER KILLER, so be it...\r\n",
    );
}

/// mudlog(): broadcast a brief log line to immortals at/above `min_level`.
fn mudlog(g: &mut GameState, line: &str, min_level: u8) {
    let formatted = format!("[ {} ]\r\n", line);
    let imms: Vec<CharId> = g
        .players_by_name
        .values()
        .copied()
        .filter(|&id| {
            g.get_char(id)
                .map(|c| c.player.level >= min_level && c.player.level >= LVL_IMMORT)
                .unwrap_or(false)
        })
        .collect();
    for id in imms {
        g.send_to_char(id, &formatted);
    }
    eprintln!("[ {} ]", line);
}

fn update_position(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        let hp = c.points.hit;
        c.position = if hp > 0 {
            if c.position == Position::Fighting { Position::Fighting } else { c.position }
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

/// Handle a death: messages, loot to a corpse, extract NPC / respawn PC.
fn die(g: &mut GameState, killer: CharId, victim: CharId) {
    stop_fighting(g, killer);
    stop_fighting(g, victim);

    // Arena fatalities are handled by the arena subsystem (concede/restore);
    // skip the normal corpse/respawn path if so.
    if crate::arena::arena_combat_death(g, killer, victim) {
        return;
    }

    // DG death trigger fires before the corpse/extract (death_mtrigger).
    crate::dg_triggers::death_mtrigger(g, victim, Some(killer));

    act(g, "$n is dead! R.I.P.", false, victim, None, ActArg::None, To::Room);
    g.send_to_char(victim, "You are dead!  Sorry...\r\n");

    // Award the killer experience for the kill (CircleMUD group_gain/gain_exp).
    let is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
    if is_npc && killer != victim {
        let exp = g.get_char(victim).map(|c| c.points.exp).unwrap_or(0).max(1);
        crate::limits::gain_exp(g, killer, exp);
        // Mark the kill against any active autoquest (fight.c PLR_QUESTOR).
        crate::quest::quest_on_kill(g, killer, victim);
    }
    let rnum = g.get_char(victim).and_then(|c| c.in_room);

    // Make a corpse holding the victim's inventory + equipment.
    if let Some(rnum) = rnum {
        let name = g.get_char(victim).map(|c| c.display_for_others()).unwrap_or_default();
        let corpse = make_corpse(g, &name);
        // Move carried + worn objects into the corpse.
        let carried = g.get_char(victim).map(|c| c.carrying.clone()).unwrap_or_default();
        for oid in carried {
            g.obj_from_anywhere(oid);
            g.obj_to_obj(oid, corpse);
        }
        let worn: Vec<usize> = (0..NUM_WEARS)
            .filter(|&p| g.get_char(victim).map(|c| c.equipment[p].is_some()).unwrap_or(false))
            .collect();
        for p in worn {
            if let Some(oid) = g.unequip_char(victim, p) {
                g.obj_to_obj(oid, corpse);
            }
        }
        g.obj_to_room(corpse, rnum);
    }

    if is_npc {
        crate::mobact::clear_memory(victim);
        crate::arena::forget_char(victim);
        g.extract_char(victim);
    } else {
        // Respawn the PC at its hometown (Tier-0 death penalty deferred).
        respawn_pc(g, victim);
    }
}

fn make_corpse(g: &mut GameState, who: &str) -> ObjId {
    let mut obj = Object::new(NOTHING, format!("corpse {}", who), format!("the corpse of {}", who));
    obj.description = format!("The corpse of {} is lying here.", who);
    obj.obj_type = ObjectType::Container;
    obj.timer = 60;
    // values[3]=1 marks this as a corpse so limits::point_update decays it.
    obj.values = [0, 0, 0, 1];
    obj.loc = ObjLoc::Nowhere;
    g.create_obj(obj)
}

fn respawn_pc(g: &mut GameState, victim: CharId) {
    g.char_from_room(victim);
    let home = g.get_char(victim).map(|c| c.player.hometown).unwrap_or(3001);
    let rnum = g.real_room(home).or_else(|| g.real_room(3001)).unwrap_or(0);
    if let Some(c) = g.get_char_mut(victim) {
        c.points.hit = 1;
        c.position = Position::Resting;
        c.fighting = None;
    }
    g.char_to_room(victim, rnum);
    g.send_to_char(victim, "\r\nYou feel your spirit drawn back to a familiar place...\r\n");
    crate::cmd_informative::look_at_room(g, victim, false);
}

/// dam_message (fight.c): the per-weapon-type damage message. The verb
/// (singular/plural) is chosen from ATTACK_HIT_TEXT by `w_type - TYPE_HIT`;
/// the severity tier (0..11, by damage) selects the message template. `#w`
/// and `#W` in the template are replaced with the singular/plural verb.
fn dam_message(g: &mut GameState, dam: i32, ch: CharId, victim: CharId, w_type: i32) {
    // `to_room`, `to_char`, `to_victim` templates per severity tier (fight.c).
    const DAM_WEAPONS: &[(&str, &str, &str)] = &[
        // 0: miss
        (
            "$n tries to #w $N, but misses.",
            "You try to #w $N, but miss.",
            "$n tries to #w you, but misses.",
        ),
        // 1: 1..27
        (
            "$n tickles $N as $e #W $M.",
            "You tickle $N as you #w $M.",
            "$n tickles you as $e #W you.",
        ),
        // 2: 28..54
        ("$n barely #W $N.", "You barely #w $N.", "$n barely #W you."),
        // 3: 55..81
        ("$n #W $N.", "You #w $N.", "$n #W you."),
        // 4: 82..108
        ("$n #W $N hard.", "You #w $N hard.", "$n #W you hard."),
        // 5: 109..135
        (
            "$n #W $N very hard.",
            "You #w $N very hard.",
            "$n #W you very hard.",
        ),
        // 6: 136..162
        (
            "$n #W $N extremely hard.",
            "You #w $N extremely hard.",
            "$n #W you extremely hard.",
        ),
        // 7: 163..189
        (
            "$n massacres $N to small fragments with $s #w.",
            "You massacre $N to small fragments with your #w.",
            "$n massacres you to small fragments with $s #w.",
        ),
        // 8: 190..216
        (
            "$n OBLITERATES $N with $s deadly #w!!",
            "You OBLITERATE $N with your deadly #w!!",
            "$n OBLITERATES you with $s deadly #w!!",
        ),
        // 9: 217..243
        (
            "$n PULVERIZES $N to bits with $s deadly #w!!",
            "You PULVERIZE $N to bits with your deadly #w!!",
            "$n PULVERIZES you to bits with $s deadly #w!!",
        ),
        // 10: 244..270
        (
            "$n VAPORIZES $N with $s deadly #w!!",
            "You VAPORIZE $N with your deadly #w!!",
            "$n VAPORIZES you with $s deadly #w!!",
        ),
        // 11: > 270
        (
            "$n ANNIHILATES $N to smithereens with $s deadly #w!!",
            "You &RANNIHILATE&Y $N to smithereens with your deadly #w!!",
            "$n &RANNIHILATES&Y you to smithereens with $s deadly #w!!",
        ),
    ];

    // Change to base of the verb table; clamp to a valid index defensively.
    let idx = (w_type - TYPE_HIT).clamp(0, (ATTACK_HIT_TEXT.len() - 1) as i32) as usize;
    let (singular, plural) = ATTACK_HIT_TEXT[idx];

    let msgnum = if dam == 0 {
        0
    } else if dam <= 27 {
        1
    } else if dam <= 54 {
        2
    } else if dam <= 81 {
        3
    } else if dam <= 108 {
        4
    } else if dam <= 135 {
        5
    } else if dam <= 162 {
        6
    } else if dam <= 189 {
        7
    } else if dam <= 216 {
        8
    } else if dam <= 243 {
        9
    } else if dam <= 270 {
        10
    } else {
        11
    };
    let (to_room, to_char, to_victim) = DAM_WEAPONS[msgnum];

    // To onlookers, to the damager (yellow), to the damagee (red).
    let s = replace_string(to_room, singular, plural);
    act(g, &s, false, ch, None, ActArg::Char(victim), To::NotVict);

    let s = format!("&Y{}&n", replace_string(to_char, singular, plural));
    act(g, &s, false, ch, None, ActArg::Char(victim), To::Char);

    let s = format!("&R{}&n", replace_string(to_victim, singular, plural));
    crate::act::act_sleep(g, &s, false, ch, None, ActArg::Char(victim), To::Vict, true);
}

/// replace_string (fight.c): substitute `#w`/`#W` with the weapon's singular /
/// plural verb. `#` followed by anything else is emitted literally.
fn replace_string(template: &str, singular: &str, plural: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c == '#' {
            match chars.next() {
                Some('W') => out.push_str(plural),
                Some('w') => out.push_str(singular),
                Some(other) => {
                    out.push('#');
                    out.push(other);
                }
                None => out.push('#'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// flee: try a random exit out of combat (CircleMUD do_flee, simplified).
pub fn do_flee(g: &mut GameState, ch: CharId) {
    let fighting = g.get_char(ch).and_then(|c| c.fighting);
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    for _ in 0..6 {
        let dir = g.rng.number(0, (NUM_OF_DIRS - 1) as i32) as usize;
        let to = g.room(rnum).exits[dir].as_ref().and_then(|e| g.real_room(e.to_room));
        if let Some(to_rnum) = to {
            act(g, "$n panics, and attempts to flee!", true, ch, None, ActArg::None, To::Room);
            stop_fighting(g, ch);
            g.char_from_room(ch);
            g.char_to_room(ch, to_rnum);
            g.send_to_char(ch, "You flee head over heels.\r\n");
            act(g, "$n glances around, panting.", true, ch, None, ActArg::None, To::Room);
            crate::cmd_informative::look_at_room(g, ch, false);
            return;
        }
    }
    let _ = fighting;
    g.send_to_char(ch, "PANIC!  You couldn't escape!\r\n");
}
