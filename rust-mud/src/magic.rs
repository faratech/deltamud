// magic.rs — the spell-template routines (CircleMUD magic.c) plus call_magic,
// the central dispatch every spell invocation passes through. Full port of
// magic.c: mag_damage, mag_affects, mag_points, mag_unaffects, mag_alter_objs,
// mag_creations, mag_summons, mag_areas, mag_groups, mag_masses — and the
// call_magic that routes a spell number to its MAG_* routines / manual spell.
//
// The SPELL_*/MAG_*/TAR_* constants and the spell_info() table live in
// spell_parser.rs; the manual spells live in spells.rs. This module imports
// both.

use crate::act::{act, ActArg, To};
use crate::character::Affect;
use crate::combat;
use crate::flags::{
    APPLY_DEFENSE, APPLY_MDEFENSE, APPLY_NONE, APPLY_POWER, APPLY_STR, APPLY_TECHNIQUE,
};
use crate::object::ObjectType;
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;

use crate::spell_parser::*;
use crate::spells;

// ---------------------------------------------------------------------------
// AFF_* / MOB_* / ITEM_* flag values not in the shared flags.rs subset
// (structs.h). Transcribed here to match the C constants exactly.
// ---------------------------------------------------------------------------
const AFF_BLIND: i64 = 1 << 0;
const AFF_DETECT_ALIGN: i64 = 1 << 2;
const AFF_DETECT_INVIS: i64 = 1 << 3;
const AFF_DETECT_MAGIC: i64 = 1 << 4;
const AFF_SENSE_LIFE: i64 = 1 << 5;
const AFF_WATERWALK: i64 = 1 << 6;
const AFF_SANCTUARY: i64 = 1 << 7;
const AFF_GROUP: i64 = 1 << 8;
const AFF_CURSE: i64 = 1 << 9;
const AFF_INFRAVISION: i64 = 1 << 10;
const AFF_POISON: i64 = 1 << 11;
const AFF_SLEEP: i64 = 1 << 14;
const AFF_CONVERGENCE: i64 = 1 << 17;
const AFF_AUTUS: i64 = 1 << 20;
const AFF_CHARM: i64 = 1 << 21;
const AFF_NOPORTAL: i64 = 1 << 22;
const AFF_INVISIBLE: i64 = 1 << 1;
const AFF_REDIRECT_CHARGE: i64 = 1 << 25;
const AFF_R_CHARGED: i64 = 1 << 26;

const MOB_NOBLIND: i64 = 1 << 17;
const MOB_NOSLEEP: i64 = 1 << 15;

const ROOM_NOMAGIC: u32 = 1 << 7; // RoomFlags::NO_MAGIC

const APPLY_NONE_I: i32 = APPLY_NONE;

// ITEM_* extra-flag bits (structs.h).
const ITEM_INVISIBLE: u64 = 1 << 5;
const ITEM_NODROP: u64 = 1 << 7;
const ITEM_BLESS: u64 = 1 << 8;
const ITEM_NOINVIS: u64 = 1 << 4;

const NOEFFECT: &str = "Nothing seems to happen.\r\n";

const MAX_PLAYER_STAT: i32 = 18;
const MAX_SPELL_AFFECTS: usize = 5;

// ---------------------------------------------------------------------------
// Helpers (utils.h macros) used across the mag_* routines.
// ---------------------------------------------------------------------------
fn get_level(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(1)
}
fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}
fn is_affected(g: &GameState, ch: CharId, bit: i64) -> bool {
    g.get_char(ch).map(|c| c.affect_flags & bit != 0).unwrap_or(false)
}
fn mob_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch).map(|c| c.is_npc && c.act_flags & flag != 0).unwrap_or(false)
}
fn affected_by_spell(g: &GameState, ch: CharId, spell: i32) -> bool {
    g.get_char(ch).map(|c| c.affected.iter().any(|a| a.spell_type == spell)).unwrap_or(false)
}

/// mag_savingthrow(ch, victim) (magic.c): does the victim avoid the spell?
/// chance() (the DeltaMUD hit-chance helper) isn't ported; C uses
/// number(0,100) > chance(ch,victim,1). With no chance() we model the classic
/// CircleMUD coin-flip on magic resistance: a victim saves on a high roll.
fn mag_savingthrow(g: &mut GameState, _ch: CharId, _victim: CharId) -> bool {
    // chance(ch,victim,1) is unported; degrade to a 50/50 save like the
    // baseline DikuMUD saving throw (number(0,100) vs a 50 threshold).
    g.rng.number(0, 100) > 50
}

