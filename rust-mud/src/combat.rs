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
use crate::character::Affect;
use crate::constants::ATTACK_HIT_TEXT;
use crate::flags::{
    AFF_GROUP, AFF_HIDE, AFF_INVISIBLE, AFF_SANCTUARY, AFF_SLEEP, APPLY_POWER, MOB_DBLATTACK,
    MOB_WIMPY,
};
use crate::object::{ObjLoc, Object, ObjectType};
use crate::room::{RoomFlags, SectorType, EX_CLOSED};
use crate::spell_parser::{
    MAX_SPELLS, SKILL_ADRENALINE, SKILL_BLOODLUST, SKILL_CARNALRAGE, SPELL_REDIRECT_CHARGE,
    SPELL_SLEEP, TYPE_UNDEFINED,
};
use crate::state::GameState;
use crate::types::*;

// Attack-type sentinels (spells.h). The weapon verb table is keyed by
// (attacktype - TYPE_HIT); SKILL_BACKSTAB pierces.
pub const TYPE_HIT: i32 = 1100;
pub const TYPE_STAB: i32 = 1114;
const SELF_DAMAGE: i32 = 1197; // spells.h
const PRF2_MERCY: i64 = 1 << 7; // structs.h PRF2_FLAGS
const NUM_ATTACK_TYPES: i32 = 15; // olc.h
const SKILL_BACKSTAB: u16 = 501;
const SKILL_SECOND_ATTACK: u16 = 524;
const SKILL_THIRD_ATTACK: u16 = 525;

// PLR_* act-flag bits for PCs (structs.h).
const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;
const PRF_SUMMONABLE: i64 = 1 << 10;
const PRF_NOAUCT: i64 = 1 << 18;
const BAIL_MULTIPLIER: i32 = 20_000;

// Victim defensive skills (spells.h) and their shared difficulty scalar.
// fight.c rolls (number(1,100) * AVOID_FACTOR) <= GET_SKILL(victim, X).
const SKILL_DODGE: u16 = 532;
const SKILL_PARRY: u16 = 533;
const SKILL_AVOID: u16 = 534;
const SKILL_RIPOSTE: u16 = 535;
const AVOID_FACTOR: i32 = 20; // spells.h

// PRF2_* flags (structs.h) used by the intangible/ghost combat guards.
const PRF2_DISPMOB: i64 = 1 << 5;
const PRF2_MBUILDING: i64 = 1 << 6;
const PRF2_INTANGIBLE: i64 = 1 << 9;
const AFF_R_CHARGED: i64 = 1 << 26;

// config.c — PvP gating (pk_allowed is false on this MUD).
const PK_ALLOWED: bool = false;
const PK_VICTIM_MIN: Level = 10;
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
    if g.get_char(ch)
        .map(|c| c.affect_flags & AFF_SLEEP != 0)
        .unwrap_or(false)
    {
        affect_from_char(g, ch, SPELL_SLEEP);
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

fn affect_from_char(g: &mut GameState, ch: CharId, spell: i32) {
    g.affect_remove_spell(ch, spell);
}

pub(crate) fn stop_fighting(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
    }
    update_position(g, ch);
}

/// One combat pulse: every fighter takes a swing (CircleMUD perform_violence).
pub fn perform_violence(g: &mut GameState) {
    let fighters: Vec<CharId> = g
        .chars
        .iter()
        .filter(|(_, x)| x.fighting.is_some())
        .map(|(&c, _)| c)
        .collect();

    for ch in fighters {
        let victim = match g.get_char(ch).and_then(|c| c.fighting) {
            Some(v) => v,
            None => continue,
        };
        // Victim must still exist, be in the same room, and be alive.
        let same_room =
            g.get_char(ch).and_then(|c| c.in_room) == g.get_char(victim).and_then(|c| c.in_room);
        if !g.char_exists(victim) || !same_room {
            stop_fighting(g, ch);
            continue;
        }
        if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
            let mut scrambled = false;
            let mut waiting = false;
            if let Some(c) = g.get_char_mut(ch) {
                if c.mob_wait > 0 {
                    c.mob_wait -= PULSE_VIOLENCE as i32;
                    waiting = true;
                    if c.mob_wait <= 0 {
                        c.mob_wait = 0;
                        if c.position < Position::Fighting {
                            c.position = Position::Fighting;
                            scrambled = true;
                        }
                    }
                } else {
                    c.mob_wait = 0;
                    if c.position < Position::Fighting {
                        c.position = Position::Fighting;
                        scrambled = true;
                    }
                }
            }
            if scrambled {
                act(
                    g,
                    "$n scrambles to $s feet!",
                    true,
                    ch,
                    None,
                    ActArg::None,
                    To::Room,
                );
            }
            if waiting {
                continue;
            }
            // MOB_MERCY (fight.c:1384-1387, #101): a mercy-flagged mob will
            // not finish off a victim below POS_SLEEPING; both disengage.
            let mercy = g
                .get_char(ch)
                .map(|c| c.is_npc && c.act_flags & (1 << 24) != 0)
                .unwrap_or(false);
            let v_pos = g.get_char(victim).map(|c| c.position).unwrap_or(Position::Dead);
            if mercy && v_pos < Position::Sleeping {
                stop_fighting(g, victim);
                stop_fighting(g, ch);
                continue;
            }
        }
        if g.get_char(ch)
            .map(|c| c.position < Position::Fighting)
            .unwrap_or(true)
        {
            g.send_to_char(ch, "You can't fight while sitting!!\r\n");
            continue;
        }
        let show_diag = g
            .get_char(ch)
            .map(|c| !c.is_npc && c.prf2_flags & PRF2_DISPMOB == 0)
            .unwrap_or(false);
        if show_diag {
            crate::cmd_informative::diag_char_to_char(g, victim, ch);
        }
        let apr = additional_attack_rounds(g, ch);
        if apr >= 0 {
            for _ in 0..=apr {
                let Some(target) = g.get_char(ch).and_then(|c| c.fighting) else {
                    break;
                };
                hit(g, ch, target);
                if g.get_char(ch).and_then(|c| c.fighting).is_some() {
                    damage_worn_equipment_after_hit(g, ch);
                }
            }
            // C fight.c:1478-1481: the MOB_SPEC/MOB_CASTER call sits inside
            // the apr >= 0 block - a mob that earned no attacks does not
            // proc (#109).
            if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
                crate::mobact::combat_mob_spec_pulse(g, ch);
            }
        }
    }
}

fn additional_attack_rounds(g: &mut GameState, ch: CharId) -> i32 {
    let (class, level, is_npc, act_flags, second_skill, third_skill) = match g.get_char(ch) {
        Some(c) => (
            c.player.class,
            c.player.level as i32,
            c.is_npc,
            c.act_flags,
            c.skill(SKILL_SECOND_ATTACK) as i32,
            c.skill(SKILL_THIRD_ATTACK) as i32,
        ),
        None => return -1,
    };
    let mut apr = 0;

    if matches!(class, Class::Warrior | Class::Thief) {
        let mut percent = g.rng.number(((second_skill as f32) * 0.85) as i32, 101)
            - (level / g.rng.number(10, 20));
        if percent <= 0 {
            percent = 1;
        }
        if second_skill > percent {
            apr += 1;
            let percent = g.rng.number(((third_skill as f32) * 0.9) as i32, 151)
                - (level / g.rng.number(10, 30));
            if third_skill > percent && class == Class::Warrior {
                apr += 1;
            }
        }
    }

    if is_npc && act_flags & MOB_DBLATTACK != 0 {
        let mut percent = g.rng.number(50, 101) - (level / g.rng.number(10, 20));
        if percent <= 0 {
            percent = 1;
        }
        if g.rng.number(1, 100) > percent {
            apr += 1;
        }
    }

    apr.clamp(-1, 4)
}

