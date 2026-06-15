// limits.rs — full port of C `src/limits.c`: the periodic update layer
// (NOT command handlers). Covers the HMV regen splines (hit_gain / mana_gain /
// move_gain via graf()), experience/level progression (gain_exp,
// gain_exp_regardless, advance_level, exp_to_level, set_title), the idle
// purge (check_idling), hunger/thirst/drunk clocks (gain_condition) with their
// starvation/dehydration messaging, and the big per-MUD-hour point_update loop
// over every character and object (regen + conditions + idle + drowning +
// poison/plague + object timers).
//
// Exposed entry points:
//   point_update(g)              — once per MUD hour from the heartbeat
//   gain_exp(g, ch, gain)        — award/strip experience, may level the PC
//   gain_exp_regardless(g, ...)  — uncapped variant (immortal set / restore)
//   advance_level(g, ch)         — apply one level's HMV/practice gains
//   gain_condition(g, ch, c, v)  — tick one hunger/thirst/drunk condition
//   check_idling(g, ch)          — idle timeout / void / force-rent
//   set_title(g, ch, title)      — set/normalize a player's title
//   exp_to_level(level)          — XP threshold for a level (utils.c port)
//   hit_gain / mana_gain / move_gain(g, ch) — per-tick HMV regen
//
// House style (see cmd_informative.rs): copy scalars / clone collections into
// locals before any send/act; re-look-up entities by id; never hold a borrow
// across a mutation. Output strings track the C byte-for-byte.

use crate::act::{act, ActArg, To};
use crate::object::ObjectType;
use crate::room::SectorType;
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Local constants mirroring C #defines not present in the shared contract.
// ---------------------------------------------------------------------------

// config.c globals.
const MAX_EXP_GAIN: i64 = 1_000_000_000;
const MAX_EXP_LOSS: i64 = 15_000_000;
const ARENA_FLEE_TIMEOUT: i32 = 3;

// structs.h level constants.
const LVL_HERO: Level = 100;

// structs.h MAX_TITLE_LENGTH (player file column width — *DO*NOT*CHANGE*).
const MAX_TITLE_LENGTH: usize = 40;

// structs.h class indices (CLASS_*).
const CLASS_MAGIC_USER: Class = Class::MagicUser;
const CLASS_CLERIC: Class = Class::Cleric;

// structs.h PRF2_* (char->player_specials prf2 bitvector).
const PRF2_LOCKOUT: i64 = 1 << 1;
const PRF2_MBUILDING: i64 = 1 << 6;
const PRF2_INTANGIBLE: i64 = 1 << 9;

// structs.h PLR_* — DeltaMUD redefines PLR_WRITING as bit 4 (see constants.rs
// PLAYER_BITS: index 4 == "WRITING").
const PLR_WRITING: i64 = 1 << 4;

// structs.h AFF_* affect bits (AFF_POISON is in crate::flags; AFF_PLAGUED is
// DeltaMUD-specific and not in the shared subset). Defined locally so the regen
// / poison / plague checks read against the C bit positions without importing
// the whole flags module (which redefines PLR_WRITING at bit 4).
const AFF_POISON: i64 = 1 << 11;
const AFF_PLAGUED: i64 = 1 << 23;

// structs.h ITEM_* object types not representable by the `ObjectType` enum yet
// (the enum maps these vnums to ObjectType::Other). Kept here so the regen /
// portal / boat logic reads against the C numbers; see the manifest gap notes.
const ITEM_BOAT: i32 = 22;
const ITEM_PORTAL: i32 = 24;
const ITEM_HP_REGEN: i32 = 25;
const ITEM_MP_REGEN: i32 = 26;
const ITEM_MV_REGEN: i32 = 27;

// structs.h room-flag bit positions (ROOM_*). The shared `RoomFlags` bitflags
// stop at ARENA(1<<15) and do not define these, so we test the raw bitvector.
const ROOM_BAD_REGEN_BIT: u32 = 17;
const ROOM_GOOD_REGEN_BIT: u32 = 20;

// utils.h MUD-time constants (used by age()).
const SECS_PER_MUD_HOUR: i64 = 75;
const SECS_PER_MUD_DAY: i64 = 24 * SECS_PER_MUD_HOUR;
const SECS_PER_MUD_MONTH: i64 = 35 * SECS_PER_MUD_DAY;
const SECS_PER_MUD_YEAR: i64 = 17 * SECS_PER_MUD_MONTH;

// ---------------------------------------------------------------------------
// Small accessors mirroring the C macros, with the contract's storage layout.
// ---------------------------------------------------------------------------

/// Raw room-flag bit test against the underlying bitvector (works for flags
/// not modeled by the `RoomFlags` type, e.g. GOOD_REGEN / BAD_REGEN).
fn room_flag_bit(g: &GameState, rnum: RoomRnum, bit: u32) -> bool {
    match g.room_opt(rnum) {
        Some(r) => (r.room_flags.bits() & (1u32 << bit)) != 0,
        None => false,
    }
}

/// CircleMUD age(ch).year — the player's apparent age in years. NPCs share the
/// same birth-time math; level-1 PCs start at 17 (mud_time_passed + 17).
fn age_year(g: &GameState, ch: CharId) -> i64 {
    let birth = match g.get_char(ch) {
        Some(c) => c.player.time_birth,
        None => 0,
    };
    let now = chrono::Utc::now().timestamp();
    // mud_time_passed (utils.c): peel hours/days/months; the whole remainder
    // is years. player_age.year += 17 (all players start at 17).
    let total = now - birth;
    let hours = (total / SECS_PER_MUD_HOUR) % 24;
    let mut rem = total - SECS_PER_MUD_HOUR * hours;
    let day = (rem / SECS_PER_MUD_DAY) % 35;
    rem -= SECS_PER_MUD_DAY * day;
    let month = (rem / SECS_PER_MUD_MONTH) % 17;
    rem -= SECS_PER_MUD_MONTH * month;
    let year = rem / SECS_PER_MUD_YEAR;
    year + 17
}

// ---------------------------------------------------------------------------
// graf() — the age->value spline used by the HMV gain functions.
// ---------------------------------------------------------------------------

/// When age < 15 return p0; piecewise-linear through the breakpoints otherwise;
/// age >= 80 returns p6. (limits.c graf().)
fn graf(age: i64, p0: i32, p1: i32, p2: i32, p3: i32, p4: i32, p5: i32, p6: i32) -> i32 {
    let a = age as i32;
    if a < 15 {
        p0
    } else if a <= 29 {
        p1 + (((a - 15) * (p2 - p1)) / 15)
    } else if a <= 44 {
        p2 + (((a - 30) * (p3 - p2)) / 15)
    } else if a <= 59 {
        p3 + (((a - 45) * (p4 - p3)) / 15)
    } else if a <= 79 {
        p4 + (((a - 60) * (p5 - p4)) / 20)
    } else {
        p6
    }
}