/// affect_join (handler.c): merge a new affect into the victim, with optional
/// duration/modifier accumulation, then recompute. The Rust affect list stores
/// each affect; replacement means removing the prior same-spell affects first.
fn affect_join(g: &mut GameState, victim: CharId, mut af: Affect, add_dur: bool, add_mod: bool) {
    let spell = af.spell_type;
    if let Some(c) = g.get_char_mut(victim) {
        // Find an existing affect of the same spell with the same location.
        if let Some(existing) = c.affected.iter().find(|a| a.spell_type == spell && a.location == af.location).cloned() {
            if add_dur {
                af.duration += existing.duration;
            }
            if add_mod {
                af.modifier += existing.modifier;
            }
            // Remove the matching prior affect (same spell + location).
            c.affected.retain(|a| !(a.spell_type == spell && a.location == af.location));
        }
        c.affected.push(af);
    }
    g.affect_total(victim);
}

/// affect_to_char (handler.c): unconditionally add the affect.
#[allow(dead_code)]
fn affect_to_char(g: &mut GameState, victim: CharId, af: Affect) {
    if let Some(c) = g.get_char_mut(victim) {
        c.affected.push(af);
    }
    g.affect_total(victim);
}

/// affect_from_char (handler.c): strip every affect of a given spell.
fn affect_from_char(g: &mut GameState, victim: CharId, spell: i32) {
    if let Some(c) = g.get_char_mut(victim) {
        c.affected.retain(|a| a.spell_type != spell);
    }
    g.affect_total(victim);
}