fn damage_worn_equipment_after_hit(g: &mut GameState, ch: CharId) {
    let victim = match g.get_char(ch).and_then(|c| c.fighting) {
        Some(v) => v,
        None => return,
    };
    let (is_npc, dex, equipment) = match g.get_char(victim) {
        Some(v) => (v.is_npc, v.aff_abils.dex as i32, v.equipment),
        None => return,
    };
    if is_npc {
        return;
    }

    // C computes this and then calls MAX(MIN(condition,30),10) without
    // assigning the result, so the unclamped value is the effective threshold.
    let condition = 20 - (((dex - 12) * 5) / 3);
    for oid in equipment.into_iter().flatten() {
        let total_slots = g.get_obj(oid).map(|o| o.total_slots).unwrap_or(0);
        if total_slots != 0 && g.rng.number(1, 100) <= condition {
            let damaged = if let Some(o) = g.get_obj_mut(oid) {
                // C fight.c:1460 writes 'curr_slots = curr_slots--', a
                // post-decrement no-op that silently neuters the decrement;
                // we keep the AUTHOR'S INTENT (a real decrement) and list
                // this C bug in the COMPATIBILITY.md register (#114).
                o.curr_slots -= 1;
                Some(o.short_description.clone())
            } else {
                None
            };
            if let Some(short) = damaged {
                g.send_to_char(
                    victim,
                    &format!("{} just got DAMAGED during the combat!\r\n", short),
                );
                g.mark_crash(victim);
            }
        }
        let crumble = g
            .get_obj(oid)
            .filter(|o| o.total_slots != 0 && o.curr_slots == 0)
            .map(|o| o.short_description.clone());
        if let Some(short) = crumble {
            g.send_to_char(
                victim,
                &format!("{} crumbles to dust as it wears out!\r\n", short),
            );
            g.obj_from_anywhere(oid);
            g.extract_obj(oid);
        }
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
        Some(c) => (
            c.points.technique as i32,
            c.aff_abils.dex as i32,
            c.aff_abils.intel as i32,
        ),
        None => return 0,
    };
    let (v_tech, v_dex, v_wis) = match g.get_char(vict) {
        Some(c) => (
            c.points.technique as i32,
            c.aff_abils.dex as i32,
            c.aff_abils.wis as i32,
        ),
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

/// Resolve one attack (fight.c hit()). Back-compat 2-arg entry: the attack type
/// is derived from the wielded weapon / bare-hand attack via get_attacktype(),
/// exactly as the in-line callers (perform_violence, mob/spec/spell hits) want.
pub fn hit(g: &mut GameState, ch: CharId, victim: CharId) {
    hit_type(g, ch, victim, TYPE_UNDEFINED);
}

/// Resolve one attack with an explicit attack type (fight.c hit(ch,vict,type)).
/// DeltaMUD: `number(0,100) > chance()` decides hit vs miss; on a hit,
/// weapon/bare/mob dice + a position multiplier build the raw damage, which
/// `damage()` then scales by `dam_multi()`.
///
/// `ty` mirrors C's `type`: TYPE_UNDEFINED means "derive the verb from the
/// wielded weapon" (the normal melee round), SKILL_BACKSTAB applies the
/// level-scaled backstab multiplier and is passed through to `damage()`.
pub fn hit_type(g: &mut GameState, ch: CharId, victim: CharId, ty: i32) {
    // C `hit(ch,victim,type)` forwards `type` to `damage()`; for an untyped
    // melee swing that resolves to the weapon verb (GET_ATTACKTYPE), and
    // SKILL_BACKSTAB stays SKILL_BACKSTAB. Match that here.
    let attacktype = if ty == TYPE_UNDEFINED {
        get_attacktype(g, ch)
    } else {
        ty
    };
    let is_backstab = ty == SKILL_BACKSTAB as i32;

    // C fight.c:1271-1274: the peaceful-room check sits at the very top of
    // hit(), BEFORE the diceroll - no RNG draw and no DG fight triggers fire
    // in a peaceful room (#137).
    {
        let ch_lvl = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
        let peaceful = g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.room(r).room_flags.contains(RoomFlags::PEACEFUL))
            .unwrap_or(false);
        if peaceful && ch != victim && ch_lvl < LVL_IMPL {
            g.send_to_char(
                ch,
                "This room just has such a peaceful, easy feeling...\r\n",
            );
            return;
        }
    }

    crate::dg_triggers::fight_mtrigger(g, ch);
    crate::dg_triggers::fight_otrigger(g, ch);

    let same_room =
        g.get_char(ch).and_then(|c| c.in_room) == g.get_char(victim).and_then(|c| c.in_room);
    if !same_room {
        if g.get_char(ch).and_then(|c| c.fighting) == Some(victim) {
            stop_fighting(g, ch);
        }
        return;
    }

    let diceroll = g.rng.number(0, 100);
    let awake = g
        .get_char(victim)
        .map(|c| c.position > Position::Sleeping)
        .unwrap_or(false);

    // Decide whether this is a hit or a miss.
    if diceroll > chance(g, ch, victim, 0) && awake {
        // The attacker missed the victim (0 damage -> miss message).
        damage_type(g, ch, victim, 0, attacktype);
        crate::dg_triggers::hitprcnt_mtrigger(g, victim);
        return;
    }

    // The victim has been hit; now calculate damage.
    let wield = g.get_char(ch).and_then(|c| c.equipment[WEAR_WIELD]);
    let mut dam: i32 = if let Some((n, s)) = wield
        .and_then(|w| g.get_obj(w))
        .and_then(|o| o.damage_dice())
    {
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
    let v_pos = g
        .get_char(victim)
        .map(|c| c.position as i32)
        .unwrap_or(POS_FIGHTING_ORD);
    if v_pos < POS_FIGHTING_ORD {
        dam *= 1 + (POS_FIGHTING_ORD - v_pos) / 3;
    }

    // At least 1 hp damage per hit.
    dam = dam.max(1);

    // Backstab: multiply the rolled damage by the level-scaled backstab table
    // (fight.c hit(): `dam *= backstab_mult(GET_LEVEL(ch))`) before damage().
    if is_backstab {
        let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
        dam *= backstab_mult(level);
    }

    // NOTE: the riposte/avoid/parry/dodge defensive block does NOT run here.
    // It lives inside damage_type() (= C do_actual_damage), which engages both
    // combatants first; running it in hit() would consume the opening swing
    // before set_fighting and combat would never start (BUG 4).
    damage_type(g, ch, victim, dam, attacktype);
    trigger_redirect_charge(g, ch, victim);
    crate::dg_triggers::hitprcnt_mtrigger(g, victim);
}

fn trigger_redirect_charge(g: &mut GameState, ch: CharId, victim: CharId) {
    let charge = g.get_char(ch).and_then(|c| {
        if c.affect_flags & AFF_R_CHARGED == 0 {
            return None;
        }
        c.affected
            .iter()
            .find(|af| af.spell_type == SPELL_REDIRECT_CHARGE && af.bitvector == AFF_R_CHARGED)
            .map(|af| af.modifier)
    });
    let Some(charge) = charge else {
        return;
    };

    act(
        g,
        "You momentarily run your finger against $N's skin and a charge of electricity jumps from your body into theirs!\r\n$N CRISPS AND FRIES!!",
        false,
        ch,
        None,
        ActArg::Char(victim),
        To::Char,
    );
    act(
        g,
        "$n touches you and you find it somewhat... &KELECTRIFYING&n!\r\nYour skin chars and crisps!",
        false,
        ch,
        None,
        ActArg::Char(victim),
        To::Vict,
    );
    act(
        g,
        "You momentarily see a flash of light and $N FRIES to a CRISP!",
        false,
        ch,
        None,
        ActArg::Char(victim),
        To::NotVict,
    );
    damage_type(g, ch, victim, charge, TYPE_UNDEFINED);
    if let Some(c) = g.get_char_mut(ch) {
        if let Some(pos) = c
            .affected
            .iter()
            .position(|af| af.spell_type == SPELL_REDIRECT_CHARGE && af.bitvector == AFF_R_CHARGED)
        {
            c.affected.remove(pos);
        }
        c.affect_flags &= !AFF_R_CHARGED;
    }
}

/// backstab_mult(level) (class.c): the level-banded damage multiplier applied
/// to the backstab weapon roll. Transcribed term-for-term.
fn backstab_mult(level: Level) -> i32 {
    let level = level as i32;
    if level <= 0 {
        1
    } else if level <= 7 {
        2
    } else if level <= 13 {
        3
    } else if level <= 20 {
        4
    } else if level <= 28 {
        5
    } else if level <= 36 {
        6
    } else if level <= 44 {
        7
    } else if level <= 52 {
        8
    } else if level <= 60 {
        9
    } else if level <= 68 {
        10
    } else if level <= 76 {
        11
    } else if level <= 84 {
        12
    } else if (level as u8) < LVL_IMMORT {
        13
    } else {
        // Immortals: C returns a much larger multiplier; keep parity with the
        // mortal cap so an immortal backstab is at least as strong.
        20
    }
}

/// Victim defensive-skill checks (fight.c do_actual_damage, ~lines 903-979),
/// run after engagement (set_fighting) but before any HP is deducted. Returns
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
        act(
            g,
            "You anticipate $N's attack, avoiding it, and striking back!",
            true,
            victim,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$n anticipates your attack, and strikes back at you!",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        act(
            g,
            "$n anticipates $N's ameteur attack, and strikes back expertly.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::NotVict,
        );

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
                let ch_pos = g
                    .get_char(ch)
                    .map(|c| c.position as i32)
                    .unwrap_or(POS_FIGHTING_ORD);
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
        act(
            g,
            "You avoid $N's attack, tossing $M to the ground.",
            true,
            victim,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$n avoids your attack, trips you, sending you to the ground.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        act(
            g,
            "$n avoids $N's pathetic attack and sends $M sprawling.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::NotVict,
        );

        if let Some(c) = g.get_char_mut(ch) {
            c.position = Position::Sitting;
        }
        g.set_wait_state(ch, PULSE_VIOLENCE as i32);
        return true;
    }

    // --- Parry: full block ----------------------------------------------
    if g.rng.number(1, 100) * AVOID_FACTOR <= skill(g, SKILL_PARRY) {
        act(
            g,
            "You parry $N's viscious attack upon your person.",
            true,
            victim,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$n spoils your attack with a deft parrying move.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        act(
            g,
            "$n parries $N's attack with a series of skillful maneuvers.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::NotVict,
        );
        return true;
    }

    // --- Dodge: full evade ----------------------------------------------
    if g.rng.number(1, 100) * AVOID_FACTOR <= skill(g, SKILL_DODGE) {
        act(
            g,
            "You narrowly dodge $N's masterful attack.",
            true,
            victim,
            None,
            ActArg::Char(ch),
            To::Char,
        );
        act(
            g,
            "$n narrowly dodges your skillful attack, just avoiding your intended blow.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        act(
            g,
            "$n narrowly dodges $N's strike.",
            false,
            victim,
            None,
            ActArg::Char(ch),
            To::NotVict,
        );
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

/// deathblow(ch, victim, dam, attacktype): same damage pipeline as C
/// do_actual_damage(..., deathblow=1), notably bypassing the normal weapon
/// death-message branch that is only for non-deathblow kills.
pub fn deathblow(g: &mut GameState, ch: CharId, victim: CharId, dmg: i32, attacktype: i32) {
    do_actual_damage(g, ch, victim, dmg, attacktype, true);
}

const ADRENALINE_HP_PERCENT: i32 = 5;

fn adrenaline_rush(g: &mut GameState, ch: CharId, damage: i32) {
    let Some(c) = g.get_char(ch) else {
        return;
    };
    if c.is_npc || c.skill(SKILL_ADRENALINE as u16) < 70 {
        return;
    }
    let max_hit = c.points.max_hit;
    let threshold = ADRENALINE_HP_PERCENT * max_hit / 100;
    if threshold <= 0 || damage < threshold {
        return;
    }

    let amount = damage / threshold;
    let (max_dur, max_aff, message) = if c.skill(SKILL_CARNALRAGE as u16) > 70 {
        (
            200,
            200,
            "You feel the &RCarnal &rRage&n build within you!!!\r\n",
        )
    } else if c.skill(SKILL_BLOODLUST as u16) > 70 {
        (100, 100, "You &rlust&n for more &RBLOOD&n!!\r\n")
    } else {
        (50, 50, "You feel a surge of &RADRENALINE&n!\r\n")
    };
    g.send_to_char(ch, message);

    let mut af = Affect {
        spell_type: SKILL_ADRENALINE,
        duration: amount,
        modifier: amount,
        location: APPLY_POWER,
        bitvector: 0,
        caster: None,
    };

    if let Some(c) = g.get_char_mut(ch) {
        if let Some(existing) = c
            .affected
            .iter()
            .find(|a| a.spell_type == SKILL_ADRENALINE && a.location == APPLY_POWER)
            .cloned()
        {
            af.duration = (af.duration + existing.duration).min(max_dur - 1);
            af.modifier = (af.modifier + existing.modifier).clamp(-max_aff, max_aff);
            c.affected
                .retain(|a| !(a.spell_type == SKILL_ADRENALINE && a.location == APPLY_POWER));
        }
        c.affected.push(af);
    }
    g.affect_total(ch);
}

/// do_actual_damage (fight.c): apply the combat guards + engagement, scale the
/// raw damage by dam_multi(), run the victim's defensive skills, apply HP,
/// emit the weapon/skill damage message, update position, drive retaliation
/// and death. `attacktype` selects the verb table and the dam_multi flavour.
pub fn damage_type(g: &mut GameState, ch: CharId, victim: CharId, dmg: i32, attacktype: i32) {
    do_actual_damage(g, ch, victim, dmg, attacktype, false);
}

fn do_actual_damage(
    g: &mut GameState,
    ch: CharId,
    victim: CharId,
    dmg: i32,
    attacktype: i32,
    deathblow: bool,
) {
    // Attempt to damage a corpse -> resolve its death and bail (fight.c ~806).
    if g.get_char(victim)
        .map(|c| c.position <= Position::Dead)
        .unwrap_or(true)
    {
        die(g, Some(ch), victim);
        return;
    }

    // PRF2_MERCY pre-damage abort (fight.c:813-817, #97): a merciful PC
    // stays their hand once the victim is below 0 hp.
    {
        let mercy = g
            .get_char(ch)
            .map(|c| !c.is_npc && c.prf2_flags & PRF2_MERCY != 0)
            .unwrap_or(false);
        let v_below = g.get_char(victim).map(|c| c.points.hit < 0).unwrap_or(false);
        if mercy && v_below && !deathblow && attacktype < SELF_DAMAGE {
            act(
                g,
                "$N is almost dead, and you decide to have mercy on them.",
                false,
                ch,
                None,
                ActArg::Char(victim),
                To::Char,
            );
            stop_fighting(g, ch);
            return;
        }
    }

    if let Some(msg) = newbie_pvp_block_message(g, ch, victim) {
        g.send_to_char(ch, msg);
        return;
    }

    if g.get_char(ch).and_then(|c| c.in_room) != g.get_char(victim).and_then(|c| c.in_room) {
        let log = format!(
            "DEBUG: Timing Bug Trigger - {} (ch) and {} (victim) are not in same room.",
            g.get_char(ch)
                .map(|c| c.get_name().to_string())
                .unwrap_or_default(),
            g.get_char(victim)
                .map(|c| c.get_name().to_string())
                .unwrap_or_default(),
        );
        mudlog(g, &log, LVL_GRGOD);
        return;
    }

    // Peaceful room: ch != victim and ch's room is PEACEFUL (imps excepted).
    if ch != victim {
        let ch_lvl = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
        let peaceful = g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.room(r).room_flags.contains(RoomFlags::PEACEFUL))
            .unwrap_or(false);
        if peaceful && ch_lvl < LVL_IMPL {
            g.send_to_char(
                ch,
                "This room just has such a peaceful, easy feeling...\r\n",
            );
            return;
        }
    }

    if !crate::shop::ok_damage_shopkeeper(g, ch, victim) {
        return;
    }

    // You can't damage an immortal victim, or (as a mortal/NPC) an intangible
    // victim -> the blow lands for 0 damage (fight.c ~854).
    let v_imm = g
        .get_char(victim)
        .map(|c| !c.is_npc && c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    let ch_mortal_or_npc = g
        .get_char(ch)
        .map(|c| c.is_npc || c.player.level < LVL_IMMORT)
        .unwrap_or(true);
    let v_intangible = g
        .get_char(victim)
        .map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0)
        .unwrap_or(false);
    let mut dmg = if v_imm || (ch_mortal_or_npc && v_intangible) {
        0
    } else {
        dmg
    };

    // Intangibles (ghosts) can't fight: stop both and bail (fight.c ~857).
    let ch_ghost = g
        .get_char(ch)
        .map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0 && c.prf2_flags & PRF2_MBUILDING == 0)
        .unwrap_or(false);
    let v_ghost = g
        .get_char(victim)
        .map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0 && c.prf2_flags & PRF2_MBUILDING == 0)
        .unwrap_or(false);
    let ch_lvl = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    let v_lvl = g.get_char(victim).map(|c| c.player.level).unwrap_or(0);
    if (ch_ghost && v_lvl < LVL_IMMORT) || (v_ghost && ch_lvl < LVL_IMMORT) {
        stop_fighting(g, ch);
        stop_fighting(g, victim);
        return;
    }

    // Engagement: start both combatants fighting BEFORE the swing is resolved
    // (fight.c ~864-877). This is what makes the opening blow actually start a
    // fight even when the victim parries/dodges it.
    if victim != ch {
        let ch_can = g
            .get_char(ch)
            .map(|c| c.position > Position::Stunned)
            .unwrap_or(false)
            && g.get_char(ch).and_then(|c| c.fighting).is_none();
        if ch_can {
            set_fighting(g, ch, victim);
        }
        let v_can = g
            .get_char(victim)
            .map(|c| c.position > Position::Stunned)
            .unwrap_or(false)
            && g.get_char(victim).and_then(|c| c.fighting).is_none();
        if v_can {
            set_fighting(g, victim, ch);
            // A mob with MEMORY remembers a PC attacker (fight.c).
            let ch_is_pc = g.get_char(ch).map(|c| !c.is_npc).unwrap_or(false);
            let vic_is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
            if ch_is_pc && vic_is_npc {
                crate::mobact::remember(g, victim, ch);
            }
        }
    }
    if g.get_char(victim).and_then(|c| c.master) == Some(ch) {
        crate::cmd_movement::stop_follower(g, victim);
    }
    if g.get_char(ch)
        .map(|c| c.affect_flags & (AFF_INVISIBLE | AFF_HIDE) != 0)
        .unwrap_or(false)
    {
        crate::cmd_other::appear(g, ch);
    }

    if dmg >= 2
        && g.get_char(victim)
            .map(|c| c.affect_flags & AFF_SANCTUARY != 0)
            .unwrap_or(false)
    {
        dmg /= 2;
    }

    // PK flagging on a non-PK MUD (fight.c do_actual_damage ~892).
    if !PK_ALLOWED {
        check_killer(g, ch, victim);
    }

    // Damage multiplier. Spells (1..=MAX_SPELLS) use the magical flavour;
    // everything else (weapons / skills / undefined) uses the physical one.
    let mt = if attacktype > 0 && attacktype <= MAX_SPELLS {
        1
    } else {
        0
    };
    dmg = (dmg as f32 * dam_multi(g, ch, victim, mt)) as i32;

    // Victim defensive skills (fight.c do_actual_damage ~904): riposte / avoid /
    // parry / dodge. A successful defense consumes the swing AFTER engagement.
    if try_defensive_skills(g, ch, victim, attacktype) {
        return;
    }

    // Clamp to the per-round window and subtract hp.
    dmg = dmg.clamp(0, 1000);
    if let Some(v) = g.get_char_mut(victim) {
        v.points.hit -= dmg;
    }

    if attacktype < SELF_DAMAGE {
        adrenaline_rush(g, victim, dmg);
    }

    update_position(g, victim);

    // Damage / skill message (fight.c damage ~1011). A non-weapon attack type
    // (skill / spell) always uses skill_message; a weapon type uses dam_message,
    // except a death blow (non-mercy attacker, non-NPC) or a miss first tries
    // the weapon's skill_message and only falls back to dam_message if none
    // exists. Invalid (TYPE_UNDEFINED), out-of-range (>= SELF_DAMAGE) and
    // SKILL_RIPOSTE attack types print nothing here — except TYPE_UNDEFINED,
    // which keeps the port's generic-hit fallback for environment / dg damage.
    if attacktype != TYPE_UNDEFINED
        && attacktype < SELF_DAMAGE
        && attacktype != SKILL_RIPOSTE as i32
    {
        if attacktype < TYPE_HIT {
            // Non-weapon attack (skill / spell): always use skill_message.
            crate::fight_messages::skill_message(g, dmg, ch, victim, attacktype);
        } else {
            // Weapon type (TYPE_HIT..SELF_DAMAGE).
            let vict_dead = g
                .get_char(victim)
                .map(|c| c.position == Position::Dead)
                .unwrap_or(false);
            let ch_npc = g.get_char(ch).map(|c| c.is_npc).unwrap_or(true);
            let ch_mercy = g
                .get_char(ch)
                .map(|c| c.prf2_flags & PRF2_MERCY != 0)
                .unwrap_or(false);
            if (vict_dead && !deathblow && !ch_mercy && !ch_npc) || dmg == 0 {
                if !crate::fight_messages::skill_message(g, dmg, ch, victim, attacktype) {
                    dam_message(g, dmg, ch, victim, attacktype);
                }
            } else {
                dam_message(g, dmg, ch, victim, attacktype);
            }
        }
    } else if attacktype == TYPE_UNDEFINED {
        // Generic strike with no weapon flavour (environment / dg damage):
        // fall back to the plain TYPE_HIT verb so onlookers still see a hit.
        dam_message(g, dmg, ch, victim, TYPE_HIT);
    }

    // PRF2_MERCY spare (fight.c:1026-1039, #97): the killing blow stops, both
    // fighters disengage, the victim is stood up at -1 hp.
    {
        let mercy = g
            .get_char(ch)
            .map(|c| !c.is_npc && c.prf2_flags & PRF2_MERCY != 0)
            .unwrap_or(false);
        let v_below = g.get_char(victim).map(|c| c.points.hit < 0).unwrap_or(false);
        if mercy && v_below && !deathblow && attacktype < SELF_DAMAGE {
            stop_fighting(g, ch);
            stop_fighting(g, victim);
            let hr = crate::act::hshr(g, victim);
            let mut buf = format!(
                "You have mercy on $N, and spare {} life... for now.",
                hr
            );
            act(g, &buf, true, ch, None, ActArg::Char(victim), To::Char);
            buf.clear();
            act(
                g,
                "$n spares your life, thank the gods!",
                false,
                ch,
                None,
                ActArg::Char(victim),
                To::Vict,
            );
            act(
                g,
                "$N is about to deliver the death blow, but suddenly spares $n's life!",
                false,
                victim,
                None,
                ActArg::Char(ch),
                To::NotVict,
            );
            let _ = &mut buf;
            if let Some(v) = g.get_char_mut(victim) {
                v.points.hit = -1;
            }
            update_position(g, victim);
        }
    }

    send_position_feedback(g, ch, victim, dmg);
    trigger_pc_escape_thresholds(g, ch, victim);
    rescue_linkdead_victim(g, victim);

    let v_dead = g
        .get_char(victim)
        .map(|c| c.position == Position::Dead)
        .unwrap_or(false);
    if v_dead {
        // Arena fatalities settled in send_position_feedback's Dead arm
        // (fight.c: match_over then return) - do not run die().
        if !crate::arena::is_arena_combatant(victim) {
            die(g, Some(ch), victim);
        }
    }

    // Autoloot / autogold / autosplit (fight.c:1197-1242, #100): the three
    // quality-of-life prefs act right after die() creates the corpse; none
    // of them ran before, so corpses had to be looted by hand.
    let ch_pc = g.get_char(ch).map(|c| !c.is_npc).unwrap_or(false);
    let ch_killer = g
        .get_char(ch)
        .map(|c| c.act_flags & PLR_KILLER != 0)
        .unwrap_or(false);
    if ch_pc && !ch_killer && ch != victim {
        let (v_npc, ch_prf, ch_grouped) = match g.get_char(ch) {
            Some(c) => (
                g.get_char(victim).map(|v| v.is_npc).unwrap_or(false),
                c.prf2_flags,
                c.affect_flags & AFF_GROUP != 0,
            ),
            None => return,
        };
        const PRF_AUTOLOOT: i64 = 1 << 24;
        const PRF_AUTOSPLIT: i64 = 1 << 23;
        const PRF_AUTOGOLD: i64 = 1 << 25;
        let local_gold = if v_npc {
            g.get_char(victim).map(|v| v.points.gold).unwrap_or(0)
        } else {
            0
        };
        let local_buf = local_gold.to_string();
        let mut gold_before;
        let mut gold_after;
        if v_npc && g.get_char(ch).map(|c| c.prf2_flags & PRF_AUTOLOOT != 0).unwrap_or(false) {
            gold_before = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
            crate::cmd_item::do_get(g, ch, "all corpse", 0);
            gold_after = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
        } else {
            gold_before = 0;
            gold_after = 0;
        }
        if ch_grouped
            && local_gold > 0
            && ch_prf & PRF_AUTOSPLIT != 0
            && ch_prf & PRF_AUTOLOOT != 0
        {
            if gold_after > gold_before {
                crate::cmd_other::do_split(g, ch, &local_buf, 0);
            }
        }
        if v_npc && g.get_char(ch).map(|c| c.prf2_flags & PRF_AUTOGOLD != 0).unwrap_or(false) {
            gold_before = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
            crate::cmd_item::do_get(g, ch, "coins corpse", 0);
            gold_after = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
        }
        if ch_grouped
            && local_gold > 0
            && ch_prf & PRF_AUTOSPLIT != 0
            && ch_prf & PRF_AUTOGOLD != 0
        {
            if gold_after > gold_before {
                crate::cmd_other::do_split(g, ch, &local_buf, 0);
            }
        }
    }
}

fn send_position_feedback(g: &mut GameState, ch: CharId, victim: CharId, dmg: i32) {
    let (position, hit, max_hit, is_npc, mob_wimpy) = match g.get_char(victim) {
        Some(v) => (
            v.position,
            v.points.hit,
            v.points.max_hit,
            v.is_npc,
            v.is_npc && v.act_flags & MOB_WIMPY != 0,
        ),
        None => return,
    };
    match position {
        Position::MortallyWounded => {
            act(
                g,
                "$n is mortally wounded, and will die soon, if not aided.",
                true,
                victim,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                victim,
                "You are mortally wounded, and will die soon, if not aided.\r\n",
            );
        }
        Position::Incapacitated => {
            act(
                g,
                "$n is incapacitated and will slowly die, if not aided.",
                true,
                victim,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                victim,
                "You are incapacitated an will slowly die, if not aided.\r\n",
            );
        }
        Position::Stunned => {
            act(
                g,
                "$n is stunned, but will probably regain consciousness again.",
                true,
                victim,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                victim,
                "You're stunned, but will probably regain consciousness again.\r\n",
            );
        }
        Position::Dead => {
            // C fight.c:1056-1064: arena fatalities settle via match_over and
            // return - die() is never reached; everyone else gets R.I.P.
            // (die() itself prints nothing in C - the port printed both, #104).
            if crate::arena::is_arena_combatant(victim) {
                crate::arena::match_over(
                    g,
                    Some(ch),
                    Some(victim),
                    "(Fatality)",
                    true,
                );
            } else {
                act(
                    g,
                    "$n is dead!  R.I.P.",
                    false,
                    victim,
                    None,
                    ActArg::None,
                    To::Room,
                );
                g.send_to_char(victim, "You are dead!  Sorry...\r\n");
            }
        }
        _ => {
            if max_hit > 0 && dmg > max_hit / 4 {
                act(
                    g,
                    "That really did HURT!",
                    false,
                    victim,
                    None,
                    ActArg::None,
                    To::Char,
                );
            }
            if max_hit > 0 && hit < max_hit / 4 {
                g.send_to_char(
                    victim,
                    "&RYou wish that your wounds would stop BLEEDING so much!&n\r\n",
                );
                if is_npc && ch != victim && mob_wimpy {
                    do_flee(g, victim);
                }
            }
        }
    }
}

fn trigger_pc_escape_thresholds(g: &mut GameState, ch: CharId, victim: CharId) {
    let (is_npc, hit, retreat_level, recall_level, wimp_level) = match g.get_char(victim) {
        Some(v) => (
            v.is_npc,
            v.points.hit,
            v.retreat_level,
            v.recall_level,
            v.wimp_level,
        ),
        None => return,
    };
    if is_npc || victim == ch || hit <= 0 {
        return;
    }

    if retreat_level > 0 && hit < retreat_level {
        g.send_to_char(victim, "You wimp out, and attempt to retreat!\r\n");
        crate::cmd_other::do_recite(g, victim, "retreat");
    }
    if recall_level > 0 && hit < recall_level {
        g.send_to_char(victim, "You wimp out, and attempt to recall!\r\n");
        crate::cmd_other::do_recite(g, victim, "recall");
    }
    if wimp_level > 0 && hit < wimp_level {
        g.send_to_char(victim, "You wimp out, and attempt to flee!\r\n");
        do_flee(g, victim);
    }
}

fn rescue_linkdead_victim(g: &mut GameState, victim: CharId) {
    let should_try = g
        .get_char(victim)
        .map(|c| !c.is_npc && c.desc.is_none())
        .unwrap_or(false);
    if !should_try {
        return;
    }
    do_flee(g, victim);
    if g.get_char(victim).and_then(|c| c.fighting).is_some() {
        return;
    }
    let was_in = match g.get_char(victim).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    if g.rooms.is_empty() {
        return;
    }
    act(
        g,
        "$n is rescued by divine forces.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    if let Some(c) = g.get_char_mut(victim) {
        c.was_in_room = Some(was_in);
    }
    g.char_from_room(victim);
    g.char_to_room(victim, 0);
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
    let ch_thief = g
        .get_char(ch)
        .map(|c| c.act_flags & PLR_THIEF != 0)
        .unwrap_or(false);
    let log = format!(
        "PC Killer bit set on {} for initiating attack on {} at {}.",
        g.get_char(ch)
            .map(|c| c.get_name().to_string())
            .unwrap_or_default(),
        g.get_char(vict)
            .map(|c| c.get_name().to_string())
            .unwrap_or_default(),
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
            // C fight.c:204-207: positive HP leaves positions above STUNNED
            // alone but stands a STUNNED (or lower) character back up (#107).
            if c.position > Position::Stunned {
                return;
            }
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

fn newbie_pvp_block_message(g: &GameState, ch: CharId, victim: CharId) -> Option<&'static str> {
    if PK_ALLOWED
        || ch == victim
        || (crate::arena::is_arena_combatant(ch) && crate::arena::is_arena_combatant(victim))
    {
        return None;
    }
    let attacker = g.get_char(ch)?;
    let vict = g.get_char(victim)?;
    if attacker.is_npc || vict.is_npc {
        return None;
    }
    if vict.player.level < PK_VICTIM_MIN && vict.act_flags & PLR_THIEF == 0 {
        return Some("Ack! But he's a newbie!\r\n");
    }
    if attacker.player.level < PK_VICTIM_MIN {
        return Some("Wait till you're level 10 at least before becoming a PLAYER KILLER.\r\n");
    }
    None
}

/// death_cry (fight.c): wail into the dying char's room, then a generic cry into
/// every adjacent room reachable through an open exit. C targets each neighbour
/// by temporarily pointing the char's `in_room` at it — act()'s TO_ROOM audience
/// is the char's room, and because only the field (not the room people lists) is
/// moved, the dying char is not among the neighbour's occupants and hears nothing
/// extra there. Shared by every death path (combat, environment, weather, purge)
/// so the neighbouring-room cries are no longer dropped.
pub(crate) fn death_cry(g: &mut GameState, ch: CharId) {
    act(
        g,
        "Your blood freezes as you hear $n's death cry.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    let was_in = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    for door in 0..NUM_OF_DIRS {
        // CAN_GO: an exit exists, resolves to a real room, and is not closed.
        let to_vnum = g.room(was_in).exits[door]
            .as_ref()
            .filter(|e| e.exit_info & EX_CLOSED == 0)
            .map(|e| e.to_room);
        let neighbor = to_vnum.and_then(|v| g.real_room(v));
        if let Some(to_rnum) = neighbor {
            // Temporarily relocate the char (C: ch->in_room = neighbour) so
            // act()'s TO_ROOM lands in the adjacent room, then restore.
            if let Some(c) = g.get_char_mut(ch) {
                c.in_room = Some(to_rnum);
            }
            act(
                g,
                "Your blood freezes as you hear someone's death cry.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            if let Some(c) = g.get_char_mut(ch) {
                c.in_room = Some(was_in);
            }
        }
    }
}

/// raw_kill(ch, killer) (fight.c): direct death path used by immortal `kill`
/// and script/environment paths that should bypass combat guards, damage
/// scaling, mercy, PK side effects, and XP loss.
pub fn raw_kill(g: &mut GameState, victim: CharId, killer: Option<CharId>) {
    if !g.char_exists(victim) {
        return;
    }
    if let Some(k) = killer {
        stop_fighting(g, k);
    }
    stop_fighting(g, victim);

    if let Some(c) = g.get_char_mut(victim) {
        c.affected.clear();
    }
    g.affect_total(victim);

    let cry = match killer {
        Some(k) => crate::dg_triggers::death_mtrigger(g, victim, Some(k)),
        None => true,
    };
    if cry {
        death_cry(g, victim);
    }

    make_corpse_for_victim(g, victim);

    let is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
    if is_npc {
        crate::mobact::clear_memory(victim);
        crate::arena::forget_char(victim);
        g.extract_char(victim);
    } else {
        let level = g.get_char(victim).map(|c| c.player.level).unwrap_or(0);
        if level < 30 {
            respawn_pc(g, victim);
        } else {
            ghost_pc(g, victim);
        }
    }
}

/// Handle a death: messages, loot to a corpse, extract NPC / respawn PC.
/// C fight.c die(ch, killer): killer may be NULL (quit-while-dying, env
/// deaths) - XP award, quest credit and PK side effects then do not apply
/// (#310). pub(crate) so cmd_other's do_quit can route through it.
pub(crate) fn die(g: &mut GameState, killer: Option<CharId>, victim: CharId) {
    if let Some(k) = killer {
        stop_fighting(g, k);
    }
    stop_fighting(g, victim);

    // Arena fatalities are handled by the arena subsystem (concede/restore);
    // skip the normal corpse/respawn path if so.
    if killer
        .map(|k| crate::arena::arena_combat_death(g, k, victim))
        .unwrap_or(false)
    {
        return;
    }

    let is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
    // Award/log combat-kill side effects before die() clears victim flags.
    if let Some(k) = killer {
        if k != victim {
            award_kill_experience(g, k, victim);
            // C fight.c:1196-1198: a MOB_MEMORY killer forgets the victim
            // when the victim dies, so it does not re-aggro on return (#185).
            if g.get_char(k).map(|m| m.is_npc && m.act_flags & (1 << 11) != 0).unwrap_or(false) {
                crate::mobact::forget(g, k, victim);
            }
        }
        if is_npc {
            // Mark the kill against any active autoquest (fight.c PLR_QUESTOR).
            crate::quest::quest_on_kill(g, k, victim);
        }
        handle_pc_kill_side_effects(g, k, victim);
    }

    apply_death_penalty(g, victim);
    if !g.char_exists(victim) {
        return;
    }

    // DG death trigger fires before the corpse/extract (death_mtrigger). C
    // raw_kill suppresses the death cry when the trigger returns false.
    let cry = crate::dg_triggers::death_mtrigger(g, victim, killer);

    // C raw_kill/die() print no death lines - the R.I.P. pair belongs to
    // damage()'s position switch (send_position_feedback here) and the old
    // duplicate printed every death twice (#104).

    // death_cry (raw_kill): wail into this room + every open-exit neighbour.
    if cry {
        death_cry(g, victim);
    }

    make_corpse_for_victim(g, victim);

    if is_npc {
        crate::mobact::clear_memory(victim);
        crate::arena::forget_char(victim);
        g.extract_char(victim);
    } else {
        let level = g.get_char(victim).map(|c| c.player.level).unwrap_or(0);
        if level < 30 {
            respawn_pc(g, victim);
        } else {
            ghost_pc(g, victim);
        }
    }
}

pub(crate) fn make_corpse_for_victim(g: &mut GameState, victim: CharId) {
    let rnum = match g.get_char(victim).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    let name = g
        .get_char(victim)
        .map(|c| c.display_for_others())
        .unwrap_or_default();
    let corpse = make_corpse(g, &name, victim);

    let carried = g
        .get_char(victim)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    for oid in carried {
        g.obj_from_anywhere(oid);
        g.obj_to_obj(oid, corpse);
    }
    let worn: Vec<usize> = (0..NUM_WEARS)
        .filter(|&p| {
            g.get_char(victim)
                .map(|c| c.equipment[p].is_some())
                .unwrap_or(false)
        })
        .collect();
    for p in worn {
        if let Some(oid) = g.unequip_char(victim, p) {
            g.obj_to_obj(oid, corpse);
        }
    }

    let gold = g.get_char(victim).map(|c| c.points.gold).unwrap_or(0);
    if gold > 0 {
        let has_desc = g
            .get_char(victim)
            .map(|c| c.is_npc || c.desc.is_some())
            .unwrap_or(false);
        if has_desc {
            let money = create_money(g, gold);
            g.obj_to_obj(money, corpse);
        }
        if let Some(c) = g.get_char_mut(victim) {
            c.points.gold = 0;
        }
    }

    g.obj_to_room(corpse, rnum);
}

fn handle_pc_kill_side_effects(g: &mut GameState, killer: CharId, victim: CharId) {
    let Some(v) = g.get_char(victim) else {
        return;
    };
    if v.is_npc {
        return;
    }
    let victim_name = v.get_name().to_string();
    let victim_level = v.player.level;
    let victim_killer = v.act_flags & PLR_KILLER != 0;
    let room_name = v
        .in_room
        .map(|r| g.room(r).name.clone())
        .unwrap_or_else(|| "Nowhere".to_string());
    let Some(k) = g.get_char(killer) else {
        return;
    };
    let killer_name = k.get_name().to_string();
    let killer_level = k.player.level;
    let killer_npc = k.is_npc;
    let killer_flagged = !k.is_npc && k.act_flags & PLR_KILLER != 0;

    mudlog(
        g,
        &format!("{victim_name} killed by {killer_name} at {room_name}"),
        LVL_IMMORT,
    );

    if killer == victim {
        return;
    }

    if killer_flagged {
        g.send_to_all_players(&format!(
            "&m[&YINFO&m]&n {victim_name} was killed by {killer_name} (jailed).\r\n"
        ));
        g.send_to_char(killer, "Oh now you've really gone and done it!\r\n");
        let diff = (killer_level as i32 - victim_level as i32).abs().max(1);
        if let Some(c) = g.get_char_mut(killer) {
            c.prf_flags &= !PRF_SUMMONABLE;
            c.prf_flags |= PRF_NOAUCT;
            c.alignment = -1000;
            c.bail_amt = diff * BAIL_MULTIPLIER;
        }
        if let Some(jail) = g.real_room(g.config.jail_num) {
            g.char_from_room(killer);
            g.char_to_room(killer, jail);
            act(
                g,
                "$n suddenly appears in the room.",
                true,
                killer,
                None,
                ActArg::None,
                To::Room,
            );
            crate::cmd_informative::look_at_room(g, killer, false);
        }
    } else if victim_killer && !killer_npc {
        g.send_to_all_players(&format!(
            "&m[&YINFO&m]&n {victim_name} was killed by {killer_name} (defending).\r\n"
        ));
    } else if !killer_npc {
        g.send_to_all_players(&format!(
            "&m[&YINFO&m]&n {victim_name} was killed by {killer_name} (offending).\r\n"
        ));
    }
}

fn apply_death_penalty(g: &mut GameState, victim: CharId) {
    if let Some(rnum) = g.get_char(victim).and_then(|c| c.in_room) {
        crate::maputils::increase_blood(g, rnum);
        crate::maputils::increase_snow(g, rnum);
        crate::maputils::increase_snow(g, rnum);
    }

    let (exp, level, is_npc) = match g.get_char(victim) {
        Some(c) => (c.points.exp, c.player.level as i32, c.is_npc),
        None => return,
    };
    let penalty = -((exp - crate::limits::exp_to_level(level - 1)) / 4);
    crate::limits::gain_exp(g, victim, penalty);
    if !g.char_exists(victim) || is_npc {
        return;
    }
    if let Some(c) = g.get_char_mut(victim) {
        c.act_flags &= !(PLR_KILLER | PLR_THIEF);
        c.conditions[FULL] = 0;
        c.conditions[THIRST] = 0;
        c.conditions[DRUNK] = 0;
    }
}

pub(crate) fn award_kill_experience(g: &mut GameState, killer: CharId, victim: CharId) {
    let Some(k) = g.get_char(killer) else {
        return;
    };
    if k.is_npc {
        return;
    }
    // C fight.c:1121: the award fires only when the victim is an NPC or a
    // connected player - killing a link-dead PC awards nothing (#114).
    let awardable = g
        .get_char(victim)
        .map(|v| v.is_npc || v.desc.is_some())
        .unwrap_or(false);
    if !awardable {
        return;
    }
    if k.affect_flags & AFF_GROUP != 0 {
        group_gain(g, killer, victim);
    } else {
        solo_gain(g, killer, victim);
    }
}

fn solo_gain(g: &mut GameState, ch: CharId, victim: CharId) {
    let exp = kill_exp(g, victim, 1);
    if exp > 1 {
        g.send_to_char(
            ch,
            &format!("You receive {} experience points.\r\n", numdisplay(exp)),
        );
    } else {
        g.send_to_char(ch, "You receive one lousy experience point.\r\n");
    }
    crate::limits::gain_exp(g, ch, exp);
    change_alignment(g, ch, victim);
}

fn group_gain(g: &mut GameState, ch: CharId, victim: CharId) {
    let Some(c) = g.get_char(ch) else {
        return;
    };
    if c.is_npc {
        return;
    }
    let leader = c.master.unwrap_or(ch);
    let ch_room = c.in_room;
    let mut members = Vec::new();
    if g.get_char(leader)
        .map(|k| k.affect_flags & AFF_GROUP != 0 && k.in_room == ch_room)
        .unwrap_or(false)
    {
        members.push(leader);
    }
    let followers = g
        .get_char(leader)
        .map(|k| k.followers.clone())
        .unwrap_or_default();
    for f in followers {
        if g.get_char(f)
            .map(|fc| fc.affect_flags & AFF_GROUP != 0 && fc.in_room == ch_room)
            .unwrap_or(false)
        {
            members.push(f);
        }
    }
    let member_count = members.len().max(1);
    let exp = kill_exp(g, victim, member_count);
    for member in members {
        perform_group_gain(g, member, exp, victim);
    }
}

fn perform_group_gain(g: &mut GameState, ch: CharId, exp: i64, victim: CharId) {
    if exp > 1 {
        g.send_to_char(
            ch,
            &format!(
                "You receive your share of experience -- {} points.\r\n",
                numdisplay(exp)
            ),
        );
    } else {
        g.send_to_char(
            ch,
            "You receive your share of experience -- one measly little point!\r\n",
        );
    }
    crate::limits::gain_exp(g, ch, exp);
    change_alignment(g, ch, victim);
}

fn kill_exp(g: &GameState, victim: CharId, member_count: usize) -> i64 {
    let Some(v) = g.get_char(victim) else {
        return 1;
    };
    if !v.is_npc {
        return 1;
    }
    const MAX_EXP_GAIN: i64 = 1_000_000_000; // config.c:116
    if member_count <= 1 {
        ((v.points.exp as f64) * 0.666) as i64
    } else {
        v.points.exp / member_count as i64
    }
    .min(MAX_EXP_GAIN)
    .max(1)
}

fn change_alignment(g: &mut GameState, ch: CharId, victim: CharId) {
    let victim_alignment = g.get_char(victim).map(|v| v.alignment).unwrap_or(0);
    if let Some(c) = g.get_char_mut(ch) {
        if c.prf2_flags & PRF2_INTANGIBLE != 0 {
            return;
        }
        c.alignment += (-victim_alignment - c.alignment) >> 4;
        if c.alignment >= 350 {
            c.act_flags &= !(PLR_THIEF | PLR_KILLER);
        }
    }
}

pub(crate) fn numdisplay(val: i64) -> String {
    let negative = val < 0;
    let digits = val.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let mut out: String = out.chars().rev().collect();
    if negative {
        out.insert(0, '-');
    }
    out
}


/// C fight.c:296-318 corpse metadata: keywords are just "corpse"-suffixed
/// GET_NAME, wear TAKE, extra ITEM_NODONATE, rent 100000, and the corpse
/// weighs the dead character plus their load (so get/drop/carry math works).
/// Applied by every corpse creator (#112). NODONATE is item-extra bit 9
/// (structs.h ITEM_NODONATE).
pub fn apply_corpse_metadata(obj: &mut crate::object::Object, g: &GameState, victim: CharId) {
    use crate::object::{ExtraFlags, WearFlags};
    obj.wear_flags |= WearFlags::TAKE;
    obj.extra_flags |= ExtraFlags::from_bits_retain(1 << 3); // ITEM_NODONATE
    obj.rent = 100000;
    if let Some(c) = g.get_char(victim) {
        let carried_w: i32 = c
            .carrying
            .iter()
            .filter_map(|oid| g.get_obj(*oid).map(|o| o.weight))
            .sum();
        let worn_w: i32 = c
            .equipment
            .iter()
            .flatten()
            .filter_map(|oid| g.get_obj(*oid).map(|o| o.weight))
            .sum();
        obj.weight = c.player.weight as i32 + carried_w + worn_w;
    }
}

fn make_corpse(g: &mut GameState, who: &str, victim: CharId) -> ObjId {
    let mut obj = Object::new(
        NOTHING,
        format!("corpse {}", who),
        format!("the corpse of {}", who),
    );
    obj.description = format!("The corpse of {} is lying here.", who);
    obj.obj_type = ObjectType::Container;
    apply_corpse_metadata(&mut obj, g, victim);
    // C fight.c:315-318: GET_OBJ_TIMER(corpse) = IS_NPC(ch) ?
// max_npc_corpse_time (5) : max_pc_corpse_time (10) (config.c:120-121),
// decremented once per mud hour by point_update. The flat 60 made
// corpses persist 6-12x longer than C (#102).
    obj.timer = if g.get_char(victim).map(|c| c.is_npc).unwrap_or(true) {
        5
    } else {
        10
    };
    // values[3]=1 marks this as a corpse so limits::point_update decays it.
    obj.values = [0, 0, 0, 1];
    obj.loc = ObjLoc::Nowhere;
    g.create_obj(obj)
}

/// money_desc(amount) (handler.c): the short-description bucket for a pile of
/// gold coins, by amount. Transcribed term-for-term.
fn money_desc(amount: i32) -> &'static str {
    if amount == 1 {
        "a gold coin"
    } else if amount <= 10 {
        "a tiny pile of gold coins"
    } else if amount <= 20 {
        "a handful of gold coins"
    } else if amount <= 75 {
        "a little pile of gold coins"
    } else if amount <= 200 {
        "a small pile of gold coins"
    } else if amount <= 1000 {
        "a pile of gold coins"
    } else if amount <= 5000 {
        "a big pile of gold coins"
    } else if amount <= 10000 {
        "a large heap of gold coins"
    } else if amount <= 20000 {
        "a huge mound of gold coins"
    } else if amount <= 75000 {
        "an enormous mound of gold coins"
    } else if amount <= 150000 {
        "a small mountain of gold coins"
    } else if amount <= 250000 {
        "a mountain of gold coins"
    } else if amount <= 500000 {
        "a huge mountain of gold coins"
    } else if amount <= 1000000 {
        "an enormous mountain of gold coins"
    } else {
        "an absolutely colossal mountain of gold coins"
    }
}

/// create_money(amount) (handler.c): mint an ITEM_MONEY object worth `amount`
/// gold coins, with value[0] = amount and matching name/short/long descriptions.
pub(crate) fn create_money(g: &mut GameState, amount: i32) -> ObjId {
    let amount = amount.max(1);
    let (name, short_desc, long_desc) = if amount == 1 {
        (
            "coin gold".to_string(),
            "a gold coin".to_string(),
            "One miserable gold coin is lying here.".to_string(),
        )
    } else {
        let md = money_desc(amount);
        // CAP() the long description's first character.
        let mut long = format!("{} is lying here.", md);
        if let Some(first) = long.get(0..1) {
            long = format!("{}{}", first.to_uppercase(), &long[1..]);
        }
        ("coins gold".to_string(), md.to_string(), long)
    };
    let mut obj = Object::new(NOTHING, name, short_desc);
    obj.description = long_desc;
    obj.obj_type = ObjectType::Money;
    // Object::new() already sets WearFlags::TAKE (ITEM_WEAR_TAKE).
    obj.values = [amount, 0, 0, 0];
    obj.cost = amount;
    obj.loc = ObjLoc::Nowhere;
    // handler.c:1386-1432: the ex_description 'look coin(s)' shows a
    // progressively vaguer count the larger the pile (#112).
    let kw = if amount == 1 { "coin gold" } else { "coins gold" };
    let ex = if amount == 1 {
        "It's just one miserable little gold coin.".to_string()
    } else if amount < 10 {
        format!("There are {} coins.", amount)
    } else if amount < 100 {
        format!("There are about {} coins.", 10 * (amount / 10))
    } else if amount < 1000 {
        format!("It looks to be about {} coins.", 100 * (amount / 100))
    } else if amount < 100000 {
        format!(
            "You guess there are, maybe, {} coins.",
            1000 * ((amount / 1000) + g.rng.number(0, amount / 1000))
        )
    } else {
        "There are a LOT of coins.".to_string()
    };
    obj.ex_descriptions.push((kw.to_string(), ex));
    g.create_obj(obj)
}

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
    g.send_to_char(
        victim,
        "\r\nYou feel your spirit drawn back to a familiar place...\r\n",
    );
    crate::cmd_informative::look_at_room(g, victim, false);
}