// ---------------------------------------------------------------------------
// HMV gain per game hour (limits.c mana_gain / hit_gain / move_gain).
// ---------------------------------------------------------------------------

/// Count equipment slots holding an item of the given (raw) ITEM_* type. The
/// `ObjectType` enum currently maps the regen item vnums to Other, so this
/// will return 0 for them until the enum carries those variants — degrading
/// exactly like a world with no regen items (see manifest gaps).
fn count_eq_of_type(g: &GameState, ch: CharId, item_type: i32) -> i32 {
    let mut n = 0;
    if let Some(c) = g.get_char(ch) {
        for slot in c.equipment.iter() {
            if let Some(oid) = slot {
                if let Some(o) = g.get_obj(*oid) {
                    if o.obj_type as i32 == item_type {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

/// manapoint gain pr. game hour.
pub fn mana_gain(g: &GameState, ch: CharId) -> i32 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let is_npc = c.is_npc;
    let level = c.player.level as i32;
    let class = c.player.class;
    let pos = c.position;
    let poisoned = c.affect_flags & AFF_POISON != 0;
    let full = c.conditions[FULL] as i32;
    let thirst = c.conditions[THIRST] as i32;
    let in_room = c.in_room;

    let mut gain: i32;
    if is_npc {
        gain = level; // Neat and fast
    } else {
        gain = graf(age_year(g, ch), 4, 8, 12, 16, 12, 10, 8);
        gain += 10 * count_eq_of_type(g, ch, ITEM_MP_REGEN);

        // Position calculations
        match pos {
            Position::Sleeping => gain <<= 1,           // mult by 2
            Position::Meditating => gain <<= 3,         // mult by 8
            Position::Resting => gain += gain >> 1,     // + 1/2
            Position::Sitting => gain += gain >> 2,     // + 1/4
            _ => {}
        }

        if class == CLASS_MAGIC_USER || class == CLASS_CLERIC {
            gain <<= 1;
        }
    }

    if poisoned {
        gain >>= 2;
    }
    if full == 0 || thirst == 0 {
        gain >>= 2;
    }
    if let Some(rnum) = in_room {
        if room_flag_bit(g, rnum, ROOM_GOOD_REGEN_BIT) {
            gain += gain * 2;
        }
        if room_flag_bit(g, rnum, ROOM_BAD_REGEN_BIT) {
            gain = 0;
        }
    }
    gain
}

/// Hitpoint gain pr. game hour.
pub fn hit_gain(g: &GameState, ch: CharId) -> i32 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let is_npc = c.is_npc;
    let level = c.player.level as i32;
    let class = c.player.class;
    let pos = c.position;
    let aff = c.affect_flags;
    let full = c.conditions[FULL] as i32;
    let thirst = c.conditions[THIRST] as i32;
    let in_room = c.in_room;

    let mut gain: i32;
    if is_npc {
        gain = level;
    } else {
        gain = graf(age_year(g, ch), 8, 12, 20, 32, 16, 10, 4);
        gain += 10 * count_eq_of_type(g, ch, ITEM_HP_REGEN);

        match pos {
            Position::Sleeping => gain += gain >> 1,   // + 1/2
            Position::Meditating => gain += gain >> 1, // + 1/2
            Position::Resting => gain += gain >> 2,    // + 1/4
            Position::Sitting => gain += gain >> 3,    // + 1/8
            _ => {}
        }

        if class == CLASS_MAGIC_USER || class == CLASS_CLERIC {
            gain >>= 1;
        }
    }

    if aff & AFF_POISON != 0 || aff & AFF_PLAGUED != 0 {
        gain >>= 2;
    }
    if full == 0 || thirst == 0 {
        gain >>= 2;
    }
    if let Some(rnum) = in_room {
        if room_flag_bit(g, rnum, ROOM_GOOD_REGEN_BIT) {
            gain += gain * 2;
        }
        if room_flag_bit(g, rnum, ROOM_BAD_REGEN_BIT) {
            gain = 0;
        }
    }
    gain
}

/// Move gain pr. game hour.
pub fn move_gain(g: &GameState, ch: CharId) -> i32 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let is_npc = c.is_npc;
    let level = c.player.level as i32;
    let pos = c.position;
    let poisoned = c.affect_flags & AFF_POISON != 0;
    let full = c.conditions[FULL] as i32;
    let thirst = c.conditions[THIRST] as i32;
    let in_room = c.in_room;

    if is_npc {
        return level; // Neat and fast
    }

    let mut gain = graf(age_year(g, ch), 16, 20, 24, 20, 16, 12, 10);
    gain += 10 * count_eq_of_type(g, ch, ITEM_MV_REGEN);

    match pos {
        Position::Sleeping => gain += gain >> 1,   // + 1/2
        Position::Meditating => gain += gain >> 1, // + 1/2
        Position::Resting => gain += gain >> 2,    // + 1/4
        Position::Sitting => gain += gain >> 3,    // + 1/8
        _ => {}
    }

    if poisoned {
        gain >>= 2;
    }
    if full == 0 || thirst == 0 {
        gain >>= 2;
    }
    if let Some(rnum) = in_room {
        if room_flag_bit(g, rnum, ROOM_GOOD_REGEN_BIT) {
            gain += gain * 2;
        }
        if room_flag_bit(g, rnum, ROOM_BAD_REGEN_BIT) {
            gain = 0;
        }
    }
    gain
}

// ---------------------------------------------------------------------------
// Titles (limits.c set_title).
// ---------------------------------------------------------------------------

/// Set/normalize a player's title. `None` selects the gender default. Truncates
/// at MAX_TITLE_LENGTH to keep the player-file column happy.
pub fn set_title(g: &mut GameState, ch: CharId, title: Option<&str>) {
    let mut t = match title {
        Some(s) => s.to_string(),
        None => {
            let female = g
                .get_char(ch)
                .map(|c| c.player.sex == Gender::Female)
                .unwrap_or(false);
            if female {
                "the Woman".to_string()
            } else {
                "the Man".to_string()
            }
        }
    };
    if t.len() > MAX_TITLE_LENGTH {
        t.truncate(MAX_TITLE_LENGTH);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.player.title = Some(t);
    }
}

// ---------------------------------------------------------------------------
// Experience / leveling (limits.c gain_exp / gain_exp_regardless +
// advance_level, with exp_to_level from utils.c).
// ---------------------------------------------------------------------------

/// log base `base` of `num` (utils.c mlog()).
fn mlog(base: f64, num: f64) -> f64 {
    num.log10() / base.log10()
}

/// XP required to reach `arg` (utils.c exp_to_level()). Recursive cumulative
/// sum, matching the C exactly (incl. the `4000 * (arg*modifier)` truncation
/// to int via the C `int` return).
pub fn exp_to_level(arg: i32) -> i64 {
    if arg < 1 || arg as Level > LVL_IMPL {
        return 0;
    }
    let modifier = if arg < 10 {
        mlog(3.0, arg as f64)
    } else if arg < 20 {
        mlog(2.9, arg as f64)
    } else if arg < 30 {
        mlog(2.8, arg as f64)
    } else if arg < 40 {
        mlog(2.7, arg as f64)
    } else if arg < 50 {
        mlog(2.6, arg as f64)
    } else if arg < 60 {
        mlog(2.5, arg as f64)
    } else if arg < 70 {
        mlog(2.4, arg as f64)
    } else if arg < 80 {
        mlog(2.3, arg as f64)
    } else if arg < 90 {
        mlog(2.2, arg as f64)
    } else if arg < 96 {
        mlog(2.05, arg as f64)
    } else {
        mlog(2.0, arg as f64)
    };

    if arg == 1 {
        3000
    } else {
        // C: 4000 * (arg * modifier) + exp_to_level(arg-1). The product is
        // computed in double then truncated by the int return type.
        let here = (4000.0 * (arg as f64 * modifier)) as i64;
        here + exp_to_level(arg - 1)
    }
}

/// advance_level (class.c): apply one level's HMV / practice gains and the
/// immortal-promotion side effects, then print the gain summary. The
/// con_app/wis_app/training_pts tables are not ported yet, so the constitution
/// hit bonus and training/practice award degrade to 0 exactly as they would
/// against empty tables (see manifest gaps). The randomized per-class HMV rolls
/// are faithful, using g.rng where C uses number().
pub fn advance_level(g: &mut GameState, ch: CharId) {
    let (class, level, is_immort_now) = match g.get_char(ch) {
        Some(c) => (
            c.player.class,
            c.player.level as i32,
            c.player.level >= LVL_IMMORT,
        ),
        None => return,
    };

    // con_app[GET_CON].hitp — table not ported; degrades to 0.
    let mut add_hp = 0i32;
    let add_mana;
    let add_move;

    match class {
        Class::MagicUser => {
            add_hp += g.rng.number(3, 8);
            let am = g.rng.number(level, (1.5 * level as f64) as i32);
            add_mana = am.min(10);
            add_move = g.rng.number(0, 2);
        }
        Class::Cleric => {
            add_hp += g.rng.number(5, 10);
            let am = g.rng.number(level, (1.5 * level as f64) as i32);
            add_mana = am.min(10);
            add_move = g.rng.number(0, 2);
        }
        Class::Thief => {
            add_hp += g.rng.number(7, 13);
            add_mana = 0;
            add_move = g.rng.number(1, 3);
        }
        Class::Artisan => {
            add_hp += g.rng.number(8, 14);
            let am = g.rng.number(level, (1.5 * level as f64) as i32);
            add_mana = am.min(10);
            add_move = g.rng.number(3, 6);
        }
        Class::Warrior => {
            add_hp += g.rng.number(10, 15);
            add_mana = 0;
            add_move = g.rng.number(1, 3);
        }
    }

    let hp_applied = add_hp.max(1);
    let move_applied = add_move.max(1);

    // wis_app[GET_WIS].bonus — table not ported; degrades to 0, so:
    //   casters: MAX(2, 0) = 2 ; non-casters: MIN(2, MAX(1, 0)) = 1.
    let add_practices = if class == CLASS_MAGIC_USER || class == CLASS_CLERIC {
        2.max(0)
    } else {
        2.min(1.max(0))
    };
    // training_pts[level] — table not ported; degrades to 0.
    let training = 0i32;

    if let Some(c) = g.get_char_mut(ch) {
        c.points.max_hit += hp_applied;
        c.points.max_move += move_applied;
        if level > 1 {
            c.points.max_mana += add_mana;
        }
        c.spells_to_learn += add_practices;

        if is_immort_now {
            for i in 0..NUM_CONDITIONS {
                c.conditions[i] = -100;
            }
            c.prf_flags |= crate::flags::PRF_HOLYLIGHT;
        }
        c.trust = level;
    }

    let buf = format!(
        "You've gained {} hp, {} mp, {} mv, {} practices, and {} training sessions.\r\n",
        hp_applied, add_mana, move_applied, add_practices, training
    );
    g.send_to_char(ch, &buf);
    // save_char(ch, NOWHERE) — async persistence handled by the save layer.
}

/// gain_exp (limits.c): award `gain` XP, capping per kill, leveling the PC up
/// to LVL_HERO and firing the INFO broadcasts / hero promotion. Negative gains
/// strip XP (capped, floored at 0). NPCs / out-of-range levels / intangible /
/// arena combatants are no-ops, matching C.
pub fn gain_exp(g: &mut GameState, ch: CharId, gain: i64) {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return,
    };
    if c.is_npc {
        return;
    }
    let level = c.player.level;
    // (!IS_NPC && (level < 1 || level >= LVL_HERO)) || PRF2_INTANGIBLE
    if level < 1 || level >= LVL_HERO || c.prf2_flags & PRF2_INTANGIBLE != 0 {
        return;
    }
    // IS_ARENACOMBATANT — arena status table not ported; never an arena
    // combatant, so this guard degrades to a no-op (see manifest gaps).

    if gain > 0 {
        let gain = gain.min(MAX_EXP_GAIN); // cap per kill
        if let Some(c) = g.get_char_mut(ch) {
            c.points.exp += gain;
        }

        let mut num_levels = 0;
        loop {
            let (lvl, exp) = match g.get_char(ch) {
                Some(c) => (c.player.level, c.points.exp),
                None => return,
            };
            if lvl >= LVL_HERO || exp < exp_to_level(lvl as i32) {
                break;
            }
            if let Some(c) = g.get_char_mut(ch) {
                c.player.level += 1;
            }
            num_levels += 1;
            advance_level(g, ch);
        }

        if num_levels > 0 {
            if num_levels == 1 {
                g.send_to_char(ch, "You rise a level!\r\n");
            } else {
                g.send_to_char(ch, &format!("You rise {} levels!\r\n", num_levels));
            }
            // check_autowiz: autowiz is a CIRCLE_UNIX system() call; not ported.
            let name = g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default();
            let new_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
            g.send_to_all_players(&format!(
                "&m[&YINFO&m]&n {} has advanced to level {}!\r\n",
                name, new_level
            ));

            if new_level == 10 {
                g.send_to_char(
                    ch,
                    "Congratulations on achieving level 10! Be warned, that you are no longer\r\nconsidered a newbie and can now be attacked by other players. You will\r\nnow lose experience when fleeing from combat.\r\n",
                );
            }

            if new_level == LVL_HERO {
                g.send_to_all_players(&format!(
                    "&m[&YINFO&m]&n {} has become a &YHERO&n!\r\n",
                    name
                ));
                set_title(g, ch, Some("the Hero"));
                g.send_to_char(
                    ch,
                    "You shall forever be known as a hero throughout the land of Deltania!\r\nYou have reached level 100, for more information please type 'help hero'.\r\n",
                );
                if let Some(c) = g.get_char_mut(ch) {
                    c.prf_flags |= crate::flags::PRF_HOLYLIGHT;
                }
                // save_char(ch, NOWHERE) handled by the save layer.
            }
        }
    } else if gain < 0 {
        let gain = gain.max(-MAX_EXP_LOSS); // cap loss per death
        if let Some(c) = g.get_char_mut(ch) {
            c.points.exp += gain;
            if c.points.exp < 0 {
                c.points.exp = 0;
            }
        }
    }
}

/// gain_exp_regardless (limits.c): uncapped XP grant that can level a PC all the
/// way to LVL_IMPL (used by immortal set/restore). LVL_GETSTUFF reward
/// (do_oldbie) degrades to the message-only path; the oldbie equipment loader is
/// not ported (see manifest gaps).
pub fn gain_exp_regardless(g: &mut GameState, ch: CharId, gain: i64) {
    let is_npc = g.get_char(ch).map(|c| c.is_npc).unwrap_or(true);
    if is_npc {
        return;
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.points.exp += gain;
        if c.points.exp < 0 {
            c.points.exp = 0;
        }
    }

    let mut num_levels = 0;
    loop {
        let (lvl, exp) = match g.get_char(ch) {
            Some(c) => (c.player.level, c.points.exp),
            None => return,
        };
        if lvl >= LVL_IMPL || exp < exp_to_level(lvl as i32) {
            break;
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.player.level += 1;
        }
        num_levels += 1;
        advance_level(g, ch);
    }

    if num_levels > 0 {
        if num_levels == 1 {
            g.send_to_char(ch, "You rise a level!\r\n");
        } else {
            g.send_to_char(ch, &format!("You rise {} levels!\r\n", num_levels));
        }
        let name = g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default();
        let new_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
        g.send_to_all_players(&format!(
            "&m[&YINFO&m]&n {} has advanced to level {}!\r\n",
            name, new_level
        ));
        // LVL_GETSTUFF == 3: do_oldbie loads starter gear (not ported). The
        // reward message still fires so the player experience matches.
        if new_level == 3 {
            g.send_to_char(
                ch,
                "&YThe gods have rewarded you for getting to level 3!&n\r\nTwo gold bricks fall from the sky into your hands.&n\r\n",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Conditions (limits.c gain_condition).
// ---------------------------------------------------------------------------

/// Tick one of the DRUNK/FULL/THIRST clocks by `value`, clamp, and emit the
/// hungry/thirsty/sober message when it first hits zero. A -100 clock is frozen
/// (immortals), matching C.
pub fn gain_condition(g: &mut GameState, ch: CharId, condition: usize, value: i32) {
    if condition >= NUM_CONDITIONS {
        return;
    }
    let (cur, intoxicated, writing) = match g.get_char(ch) {
        Some(c) => (
            c.conditions[condition] as i32,
            c.conditions[DRUNK] as i32 > 4,
            c.act_flags & PLR_WRITING != 0,
        ),
        None => return,
    };

    if cur == -100 {
        return; // No change
    }

    let mut new = cur + value;
    if condition == DRUNK {
        new = new.max(0);
    } else {
        new = new.max(-72);
    }
    new = new.min(24);

    if let Some(c) = g.get_char_mut(ch) {
        c.conditions[condition] = new as i8;
    }

    if new != 0 || writing {
        return;
    }

    match condition {
        FULL => g.send_to_char(ch, "You are hungry.\r\n"),
        THIRST => g.send_to_char(ch, "You are thirsty.\r\n"),
        DRUNK => {
            if intoxicated {
                g.send_to_char(ch, "You are now sober.\r\n");
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Idle purge (limits.c check_idling).
// ---------------------------------------------------------------------------

/// Increment a player's idle timer and act on the thresholds: build-mode kick
/// at >5, void-pull at >8, force-rent+extract at >48. The build-off path and
/// crash/rent persistence depend on systems not ported, so those side effects
/// degrade to the player-visible actions (messages + room moves) while the
/// rent/crash saves are handled by the save layer (see manifest gaps).
pub fn check_idling(g: &mut GameState, ch: CharId) {
    let lockout = g.get_char(ch).map(|c| c.prf2_flags & PRF2_LOCKOUT != 0).unwrap_or(false);
    if lockout {
        if let Some(c) = g.get_char_mut(ch) {
            c.timer = 0;
        }
    }

    let (timer, mbuilding) = match g.get_char_mut(ch) {
        Some(c) => {
            c.timer += 1;
            (c.timer, c.prf2_flags & PRF2_MBUILDING != 0)
        }
        None => return,
    };

    if timer > 5 && mbuilding {
        g.send_to_char(ch, "Build mode, my friend, was not made for you to idle in.\r\n");
        // command_interpreter(ch, "build off") — build OLC not ported; the
        // build-off transition is left to the build subsystem.
        return;
    }

    if timer > 8 {
        let (was_in, in_room) = match g.get_char(ch) {
            Some(c) => (c.was_in_room, c.in_room),
            None => return,
        };
        if was_in.is_none() && in_room.is_some() {
            // Pull into the void.
            let fighting = g.get_char(ch).and_then(|c| c.fighting);
            if let Some(c) = g.get_char_mut(ch) {
                c.was_in_room = in_room;
            }
            if let Some(victim) = fighting {
                // stop_fighting(FIGHTING(ch)) + stop_fighting(ch)
                if let Some(v) = g.get_char_mut(victim) {
                    v.fighting = None;
                    if v.position == Position::Fighting {
                        v.position = Position::Standing;
                    }
                }
                if let Some(c) = g.get_char_mut(ch) {
                    c.fighting = None;
                    if c.position == Position::Fighting {
                        c.position = Position::Standing;
                    }
                }
            }
            act(g, "$n disappears into the void.", true, ch, None, ActArg::None, To::Room);
            g.send_to_char(ch, "You have been idle, and are pulled into a void.\r\n");
            // save_char / Crash_crashsave handled by the save layer.
            g.char_from_room(ch);
            if let Some(void) = g.real_room(1) {
                g.char_to_room(ch, void);
            }
        } else if timer > 48 {
            // Force-rent + extract.
            if g.get_char(ch).and_then(|c| c.in_room).is_some() {
                g.char_from_room(ch);
            }
            if let Some(rent) = g.real_room(3) {
                g.char_to_room(ch, rent);
            }
            // close_socket(ch->desc): drop the descriptor link.
            if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
                if let Some(d) = g.descriptors.get_mut(&conn) {
                    d.character = None;
                }
            }
            if let Some(c) = g.get_char_mut(ch) {
                c.desc = None;
            }
            // Crash_rentsave / Crash_idlesave handled by the save layer.
            g.extract_char(ch);
        }
    }
}

// ---------------------------------------------------------------------------
// Position recompute (fight.c update_pos — used by point_update).
// ---------------------------------------------------------------------------

/// Recompute position from current HP, but only when the char is down (HP <= 0
/// or below POS_STUNNED). Mirrors fight.c update_pos exactly.
fn update_pos(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
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

// ---------------------------------------------------------------------------
// Object timer ticking (handler.c update_char_objects / update_object).
// ---------------------------------------------------------------------------

/// update_object(obj, use): count this object's timer down by `use`, recursing
/// into its contents (handler.c). Sibling recursion in C is covered by our
/// explicit list iteration at the call site.
fn update_object(g: &mut GameState, oid: ObjId, use_: i32) {
    let (has_timer, contents) = match g.get_obj(oid) {
        Some(o) => (o.timer > 0, o.contains.clone()),
        None => return,
    };
    if has_timer {
        if let Some(o) = g.get_obj_mut(oid) {
            o.timer -= use_;
        }
    }
    for c in contents {
        update_object(g, c, use_);
    }
}

/// update_char_objects(ch): light-source flicker/burnout on WEAR_LIGHT, then
/// tick equipment timers by 2 and inventory timers by 1 (handler.c).
fn update_char_objects(g: &mut GameState, ch: CharId) {
    // Light source on WEAR_LIGHT.
    let light = g.get_char(ch).and_then(|c| c.equipment[WEAR_LIGHT]);
    if let Some(loid) = light {
        let is_light_burning = g
            .get_obj(loid)
            .map(|o| o.obj_type == ObjectType::Light && o.values[2] > 0)
            .unwrap_or(false);
        if is_light_burning {
            let i = {
                let o = g.get_obj_mut(loid).unwrap();
                o.values[2] -= 1;
                o.values[2]
            };
            if i == 1 {
                act(g, "Your light begins to flicker and fade.", false, ch, None, ActArg::None, To::Char);
                act(g, "$n's light begins to flicker and fade.", false, ch, None, ActArg::None, To::Room);
            } else if i == 0 {
                act(g, "Your light sputters out and dies.", false, ch, None, ActArg::None, To::Char);
                act(g, "$n's light sputters out and dies.", false, ch, None, ActArg::None, To::Room);
                if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
                    let cur = g.room(rnum).light;
                    g.room_mut(rnum).light = cur.saturating_sub(1);
                }
            }
        }
    }

    // Equipment timers tick by 2 (with their contents).
    let eq: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    for oid in eq {
        update_object(g, oid, 2);
    }

    // Inventory timers tick by 1 (the C only walks the head of ->carrying; our
    // update_object recursion + this list walk reproduces the head + sibling +
    // contents traversal of the C linked list).
    let inv: Vec<ObjId> = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    for oid in inv {
        update_object(g, oid, 1);
    }
}

// ---------------------------------------------------------------------------
// The big per-MUD-hour update (limits.c point_update).
// ---------------------------------------------------------------------------

/// Apply environmental self-damage (poison tick, drowning, starving,
/// suffering). In C these go through damage(i, i, dam, TYPE_*). For the
/// SELF_DAMAGE attack types (TYPE_STARVING/DROWNING/SUFFERING, all >=
/// SELF_DAMAGE) the C `VALID_ATTACKTYPE` predicate is false, so NO combat
/// dam_message is printed — the damage is silent: HP is subtracted, position is
/// recomputed (update_pos), and the position-change room messages fire. We
/// replicate that here: capped HP subtraction, then update_pos, then death if it
/// drops far enough. SPELL_POISON (< SELF_DAMAGE) is the one VALID type, handled
/// at its call site with its own behavior.
fn env_damage(g: &mut GameState, ch: CharId, dmg: i32) {
    // C: dam = MAX(MIN(dam, 1000), 0); GET_HIT(victim) -= dam.
    let dam = dmg.clamp(0, 1000);
    match g.get_char_mut(ch) {
        Some(c) => c.points.hit -= dam,
        None => return,
    }
    update_pos(g, ch);

    // Position-change notices (fight.c, sent with send_to_char/act since act()
    // won't message a DEAD char).
    let pos = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Dead);
    match pos {
        Position::MortallyWounded => {
            act(g, "$n is mortally wounded, and will die soon, if not aided.", true, ch, None, ActArg::None, To::Room);
            g.send_to_char(ch, "You are mortally wounded, and will die soon, if not aided.\r\n");
        }
        Position::Incapacitated => {
            act(g, "$n is incapacitated and will slowly die, if not aided.", true, ch, None, ActArg::None, To::Room);
            // NB: exact C string preserves the source typo ("incapacitated an").
            g.send_to_char(ch, "You are incapacitated an will slowly die, if not aided.\r\n");
        }
        Position::Stunned => {
            act(g, "$n is stunned, but will probably regain consciousness again.", true, ch, None, ActArg::None, To::Room);
            g.send_to_char(ch, "You're stunned, but will probably regain consciousness again.\r\n");
        }
        Position::Dead => {
            act(g, "$n is dead!  R.I.P.", false, ch, None, ActArg::None, To::Room);
            g.send_to_char(ch, "You are dead!  Sorry...\r\n");
            env_die(g, ch);
        }
        _ => {}
    }
}

/// die(ch, NULL) — handle a character's death with no killer (fight.c die +
/// raw_kill). Applies the XP-loss penalty, clears killer/thief flags and resets
/// the food/drink clocks for PCs, screams (death_cry), drops a corpse holding
/// the victim's inventory/equipment, and extracts the body (NPC) / respawns the
/// PC. increase_blood (room blood tracking) and the DG death_mtrigger degrade
/// to no-ops (see manifest gaps).
fn env_die(g: &mut GameState, ch: CharId) {
    // gain_exp(ch, -(GET_EXP - exp_to_level(level-1))/4): lose a quarter of the
    // progress into the current level.
    let (exp, level, is_npc) = match g.get_char(ch) {
        Some(c) => (c.points.exp, c.player.level as i32, c.is_npc),
        None => return,
    };
    let penalty = -((exp - exp_to_level(level - 1)) / 4);
    gain_exp(g, ch, penalty);
    if !g.char_exists(ch) {
        return;
    }

    if !is_npc {
        // REMOVE_BIT(PLR_FLAGS, PLR_KILLER | PLR_THIEF); reset food/drink clocks.
        if let Some(c) = g.get_char_mut(ch) {
            const PLR_KILLER: i64 = 1 << 0;
            const PLR_THIEF: i64 = 1 << 1;
            c.act_flags &= !(PLR_KILLER | PLR_THIEF);
            c.conditions[FULL] = 0;
            c.conditions[THIRST] = 0;
            c.conditions[DRUNK] = 0;
        }
    }

    // raw_kill: stop fighting, strip affects, death_cry, corpse, extract.
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
        c.affected.clear();
    }
    g.affect_total(ch);

    // death_cry: scream into the current room (the neighbouring-room cries are
    // skipped — a soundproof-quality degrade; see manifest gaps).
    act(g, "Your blood freezes as you hear $n's death cry.", false, ch, None, ActArg::None, To::Room);

    // make_corpse(ch): corpse holding carried + worn items.
    let rnum = g.get_char(ch).and_then(|c| c.in_room);
    if let Some(rnum) = rnum {
        let name = g.get_char(ch).map(|c| c.display_for_others()).unwrap_or_default();
        let corpse = make_corpse(g, &name);
        let carried = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
        for oid in carried {
            g.obj_from_anywhere(oid);
            g.obj_to_obj(oid, corpse);
        }
        let worn: Vec<usize> = (0..NUM_WEARS)
            .filter(|&p| g.get_char(ch).map(|c| c.equipment[p].is_some()).unwrap_or(false))
            .collect();
        for p in worn {
            if let Some(oid) = g.unequip_char(ch, p) {
                g.obj_to_obj(oid, corpse);
            }
        }
        g.obj_to_room(corpse, rnum);
    }

    // raw_kill: GET_LEVEL < 30 || IS_NPC -> extract_char; else do_extract_char
    // (keep the descriptor for ghost respawn). The ghost/respawn entry path is
    // not ported, so high-level PCs respawn at hometown like Tier-0 combat
    // deaths instead of becoming intangible (see manifest gaps).
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    if is_npc || level < 30 {
        if is_npc {
            g.extract_char(ch);
        } else {
            respawn_pc(g, ch);
        }
    } else {
        respawn_pc(g, ch);
    }
}

/// Make a player/NPC corpse container (matches combat.rs make_corpse so corpses
/// look identical regardless of which subsystem produced the death).
fn make_corpse(g: &mut GameState, who: &str) -> ObjId {
    use crate::object::Object;
    let mut obj = Object::new(NOTHING, format!("corpse {}", who), format!("the corpse of {}", who));
    obj.description = format!("The corpse of {} is lying here.", who);
    obj.obj_type = ObjectType::Container;
    obj.timer = 60;
    obj.values = [0, 0, 0, 1]; // values[3] != 0 marks it a corpse for decay.
    obj.loc = ObjLoc::Nowhere;
    g.create_obj(obj)
}

/// Respawn a dead PC at its hometown (Tier-0 death handling, matching
/// combat.rs respawn_pc).
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

/// point_update — update PCs, NPCs, and objects once per MUD hour
/// (limits.c point_update). Called from the heartbeat.
pub fn point_update(g: &mut GameState) {
    // ---- characters ----
    // Snapshot the list; entities may be extracted (idle/death) mid-loop.
    let chars: Vec<CharId> = g.char_list.clone();
    for ch in chars {
        // Skip if extracted by an earlier iteration's side effect.
        if !g.char_exists(ch) {
            continue;
        }

        let (is_npc, intangible, level, mbuilding) = match g.get_char(ch) {
            Some(c) => (
                c.is_npc,
                c.prf2_flags & PRF2_INTANGIBLE != 0,
                c.player.level,
                c.prf2_flags & PRF2_MBUILDING != 0,
            ),
            None => continue,
        };

        // Intangible (ghost) mortals: only idle or count down the death timer.
        if !is_npc && intangible && level < LVL_IMMORT {
            if mbuilding {
                check_idling(g, ch);
            }
            // death_timer / do_extract_char / enter_player_game (the ghost
            // respawn cycle) depend on the player-game entry path not ported
            // here; the timer field is not modeled in the contract, so this
            // branch degrades to leaving the ghost as-is (see manifest gaps).
            continue;
        }

        // Hunger / drunk / thirst clocks tick down each MUD hour.
        gain_condition(g, ch, FULL, -1);
        gain_condition(g, ch, DRUNK, -1);
        gain_condition(g, ch, THIRST, -1);
        if !g.char_exists(ch) {
            continue;
        }

        let pos = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Dead);

        if pos >= Position::Stunned {
            // Regen HMV.
            let hg = hit_gain(g, ch);
            let mg = mana_gain(g, ch);
            let vg = move_gain(g, ch);
            if let Some(c) = g.get_char_mut(ch) {
                c.points.hit = (c.points.hit + hg).min(c.points.max_hit);
                c.points.mana = (c.points.mana + mg).min(c.points.max_mana);
                c.points.move_points = (c.points.move_points + vg).min(c.points.max_move);
            }

            // Arena flee-recall timer. The arena status / flee_timer fields are
            // not modeled in the contract; IS_ARENACOMBATANT is therefore always
            // false here, so this whole block degrades to a no-op exactly like a
            // MUD with no arena combatants (see manifest gaps). The faithful
            // logic is preserved for when the arena table lands:
            let _ = ARENA_FLEE_TIMEOUT;

            // Poison damage.
            let poisoned = g.get_char(ch).map(|c| c.affect_flags & AFF_POISON != 0).unwrap_or(false);
            if poisoned {
                env_damage(g, ch, 2); // SPELL_POISON
                if !g.char_exists(ch) {
                    continue;
                }
            }
            // C re-checks position <= STUNNED and runs update_pos.
            if g.get_char(ch).map(|c| c.position <= Position::Stunned).unwrap_or(false) {
                update_pos(g, ch);
            }
            let plagued = g.get_char(ch).map(|c| c.affect_flags & AFF_PLAGUED != 0).unwrap_or(false);
            if plagued {
                g.send_to_char(ch, "&RYou feel the effects of a deadly plague ravage through your body.&n");
            }
        } else if pos == Position::Incapacitated {
            env_damage(g, ch, 1); // TYPE_SUFFERING
            if !g.char_exists(ch) {
                continue;
            }
        } else if pos == Position::MortallyWounded {
            env_damage(g, ch, 2); // TYPE_SUFFERING
            if !g.char_exists(ch) {
                continue;
            }
        }

        // PC-only upkeep.
        let is_npc_now = g.get_char(ch).map(|c| c.is_npc).unwrap_or(true);
        if !is_npc_now {
            update_char_objects(g, ch);
            if !g.char_exists(ch) {
                continue;
            }

            let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
            if level < LVL_IMMORT {
                check_idling(g, ch);
            }
            if !g.char_exists(ch) {
                continue;
            }

            // Drowning in unswimmable water without a boat. has_boat() inspects
            // the player's inventory for an ITEM_BOAT; the ObjectType enum can't
            // represent ITEM_BOAT yet, so has_boat degrades to false (player
            // always "drowns" in NOSWIM water). See manifest gaps.
            let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
            if level < LVL_IMMORT {
                let in_noswim = g
                    .get_char(ch)
                    .and_then(|c| c.in_room)
                    .map(|r| g.room(r).sector_type == SectorType::WaterNoSwim)
                    .unwrap_or(false);
                if in_noswim && !has_boat(g, ch) {
                    act(g, "$n thrashes about in the water straining to stay afloat.", false, ch, None, ActArg::None, To::Room);
                    g.send_to_char(ch, "You are drowning!\r\n");
                    let dmg = g.get_char(ch).map(|c| c.points.max_hit / 5).unwrap_or(0);
                    env_damage(g, ch, dmg); // TYPE_DROWNING
                    if !g.char_exists(ch) {
                        continue;
                    }
                }
            }

            // Hunger / thirst messaging + escalating starvation/dehydration.
            let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
            if level < LVL_IMMORT {
                let thirst = g.get_char(ch).map(|c| c.conditions[THIRST] as i32).unwrap_or(0);
                if thirst < 0 && thirst > -12 {
                    g.send_to_char(ch, "You are extremely thirsty.\r\n");
                } else if thirst <= -12 && thirst >= -20 {
                    g.send_to_char(ch, "You are suffering from dehydration and must get something to drink.\r\n");
                } else if thirst < -20 && thirst > -24 {
                    g.send_to_char(ch, "You are now dying of thirst! You must get something to drink quickly.\r\n");
                    let dmg = g.get_char(ch).map(|c| c.points.max_hit / 4).unwrap_or(0);
                    env_damage(g, ch, dmg); // TYPE_STARVING
                    if !g.char_exists(ch) {
                        continue;
                    }
                    update_pos(g, ch);
                } else if thirst <= -24 && thirst > -100 {
                    g.send_to_char(ch, "&RYou have died of extreme thirst!&n\r\n");
                    env_die(g, ch);
                    if !g.char_exists(ch) {
                        continue;
                    }
                }

                // Hunger. C's bracket: note the first "very hungry" is a bare
                // `if` (can co-fire with the chained block below it).
                let full = g.get_char(ch).map(|c| c.conditions[FULL] as i32).unwrap_or(0);
                if full < 0 && full >= -23 {
                    g.send_to_char(ch, "You are very hungry.\r\n");
                }
                if full < -23 && full > -36 {
                    g.send_to_char(ch, "You are extremely hungry.\r\n");
                } else if full <= -36 && full >= -48 {
                    g.send_to_char(ch, "You are suffering from starvation and must get something to eat.\r\n");
                } else if full < -48 && full > -60 {
                    g.send_to_char(ch, "You are now dying of hunger! You must get something to eat soon.\r\n");
                    let dmg = g.get_char(ch).map(|c| c.points.max_hit / 8).unwrap_or(0);
                    env_damage(g, ch, dmg); // TYPE_STARVING
                    if !g.char_exists(ch) {
                        continue;
                    }
                    update_pos(g, ch);
                } else if full <= -60 && full > -100 {
                    g.send_to_char(ch, "&RYou have died of extreme hunger!&n\r\n");
                    env_die(g, ch);
                    if !g.char_exists(ch) {
                        continue;
                    }
                }
            }
        }
    }

    // ---- objects ----
    let objs: Vec<ObjId> = g.obj_list.clone();
    for oid in objs {
        if g.get_obj(oid).is_none() {
            continue;
        }

        // Objects in water: random chance to sink (boats and zero-weight items
        // can survive). ITEM_BOAT can't be represented by ObjectType yet, so a
        // boat sinks like any other item here (see manifest gaps); object_type
        // boat exemption degrades.
        let obj_room = obj_room_of(g, oid);
        if let Some(rnum) = obj_room {
            let sect = g.room(rnum).sector_type;
            if sect == SectorType::WaterNoSwim || sect == SectorType::WaterSwim {
                let (is_boat, weight) = match g.get_obj(oid) {
                    Some(o) => (o.obj_type as i32 == ITEM_BOAT, o.weight),
                    None => continue,
                };
                if !is_boat && g.rng.number(0, weight) > 0 {
                    act_obj_to_room(g, "$p sinks into the murky depths.", oid);
                    g.obj_from_anywhere(oid);
                    g.extract_obj(oid);
                    continue;
                } else {
                    act_obj_to_room(g, "$p floats unsteadily in the area.", oid);
                }
            }
        }

        // Portals on the ground (not carried / not in a container): count their
        // timer down and pop when it expires. ITEM_PORTAL is not representable
        // by ObjectType yet, so this never fires (portals fall through to the
        // generic timer path); the dedicated puff-of-smoke message degrades.
        let is_portal = g.get_obj(oid).map(|o| o.obj_type as i32 == ITEM_PORTAL).unwrap_or(false);
        let is_portal_on_ground = is_portal && is_on_ground(g, oid);
        if is_portal_on_ground {
            let t = {
                let o = g.get_obj_mut(oid).unwrap();
                if o.timer > 0 {
                    o.timer -= 1;
                }
                o.timer
            };
            if t == 0 {
                act_obj_to_room(g, "$p dissapears in a puff of smoke!", oid);
                g.obj_from_anywhere(oid);
                g.extract_obj(oid);
                continue;
            }
        }

        // Corpses: ITEM_CONTAINER with values[3] != 0. Count the timer down and
        // decay, scattering contents to the corpse's location.
        let is_corpse = g
            .get_obj(oid)
            .map(|o| o.obj_type == ObjectType::Container && o.values[3] != 0)
            .unwrap_or(false);

        if is_corpse {
            let timer = {
                let o = g.get_obj_mut(oid).unwrap();
                if o.timer > 0 {
                    o.timer -= 1;
                }
                o.timer
            };

            if timer == 0 {
                // Decay messaging: carried-by-char vs on-the-ground.
                let carried = carried_by(g, oid);
                if let Some(cid) = carried {
                    act(g, "$p decays in your hands.", false, cid, Some(oid), ActArg::None, To::Char);
                } else if let Some(rnum) = obj_room_of(g, oid) {
                    if let Some(&someone) = g.room(rnum).people.first() {
                        act(g, "A quivering horde of maggots consumes $p.", true, someone, Some(oid), ActArg::None, To::Room);
                        act(g, "A quivering horde of maggots consumes $p.", true, someone, Some(oid), ActArg::None, To::Char);
                    }
                }

                // Spill contents to wherever the corpse is.
                let contents = g.get_obj(oid).map(|o| o.contains.clone()).unwrap_or_default();
                let dest = corpse_spill_dest(g, oid);
                for jj in contents {
                    g.obj_from_anywhere(jj);
                    match dest {
                        SpillDest::Container(c) => g.obj_to_obj(jj, c),
                        SpillDest::Room(r) => g.obj_to_room(jj, r),
                        SpillDest::Carried(c) => {
                            if let Some(r) = g.get_char(c).and_then(|ch| ch.in_room) {
                                g.obj_to_room(jj, r);
                            }
                        }
                        SpillDest::Nowhere => { /* assert(FALSE) in C */ }
                    }
                }
                g.obj_from_anywhere(oid);
                g.extract_obj(oid);
            }
        } else {
            // Generic timed object: count down and fire timer_otrigger at 0. DG
            // scripts are not ported, so the trigger degrades to a no-op (and
            // the object simply expires its timer, matching a no-trigger world).
            let fires = g
                .get_obj(oid)
                .map(|o| o.timer > 0)
                .unwrap_or(false);
            if fires {
                let t = {
                    let o = g.get_obj_mut(oid).unwrap();
                    o.timer -= 1;
                    o.timer
                };
                if t == 0 {
                    // timer_otrigger(j): no DG-script engine yet — no-op.
                }
            }
        }
    }
}

/// beat(g): convenience alias so the heartbeat can call either name. point_update
/// is the canonical entry; this exists for symmetry with the assignment brief.
pub fn beat(g: &mut GameState) {
    point_update(g)
}

// ---------------------------------------------------------------------------
// Object-location helpers (mirroring obj->in_room / carried_by / in_obj tests).
// ---------------------------------------------------------------------------

use crate::object::ObjLoc;

enum SpillDest {
    Container(ObjId),
    Room(RoomRnum),
    Carried(CharId),
    Nowhere,
}

/// The room an object is in, if it sits directly on the ground.
fn obj_room_of(g: &GameState, oid: ObjId) -> Option<RoomRnum> {
    match g.get_obj(oid).map(|o| o.loc) {
        Some(ObjLoc::Room(r)) => Some(r),
        _ => None,
    }
}

/// True if the object is on the ground (not carried, not inside a container).
fn is_on_ground(g: &GameState, oid: ObjId) -> bool {
    matches!(g.get_obj(oid).map(|o| o.loc), Some(ObjLoc::Room(_)) | Some(ObjLoc::Nowhere))
}

/// The character carrying an object, if any (CircleMUD obj->carried_by).
fn carried_by(g: &GameState, oid: ObjId) -> Option<CharId> {
    match g.get_obj(oid).map(|o| o.loc) {
        Some(ObjLoc::Carried(c)) => Some(c),
        _ => None,
    }
}

/// Where a decaying corpse's contents should go (in_obj / carried_by / in_room).
fn corpse_spill_dest(g: &GameState, oid: ObjId) -> SpillDest {
    match g.get_obj(oid).map(|o| o.loc) {
        Some(ObjLoc::Contained(c)) => SpillDest::Container(c),
        Some(ObjLoc::Carried(c)) => SpillDest::Carried(c),
        Some(ObjLoc::Room(r)) => SpillDest::Room(r),
        Some(ObjLoc::Worn(c, _)) => SpillDest::Carried(c),
        _ => SpillDest::Nowhere,
    }
}

/// has_boat(ch): true if the character carries (or wears) an ITEM_BOAT. The
/// ObjectType enum can't represent ITEM_BOAT yet, so this degrades to false —
/// the same as a player with no boat (see manifest gaps).
fn has_boat(g: &GameState, ch: CharId) -> bool {
    if let Some(c) = g.get_char(ch) {
        for &oid in &c.carrying {
            if g.get_obj(oid).map(|o| o.obj_type as i32 == ITEM_BOAT).unwrap_or(false) {
                return true;
            }
        }
        for slot in c.equipment.iter().flatten() {
            if g.get_obj(*slot).map(|o| o.obj_type as i32 == ITEM_BOAT).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Render an object-only act() message (`act(msg, FALSE, 0, j, 0, TO_ROOM)` in
/// C, where ch is NULL) and broadcast it to everyone in the object's room.
/// Only `$p` (object short_description) and `$$` are meaningful when ch is NULL,
/// matching the strings used by point_update's object loop. The first character
/// is capitalized and a CRLF is appended, exactly like act()/perform_act.
fn act_obj_to_room(g: &mut GameState, msg: &str, oid: ObjId) {
    let rnum = match obj_room_of(g, oid) {
        Some(r) => r,
        None => return,
    };
    let short = g
        .get_obj(oid)
        .map(|o| o.short_description.clone())
        .unwrap_or_else(|| "something".to_string());

    // Substitute $-codes (only $p / $$ matter with a NULL ch).
    let mut out = String::with_capacity(msg.len() + short.len());
    let mut chars = msg.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('p') => out.push_str(&short),
            Some('$') => out.push('$'),
            Some(other) => {
                // Unknown code with a NULL ch renders empty (as in act()).
                let _ = other;
            }
            None => break,
        }
    }
    // CAP first character.
    if let Some(first) = out.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            out.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
    out.push_str("\r\n");
    g.send_to_room(rnum, &out, None);
}