/// update_pos (fight.c): recompute a character's position from HP.
fn update_pos(g: &mut GameState, victim: CharId) {
    if let Some(c) = g.get_char_mut(victim) {
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

/// Compatibility shim for the existing `commands::do_cast` wrapper, which
/// calls `magic::do_cast(g, ch, arg)`. The real pipeline lives in
/// spell_parser::do_cast (contract owner). Forwarding here keeps the existing
/// interpreter dispatch (`commands::do_cast`) working without editing those
/// modules; the recommended rewire points the interpreter straight at
/// `spell_parser::do_cast`.
pub fn do_cast(g: &mut GameState, ch: CharId, arg: &str) {
    crate::spell_parser::do_cast(g, ch, arg, 0);
}

// ===========================================================================
// call_magic (spell_parser.c): the heart of the magic system.
// ===========================================================================
pub fn call_magic(
    g: &mut GameState,
    caster: CharId,
    cvict: Option<CharId>,
    ovict: Option<ObjId>,
    spellnum: i32,
    level: i32,
) -> i32 {
    if spellnum < 1 || spellnum > TOP_SPELL_DEFINE {
        return 0;
    }
    let si = spell_info(spellnum);

    let rnum = g.get_char(caster).and_then(|c| c.in_room);
    let caster_level = get_level(g, caster);

    // ROOM_NOMAGIC fizzle.
    if let Some(r) = rnum {
        let nomagic = g.room(r).room_flags.bits() & ROOM_NOMAGIC != 0;
        if nomagic && caster_level < LVL_IMPL as i32 {
            g.send_to_char(caster, "Your magic fizzles out and dies.\r\n");
            act(g, "$n's magic fizzles out and dies.", false, caster, None, ActArg::None, To::Room);
            return 0;
        }
        let peaceful = g.room(r).room_flags.contains(RoomFlags::PEACEFUL);
        if caster_level < LVL_IMPL as i32 && peaceful && (si.violent || si.routines & MAG_DAMAGE != 0) {
            g.send_to_char(caster, "A flash of white light fills the room, dispelling your violent magic!\r\n");
            act(g, "White light from no particular source suddenly fills the room, then vanishes.", false, caster, None, ActArg::None, To::Room);
            return 0;
        }
    }

    if si.routines & MAG_DAMAGE != 0 {
        mag_damage(g, level, caster, cvict, spellnum);
    }
    if si.routines & MAG_AFFECTS != 0 {
        mag_affects(g, level, caster, cvict, spellnum);
    }
    if si.routines & MAG_UNAFFECTS != 0 {
        mag_unaffects(g, level, caster, cvict, spellnum);
    }
    if si.routines & MAG_POINTS != 0 {
        mag_points(g, level, caster, cvict, spellnum);
    }
    if si.routines & MAG_ALTER_OBJS != 0 {
        mag_alter_objs(g, level, caster, ovict, spellnum);
    }
    if si.routines & MAG_GROUPS != 0 {
        mag_groups(g, level, caster, spellnum);
    }
    if si.routines & MAG_MASSES != 0 {
        mag_masses(g, level, caster, spellnum);
    }
    if si.routines & MAG_AREAS != 0 {
        mag_areas(g, level, caster, spellnum);
    }
    if si.routines & MAG_SUMMONS != 0 {
        mag_summons(g, level, caster, ovict, spellnum);
    }
    if si.routines & MAG_CREATIONS != 0 {
        mag_creations(g, level, caster, spellnum);
    }

    if si.routines & MAG_MANUAL != 0 {
        match spellnum {
            SPELL_CHARM => spells::spell_charm(g, level, caster, cvict, ovict),
            SPELL_CREATE_WATER => spells::spell_create_water(g, level, caster, cvict, ovict),
            SPELL_DETECT_POISON => spells::spell_detect_poison(g, level, caster, cvict, ovict),
            SPELL_ENCHANT_WEAPON => spells::spell_enchant_weapon(g, level, caster, cvict, ovict),
            SPELL_IDENTIFY => spells::spell_identify(g, level, caster, cvict, ovict),
            SPELL_LOCATE_OBJECT => spells::spell_locate_object(g, level, caster, cvict, ovict),
            SPELL_SUMMON => spells::spell_summon(g, level, caster, cvict, ovict),
            SPELL_WORD_OF_RECALL => spells::spell_recall(g, level, caster, cvict, ovict),
            SPELL_FEAR => spells::spell_fear(g, level, caster, cvict, ovict),
            SPELL_RECHARGE => spells::spell_recharge(g, level, caster, cvict, ovict),
            SPELL_PORTAL => spells::spell_portal(g, level, caster, cvict, ovict),
            SPELL_LOCATE_TARGET => spells::spell_locate_target(g, level, caster, cvict, ovict),
            SPELL_HOME => spells::spell_home(g, level, caster, cvict, ovict),
            SPELL_WORD_OF_RETREAT => spells::spell_retreat(g, level, caster, cvict, ovict),
            SPELL_CONTROL_WEATHER => spells::spell_control_weather(g, level, caster, cvict, ovict),
            _ => {}
        }
    }

    1
}

// ===========================================================================
// mag_damage (magic.c)
// ===========================================================================
pub fn mag_damage(g: &mut GameState, level: i32, ch: CharId, victim: Option<CharId>, spellnum: i32) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    let mut dam: i32 = match spellnum {
        SPELL_EARTHQUAKE => g.rng.dice(2, 8) + level,
        _ => 0,
    };

    // Divide damage by two if victim saves.
    if mag_savingthrow(g, ch, victim) {
        dam >>= 1;
    }
    // Convergence of power doubles the damage.
    if is_affected(g, ch, AFF_CONVERGENCE) {
        dam <<= 1;
    }
    // dam_multi(ch, victim, 1) is the DeltaMUD power/mpower scaling, not yet
    // ported; the multiplier is 1 in the base data model (documented gap).

    combat::damage(g, ch, victim, dam);
}

// ===========================================================================
// mag_affects (magic.c)
// ===========================================================================
struct AfSlot {
    location: i32,
    modifier: i32,
    duration: i32,
    bitvector: i64,
}
impl AfSlot {
    fn blank() -> Self {
        AfSlot { location: APPLY_NONE_I, modifier: 0, duration: 0, bitvector: 0 }
    }
}

pub fn mag_affects(g: &mut GameState, level: i32, ch: CharId, victim: Option<CharId>, spellnum: i32) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    let mut af: [AfSlot; MAX_SPELL_AFFECTS] = [
        AfSlot::blank(), AfSlot::blank(), AfSlot::blank(), AfSlot::blank(), AfSlot::blank(),
    ];
    let mut accum_affect = false;
    let mut accum_duration = false;
    let mut to_room: Option<&str> = None;
    let ch_level = get_level(g, ch);

    match spellnum {
        SPELL_ARMOR => {
            af[0].location = APPLY_DEFENSE;
            af[0].modifier = 10;
            af[0].duration = 24;
            accum_duration = true;
        }
        SPELL_BLESS => {
            af[0].location = APPLY_TECHNIQUE;
            af[0].modifier = 8;
            af[0].duration = 6;
            af[1].location = APPLY_MDEFENSE;
            af[1].modifier = 5;
            af[1].duration = 6;
            accum_duration = true;
        }
        SPELL_BLINDNESS => {
            if mob_flagged(g, victim, MOB_NOBLIND) || mag_savingthrow(g, ch, victim) {
                g.send_to_char(ch, "You fail.\r\n");
                return;
            }
            af[0].location = APPLY_TECHNIQUE;
            af[0].modifier = -7;
            af[0].duration = 2;
            af[0].bitvector = AFF_BLIND;
            af[1].location = APPLY_DEFENSE;
            af[1].modifier = -10;
            af[1].duration = 2;
            af[1].bitvector = AFF_BLIND;
            to_room = Some("$n seems to be blinded!");
        }
        SPELL_CURSE => {
            if mag_savingthrow(g, ch, victim) {
                g.send_to_char(ch, NOEFFECT);
                return;
            }
            af[0].location = APPLY_TECHNIQUE;
            af[0].duration = 1 + (ch_level >> 1);
            af[0].modifier = -3;
            af[0].bitvector = AFF_CURSE;
            af[1].location = APPLY_POWER;
            af[1].duration = 1 + (ch_level >> 1);
            af[1].modifier = -4;
            af[1].bitvector = AFF_CURSE;
            accum_duration = true;
            accum_affect = true;
            to_room = Some("$n briefly glows red!");
        }
        SPELL_DETECT_ALIGN => {
            af[0].duration = 12 + level;
            af[0].bitvector = AFF_DETECT_ALIGN;
            accum_duration = true;
        }
        SPELL_DETECT_INVIS => {
            af[0].duration = 12 + level;
            af[0].bitvector = AFF_DETECT_INVIS;
            accum_duration = true;
        }
        SPELL_DETECT_MAGIC => {
            af[0].duration = 12 + level;
            af[0].bitvector = AFF_DETECT_MAGIC;
            accum_duration = true;
        }
        SPELL_INFRAVISION => {
            af[0].duration = 12 + level;
            af[0].bitvector = AFF_INFRAVISION;
            accum_duration = true;
            to_room = Some("$n's eyes glow red.");
        }
        SPELL_INVISIBLE => {
            af[0].duration = 12 + (ch_level >> 2);
            af[0].modifier = 10;
            af[0].location = APPLY_DEFENSE;
            af[0].bitvector = AFF_INVISIBLE;
            accum_duration = true;
            to_room = Some("$n slowly fades out of existence.");
        }
        SPELL_POISON => {
            if mag_savingthrow(g, ch, victim) {
                g.send_to_char(ch, NOEFFECT);
                return;
            }
            af[0].location = APPLY_STR;
            af[0].duration = ch_level;
            af[0].modifier = -2;
            af[0].bitvector = AFF_POISON;
            to_room = Some("$n gets violently ill!");
        }
        SPELL_SANCTUARY => {
            af[0].duration = 4;
            af[0].bitvector = AFF_SANCTUARY;
            accum_duration = true;
            to_room = Some("$n is surrounded by a white aura.");
        }
        SPELL_CONVERGENCE => {
            if is_affected(g, victim, AFF_AUTUS) {
                g.send_to_char(ch, "A green aura nullifies your magick!\r\n");
                return;
            }
            af[0].duration = 4;
            af[0].bitvector = AFF_CONVERGENCE;
            accum_duration = true;
            to_room = Some("$n is surrounded by a red aura.");
        }
        SPELL_AUTUS => {
            if is_affected(g, victim, AFF_CONVERGENCE) {
                g.send_to_char(ch, "A red aura nullifies your magick!\r\n");
                return;
            }
            af[0].duration = 4;
            af[0].bitvector = AFF_AUTUS;
            accum_duration = true;
            to_room = Some("$n is surrounded by a green aura.");
        }
        SPELL_SLEEP => {
            // !pk_allowed && !IS_NPC(ch) && !IS_NPC(victim) -> no PvP sleep.
            if !PK_ALLOWED && !is_npc(g, ch) && !is_npc(g, victim) {
                return;
            }
            if mob_flagged(g, victim, MOB_NOSLEEP) {
                return;
            }
            if mag_savingthrow(g, ch, victim) {
                return;
            }
            af[0].duration = 4 + (ch_level >> 2);
            af[0].bitvector = AFF_SLEEP;
            if g.get_char(victim).map(|c| c.position > Position::Sleeping).unwrap_or(false) {
                act(g, "$n goes to sleep.", true, victim, None, ActArg::None, To::Room);
                if let Some(c) = g.get_char_mut(victim) {
                    c.position = Position::Sleeping;
                }
            }
        }
        SPELL_STRENGTH => {
            af[0].location = APPLY_STR;
            af[0].duration = (ch_level >> 1) + 4;
            af[0].modifier = 1 + (level > MAX_PLAYER_STAT) as i32;
            accum_duration = true;
            accum_affect = true;
        }
        SPELL_SENSE_LIFE => {
            af[0].duration = ch_level;
            af[0].bitvector = AFF_SENSE_LIFE;
            accum_duration = true;
        }
        SPELL_WATERWALK => {
            af[0].duration = 24;
            af[0].bitvector = AFF_WATERWALK;
            accum_duration = true;
        }
        SPELL_STONE_SKIN => {
            af[0].location = APPLY_DEFENSE;
            af[0].modifier = 20;
            af[0].duration = 24;
            accum_duration = false;
            to_room = Some("You watch in fascination as $n's skin turns to stone!");
        }
        SPELL_RESIST_PORTAL => {
            af[0].duration = 16;
            af[0].bitvector = AFF_NOPORTAL;
            accum_duration = true;
        }
        SPELL_REDIRECT_CHARGE => {
            if is_affected(g, victim, AFF_R_CHARGED) {
                g.send_to_char(ch, "Target is already carrying a charge.\r\n");
                return;
            }
            af[0].location = APPLY_NONE_I;
            af[0].duration = 24;
            af[0].modifier = 0;
            af[0].bitvector = AFF_REDIRECT_CHARGE;
            to_room = Some("$n suddenly &Kglows&n.");
        }
        _ => return,
    }

    let to_vict = spell_affect_msg(spellnum);

    // NPC with this affect in its mob file: don't let players un-sanct it.
    if is_npc(g, victim) && !affected_by_spell(g, victim, spellnum) {
        for slot in &af {
            if slot.bitvector != 0 && is_affected(g, victim, slot.bitvector) {
                g.send_to_char(ch, NOEFFECT);
                return;
            }
        }
    }

    // Already affected by this spell and non-accumulative -> fail.
    if affected_by_spell(g, victim, spellnum) && !(accum_duration || accum_affect) {
        g.send_to_char(ch, NOEFFECT);
        return;
    }

    for slot in af.iter() {
        if slot.bitvector != 0 || slot.location != APPLY_NONE_I {
            let new_af = Affect {
                spell_type: spellnum,
                duration: slot.duration,
                modifier: slot.modifier,
                location: slot.location,
                bitvector: slot.bitvector,
                caster: Some(ch),
            };
            affect_join(g, victim, new_af, accum_duration, accum_affect);
        }
    }

    if let Some(tv) = to_vict {
        if spellnum != SPELL_SLEEP {
            act(g, tv, false, victim, None, ActArg::Char(ch), To::Char);
        }
    }
    if let Some(tr) = to_room {
        act(g, tr, true, victim, None, ActArg::Char(ch), To::Room);
    }
}

