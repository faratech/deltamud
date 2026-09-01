// magic.rs — the spell-template routines (CircleMUD magic.c) plus call_magic,
// the central dispatch every spell invocation passes through. Full port of
// magic.c: mag_damage, mag_affects, mag_points, mag_unaffects, mag_alter_objs,
// mag_creations, mag_summons, mag_areas, mag_groups, mag_masses — and the
// call_magic that routes a spell number to its MAG_* routines / manual spell.
//
// The SPELL_*/MAG_*/TAR_* constants and the spell_info() table live in
// spell_parser.rs; the manual spells live in spells.rs. This module imports
// both.

use crate::act::{ActArg, To, act};
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
/// NO_MAGIC and peaceful-room exceptions are administrative capabilities.
/// Only direct input from a live authenticated Implementor principal may use
/// them; indirect casts and high-level NPC bodies remain ordinary magic.
fn has_direct_implementor_authority(g: &GameState, ch: CharId) -> bool {
    crate::interpreter::authenticated_input_authority(g, ch)
        .is_some_and(|authority| authority.authority >= i32::from(LVL_IMPL))
}
fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}
fn is_affected(g: &GameState, ch: CharId, bit: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.affect_flags & bit != 0)
        .unwrap_or(false)
}
fn mob_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.is_npc && c.act_flags & flag != 0)
        .unwrap_or(false)
}
fn affected_by_spell(g: &GameState, ch: CharId, spell: i32) -> bool {
    g.get_char(ch)
        .map(|c| c.affected.iter().any(|a| a.spell_type == spell))
        .unwrap_or(false)
}

/// mag_savingthrow(ch, victim) (magic.c): does the victim avoid the spell?
/// DeltaMUD: `number(0,100) > chance(ch, victim, 1)` — a magical to-hit roll
/// against the attacker's magical chance. The save succeeds (returns true)
/// when the roll beats that chance, so a weak caster's spells get resisted
/// more often. Uses the canonical combat::chance (no forked formula).
fn mag_savingthrow(g: &mut GameState, ch: CharId, victim: CharId) -> bool {
    g.rng.number(0, 100) > combat::chance(g, ch, victim, 1)
}