fn ghost_pc(g: &mut GameState, victim: CharId) {
    // C handler.c:1129-1132 (extract_char type==1 ghost path): save_char
    // then Crash_delete_crashfile, so a ghost does not keep a stale rent
    // file that would resurrect old gear on next login (#115).
    if let Some(ch) = g.get_char(victim) {
        if !ch.is_npc {
            g.request_player_save(victim);
            crate::objsave::crash_delete_crashfile(g, victim);
        }
    }
    g.char_from_room(victim);
    let rnum = g.real_room(99).unwrap_or(0);
    if let Some(c) = g.get_char_mut(victim) {
        c.prf2_flags |= PRF2_INTANGIBLE;
        c.position = Position::Standing;
        c.points.hit = 1;
        c.points.mana = 1;
        c.points.move_points = 1;
        c.death_timer = 96;
        c.fighting = None;
    }
    g.char_to_room(victim, rnum);
    g.send_to_char(
        victim,
        "You suddenly find yourself floating in space... you feel nothing.\r\n",
    );
    act(
        g,
        "$n slowly materializes before you...\r\n",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
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

/// flee: combat-internal wrapper around the command implementation, so wimpy
/// mobs/players and link-dead rescue use the same C-fidelity path as typed flee.
pub fn do_flee(g: &mut GameState, ch: CharId) {
    crate::cmd_offensive::do_flee(g, ch, "", 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{Affect, Character};
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::dg_handler::{
        self, add_trigger, install_trig, ScriptKey, TrigData, DG_TEST_LOCK, MOB_TRIGGER,
        MTRIG_FIGHT, MTRIG_HITPRCNT, OBJ_TRIGGER, OTRIG_FIGHT,
    };
    use crate::flags::{AFF_CHARM, AFF_GROUP, MOB_DBLATTACK, MOB_SPEC};
    use crate::room::{Exit, Room};
    use std::collections::HashMap;

    const TEST_W_BODY: usize = 5;

    fn player(g: &mut GameState, name: &str) -> CharId {
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.player.level = PK_VICTIM_MIN;
        g.create_char(ch)
    }

    fn connected_player(g: &mut GameState, name: &str, conn: ConnId) -> CharId {
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.player.level = PK_VICTIM_MIN;
        g.create_char(ch)
    }

    fn durable_item(g: &mut GameState, wearer: CharId, curr_slots: i32, total_slots: i32) -> ObjId {
        let mut obj = Object::new(100, "armor".to_string(), "A test breastplate".to_string());
        obj.curr_slots = curr_slots;
        obj.total_slots = total_slots;
        let oid = g.create_obj(obj);
        g.equip_char(wearer, oid, TEST_W_BODY);
        oid
    }

    fn scroll(g: &mut GameState, owner: CharId, keyword: &str, spellnum: i32) -> ObjId {
        let mut obj = Object::new(200, keyword.to_string(), format!("a {keyword} scroll"));
        obj.obj_type = ObjectType::Scroll;
        obj.values = [1, spellnum, -1, -1];
        let oid = g.create_obj(obj);
        g.obj_to_char(oid, owner);
        oid
    }

    #[test]
    fn raw_kill_respawns_pc_without_death_penalty() {
        let mut g = GameState::new(Config::default());
        let start = g.add_room(Room::new(
            3001,
            0,
            "Start".to_string(),
            "Start.".to_string(),
        ));
        let pit = g.add_room(Room::new(3002, 0, "Pit".to_string(), "Pit.".to_string()));
        let killer = player(&mut g, "Killer");
        let victim = player(&mut g, "Victim");
        {
            let v = g.get_char_mut(victim).unwrap();
            v.player.level = 20;
            v.player.hometown = 3001;
            v.points.exp = 12_345;
            v.points.hit = -5;
            v.position = Position::Dead;
        }
        g.char_to_room(killer, pit);
        g.char_to_room(victim, pit);

        raw_kill(&mut g, victim, Some(killer));

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.points.exp, 12_345);
        assert_eq!(v.points.hit, 1);
        assert_eq!(v.position, Position::Resting);
        assert_eq!(v.in_room, Some(start));
    }

    #[test]
    fn deathblow_uses_dedicated_lethal_damage_path() {
        let mut g = GameState::new(Config::default());
        let start = g.add_room(Room::new(
            3001,
            0,
            "Start".to_string(),
            "Start.".to_string(),
        ));
        let killer = player(&mut g, "Killer");
        let victim = player(&mut g, "Victim");
        {
            let v = g.get_char_mut(victim).unwrap();
            v.player.level = 20;
            v.player.hometown = 3001;
            v.points.hit = 1;
            v.points.max_hit = 1;
        }
        g.char_to_room(killer, start);
        g.char_to_room(victim, start);

        deathblow(&mut g, killer, victim, 100, TYPE_HIT);

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.points.hit, 1);
        assert_eq!(v.position, Position::Resting);
        assert_eq!(v.in_room, Some(start));
    }

    fn make_dg_trigger(
        attach_type: i32,
        trigger_type: i64,
        narg: i32,
        cmds: &[&str],
    ) -> crate::dg_handler::TrigId {
        install_trig(TrigData {
            nr: 0,
            vnum: 9999,
            attach_type,
            name: "combat test".to_string(),
            trigger_type,
            narg,
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

    #[test]
    fn sanctuary_halves_combat_damage_of_two_or_more() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Victim");
        g.get_char_mut(victim).unwrap().affect_flags |= AFF_SANCTUARY;

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().points.hit, 15);
    }

    #[test]
    fn set_fighting_removes_sleep_affect() {
        let mut g = GameState::new(Config::default());
        let ch = player(&mut g, "Sleeper");
        let victim = player(&mut g, "Victim");
        {
            let c = g.get_char_mut(ch).unwrap();
            c.position = Position::Sleeping;
            c.affect_flags |= AFF_SLEEP;
            c.affected.push(Affect {
                spell_type: SPELL_SLEEP,
                duration: 5,
                modifier: 0,
                location: 0,
                bitvector: AFF_SLEEP,
                caster: None,
            });
        }

        set_fighting(&mut g, ch, victim);

        let c = g.get_char(ch).unwrap();
        assert_eq!(c.fighting, Some(victim));
        assert_eq!(c.position, Position::Fighting);
        assert_eq!(c.affect_flags & AFF_SLEEP, 0);
        assert!(c.affected.iter().all(|a| a.spell_type != SPELL_SLEEP));
    }

    #[test]
    fn hit_type_releases_redirected_charge_after_hit() {
        let mut g = GameState::new(Config::default());
        let ch = player(&mut g, "Charged");
        let victim = player(&mut g, "Victim");
        {
            let c = g.get_char_mut(ch).unwrap();
            c.affect_flags |= AFF_R_CHARGED;
            c.affected.push(Affect {
                spell_type: SPELL_REDIRECT_CHARGE,
                duration: 100,
                modifier: 40,
                location: 22,
                bitvector: AFF_R_CHARGED,
                caster: None,
            });
        }
        {
            let v = g.get_char_mut(victim).unwrap();
            v.position = Position::Sleeping;
            v.points.hit = 100;
            v.points.max_hit = 100;
        }

        hit_type(&mut g, ch, victim, TYPE_UNDEFINED);

        let attacker = g.get_char(ch).unwrap();
        assert_eq!(attacker.affect_flags & AFF_R_CHARGED, 0);
        assert!(attacker
            .affected
            .iter()
            .all(|af| af.spell_type != SPELL_REDIRECT_CHARGE));
        assert!(g.get_char(victim).unwrap().points.hit <= 59);
    }

    #[test]
    fn sanctuary_does_not_reduce_one_point_damage() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Victim");
        g.get_char_mut(victim).unwrap().affect_flags |= AFF_SANCTUARY;

        damage_type(&mut g, attacker, victim, 1, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().points.hit, 19);
    }

    #[test]
    fn sanctuary_halves_before_damage_multiplier() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Victim");
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.points.power = 200;
        }
        g.get_char_mut(victim).unwrap().affect_flags |= AFF_SANCTUARY;

        damage_type(&mut g, attacker, victim, 9, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().points.hit, 15);
    }

    #[test]
    fn damage_blocks_pc_attacking_newbie_victim() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let attacker = connected_player(&mut g, "Attacker", ConnId(1));
        let victim = connected_player(&mut g, "Victim", ConnId(2));
        g.get_char_mut(victim).unwrap().player.level = PK_VICTIM_MIN - 1;
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().points.hit, 20);
        assert_eq!(g.get_char(attacker).unwrap().fighting, None);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("Ack! But he's a newbie!\r\n"));
    }

    #[test]
    fn damage_blocks_underlevel_pc_attacker() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let attacker = connected_player(&mut g, "Attacker", ConnId(1));
        let victim = connected_player(&mut g, "Victim", ConnId(2));
        g.get_char_mut(attacker).unwrap().player.level = PK_VICTIM_MIN - 1;
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().points.hit, 20);
        assert_eq!(g.get_char(attacker).unwrap().fighting, None);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("Wait till you're level 10 at least before becoming a PLAYER KILLER.\r\n"));
    }

    #[test]
    fn damage_triggers_adrenaline_power_affect() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 100;
            v.set_skill(SKILL_ADRENALINE as u16, 70);
        }

        damage_type(&mut g, attacker, victim, 10, TYPE_HIT);

        let v = g.get_char(victim).unwrap();
        let af = v
            .affected
            .iter()
            .find(|a| a.spell_type == SKILL_ADRENALINE)
            .unwrap();
        assert_eq!(af.location, APPLY_POWER);
        assert_eq!(af.modifier, 2);
        assert_eq!(af.duration, 2);
        assert_eq!(v.points.power, 2);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You feel a surge of &RADRENALINE&n!\r\n"));
    }

    #[test]
    fn damage_triggers_bloodlust_variant() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 100;
            v.set_skill(SKILL_ADRENALINE as u16, 70);
            v.set_skill(SKILL_BLOODLUST as u16, 71);
        }

        damage_type(&mut g, attacker, victim, 10, TYPE_HIT);

        assert_eq!(g.get_char(victim).unwrap().points.power, 2);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You &rlust&n for more &RBLOOD&n!!\r\n"));
    }

    #[test]
    fn carnal_rage_adrenaline_merge_uses_higher_cap() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 100;
            v.set_skill(SKILL_ADRENALINE as u16, 70);
            v.set_skill(SKILL_BLOODLUST as u16, 71);
            v.set_skill(SKILL_CARNALRAGE as u16, 71);
            v.affected.push(Affect {
                spell_type: SKILL_ADRENALINE,
                duration: 198,
                modifier: 198,
                location: APPLY_POWER,
                bitvector: 0,
                caster: None,
            });
        }
        g.affect_total(victim);

        damage_type(&mut g, attacker, victim, 10, TYPE_HIT);

        let v = g.get_char(victim).unwrap();
        let af = v
            .affected
            .iter()
            .find(|a| a.spell_type == SKILL_ADRENALINE)
            .unwrap();
        assert_eq!(af.duration, 199);
        assert_eq!(af.modifier, 200);
        assert_eq!(v.points.power, 200);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You feel the &RCarnal &rRage&n build within you!!!\r\n"));
    }

    #[test]
    fn combat_durability_decrements_worn_pc_equipment() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        let armor = durable_item(&mut g, victim, 3, 3);
        g.get_char_mut(attacker).unwrap().fighting = Some(victim);
        g.get_char_mut(victim).unwrap().aff_abils.dex = -100;

        damage_worn_equipment_after_hit(&mut g, attacker);

        assert_eq!(g.get_obj(armor).unwrap().curr_slots, 2);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("A test breastplate just got DAMAGED during the combat!\r\n"));
        assert_ne!(
            g.get_char(victim).unwrap().act_flags & crate::objsave::PLR_CRASH,
            0
        );
    }

    #[test]
    fn combat_durability_extracts_worn_equipment_at_zero_slots() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        let armor = durable_item(&mut g, victim, 1, 3);
        g.get_char_mut(attacker).unwrap().fighting = Some(victim);
        g.get_char_mut(victim).unwrap().aff_abils.dex = -100;

        damage_worn_equipment_after_hit(&mut g, attacker);

        assert!(g.get_obj(armor).is_none());
        assert_eq!(g.get_char(victim).unwrap().equipment[TEST_W_BODY], None);
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("A test breastplate just got DAMAGED during the combat!\r\n"));
        assert!(out.contains("A test breastplate crumbles to dust as it wears out!\r\n"));
    }

    #[test]
    fn combat_durability_skips_npc_victims_and_indestructible_items() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let npc = g.create_char(Character::new_npc(1));
        let indestructible_pc = player(&mut g, "Victim");
        let npc_armor = durable_item(&mut g, npc, 3, 3);
        let pc_armor = durable_item(&mut g, indestructible_pc, 3, 0);

        g.get_char_mut(attacker).unwrap().fighting = Some(npc);
        damage_worn_equipment_after_hit(&mut g, attacker);
        assert_eq!(g.get_obj(npc_armor).unwrap().curr_slots, 3);

        g.get_char_mut(attacker).unwrap().fighting = Some(indestructible_pc);
        g.get_char_mut(indestructible_pc).unwrap().aff_abils.dex = -100;
        damage_worn_equipment_after_hit(&mut g, attacker);
        assert_eq!(g.get_obj(pc_armor).unwrap().curr_slots, 3);
    }

    #[test]
    fn perform_violence_shows_victim_health_to_pc_attacker() {
        let mut g = GameState::new(Config::default());
        let attacker = connected_player(&mut g, "Attacker", ConnId(1));
        let victim = player(&mut g, "Victim");
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.fighting = Some(victim);
            a.position = Position::Fighting;
        }
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 80;
        }

        perform_violence(&mut g);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Victim has some small wounds and bruises.\r\n"));
    }

    #[test]
    fn perform_violence_respects_disp_mob_suppression() {
        let mut g = GameState::new(Config::default());
        let attacker = connected_player(&mut g, "Attacker", ConnId(1));
        let victim = player(&mut g, "Victim");
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.fighting = Some(victim);
            a.position = Position::Fighting;
            a.prf2_flags |= PRF2_DISPMOB;
        }
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 80;
        }

        perform_violence(&mut g);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(!out.contains("Victim has some small wounds and bruises.\r\n"));
    }

    #[test]
    fn perform_violence_calls_registered_mob_combat_spec() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let mut snake = Character::new_npc(3618);
        snake.player.name = "Snake".to_string();
        snake.player.level = 42;
        snake.points.hit = 100;
        snake.points.max_hit = 100;
        snake.position = Position::Fighting;
        snake.act_flags |= MOB_SPEC;
        let snake = g.create_char(snake);
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.hit = 100;
            v.points.max_hit = 100;
            v.fighting = Some(snake);
        }
        g.char_to_room(snake, room);
        g.char_to_room(victim, room);
        g.get_char_mut(snake).unwrap().fighting = Some(victim);

        perform_violence(&mut g);

        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("Snake bites you!\r\n"));
    }

    #[test]
    fn npc_mob_wait_counts_down_and_recovers_to_fighting() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let mut mob = Character::new_npc(99);
        mob.player.name = "Bruiser".to_string();
        mob.position = Position::Sitting;
        mob.points.hit = 100;
        mob.points.max_hit = 100;
        mob.points.power = 1000;
        mob.points.technique = 1000;
        let mob = g.create_char(mob);
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.hit = 100;
            v.points.max_hit = 100;
        }
        g.char_to_room(mob, room);
        g.char_to_room(victim, room);
        g.get_char_mut(mob).unwrap().fighting = Some(victim);
        g.set_wait_state(mob, PULSE_VIOLENCE as i32);

        perform_violence(&mut g);

        assert_eq!(g.get_char(mob).unwrap().mob_wait, 0);
        assert_eq!(g.get_char(mob).unwrap().position, Position::Fighting);
        assert_eq!(g.get_char(victim).unwrap().points.hit, 100);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("Bruiser scrambles to its feet!\r\n"));

        perform_violence(&mut g);

        assert!(g.get_char(victim).unwrap().points.hit < 100);
    }

    #[test]
    fn warrior_second_and_third_attack_add_melee_hits() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        // Place both fighters FIRST: char_to_room breaks combat links across
        // rooms (C handler.c:496-499, #111).
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.player.class = Class::Warrior;
            a.player.level = 100;
            a.points.power = 1000;
            a.points.technique = 1000;
            a.position = Position::Fighting;
            a.fighting = Some(victim);
            a.set_skill(SKILL_SECOND_ATTACK, 255);
            a.set_skill(SKILL_THIRD_ATTACK, 255);
        }
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.hit = 10_000;
            v.points.max_hit = 10_000;
        }

        perform_violence(&mut g);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.matches("\r\n").count() >= 3, "{out:?}");
    }

    #[test]
    fn mob_dblattack_adds_extra_melee_hit() {
        let mut g = GameState::new(Config::default());
        g.rng.srandom(5);
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let mut mob = Character::new_npc(99);
        mob.player.name = "Raider".to_string();
        mob.player.level = 100;
        mob.position = Position::Fighting;
        mob.points.power = 1000;
        mob.points.technique = 1000;
        mob.act_flags |= MOB_DBLATTACK;
        let mob = g.create_char(mob);
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.hit = 10_000;
            v.points.max_hit = 10_000;
        }
        g.char_to_room(mob, room);
        g.char_to_room(victim, room);
        g.get_char_mut(mob).unwrap().fighting = Some(victim);

        perform_violence(&mut g);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.matches("\r\n").count() >= 2, "{out:?}");
    }

    #[test]
    fn hit_fires_mob_fight_trigger() {
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let mut mob = Character::new_npc(99);
        mob.player.name = "Sentry".to_string();
        mob.position = Position::Fighting;
        mob.points.power = 1000;
        mob.points.technique = 1000;
        let mob = g.create_char(mob);
        let victim = player(&mut g, "Victim");
        g.char_to_room(mob, room);
        g.char_to_room(victim, room);
        g.get_char_mut(mob).unwrap().fighting = Some(victim);
        let trig = make_dg_trigger(
            MOB_TRIGGER,
            MTRIG_FIGHT,
            100,
            &["set fired yes", "global fired", "halt"],
        );
        add_trigger(ScriptKey::Mob(mob), trig, -1);

        hit(&mut g, mob, victim);

        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Mob(mob), "fired").as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn hit_fires_equipped_object_fight_trigger() {
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Victim");
        let obj = durable_item(&mut g, attacker, 1, 1);
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.position = Position::Fighting;
            a.fighting = Some(victim);
            a.points.power = 1000;
            a.points.technique = 1000;
        }
        let trig = make_dg_trigger(
            OBJ_TRIGGER,
            OTRIG_FIGHT,
            100,
            &["set fired yes", "global fired", "halt"],
        );
        add_trigger(ScriptKey::Obj(obj), trig, -1);

        hit(&mut g, attacker, victim);

        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Obj(obj), "fired").as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn hit_fires_victim_hitprcnt_trigger_after_damage() {
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let attacker = player(&mut g, "Attacker");
        let mut mob = Character::new_npc(99);
        mob.player.name = "Target".to_string();
        mob.position = Position::Fighting;
        mob.points.hit = 2;
        mob.points.max_hit = 100;
        let victim = g.create_char(mob);
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.position = Position::Fighting;
            a.fighting = Some(victim);
            a.points.power = 1000;
            a.points.technique = 1000;
        }
        g.get_char_mut(victim).unwrap().fighting = Some(attacker);
        let trig = make_dg_trigger(
            MOB_TRIGGER,
            MTRIG_HITPRCNT,
            50,
            &["set low yes", "global low", "halt"],
        );
        add_trigger(ScriptKey::Mob(victim), trig, -1);

        hit(&mut g, attacker, victim);

        assert_eq!(
            dg_handler::get_global_var(ScriptKey::Mob(victim), "low").as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn die_awards_solo_kill_scaled_experience_and_alignment() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let killer = connected_player(&mut g, "Killer", ConnId(1));
        let mut mob = Character::new_npc(99);
        mob.player.name = "Bandit".to_string();
        mob.points.exp = 300;
        mob.alignment = -1000;
        let victim = g.create_char(mob);
        g.char_to_room(killer, room);
        g.char_to_room(victim, room);

        die(&mut g, Some(killer), victim);

        let k = g.get_char(killer).unwrap();
        assert_eq!(k.points.exp, 199);
        assert_eq!(k.alignment, 62);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You receive 199 experience points.\r\n"));
    }

    #[test]
    fn die_shares_group_experience_with_same_room_members() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let leader = connected_player(&mut g, "Leader", ConnId(1));
        let follower = connected_player(&mut g, "Follower", ConnId(2));
        {
            let l = g.get_char_mut(leader).unwrap();
            l.affect_flags |= AFF_GROUP;
            l.followers.push(follower);
        }
        {
            let f = g.get_char_mut(follower).unwrap();
            f.affect_flags |= AFF_GROUP;
            f.master = Some(leader);
        }
        let mut mob = Character::new_npc(99);
        mob.player.name = "Ogre".to_string();
        mob.points.exp = 300;
        let victim = g.create_char(mob);
        g.char_to_room(leader, room);
        g.char_to_room(follower, room);
        g.char_to_room(victim, room);

        die(&mut g, Some(leader), victim);

        assert_eq!(g.get_char(leader).unwrap().points.exp, 150);
        assert_eq!(g.get_char(follower).unwrap().points.exp, 150);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You receive your share of experience -- 150 points.\r\n"));
        assert!(g
            .descriptors
            .get(&ConnId(2))
            .unwrap()
            .outbuf
            .contains("You receive your share of experience -- 150 points.\r\n"));
    }

    #[test]
    fn die_applies_pc_death_penalty_flags_conditions_and_blood() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        g.add_room(Room::new(
            3001,
            0,
            "Home".to_string(),
            "A hometown.".to_string(),
        ));
        let killer = player(&mut g, "Killer");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.player.level = 10;
            v.points.exp = crate::limits::exp_to_level(9) + 400;
            v.act_flags |= PLR_KILLER | PLR_THIEF;
            v.conditions[FULL] = 12;
            v.conditions[THIRST] = 12;
            v.conditions[DRUNK] = 12;
        }
        g.char_to_room(killer, room);
        g.char_to_room(victim, room);

        die(&mut g, Some(killer), victim);

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.points.exp, crate::limits::exp_to_level(9) + 300);
        assert_eq!(v.act_flags & (PLR_KILLER | PLR_THIEF), 0);
        assert_eq!(v.conditions[FULL], 0);
        assert_eq!(v.conditions[THIRST], 0);
        assert_eq!(v.conditions[DRUNK], 0);
        assert_eq!(g.room(room).blood, 1);
    }

    #[test]
    fn die_sends_high_level_pc_to_ghost_limbo() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let limbo = g.add_room(Room::new(
            99,
            0,
            "Limbo".to_string(),
            "The ghost room.".to_string(),
        ));
        let killer = player(&mut g, "Killer");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.player.level = 30;
            v.points.hit = -20;
            v.points.mana = 50;
            v.points.move_points = 50;
        }
        g.char_to_room(killer, room);
        g.char_to_room(victim, room);

        die(&mut g, Some(killer), victim);

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.in_room, Some(limbo));
        assert_eq!(v.prf2_flags & PRF2_INTANGIBLE, PRF2_INTANGIBLE);
        assert_eq!(v.points.hit, 1);
        assert_eq!(v.points.mana, 1);
        assert_eq!(v.points.move_points, 1);
        assert_eq!(v.death_timer, 96);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You suddenly find yourself floating in space... you feel nothing.\r\n"));
    }

    #[test]
    fn die_logs_pc_kill_and_jails_killer_flagged_attacker() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        let jail = g.add_room(Room::new(
            g.config.jail_num,
            0,
            "Jail".to_string(),
            "A jail cell.".to_string(),
        ));
        g.add_room(Room::new(
            3001,
            0,
            "Home".to_string(),
            "A hometown.".to_string(),
        ));
        let killer = connected_player(&mut g, "Killer", ConnId(1));
        let victim = connected_player(&mut g, "Victim", ConnId(2));
        let imm = connected_player(&mut g, "Imm", ConnId(3));
        g.players_by_name.insert("killer".to_string(), killer);
        g.players_by_name.insert("victim".to_string(), victim);
        g.players_by_name.insert("imm".to_string(), imm);
        {
            let k = g.get_char_mut(killer).unwrap();
            k.player.level = 20;
            k.act_flags |= PLR_KILLER;
            k.prf_flags |= PRF_SUMMONABLE;
        }
        {
            let v = g.get_char_mut(victim).unwrap();
            v.player.level = 10;
        }
        g.get_char_mut(imm).unwrap().player.level = LVL_IMMORT;
        g.char_to_room(killer, room);
        g.char_to_room(victim, room);
        g.char_to_room(imm, room);

        die(&mut g, Some(killer), victim);

        let k = g.get_char(killer).unwrap();
        assert_eq!(k.in_room, Some(jail));
        assert_eq!(k.bail_amt, 200_000);
        assert_eq!(k.prf_flags & PRF_SUMMONABLE, 0);
        assert_eq!(k.prf_flags & PRF_NOAUCT, PRF_NOAUCT);
        assert_eq!(k.alignment, -1000);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("Oh now you've really gone and done it!\r\n"));
        assert!(g
            .descriptors
            .get(&ConnId(2))
            .unwrap()
            .outbuf
            .contains("&m[&YINFO&m]&n Victim was killed by Killer (jailed).\r\n"));
        assert!(g
            .descriptors
            .get(&ConnId(3))
            .unwrap()
            .outbuf
            .contains("[ Victim killed by Killer at Pit ]\r\n"));
    }

    #[test]
    fn die_broadcasts_defending_pc_kill() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Pit".to_string(),
            "A fighting pit.".to_string(),
        ));
        g.add_room(Room::new(
            3001,
            0,
            "Home".to_string(),
            "A hometown.".to_string(),
        ));
        let killer = connected_player(&mut g, "Defender", ConnId(1));
        let victim = connected_player(&mut g, "Outlaw", ConnId(2));
        g.players_by_name.insert("defender".to_string(), killer);
        g.players_by_name.insert("outlaw".to_string(), victim);
        g.get_char_mut(victim).unwrap().act_flags |= PLR_KILLER;
        g.char_to_room(killer, room);
        g.char_to_room(victim, room);

        die(&mut g, Some(killer), victim);

        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("&m[&YINFO&m]&n Outlaw was killed by Defender (defending).\r\n"));
    }

    #[test]
    fn damage_breaks_victim_follower_bond_to_attacker() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Master");
        let victim = player(&mut g, "Pet");
        {
            let v = g.get_char_mut(victim).unwrap();
            v.master = Some(attacker);
            v.affect_flags |= AFF_CHARM | AFF_GROUP;
        }
        g.get_char_mut(attacker).unwrap().followers.push(victim);

        damage_type(&mut g, attacker, victim, 1, TYPE_UNDEFINED);

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.master, None);
        assert_eq!(v.affect_flags & (AFF_CHARM | AFF_GROUP), 0);
        assert!(!g.get_char(attacker).unwrap().followers.contains(&victim));
    }

    #[test]
    fn damage_makes_hidden_or_invisible_attacker_appear() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Victim");
        let observer = connected_player(&mut g, "Observer", ConnId(1));
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        g.char_to_room(observer, room);
        g.get_char_mut(attacker).unwrap().affect_flags |= AFF_HIDE | AFF_INVISIBLE;

        damage_type(&mut g, attacker, victim, 1, TYPE_UNDEFINED);

        assert_eq!(
            g.get_char(attacker).unwrap().affect_flags & (AFF_HIDE | AFF_INVISIBLE),
            0
        );
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("Attacker slowly fades into existence.\r\n"));
    }

    #[test]
    fn damage_reports_stunned_position_to_victim_and_room() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        let observer = connected_player(&mut g, "Observer", ConnId(2));
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        g.char_to_room(observer, room);

        damage_type(&mut g, attacker, victim, 21, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().position, Position::Stunned);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You're stunned, but will probably regain consciousness again.\r\n"));
        assert!(g
            .descriptors
            .get(&ConnId(2))
            .unwrap()
            .outbuf
            .contains("Victim is stunned, but will probably regain consciousness again.\r\n"));
    }

    #[test]
    fn damage_reports_hurt_and_bleeding_thresholds() {
        let mut g = GameState::new(Config::default());
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 100;
        }

        damage_type(&mut g, attacker, victim, 30, TYPE_UNDEFINED);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("That really did HURT!\r\n"));

        g.descriptors.get_mut(&ConnId(1)).unwrap().outbuf.clear();
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 30;
            v.position = Position::Standing;
        }
        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("&RYou wish that your wounds would stop BLEEDING so much!&n\r\n"));
    }

    #[test]
    fn bleeding_mob_with_wimpy_flag_attempts_to_flee() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let safe = g.add_room(Room::new(101, 0, "Safe".to_string(), "Safe.".to_string()));
        for dir in 0..NUM_OF_DIRS {
            g.rooms[room].exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: 101,
            });
        }
        let attacker = player(&mut g, "Attacker");
        let victim = g.create_char(Character::new_npc(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 30;
            v.act_flags |= MOB_WIMPY;
        }
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(safe));
        assert_eq!(g.get_char(victim).unwrap().fighting, None);
    }

    #[test]
    fn injured_pc_below_recall_level_without_scroll_attempts_recite() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 30;
            v.recall_level = 25;
        }
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(room));
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("You wimp out, and attempt to recall!\r\n"));
        assert!(out.contains("You don't seem to have a recall.\r\n"));
    }

    #[test]
    fn injured_pc_below_recall_level_recites_scroll() {
        let mut g = GameState::new(Config::default());
        let recall_room = g.add_room(Room::new(
            100,
            0,
            "Recall".to_string(),
            "Recall.".to_string(),
        ));
        let combat_room = g.add_room(Room::new(101, 0, "Arena".to_string(), "Arena.".to_string()));
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 30;
            v.recall_level = 25;
        }
        let scroll_id = scroll(
            &mut g,
            victim,
            "recall",
            crate::spell_parser::SPELL_WORD_OF_RECALL,
        );
        g.char_to_room(attacker, combat_room);
        g.char_to_room(victim, combat_room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(recall_room));
        assert!(g.get_obj(scroll_id).is_none());
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("You wimp out, and attempt to recall!\r\n"));
        assert!(out.contains("You recite a recall scroll which dissolves.\r\n"));
    }

    #[test]
    fn injured_pc_below_retreat_level_recites_scroll() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 30;
            v.retreat_level = 25;
        }
        let scroll_id = scroll(
            &mut g,
            victim,
            "retreat",
            crate::spell_parser::SPELL_WORD_OF_RETREAT,
        );
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert!(g.get_obj(scroll_id).is_none());
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("You wimp out, and attempt to retreat!\r\n"));
        assert!(out.contains("You recite a retreat scroll which dissolves.\r\n"));
        assert!(out.contains("You must rent somewhere before you can retreat!\r\n"));
    }

    #[test]
    fn injured_pc_below_wimp_level_attempts_to_flee() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let safe = g.add_room(Room::new(101, 0, "Safe".to_string(), "Safe.".to_string()));
        for dir in 0..NUM_OF_DIRS {
            g.rooms[room].exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: 101,
            });
        }
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let v = g.get_char_mut(victim).unwrap();
            v.points.max_hit = 100;
            v.points.hit = 30;
            v.wimp_level = 25;
        }
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        damage_type(&mut g, attacker, victim, 10, TYPE_UNDEFINED);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(safe));
        assert_eq!(g.get_char(victim).unwrap().fighting, None);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You wimp out, and attempt to flee!\r\n"));
    }

    #[test]
    fn flee_refuses_characters_below_fighting_position() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let safe = g.add_room(Room::new(101, 0, "Safe".to_string(), "Safe.".to_string()));
        g.rooms[room].exits[NORTH] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: NOTHING,
            to_room: 101,
        });
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        {
            let v = g.get_char_mut(victim).unwrap();
            v.position = Position::Stunned;
            v.fighting = Some(attacker);
        }

        do_flee(&mut g, victim);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(room));
        assert!(!g.rooms[safe].people.contains(&victim));
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You are in pretty bad shape, unable to flee!\r\n"));
    }

    #[test]
    fn flee_does_not_choose_death_room_exits() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let death = g.add_room(Room::new(101, 0, "Death".to_string(), "Death.".to_string()));
        g.rooms[death].room_flags |= RoomFlags::DEATH;
        for dir in 0..NUM_OF_DIRS {
            g.rooms[room].exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: 101,
            });
        }
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);
        {
            let v = g.get_char_mut(victim).unwrap();
            v.position = Position::Fighting;
            v.fighting = Some(attacker);
        }

        do_flee(&mut g, victim);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(room));
        assert!(!g.rooms[death].people.contains(&victim));
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("PANIC!  You couldn't escape!\r\n"));
    }

    #[test]
    fn successful_non_arena_pc_flee_loses_experience() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let safe = g.add_room(Room::new(101, 0, "Safe".to_string(), "Safe.".to_string()));
        for dir in 0..NUM_OF_DIRS {
            g.rooms[room].exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: 101,
            });
        }
        let attacker = player(&mut g, "Attacker");
        let victim = connected_player(&mut g, "Victim", ConnId(1));
        {
            let a = g.get_char_mut(attacker).unwrap();
            a.points.max_hit = 100;
            a.points.hit = 90;
            a.player.level = 10;
        }
        {
            let v = g.get_char_mut(victim).unwrap();
            v.position = Position::Fighting;
            v.fighting = Some(attacker);
            v.player.level = 15;
            v.points.exp = 1_000;
        }
        g.char_to_room(attacker, room);
        g.char_to_room(victim, room);

        do_flee(&mut g, victim);

        assert_eq!(g.get_char(victim).unwrap().in_room, Some(safe));
        assert_eq!(g.get_char(victim).unwrap().points.exp, 900);
        assert!(g
            .descriptors
            .get(&ConnId(1))
            .unwrap()
            .outbuf
            .contains("You lost 100 experience points for fleeing"));
    }

    #[test]
    fn link_dead_pc_successful_flee_is_rescued_to_room_zero() {
        let mut g = GameState::new(Config::default());
        let limbo = g.add_room(Room::new(0, 0, "Limbo".to_string(), "Limbo.".to_string()));
        let combat_room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let flee_room = g.add_room(Room::new(101, 0, "Safe".to_string(), "Safe.".to_string()));
        for dir in 0..NUM_OF_DIRS {
            g.rooms[combat_room].exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: 101,
            });
        }
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Linkdead");
        g.char_to_room(attacker, combat_room);
        g.char_to_room(victim, combat_room);
        g.get_char_mut(attacker).unwrap().fighting = Some(victim);
        g.get_char_mut(victim).unwrap().fighting = Some(attacker);

        damage_type(&mut g, attacker, victim, 1, TYPE_UNDEFINED);

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.fighting, None);
        assert_eq!(v.was_in_room, Some(flee_room));
        assert_eq!(v.in_room, Some(limbo));
        assert!(g.rooms[limbo].people.contains(&victim));
    }

    #[test]
    fn link_dead_pc_failed_flee_is_not_rescued() {
        let mut g = GameState::new(Config::default());
        let limbo = g.add_room(Room::new(0, 0, "Limbo".to_string(), "Limbo.".to_string()));
        let combat_room = g.add_room(Room::new(100, 0, "Arena".to_string(), "Arena.".to_string()));
        let attacker = player(&mut g, "Attacker");
        let victim = player(&mut g, "Linkdead");
        g.char_to_room(attacker, combat_room);
        g.char_to_room(victim, combat_room);
        g.get_char_mut(attacker).unwrap().fighting = Some(victim);
        g.get_char_mut(victim).unwrap().fighting = Some(attacker);

        damage_type(&mut g, attacker, victim, 1, TYPE_UNDEFINED);

        let v = g.get_char(victim).unwrap();
        assert_eq!(v.fighting, Some(attacker));
        assert_eq!(v.was_in_room, None);
        assert_eq!(v.in_room, Some(combat_room));
        assert!(!g.rooms[limbo].people.contains(&victim));
    }

    #[test]
    fn stop_fighting_updates_negative_hit_position() {
        let mut g = GameState::new(Config::default());
        let ch = player(&mut g, "Fighter");
        let victim = player(&mut g, "Victim");
        {
            let c = g.get_char_mut(ch).unwrap();
            c.fighting = Some(victim);
            c.position = Position::Fighting;
            c.points.hit = -4;
        }

        stop_fighting(&mut g, ch);

        let c = g.get_char(ch).unwrap();
        assert_eq!(c.fighting, None);
        assert_eq!(c.position, Position::Incapacitated);
    }
}