/// spell_affect_msg[] (the to-vict "wears on" messages). The C table is sparse;
/// most spells have no to-vict here (they use to_room only), so an empty string
/// is the faithful default.
fn spell_affect_msg(_spellnum: i32) -> Option<&'static str> {
    None
}

// ===========================================================================
// mag_points (magic.c)
// ===========================================================================
pub fn mag_points(g: &mut GameState, level: i32, _ch: CharId, victim: Option<CharId>, spellnum: i32) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    let mut hit = 0;
    let mut mana = 0;
    let move_p = 0;

    match spellnum {
        SPELL_CURE_LIGHT => {
            hit = g.rng.dice(1, 8) + 1 + (level >> 2);
            g.send_to_char(victim, "You feel better.\r\n");
        }
        SPELL_CURE_CRITIC => {
            hit = g.rng.dice(3, 8) + 3 + (level >> 2);
            g.send_to_char(victim, "You feel a lot better!\r\n");
        }
        SPELL_HEAL => {
            hit = 100 + g.rng.dice(3, 8);
            g.send_to_char(victim, "A warm feeling floods your body.\r\n");
        }
        SPELL_REGEN_MANA => {
            mana = 150;
            g.send_to_char(victim, "A tingling sensation floods your body.\r\n");
        }
        _ => {}
    }

    if let Some(c) = g.get_char_mut(victim) {
        c.points.hit = (c.points.hit + hit).min(c.points.max_hit);
        c.points.mana = (c.points.mana + mana).min(c.points.max_mana);
        c.points.move_points = (c.points.move_points + move_p).min(c.points.max_move);
    }
    update_pos(g, victim);
}