/// affect_join (handler.c): merge a new affect into the victim, with optional
/// duration/modifier accumulation, then recompute. The Rust affect list stores
/// each affect; replacement means removing the prior same-spell affects first.
fn affect_join(g: &mut GameState, victim: CharId, mut af: Affect, add_dur: bool, add_mod: bool) {
    let spell = af.spell_type;
    if let Some(c) = g.get_char_mut(victim) {
        // Find an existing affect of the same spell with the same location.
        if let Some(existing) = c
            .affected
            .iter()
            .find(|a| a.spell_type == spell && a.location == af.location)
            .cloned()
        {
            if add_dur {
                af.duration += existing.duration;
            }
            if add_mod {
                af.modifier += existing.modifier;
            }
            // Remove the matching prior affect (same spell + location).
            c.affected
                .retain(|a| !(a.spell_type == spell && a.location == af.location));
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
    g.affect_remove_spell(victim, spell);
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

/// Thin alias so callers can spell the cast entry point `magic::do_cast`.
/// The real pipeline lives in spell_parser::do_cast (contract owner).
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
    let administrative_override = has_direct_implementor_authority(g, caster);

    // ROOM_NOMAGIC fizzle.
    if let Some(r) = rnum {
        let nomagic = g.room(r).room_flags.bits() & ROOM_NOMAGIC != 0;
        if nomagic && !administrative_override {
            g.send_to_char(caster, "Your magic fizzles out and dies.\r\n");
            act(
                g,
                "$n's magic fizzles out and dies.",
                false,
                caster,
                None,
                ActArg::None,
                To::Room,
            );
            return 0;
        }
        let peaceful = g.room(r).room_flags.contains(RoomFlags::PEACEFUL);
        if !administrative_override && peaceful && (si.violent || si.routines & MAG_DAMAGE != 0) {
            g.send_to_char(
                caster,
                "A flash of white light fills the room, dispelling your violent magic!\r\n",
            );
            act(
                g,
                "White light from no particular source suddenly fills the room, then vanishes.",
                false,
                caster,
                None,
                ActArg::None,
                To::Room,
            );
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
pub fn mag_damage(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    victim: Option<CharId>,
    spellnum: i32,
) {
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
    // DeltaMUD magical power scaling: dam *= dam_multi(ch, victim, 1). As in C
    // this is applied here AND again inside do_actual_damage (combat::damage_type
    // re-applies the type-1 multiplier for spellnums 1..=MAX_SPELLS).
    dam = (dam as f32 * combat::dam_multi(g, ch, victim, 1)) as i32;

    // Pass the spell number as the attack type so the damage path treats it as
    // magical (type-1 multiplier) and routes the spell message correctly.
    combat::damage_type(g, ch, victim, dam, spellnum);
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
        AfSlot {
            location: APPLY_NONE_I,
            modifier: 0,
            duration: 0,
            bitvector: 0,
        }
    }
}

pub fn mag_affects(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    victim: Option<CharId>,
    spellnum: i32,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    let mut af: [AfSlot; MAX_SPELL_AFFECTS] = [
        AfSlot::blank(),
        AfSlot::blank(),
        AfSlot::blank(),
        AfSlot::blank(),
        AfSlot::blank(),
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
            if !g.pk_allowed && !is_npc(g, ch) && !is_npc(g, victim) {
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
            if g.get_char(victim)
                .map(|c| c.position > Position::Sleeping)
                .unwrap_or(false)
            {
                act(
                    g,
                    "$n goes to sleep.",
                    true,
                    victim,
                    None,
                    ActArg::None,
                    To::Room,
                );
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

/// spell_affect_msg[] (constants.c:889): the to-vict message a successful
/// buff prints via magic.c:507/535-538 (suppressed for SPELL_SLEEP). The old
/// stub returned None, so the recipient never learned armor/sanctuary/stone
/// skin etc. took hold (#244). '!'-prefixed placeholders mean no message.
fn spell_affect_msg(spellnum: i32) -> Option<&'static str> {
    const TABLE: &[&str] = &[
        "RESERVED DB.C",                            // 0
        "You feel someone protecting you.",         // 1
        "!Teleport!",                               // 2
        "You feel righteous.",                      // 3
        "You have been blinded!",                   // 4
        "You feel very uncomfortable.",             // 5
        "!Clone!",                                  // 6
        "!Control Weather!",                        // 7
        "!Create Food!",                            // 8
        "!Create Water!",                           // 9
        "!Cure Blind!",                             // 10
        "!Cure Critic!",                            // 11
        "!Cure Light!",                             // 12
        "You feel very uncomfortable.",             // 13
        "Your eyes tingle.",                        // 14
        "Your eyes tingle.",                        // 15
        "Your eyes tingle.",                        // 16
        "Your eyes tingle.",                        // 17
        "Your eyes tingle.",                        // 18
        "!Earthquake!",                             // 19
        "!Enchant Weapon!",                         // 20
        "!Heal!",                                   // 21
        "You vanish.",                              // 22
        "!Locate object!",                          // 23
        "You feel very sick.",                      // 24
        "!Remove Curse!",                           // 25
        "A white aura momentarily surrounds you.",  // 26
        "You feel very sleepy...  Zzzz......",      // 27
        "You feel stronger!",                       // 28
        "!Summon!",                                 // 29
        "!Word of Recall!",                         // 30
        "!Remove Poison!",                          // 31
        "Your feel your awareness improve.",        // 32
        "!Animate Dead!",                           // 33
        "!Group Armor!",                            // 34
        "!Group Heal!",                             // 35
        "!Group Recall!",                           // 36
        "Your eyes glow red.",                      // 37
        "Your feel webbing between your toes.",     // 38
        "Your skin turns to stone!.",               // 39
        "!Fear!",                                   // 40
        "!Recharge!",                               // 41
        "!Portal!",                                 // 42
        "!Group Stone Skin!",                       // 43
        "!Locate Target!",                          // 44
        "A red aura momentarily surrounds you.",    // 45
        "A green aura momentarily surrounds you.",  // 46
        "You feel a protection from the heavens.",  // 47
        "!Regen Mana!",                             // 48
        "!Home!",                                   // 49
        "!Word of Retreat!",                        // 50
        "Your feet are suddenly chained together!", // 51
        "You feel electrified and &Kglow&n!",       // 52
    ];
    let entry = TABLE.get(spellnum as usize).copied().unwrap_or("");
    if entry.is_empty() || entry.starts_with('!') {
        None
    } else {
        Some(entry)
    }
}

// ===========================================================================
// mag_points (magic.c)
// ===========================================================================
pub fn mag_points(
    g: &mut GameState,
    level: i32,
    _ch: CharId,
    victim: Option<CharId>,
    spellnum: i32,
) {
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
pub fn mag_unaffects(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    victim: Option<CharId>,
    spellnum: i32,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };

    // check_perm_duration (handler.c) gates removal: a permanent affect
    // (duration == -1, type == -1, matching bitvector) cannot be cured/removed.
    // C checks this on the caster `ch` (not the victim) — preserved here.
    let (spell, to_vict, to_room): (i32, Option<&str>, Option<&str>) = match spellnum {
        SPELL_CURE_BLIND | SPELL_HEAL => {
            if crate::handler::check_perm_duration(g, ch, AFF_BLIND) {
                if spellnum != SPELL_HEAL {
                    g.send_to_char(ch, NOEFFECT);
                }
                return;
            }
            (
                SPELL_BLINDNESS,
                Some("Your vision returns!"),
                Some("There's a momentary gleam in $n's eyes."),
            )
        }
        SPELL_REMOVE_POISON => {
            if crate::handler::check_perm_duration(g, ch, AFF_POISON) {
                g.send_to_char(ch, NOEFFECT);
                return;
            }
            (
                SPELL_POISON,
                Some("A warm feeling runs through your body!"),
                Some("$n looks better."),
            )
        }
        SPELL_REMOVE_CURSE => {
            if crate::handler::check_perm_duration(g, ch, AFF_CURSE) {
                g.send_to_char(ch, NOEFFECT);
                return;
            }
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
pub fn mag_alter_objs(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    obj: Option<ObjId>,
    spellnum: i32,
) {
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
            if matches!(
                otype,
                ObjectType::LiqContainer | ObjectType::Fountain | ObjectType::Food
            ) && g.get_obj(obj).map(|o| o.values[3] == 0).unwrap_or(false)
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
            if matches!(
                otype,
                ObjectType::LiqContainer | ObjectType::Fountain | ObjectType::Food
            ) && g.get_obj(obj).map(|o| o.values[3] != 0).unwrap_or(false)
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
    // C magic.c:1044-1046: load_otrigger fires on the created object so
    // OTRIG_LOAD scripts (shipped trigger on vnum 10 food) run (#146).
    crate::dg_triggers::load_otrigger(g, tobj);
    act(
        g,
        "$n creates $p.",
        false,
        ch,
        Some(tobj),
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "You create $p.",
        false,
        ch,
        Some(tobj),
        ActArg::None,
        To::Char,
    );
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
                act(
                    g,
                    SUMMON_FAIL_MSGS[7],
                    false,
                    ch,
                    None,
                    ActArg::None,
                    To::Char,
                );
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
            g.send_to_char(
                ch,
                "You don't quite remember how to make that creature.\r\n",
            );
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
        let name = g
            .get_char(ch)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        if let Some(m) = g.get_char_mut(mob) {
            m.player.name = name.clone();
            m.short_desc = Some(name);
        }
    }
    act(
        g,
        SUMMON_MSGS[msg],
        false,
        ch,
        None,
        ActArg::Char(mob),
        To::Room,
    );
    // C magic.c:828: load_mtrigger(mob) fires before add_follower - summoned
    // undead/clones run their MTRIG_LOAD scripts (#145).
    crate::dg_triggers::load_mtrigger(g, mob);
    add_follower(g, mob, ch);

    if handle_corpse {
        if let Some(corpse) = obj {
            let contents = g
                .get_obj(corpse)
                .map(|o| o.contains.clone())
                .unwrap_or_default();
            for c in contents {
                g.obj_from_anywhere(c);
                g.obj_to_char(c, mob);
            }
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
        if !g.pk_allowed && !ch_npc && !is_npc(g, tch) {
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
    let followers = g
        .get_char(k)
        .map(|c| c.followers.clone())
        .unwrap_or_default();

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
    act(
        g,
        "You now follow $N.",
        false,
        ch,
        None,
        ActArg::Char(leader),
        To::Char,
    );
    if g.can_see(leader, ch) {
        act(
            g,
            "$n starts following you.",
            true,
            ch,
            None,
            ActArg::Char(leader),
            To::Vict,
        );
    }
    act(
        g,
        "$n starts to follow $N.",
        true,
        ch,
        None,
        ActArg::Char(leader),
        To::NotVict,
    );
}

// pk_allowed (config) — DeltaMUD ships with player-killing disabled by default.

/// C magic.c:65-117 `affect_update()`, called once per MUD hour from the
/// heartbeat (comm.c:1038) right before point_update: decrements every
/// affect's duration, prints the wear-off message, discharges an expiring
/// AFF_R_CHARGED affect as self-damage, and runs the SKILL_ADRENALINE
/// sustain/exhaustion branches (issue #96).
pub fn affect_update(g: &mut GameState) {
    let ids: Vec<CharId> = g.char_ids();
    for cid in ids {
        // C skips the PRF2_INTANGIBLE check for PCs only (!IS_NPC guard):
        // intangible NPCs still age their affects.
        let skip = match g.get_char(cid) {
            Some(c) => !c.is_npc && (c.prf2_flags & crate::flags::PRF2_INTANGIBLE) != 0,
            None => continue,
        };
        if skip {
            continue;
        }
        affect_update_char(g, cid);
    }
}

enum AfUpdate {
    Keep,
    Remove,
}

fn affect_update_char(g: &mut GameState, cid: CharId) {
    let mut i = 0usize;
    while g
        .get_char(cid)
        .map(|c| i < c.affected.len())
        .unwrap_or(false)
    {
        let (stype, dur, modifier, bitvector) = {
            let c = g.get_char(cid).unwrap();
            let a = &c.affected[i];
            (a.spell_type, a.duration, a.modifier, a.bitvector)
        };
        let mut action = AfUpdate::Keep;
        // Adrenaline branch of the decremented affect: 1 = brink of death
        // sustain, 2 = exhausted collapse, 3 = slow wear-off while upright.
        let mut adrenaline: u8 = 0;
        if dur >= 1 {
            let new_dur = {
                let c = g.get_char_mut(cid).unwrap();
                c.affected[i].duration -= 1;
                c.affected[i].duration
            };
            let (fighting, pos) = match g.get_char(cid) {
                Some(c) => (c.fighting, c.position),
                None => return,
            };
            if stype == SKILL_ADRENALINE && fighting.is_none() {
                if pos < Position::Standing {
                    adrenaline = if pos < Position::Sleeping { 1 } else { 2 };
                    action = AfUpdate::Remove;
                } else {
                    adrenaline = 3;
                }
            }
            let _ = new_dur;
            match adrenaline {
                1 => {
                    g.send_to_char(cid, "The &Radrenaline&n flowing through your veins sustains you on the brink of &Kdeath&n!\r\n");
                    let heal = g.rng.number(1, 500);
                    let c = g.get_char_mut(cid).unwrap();
                    c.points.hit += heal;
                    c.position = Position::Standing;
                }
                2 => {
                    g.send_to_char(
                        cid,
                        "Your &Radrenaline&n rush completely wears off, leaving you exhausted.\r\n",
                    );
                    let c = g.get_char_mut(cid).unwrap();
                    c.points.hit = (c.points.hit - new_dur * 100).max(10);
                    c.points.move_points = (c.points.move_points - new_dur * 15).max(10);
                }
                3 => {
                    g.send_to_char(
                        cid,
                        "Your &Radrenaline&n rush slowly wears off, leaving you tired.\r\n",
                    );
                    let c = g.get_char_mut(cid).unwrap();
                    c.points.hit = (c.points.hit - 100).max(10);
                    c.points.move_points = (c.points.move_points - 15).max(0);
                }
                _ => {}
            }
        } else if dur == -1 {
            // No action: unlimited duration (gods only!).
        } else {
            // Expiring. C suppresses the wear-off message when the next
            // affect in the list is the same type still ticking (stacked
            // affects show one message, on the last one to expire).
            let show = stype > 0
                && stype <= MAX_SPELLS
                && match g.get_char(cid).unwrap().affected.get(i + 1) {
                    None => true,
                    Some(next) => next.spell_type != stype || next.duration > 0,
                };
            if show && stype <= 499 {
                let msg = crate::constants::SPELL_WEAR_OFF_MSG
                    .get(stype as usize)
                    .copied()
                    .unwrap_or("");
                if !msg.is_empty() {
                    g.send_to_char(cid, msg);
                    g.send_to_char(cid, "\r\n");
                    // C: an expiring redirect charge discharges into its
                    // host as TYPE_UNDEFINED self-damage; this can kill and
                    // extract the character.
                    if bitvector == AFF_R_CHARGED {
                        combat::damage_type(g, cid, cid, modifier, TYPE_UNDEFINED);
                    }
                }
            }
            action = AfUpdate::Remove;
        }
        if matches!(action, AfUpdate::Remove) {
            // The discharge above may have extracted the character. The
            // expired affect's bits are cleared with it (affect_remove).
            if let Some(c) = g.get_char(cid) {
                if i < c.affected.len() {
                    let bv = c.affected[i].bitvector;
                    let c2 = g.get_char_mut(cid).unwrap();
                    c2.affected.remove(i);
                    c2.affect_flags &= !bv;
                }
            }
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod affect_update_tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::types::Class;
    use crate::types::ConnId;

    fn spell_affect(stype: i32, duration: i32) -> Affect {
        Affect {
            spell_type: stype,
            duration,
            modifier: 0,
            location: 0,
            bitvector: 0,
            caster: None,
        }
    }

    fn authority_magic_fixture(
        flags: RoomFlags,
        level: Level,
        trust: i32,
    ) -> (GameState, CharId, CharId, ConnId) {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(crate::room::Room::new(
            86_100,
            0,
            "Ward".to_string(),
            "A warded test room.".to_string(),
        ));
        g.room_mut(room).room_flags = flags;
        let conn = ConnId(86_101);
        let mut caster = Character::new_player(
            "Caster".to_string(),
            Class::MagicUser,
            crate::types::Race::Human,
        );
        caster.desc = Some(conn);
        caster.idnum = 86_101;
        caster.player.level = level;
        caster.trust = trust;
        caster.points.mana = 10_000;
        caster.points.max_mana = 10_000;
        caster.set_skill(SPELL_ARMOR as u16, u8::MAX);
        caster.set_skill(SPELL_CURSE as u16, u8::MAX);
        let caster = g.create_char(caster);
        let mut descriptor = Descriptor::new(conn, "magic-authority.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(caster);
        g.descriptors.insert(conn, descriptor);
        g.players_by_name.insert("caster".to_string(), caster);

        let mut victim = Character::new_npc(86_102);
        victim.player.name = "Target".to_string();
        let victim = g.create_char(victim);
        g.char_to_room(caster, room);
        g.char_to_room(victim, room);
        (g, caster, victim, conn)
    }

    #[test]
    fn no_magic_override_requires_direct_persisted_implementor_trust() {
        // The DG runtime is process-global in production. Serialize this
        // low-level command-dispatch regression with DG tests so a trigger
        // attached to another test's room-rnum zero cannot consume `cast`.
        let _dg = crate::lock_ok::lock(&crate::dg_handler::DG_TEST_LOCK);
        crate::dg_handler::boot_handler();

        let (mut display_g, display, _, display_conn) =
            authority_magic_fixture(RoomFlags::NO_MAGIC, LVL_IMPL, 1);
        crate::interpreter::run_authenticated_command(&mut display_g, display, "cast 'armor'");
        assert!(
            display_g.descriptors[&display_conn]
                .outbuf
                .contains("magic fizzles out")
        );

        let (mut trusted_g, trusted, _, trusted_conn) =
            authority_magic_fixture(RoomFlags::NO_MAGIC, 50, i32::from(LVL_IMPL));
        crate::interpreter::run_authenticated_command(&mut trusted_g, trusted, "cast 'armor'");
        assert!(
            !trusted_g.descriptors[&trusted_conn]
                .outbuf
                .contains("magic fizzles out")
        );
        assert!(
            trusted_g
                .get_char(trusted)
                .unwrap()
                .affected
                .iter()
                .any(|affect| affect.spell_type == SPELL_ARMOR),
            "direct Implementor cast did not apply armor; output was {:?}",
            trusted_g.descriptors[&trusted_conn].outbuf
        );

        let (mut indirect_g, indirect, _, indirect_conn) =
            authority_magic_fixture(RoomFlags::NO_MAGIC, 50, i32::from(LVL_IMPL));
        assert_eq!(
            call_magic(
                &mut indirect_g,
                indirect,
                Some(indirect),
                None,
                SPELL_ARMOR,
                50,
            ),
            0
        );
        assert!(
            indirect_g.descriptors[&indirect_conn]
                .outbuf
                .contains("magic fizzles out")
        );
    }

    #[test]
    fn peaceful_magic_override_rejects_display_level_and_indirect_casts() {
        let _dg = crate::lock_ok::lock(&crate::dg_handler::DG_TEST_LOCK);
        crate::dg_handler::boot_handler();

        let (mut display_g, display, _, display_conn) =
            authority_magic_fixture(RoomFlags::PEACEFUL, LVL_IMPL, 1);
        crate::interpreter::run_authenticated_command(
            &mut display_g,
            display,
            "cast 'curse' Target",
        );
        assert!(
            display_g.descriptors[&display_conn]
                .outbuf
                .contains("dispelling your violent magic"),
            "{}",
            display_g.descriptors[&display_conn].outbuf
        );

        let (mut trusted_g, trusted, _, trusted_conn) =
            authority_magic_fixture(RoomFlags::PEACEFUL, 50, i32::from(LVL_IMPL));
        crate::interpreter::run_authenticated_command(
            &mut trusted_g,
            trusted,
            "cast 'curse' Target",
        );
        assert!(
            !trusted_g.descriptors[&trusted_conn]
                .outbuf
                .contains("dispelling your violent magic"),
            "{}",
            trusted_g.descriptors[&trusted_conn].outbuf
        );
        assert!(
            trusted_g.descriptors[&trusted_conn].outbuf.contains("Ok."),
            "direct Implementor input never reached the cast pipeline: {:?}",
            trusted_g.descriptors[&trusted_conn].outbuf
        );

        let (mut indirect_g, indirect, target, indirect_conn) =
            authority_magic_fixture(RoomFlags::PEACEFUL, 50, i32::from(LVL_IMPL));
        assert_eq!(
            call_magic(
                &mut indirect_g,
                indirect,
                Some(target),
                None,
                SPELL_CURSE,
                50,
            ),
            0
        );
        assert!(
            indirect_g.descriptors[&indirect_conn]
                .outbuf
                .contains("dispelling your violent magic")
        );
    }

    #[test]
    fn expiring_affect_prints_wear_off_message_and_is_removed() {
        // SPELL_ARMOR (1) wears off with "You feel less protected."
        let mut g = GameState::new(Config::default());
        let mut ch =
            Character::new_player("Af".to_string(), Class::Warrior, crate::types::Race::Human);
        ch.affected.push(spell_affect(SPELL_ARMOR, 1));
        let conn = ConnId(91);
        ch.desc = Some(conn);
        let cid = g.create_char(ch);

        g.descriptors
            .insert(conn, Descriptor::new(conn, "example.test".to_string()));
        g.descriptors.get_mut(&conn).unwrap().character = Some(cid);

        crate::magic::affect_update(&mut g);

        // C magic.c:75: duration 1 only decrements to 0 on this pass; the
        // expired affect (and its wear-off message) are handled next pass.
        let c = g.get_char(cid).unwrap();
        assert_eq!(c.affected.len(), 1);
        assert_eq!(c.affected[0].duration, 0);
        assert!(
            !g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("less protected")
        );

        crate::magic::affect_update(&mut g);

        let c = g.get_char(cid).unwrap();
        assert!(c.affected.is_empty(), "second pass removes the affect");
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(
            out.contains("You feel less protected."),
            "wear-off message missing, got: {out:?}"
        );
    }

    #[test]
    fn permanent_affect_never_expires() {
        let mut g = GameState::new(Config::default());
        let mut ch = Character::new_player(
            "Perm".to_string(),
            Class::Warrior,
            crate::types::Race::Human,
        );
        ch.affected.push(spell_affect(SPELL_ARMOR, -1));
        ch.affected.push(spell_affect(SPELL_BLESS, 3));
        let cid = g.create_char(ch);

        crate::magic::affect_update(&mut g);

        let c = g.get_char(cid).unwrap();
        assert_eq!(
            c.affected.len(),
            2,
            "permanent stays; ticking only decrements"
        );
        assert_eq!(c.affected[0].duration, -1);
        assert_eq!(c.affected[1].spell_type, SPELL_BLESS);
        assert_eq!(c.affected[1].duration, 2);
    }

    #[test]
    fn ticking_affect_only_decrements() {
        let mut g = GameState::new(Config::default());
        let mut ch = Character::new_player(
            "Tick".to_string(),
            Class::Warrior,
            crate::types::Race::Human,
        );
        ch.affected.push(spell_affect(SPELL_BLESS, 5));
        let cid = g.create_char(ch);

        crate::magic::affect_update(&mut g);

        let c = g.get_char(cid).unwrap();
        assert_eq!(c.affected[0].duration, 4);
        assert_eq!(c.affected.len(), 1);
    }
}