// ===========================================================================
// mag_unaffects (magic.c)
// ===========================================================================
pub fn mag_unaffects(g: &mut GameState, _level: i32, ch: CharId, victim: Option<CharId>, spellnum: i32) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };

    let (spell, to_vict, to_room): (i32, Option<&str>, Option<&str>) = match spellnum {
        SPELL_CURE_BLIND | SPELL_HEAL => {
            // check_perm_duration (permanent-affect guard) is not ported; the
            // affect set here is never permanent, so the guard is a no-op.
            (SPELL_BLINDNESS, Some("Your vision returns!"), Some("There's a momentary gleam in $n's eyes."))
        }
        SPELL_REMOVE_POISON => {
            (SPELL_POISON, Some("A warm feeling runs through your body!"), Some("$n looks better."))
        }
        SPELL_REMOVE_CURSE => {
            (SPELL_CURSE, Some("You don't feel so unlucky."), None)
        }
        _ => return,
    };

    if !affected_by_spell(g, victim, spell) {
        g.send_to_char(ch, NOEFFECT);
        return;
    }

    affect_from_char(g, victim, spell);
    if let Some(tv) = to_vict {
        act(g, tv, false, victim, None, ActArg::Char(ch), To::Char);
    }
    if let Some(tr) = to_room {
        act(g, tr, true, victim, None, ActArg::Char(ch), To::Room);
    }
}

// ===========================================================================
// mag_alter_objs (magic.c)
// ===========================================================================
pub fn mag_alter_objs(g: &mut GameState, _level: i32, ch: CharId, obj: Option<ObjId>, spellnum: i32) {
    let obj = match obj {
        Some(o) => o,
        None => return,
    };
    let ch_level = get_level(g, ch);
    let mut to_char: Option<&str> = None;
    let to_room: Option<&str> = None;

    let (otype, oweight, oextra) = match g.get_obj(obj) {
        Some(o) => (o.obj_type, o.weight, o.extra_flags.bits()),
        None => return,
    };

    match spellnum {
        SPELL_BLESS => {
            if oextra & ITEM_BLESS == 0 && oweight <= 5 * ch_level {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.extra_flags |= crate::object::ExtraFlags::from_bits_truncate(ITEM_BLESS);
                }
                to_char = Some("$p glows briefly.");
            }
        }
        SPELL_CURSE => {
            if oextra & ITEM_NODROP == 0 {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.extra_flags |= crate::object::ExtraFlags::from_bits_truncate(ITEM_NODROP);
                    if o.obj_type == ObjectType::Weapon {
                        o.values[2] -= 1;
                    }
                }
                to_char = Some("$p briefly glows red.");
            }
        }
        SPELL_INVISIBLE => {
            if oextra & (ITEM_NOINVIS | ITEM_INVISIBLE) == 0 {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.extra_flags |= crate::object::ExtraFlags::from_bits_truncate(ITEM_INVISIBLE);
                }
                to_char = Some("$p vanishes.");
            }
        }
        SPELL_POISON => {
            if matches!(otype, ObjectType::LiqContainer | ObjectType::Fountain | ObjectType::Food)
                && g.get_obj(obj).map(|o| o.values[3] == 0).unwrap_or(false)
            {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.values[3] = 1;
                }
                to_char = Some("$p steams briefly.");
            }
        }
        SPELL_REMOVE_CURSE => {
            if oextra & ITEM_NODROP != 0 {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.extra_flags &= !crate::object::ExtraFlags::from_bits_truncate(ITEM_NODROP);
                    if o.obj_type == ObjectType::Weapon {
                        o.values[2] += 1;
                    }
                }
                to_char = Some("$p briefly glows blue.");
            }
        }
        SPELL_REMOVE_POISON => {
            if matches!(otype, ObjectType::LiqContainer | ObjectType::Fountain | ObjectType::Food)
                && g.get_obj(obj).map(|o| o.values[3] != 0).unwrap_or(false)
            {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.values[3] = 0;
                }
                to_char = Some("$p steams briefly.");
            }
        }
        _ => {}
    }

    match to_char {
        None => g.send_to_char(ch, NOEFFECT),
        Some(tc) => act(g, tc, true, ch, Some(obj), ActArg::None, To::Char),
    }
    if let Some(tr) = to_room {
        act(g, tr, true, ch, Some(obj), ActArg::None, To::Room);
    } else if let Some(tc) = to_char {
        act(g, tc, true, ch, Some(obj), ActArg::None, To::Room);
    }
}

// ===========================================================================
// mag_creations (magic.c)
// ===========================================================================
pub fn mag_creations(g: &mut GameState, _level: i32, ch: CharId, spellnum: i32) {
    if !g.char_exists(ch) {
        return;
    }
    let z: ObjVnum = match spellnum {
        SPELL_CREATE_FOOD => 10,
        _ => {
            g.send_to_char(ch, "Spell unimplemented, it would seem.\r\n");
            return;
        }
    };
    let tobj = match g.load_object(z) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "I seem to have goofed.\r\n");
            return;
        }
    };
    g.obj_to_char(tobj, ch);
    act(g, "$n creates $p.", false, ch, Some(tobj), ActArg::None, To::Room);
    act(g, "You create $p.", false, ch, Some(tobj), ActArg::None, To::Char);
}

// ===========================================================================
// mag_summons (magic.c)
// ===========================================================================

// Summoned-mob vnums (magic.c).
const MOB_CLONE: MobVnum = 10;
const MOB_ZOMBIE: MobVnum = 11;

// fail/summon message tables (magic.c). Index by msg/fmsg.
const SUMMON_MSGS: &[&str] = &[
    "\r\n",
    "$n makes a strange magical gesture; you feel a strong breeze!",
    "$n animates a corpse!",
    "$N appears from a cloud of thick blue smoke!",
    "$N appears from a cloud of thick green smoke!",
    "$N appears from a cloud of thick red smoke!",
    "$N disappears in a thick black cloud!As $n makes a strange magical gesture, you feel a strong breeze.",
    "As $n makes a strange magical gesture, you feel a searing heat.",
    "As $n makes a strange magical gesture, you feel a sudden chill.",
    "As $n makes a strange magical gesture, you feel the dust swirl.",
    "$n magically divides!",
    "$n animates a corpse!",
];
const SUMMON_FAIL_MSGS: &[&str] = &[
    "\r\n",
    "There are no such creatures.\r\n",
    "Your attempt to raise the dead failed.\r\n",
    "The elemental forces were not powerful enough.\r\n",
    "It did not work..\r\n",
    "The elements resist!\r\n",
    "You failed.\r\n",
    "There is no corpse!\r\n",
];

pub fn mag_summons(g: &mut GameState, _level: i32, ch: CharId, obj: Option<ObjId>, spellnum: i32) {
    if !g.char_exists(ch) {
        return;
    }

    let (msg, mob_num, pfail, handle_corpse): (usize, MobVnum, i32, bool) = match spellnum {
        SPELL_CLONE => (10, MOB_CLONE, 50, false),
        SPELL_ANIMATE_DEAD => {
            // obj must be a corpse (values[3]==1, container, per make_corpse).
            let is_corpse = obj
                .and_then(|o| g.get_obj(o))
                .map(|o| o.obj_type == ObjectType::Container && o.values[3] == 1)
                .unwrap_or(false);
            if !is_corpse {
                act(g, SUMMON_FAIL_MSGS[7], false, ch, None, ActArg::None, To::Char);
                return;
            }
            (11, MOB_ZOMBIE, 10, true)
        }
        _ => return,
    };
    let fmsg = g.rng.number(2, 6) as usize; // random fail message

    if is_affected(g, ch, AFF_CHARM) {
        g.send_to_char(ch, "You are too giddy to have any followers!\r\n");
        return;
    }
    if g.rng.number(0, 101) < pfail {
        g.send_to_char(ch, SUMMON_FAIL_MSGS[fmsg.min(SUMMON_FAIL_MSGS.len() - 1)]);
        return;
    }

    let mob = match g.load_mobile(mob_num) {
        Some(m) => m,
        None => {
            g.send_to_char(ch, "You don't quite remember how to make that creature.\r\n");
            return;
        }
    };
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    g.char_to_room(mob, rnum);
    if let Some(m) = g.get_char_mut(mob) {
        m.carry_weight = 0;
        m.carry_items = 0;
        m.affect_flags |= AFF_CHARM;
    }
    if spellnum == SPELL_CLONE {
        let name = g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default();
        if let Some(m) = g.get_char_mut(mob) {
            m.player.name = name.clone();
            m.short_desc = Some(name);
        }
    }
    act(g, SUMMON_MSGS[msg], false, ch, None, ActArg::Char(mob), To::Room);
    add_follower(g, mob, ch);

    if handle_corpse {
        if let Some(corpse) = obj {
            let contents = g.get_obj(corpse).map(|o| o.contains.clone()).unwrap_or_default();
            for c in contents {
                g.obj_from_anywhere(c);
                g.obj_to_char(c, mob);
            }
            g.obj_from_anywhere(corpse);
            g.extract_obj(corpse);
        }
    }
}

// ===========================================================================
// mag_areas (magic.c)
// ===========================================================================
pub fn mag_areas(g: &mut GameState, _level: i32, ch: CharId, spellnum: i32) {
    if !g.char_exists(ch) {
        return;
    }
    let (to_char, to_room): (Option<&str>, Option<&str>) = match spellnum {
        SPELL_EARTHQUAKE => (
            Some("You gesture and the earth begins to shake all around you!"),
            Some("$n gracefully gestures and the earth begins to shake violently!"),
        ),
        _ => (None, None),
    };
    if let Some(tc) = to_char {
        act(g, tc, false, ch, None, ActArg::None, To::Char);
    }
    if let Some(tr) = to_room {
        act(g, tr, false, ch, None, ActArg::None, To::Room);
    }

    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    let ch_level = get_level(g, ch);
    let ch_npc = is_npc(g, ch);
    for tch in people {
        if tch == ch {
            continue;
        }
        // Skip immortals.
        if !is_npc(g, tch) && get_level(g, tch) >= LVL_IMMORT as i32 {
            continue;
        }
        // No PvP if pk disabled.
        if !PK_ALLOWED && !ch_npc && !is_npc(g, tch) {
            continue;
        }
        // Players can't hit their own charmed pets.
        if !ch_npc && is_npc(g, tch) && is_affected(g, tch, AFF_CHARM) {
            continue;
        }
        mag_damage(g, ch_level, ch, Some(tch), spellnum);
    }
}

// ===========================================================================
// mag_groups (magic.c) + perform_mag_groups
// ===========================================================================
fn perform_mag_groups(g: &mut GameState, level: i32, ch: CharId, tch: CharId, spellnum: i32) {
    match spellnum {
        SPELL_GROUP_HEAL => mag_points(g, level, ch, Some(tch), SPELL_HEAL),
        SPELL_GROUP_ARMOR => mag_affects(g, level, ch, Some(tch), SPELL_ARMOR),
        SPELL_GROUP_RECALL => spells::spell_recall(g, level, ch, Some(tch), None),
        SPELL_GROUP_STONE_SKIN => mag_affects(g, level, ch, Some(tch), SPELL_STONE_SKIN),
        _ => {}
    }
}

pub fn mag_groups(g: &mut GameState, level: i32, ch: CharId, spellnum: i32) {
    if !g.char_exists(ch) {
        return;
    }
    if !is_affected(g, ch, AFF_GROUP) {
        return;
    }
    let k = g.get_char(ch).and_then(|c| c.master).unwrap_or(ch);
    let ch_room = g.get_char(ch).and_then(|c| c.in_room);
    let followers = g.get_char(k).map(|c| c.followers.clone()).unwrap_or_default();

    for tch in followers {
        if g.get_char(tch).and_then(|c| c.in_room) != ch_room {
            continue;
        }
        if !is_affected(g, tch, AFF_GROUP) {
            continue;
        }
        if tch == ch {
            continue;
        }
        perform_mag_groups(g, level, ch, tch, spellnum);
    }

    if k != ch && is_affected(g, k, AFF_GROUP) {
        perform_mag_groups(g, level, ch, k, spellnum);
    }
    perform_mag_groups(g, level, ch, ch, spellnum);
}

// ===========================================================================
// mag_masses (magic.c) — no spells of this class implemented.
// ===========================================================================
pub fn mag_masses(_g: &mut GameState, _level: i32, _ch: CharId, _spellnum: i32) {}

// ---------------------------------------------------------------------------
// add_follower (handler.c) — local copy for mag_summons (the cmd_movement one
// is private). Links mob->ch and announces.
// ---------------------------------------------------------------------------
fn add_follower(g: &mut GameState, ch: CharId, leader: CharId) {
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

// pk_allowed (config) — DeltaMUD ships with player-killing disabled by default.
const PK_ALLOWED: bool = false;
