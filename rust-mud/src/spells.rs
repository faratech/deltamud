// spells.rs — the "manual" (ASPELL) spells (CircleMUD spells.c), the ones whose
// effect is too bespoke for the mag_* templates. Each has the canonical spell
// routine signature `fn(&mut GameState, level, ch, victim, obj)` and is invoked
// from magic::call_magic's MAG_MANUAL switch.
//
// Arena is ported (arena.rs), so the arena branches in the location-changing
// spells are live: spell_recall and spell_home call arena::arena_recall (a
// combatant concedes / is rebounded to the prep room, an observer bounces to
// the observatory), and spell_summon / spell_portal enforce the arena-boundary
// guard via arena::arena_stat. spell_retreat's arena branch is dead code in C
// too (load_room is hard-coded 0, so the "must rent first" check always fires
// first), so it stays a faithful "must rent first" no-op. House ownership IS
// modelled (house.rs): spell_home teleports the caster to their owned house via
// house::house_for_owner().

use crate::act::{act, ActArg, To};
use crate::character::Affect;
use crate::flags::{
    APPLY_CON, APPLY_DEFENSE, APPLY_DEX, APPLY_HIT, APPLY_INT, APPLY_MANA, APPLY_MDEFENSE,
    APPLY_MOVE, APPLY_MPOWER, APPLY_NONE, APPLY_POWER, APPLY_STR, APPLY_TECHNIQUE, APPLY_WIS,
    PRF2_INTANGIBLE,
};
use crate::object::{ObjLoc, ObjectType};
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;

use crate::cmd_informative::look_at_room;
use crate::config::{MORTAL_START_ROOM, NUM_STARTROOMS};
use crate::constants::{AFFECTED_BITS, DRINKS, EXTRA_BITS};
use crate::spell_parser::*;

// ---------------------------------------------------------------------------
// Flag / type constants not in the shared subset (structs.h). Local privates.
// ---------------------------------------------------------------------------
const AFF_POISON: i64 = 1 << 11;
const AFF_SANCTUARY: i64 = 1 << 7;
const AFF_CHARM: i64 = 1 << 21;
const AFF_NOPORTAL: i64 = 1 << 22;

const MOB_AGGRESSIVE: i64 = 1 << 5;
const MOB_SPEC: i64 = 1 << 0;
const MOB_NOCHARM: i64 = 1 << 13;
const MOB_NOSUMMON: i64 = 1 << 14;

const PRF_SUMMONABLE: i64 = 1 << 10;

const ITEM_MAGIC: u64 = 1 << 6;
const ITEM_ANTI_EVIL: u64 = 1 << 10;
const ITEM_ANTI_GOOD: u64 = 1 << 9;

const ROOM_NOMAGIC: u32 = 1 << 7;

const LIQ_WATER: i32 = 0;
const LIQ_SLIME: i32 = 9;

const MAX_OBJ_AFFECT: usize = 6;
const MAX_PLAYER_STAT: i32 = 18;

const PORTAL_VNUM: ObjVnum = 20;
const LVL_HERO_IDENTIFY: Level = 100;
const SECS_PER_MUD_HOUR: i64 = 75;
const SECS_PER_MUD_DAY: i64 = 24 * SECS_PER_MUD_HOUR;
const SECS_PER_MUD_MONTH: i64 = 35 * SECS_PER_MUD_DAY;
const SECS_PER_MUD_YEAR: i64 = 17 * SECS_PER_MUD_MONTH;

// ---------------------------------------------------------------------------
// Small utils.h-style helpers.
// ---------------------------------------------------------------------------
fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}
fn get_level(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(1)
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
fn prf_flagged(g: &GameState, ch: CharId, flag: i64) -> bool {
    g.get_char(ch)
        .map(|c| c.prf_flags & flag != 0)
        .unwrap_or(false)
}
fn is_good(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.alignment >= 350).unwrap_or(false)
}
fn is_evil(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.alignment <= -350).unwrap_or(false)
}
fn fname(namelist: &str) -> String {
    namelist.split_whitespace().next().unwrap_or("").to_string()
}
fn pers(g: &GameState, viewer: CharId, target: CharId) -> String {
    if g.can_see(viewer, target) {
        if let Some(c) = g.get_char(target) {
            return if c.is_npc {
                c.short_desc
                    .clone()
                    .unwrap_or_else(|| c.player.name.clone())
            } else {
                c.player.name.clone()
            };
        }
    }
    "someone".to_string()
}
fn cap_first(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(f) = out.chars().next() {
        if f.is_ascii_lowercase() {
            let up = f.to_ascii_uppercase();
            out.replace_range(0..f.len_utf8(), &up.to_string());
        }
    }
    out
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

/// mag_savingthrow (magic.c): `number(0,100) > chance(ch, victim, 1)` — the
/// magic save routes through the same canonical DeltaMUD `chance()` as melee.
fn mag_savingthrow(g: &mut GameState, ch: CharId, victim: CharId) -> bool {
    let roll = g.rng.number(0, 100);
    roll > crate::combat::chance(g, ch, victim, 1)
}

/// Resolve a player's home town index to a `MORTAL_START_ROOM[]` table slot,
/// applying C's clamping (spell_recall, spells.c): a `GET_HOME(ch)` outside
/// `1..=NUM_STARTROOMS` is treated as 0, which then loads start town 1.
fn mortal_start_index(home: i32) -> usize {
    let home = if home >= 1 && home <= NUM_STARTROOMS as i32 {
        home
    } else {
        0
    };
    // home == 0 falls back to table slot 1 (start town 1), matching C.
    if home == 0 {
        1
    } else {
        home as usize
    }
}

/// The player's recall destination room rnum
/// (`real_room(mortal_start_room[GET_HOME(ch)])`, spells.c).
fn recall_room(g: &GameState, ch: CharId) -> Option<RoomRnum> {
    let home = g.get_char(ch).map(|c| c.player.hometown).unwrap_or(NOWHERE);
    let vnum = MORTAL_START_ROOM[mortal_start_index(home)];
    g.real_room(vnum)
}

// ===========================================================================
// spell_create_water (spells.c)
// ===========================================================================
pub fn spell_create_water(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    _victim: Option<CharId>,
    obj: Option<ObjId>,
) {
    let obj = match obj {
        Some(o) => o,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }
    let level = level.clamp(1, LVL_IMPL as i32);

    if g.get_obj(obj).map(|o| o.obj_type) != Some(ObjectType::LiqContainer) {
        return;
    }
    let (cap, cur, liq) = g
        .get_obj(obj)
        .map(|o| (o.values[0], o.values[1], o.values[2]))
        .unwrap_or((0, 0, 0));

    if liq != LIQ_WATER && cur != 0 {
        // Contaminate to slime.
        name_from_drinkcon(g, obj);
        if let Some(o) = g.get_obj_mut(obj) {
            o.values[2] = LIQ_SLIME;
        }
        name_to_drinkcon(g, obj, LIQ_SLIME);
    } else {
        let water = (cap - cur).max(0);
        if water > 0 {
            if cur >= 0 {
                name_from_drinkcon(g, obj);
            }
            if let Some(o) = g.get_obj_mut(obj) {
                o.values[2] = LIQ_WATER;
                o.values[1] += water;
            }
            name_to_drinkcon(g, obj, LIQ_WATER);
            weight_change_object(g, obj, water);
            act(
                g,
                "$p is filled.",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
        }
    }
    let _ = level;
}

/// name_from_drinkcon (act.item.c): strip the leading liquid word.
fn name_from_drinkcon(g: &mut GameState, obj: ObjId) {
    if let Some(o) = g.get_obj_mut(obj) {
        if let Some(space) = o.name.find(' ') {
            o.name = o.name[space + 1..].to_string();
        }
    }
}

/// name_to_drinkcon (act.item.c): prepend the liquid word.
fn name_to_drinkcon(g: &mut GameState, obj: ObjId, liq_type: i32) {
    let liq = DRINKS.get(liq_type as usize).copied().unwrap_or("water");
    if let Some(o) = g.get_obj_mut(obj) {
        o.name = format!("{} {}", liq, o.name);
    }
}

/// weight_change_object (act.item.c).
fn weight_change_object(g: &mut GameState, obj: ObjId, weight: i32) {
    if let Some(o) = g.get_obj_mut(obj) {
        o.weight += weight;
    }
}

// ===========================================================================
// spell_recall (spells.c) — word of recall
// ===========================================================================
pub fn spell_recall(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if is_npc(g, victim) {
        return;
    }
    if g.get_char(victim)
        .map(|c| c.riding.is_some() || c.ridden_by.is_some())
        .unwrap_or(false)
    {
        g.send_to_char(
            ch,
            "The spell fails because your victim is atop a mount.\r\n",
        );
        return;
    }

    // Arena branch (spells.c): a combatant who recalls concedes / is rebounded
    // to the prep room, an observer bounces to the observatory. arena_recall
    // emits the disappear/appear acts and look itself, so return when handled.
    if crate::arena::arena_recall(g, victim) {
        return;
    }

    act(
        g,
        "$n disappears.",
        true,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(victim);
    let dest = recall_room(g, victim).unwrap_or(0);
    g.char_to_room(victim, dest);
    act(
        g,
        "$n appears in the middle of the room.",
        true,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    look_at_room(g, victim, false);
}

// ===========================================================================
// spell_teleport (spells.c) — random destination (not in the MANUAL switch,
// but kept for parity / object use).
// ===========================================================================
pub fn spell_teleport(
    g: &mut GameState,
    _level: i32,
    _ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    let top = g.rooms.len();
    if top == 0 {
        return;
    }
    let mut to_room;
    loop {
        to_room = g.rng.number(0, (top - 1) as i32) as usize;
        let flags = g.room(to_room).room_flags;
        if !flags.intersects(RoomFlags::PRIVATE | RoomFlags::DEATH) {
            break;
        }
    }
    act(
        g,
        "$n slowly fades out of existence and is gone.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(victim);
    g.char_to_room(victim, to_room);
    act(
        g,
        "$n slowly fades into existence.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    look_at_room(g, victim, false);
}

// ===========================================================================
// spell_summon (spells.c)
// ===========================================================================
pub fn spell_summon(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    let imm_minus_1 = LVL_IMMORT as i32 - 1;
    if get_level(g, victim) > imm_minus_1.min(level + 3) {
        g.send_to_char(ch, "You failed.\r\n");
        return;
    }
    if is_npc(g, victim) {
        let mob_vnum = g.get_char(victim).map(|c| c.nr).unwrap_or(NOBODY);
        if let Some(zone) = crate::olc::real_zone(g, mob_vnum) {
            if g.zones
                .get(zone)
                .map(|z| z.status_mode == 0)
                .unwrap_or(false)
            {
                g.send_to_char(ch, "You failed.\r\n");
                return;
            }
        }
    }

    // Arena gates (spells.c): summon may not bridge the arena boundary — block
    // if exactly one of caster/victim is currently in the arena.
    let v_in_arena = crate::arena::arena_stat(victim) != crate::arena::ARENA_NOT;
    let ch_in_arena = crate::arena::arena_stat(ch) != crate::arena::ARENA_NOT;
    if v_in_arena && !ch_in_arena {
        g.send_to_char(
            ch,
            "Your target is in the arena right now.\r\nEldrich magic obstructs thee!\r\n",
        );
        return;
    }
    if !v_in_arena && ch_in_arena {
        g.send_to_char(ch, "You're in the arena right now whereas your target is not.\r\nEldrich magic obstructs thee!\r\n");
        return;
    }

    if mob_flagged(g, victim, MOB_AGGRESSIVE) {
        act(g, "As the words escape your lips and $N travels\r\nthrough time and space towards you, you realize that $E is\r\naggressive and might harm you, so you wisely send $M back.", false, ch, None, ActArg::Char(victim), To::Char);
        return;
    }
    if !is_npc(g, victim) && !prf_flagged(g, victim, PRF_SUMMONABLE) {
        let caster_name = g
            .get_char(ch)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        let room_name = g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.room(r).name.clone())
            .unwrap_or_default();
        let pron = if g.get_char(ch).map(|c| c.player.sex) == Some(Gender::Male) {
            "He"
        } else {
            "She"
        };
        let vname = g
            .get_char(victim)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        g.send_to_char(victim, &format!(
            "{} just tried to summon you to: {}.\r\n{} failed because you have summon protection on.\r\nType NOSUMMON to allow other players to summon you.\r\n",
            caster_name, room_name, pron));
        g.send_to_char(
            ch,
            &format!("You failed because {} has summon protection on.\r\n", vname),
        );
        // C spells.c:250-252: immortal audit trail for blocked summons (#249).
        crate::syslog::mudlog(
            g,
            &format!("{} failed summoning {} to {}.", caster_name, vname, room_name),
            crate::syslog::BRF,
            LVL_IMMORT,
        );
        return;
    }

    if mob_flagged(g, victim, MOB_NOSUMMON) || (is_npc(g, victim) && mag_savingthrow(g, ch, victim))
    {
        g.send_to_char(ch, "You failed.\r\n");
        return;
    }

    act(
        g,
        "$n disappears suddenly.",
        true,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    let dest = g.get_char(ch).and_then(|c| c.in_room).unwrap_or(0);
    g.char_from_room(victim);
    g.char_to_room(victim, dest);
    act(
        g,
        "$n arrives suddenly.",
        true,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "$n has summoned you!",
        false,
        ch,
        None,
        ActArg::Char(victim),
        To::Vict,
    );
    look_at_room(g, victim, false);
}

// ===========================================================================
// spell_locate_object (spells.c)
// ===========================================================================
pub fn spell_locate_object(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    _victim: Option<CharId>,
    obj: Option<ObjId>,
) {
    let obj = match obj {
        Some(o) => o,
        None => return,
    };
    let name = g.get_obj(obj).map(|o| fname(&o.name)).unwrap_or_default();
    let mut j = level >> 1;
    let start_j = j;

    let ids: Vec<ObjId> = g.obj_ids();
    for oid in ids {
        if j <= 0 {
            break;
        }
        let matches = g
            .get_obj(oid)
            .map(|o| crate::handler::isname(&name, &o.name))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let short = g
            .get_obj(oid)
            .map(|o| o.short_description.clone())
            .unwrap_or_default();
        let line = match g.get_obj(oid).map(|o| o.loc) {
            Some(ObjLoc::Carried(holder)) => {
                format!("{} is being carried by {}.\n\r", short, pers(g, ch, holder))
            }
            Some(ObjLoc::Room(r)) => format!("{} is in {}.\n\r", short, g.room(r).name.clone()),
            Some(ObjLoc::Contained(container)) => {
                let cs = g
                    .get_obj(container)
                    .map(|o| o.short_description.clone())
                    .unwrap_or_default();
                format!("{} is in {}.\n\r", short, cs)
            }
            Some(ObjLoc::Worn(wearer, _)) => {
                format!("{} is being worn by {}.\n\r", short, pers(g, ch, wearer))
            }
            _ => format!("{}'s location is uncertain.\n\r", short),
        };
        g.send_to_char(ch, &cap_first(&line));
        j -= 1;
    }

    if j == start_j {
        g.send_to_char(ch, "You sense nothing.\n\r");
    }
}

// ===========================================================================
// spell_locate_target (spells.c) — locate life
// ===========================================================================
pub fn spell_locate_target(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    let name = g
        .get_char(victim)
        .map(|c| fname(&c.player.name))
        .unwrap_or_default();
    let mut j = level >> 1;
    let start_j = j;

    let ids: Vec<CharId> = g.char_ids();
    for cid in ids {
        if j <= 0 {
            break;
        }
        let matches = g
            .get_char(cid)
            .map(|c| crate::handler::isname(&name, &c.player.name))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let disp = g
            .get_char(cid)
            .map(|c| {
                if c.is_npc {
                    c.short_desc
                        .clone()
                        .unwrap_or_else(|| c.player.name.clone())
                } else {
                    c.player.name.clone()
                }
            })
            .unwrap_or_default();
        let line = match g.get_char(cid).and_then(|c| c.in_room) {
            Some(r) => format!("{} is in {}.\n\r", disp, g.room(r).name.clone()),
            None => format!("{}'s location is uncertain.\r\n", disp),
        };
        g.send_to_char(ch, &cap_first(&line));
        j -= 1;
    }

    if j == start_j {
        g.send_to_char(ch, "You sense nothing.\n\r");
    }
}

// ===========================================================================
// spell_charm (spells.c)
// ===========================================================================
pub fn spell_charm(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    if victim == ch {
        g.send_to_char(ch, "You like yourself even better!\r\n");
    } else if !is_npc(g, victim) && !prf_flagged(g, victim, PRF_SUMMONABLE) {
        g.send_to_char(ch, "You fail because SUMMON protection is on!\r\n");
    } else if is_affected(g, victim, AFF_SANCTUARY) {
        g.send_to_char(ch, "Your victim is protected by sanctuary!\r\n");
    } else if mob_flagged(g, victim, MOB_NOCHARM) {
        g.send_to_char(ch, "Your victim resists!\r\n");
    } else if is_affected(g, ch, AFF_CHARM) {
        g.send_to_char(ch, "You can't have any followers of your own!\r\n");
    } else if is_affected(g, victim, AFF_CHARM) || level < get_level(g, victim) {
        g.send_to_char(ch, "You fail.\r\n");
    } else if !PK_ALLOWED && !is_npc(g, victim) {
        g.send_to_char(ch, "You fail - shouldn't be doing it anyway.\r\n");
    } else if circle_follow(g, victim, ch) {
        g.send_to_char(ch, "Sorry, following in circles can not be allowed.\r\n");
    } else if mag_savingthrow(g, ch, victim) {
        g.send_to_char(ch, "Your victim resists!\r\n");
    } else {
        if g.get_char(victim).and_then(|c| c.master).is_some() {
            stop_follower(g, victim);
        }
        add_follower(g, victim, ch);

        let int_score = g
            .get_char(victim)
            .map(|c| c.aff_abils.intel as i32)
            .unwrap_or(0);
        let duration = if int_score != 0 {
            24 * MAX_PLAYER_STAT / int_score
        } else {
            24 * MAX_PLAYER_STAT
        };
        let af = Affect {
            spell_type: SPELL_CHARM,
            duration,
            modifier: 0,
            location: 0,
            bitvector: AFF_CHARM,
            caster: Some(ch),
        };
        if let Some(c) = g.get_char_mut(victim) {
            c.affected.push(af);
        }
        g.affect_total(victim);

        act(
            g,
            "Isn't $n just such a nice fellow?",
            false,
            ch,
            None,
            ActArg::Char(victim),
            To::Vict,
        );
        if is_npc(g, victim) {
            if let Some(c) = g.get_char_mut(victim) {
                c.act_flags &= !MOB_AGGRESSIVE;
                c.act_flags &= !MOB_SPEC;
            }
        }
    }
}

// ===========================================================================
// spell_identify (spells.c)
// ===========================================================================
pub fn spell_identify(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    victim: Option<CharId>,
    obj: Option<ObjId>,
) {
    if let Some(obj) = obj {
        g.send_to_char(ch, "You feel informed:\r\n");
        let (short, otype, weight, cost, rent, bitvector, extra_flags, curr_slots, total_slots) = g
            .get_obj(obj)
            .map(|o| {
                (
                    o.short_description.clone(),
                    o.obj_type,
                    o.weight,
                    o.cost,
                    o.rent,
                    o.bitvector,
                    o.extra_flags.bits() as i64,
                    o.curr_slots,
                    o.total_slots,
                )
            })
            .unwrap_or((String::new(), ObjectType::Other, 0, 0, 0, 0, 0, 0, 0));

        g.send_to_char(
            ch,
            &format!(
                "Object '{}', Item type: {}\r\n",
                short,
                item_type_name(otype)
            ),
        );
        if bitvector != 0 {
            g.send_to_char(
                ch,
                &format!(
                    "Item will give you following abilities:  {}\r\n",
                    crate::olc::sprintbit(bitvector, AFFECTED_BITS)
                ),
            );
        }
        g.send_to_char(
            ch,
            &format!(
                "Item is: {}\r\n",
                crate::olc::sprintbit(extra_flags, EXTRA_BITS)
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "Weight: {}, Value: {}, Rent: {}\r\n",
                crate::combat::numdisplay(weight as i64),
                crate::combat::numdisplay(cost as i64),
                crate::combat::numdisplay(rent as i64)
            ),
        );
        let viewer_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
        g.send_to_char(
            ch,
            &identify_quality_line(curr_slots, total_slots, viewer_level),
        );

        // Type-specific details.
        let (v0, v1, v2, v3) = g
            .get_obj(obj)
            .map(|o| (o.values[0], o.values[1], o.values[2], o.values[3]))
            .unwrap_or((0, 0, 0, 0));
        match otype {
            ObjectType::Scroll | ObjectType::Potion => {
                let mut line = format!("This {} casts: ", item_type_name(otype));
                for v in [v1, v2, v3] {
                    if v >= 1 {
                        line.push_str(&format!(" {}", skill_name(v)));
                    }
                }
                line.push_str("\r\n");
                g.send_to_char(ch, &line);
            }
            ObjectType::Wand | ObjectType::Staff => {
                let plural = if v1 == 1 { "" } else { "s" };
                g.send_to_char(
                    ch,
                    &format!(
                        "This {} casts: {}\r\nIt has {} maximum charge{} and {} remaining.\r\n",
                        item_type_name(otype),
                        skill_name(v3),
                        v1,
                        plural,
                        v2
                    ),
                );
            }
            ObjectType::Weapon => {
                let avg = ((v2 + 1) as f64 / 2.0) * v1 as f64;
                g.send_to_char(
                    ch,
                    &format!(
                        "Damage Dice is '{}D{}' for an average per-round damage of {:.1}.\r\n",
                        v1, v2, avg
                    ),
                );
            }
            ObjectType::Armor => {
                g.send_to_char(ch, &format!("Defense apply is {}\r\n", v0));
            }
            _ => {}
        }

        // Affects.
        let affects: Vec<(i32, i32)> = g
            .get_obj(obj)
            .map(|o| o.affects.iter().map(|a| (a.location, a.modifier)).collect())
            .unwrap_or_default();
        let mut found = false;
        for (loc, modi) in affects {
            if loc != APPLY_NONE && modi != 0 {
                if !found {
                    g.send_to_char(ch, "Can affect you as :\r\n");
                    found = true;
                }
                g.send_to_char(
                    ch,
                    &format!("   Affects: {} By {}\r\n", apply_type_name(loc), modi),
                );
            }
        }
    } else if let Some(victim) = victim {
        let (name, is_npc, birth, height, weight, level, hit, mana) = g
            .get_char(victim)
            .map(|c| {
                (
                    c.player.name.clone(),
                    c.is_npc,
                    c.player.time_birth,
                    c.player.height as i32,
                    c.player.weight as i32,
                    c.player.level as i32,
                    c.points.hit,
                    c.points.mana,
                )
            })
            .unwrap_or((String::new(), true, 0, 0, 0, 1, 0, 0));
        g.send_to_char(ch, &format!("Name: {}\r\n", name));
        if !is_npc {
            let (years, months, days, hours) = character_age_parts(birth);
            g.send_to_char(
                ch,
                &format!(
                    "{} is {} years, {} months, {} days and {} hours old.\r\n",
                    name, years, months, days, hours
                ),
            );
        }
        g.send_to_char(
            ch,
            &format!("Height {} cm, Weight {} pounds\r\n", height, weight),
        );
        g.send_to_char(
            ch,
            &format!("Level: {}, Hits: {}, Mana: {}\r\n", level, hit, mana),
        );
        let (power, mpower, defense, mdefense, technique) = g
            .get_char(victim)
            .map(|c| {
                (
                    c.points.power,
                    c.points.mpower,
                    c.points.defense,
                    c.points.mdefense,
                    c.points.technique,
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));
        g.send_to_char(
            ch,
            &format!(
                "Power: {}, Magic Power: {}, Defense: {}, Magic Defense: {}, Technique: {}\r\n",
                power, mpower, defense, mdefense, technique
            ),
        );
        let a = g.get_char(victim).map(|c| c.aff_abils).unwrap_or_default();
        g.send_to_char(
            ch,
            &format!(
                "Str: {}/{}, Int: {}, Wis: {}, Dex: {}, Con: {}, Cha: {}\r\n",
                a.str, a.str_add, a.intel, a.wis, a.dex, a.con, a.cha
            ),
        );
    }
}

fn item_type_name(t: ObjectType) -> &'static str {
    // ObjectType discriminant == C ITEM_* number == ITEM_TYPES index.
    crate::constants::ITEM_TYPES
        .get(t as usize)
        .copied()
        .unwrap_or("UNDEFINED")
}

fn identify_quality_line(curr_slots: i32, total_slots: i32, viewer_level: Level) -> String {
    let condition = if total_slots != 0 {
        curr_slots * 100 / total_slots
    } else {
        0
    };
    if total_slots == 0 && curr_slots == 0 {
        "Quality: INDESTRUCTABLE\r\n".to_string()
    } else if viewer_level >= LVL_HERO_IDENTIFY {
        format!(
            "Quality: {}/{}\r\nCondition: {} percent\r\n",
            curr_slots, total_slots, condition
        )
    } else {
        // C spells.c:434-482 uses a <=10 ladder; the old 0..=19 ranges
        // shifted every bucket by ~9 points (#246).
        let label = match condition {
            0..=10 => "Extremley Poor",
            11..=20 => "Poor",
            21..=30 => "Fair",
            31..=40 => "Moderate",
            41..=50 => "Good",
            51..=60 => "Very Good",
            61..=70 => "Excellent",
            71..=80 => "Superior",
            81..=90 => "Extremely Superior",
            _ => "Brand New",
        };
        format!("Quality: {}\r\n", label)
    }
}

fn character_age_parts(birth: i64) -> (i64, i64, i64, i64) {
    let mut total = (chrono::Utc::now().timestamp() - birth).max(0);
    let hours = (total / SECS_PER_MUD_HOUR) % 24;
    total -= SECS_PER_MUD_HOUR * hours;
    let days = (total / SECS_PER_MUD_DAY) % 35;
    total -= SECS_PER_MUD_DAY * days;
    let months = (total / SECS_PER_MUD_MONTH) % 17;
    total -= SECS_PER_MUD_MONTH * months;
    let years = total / SECS_PER_MUD_YEAR + 17;
    (years, months, days, hours)
}

fn apply_type_name(loc: i32) -> &'static str {
    match loc {
        APPLY_STR => "STRENGTH",
        APPLY_DEX => "DEXTERITY",
        APPLY_INT => "INTELLIGENCE",
        APPLY_WIS => "WISDOM",
        APPLY_CON => "CONSTITUTION",
        APPLY_HIT => "HIT POINTS",
        APPLY_MANA => "MANA",
        APPLY_MOVE => "MOVE",
        APPLY_DEFENSE => "DEFENSE",
        APPLY_MDEFENSE => "MAGIC DEFENSE",
        APPLY_POWER => "POWER",
        APPLY_MPOWER => "MAGIC POWER",
        APPLY_TECHNIQUE => "TECHNIQUE",
        _ => "NONE",
    }
}

// ===========================================================================
// spell_enchant_weapon (spells.c)
// ===========================================================================
pub fn spell_enchant_weapon(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    _victim: Option<CharId>,
    obj: Option<ObjId>,
) {
    let obj = match obj {
        Some(o) => o,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }
    let (is_weapon, is_magic) = g
        .get_obj(obj)
        .map(|o| {
            (
                o.obj_type == ObjectType::Weapon,
                o.extra_flags.bits() & ITEM_MAGIC != 0,
            )
        })
        .unwrap_or((false, false));
    if !is_weapon || is_magic {
        return;
    }

    // If any pre-existing apply, abort (C returns).
    let has_apply = g
        .get_obj(obj)
        .map(|o| {
            o.affects
                .iter()
                .take(MAX_OBJ_AFFECT)
                .any(|a| a.location != APPLY_NONE)
        })
        .unwrap_or(false);
    if has_apply {
        return;
    }

    let bonus = 1 + (level >= MAX_PLAYER_STAT) as i32;
    if let Some(o) = g.get_obj_mut(obj) {
        o.extra_flags |= crate::object::ExtraFlags::from_bits_truncate(ITEM_MAGIC);
        o.affects.clear();
        o.affects.push(crate::object::ObjectAffect {
            location: APPLY_POWER,
            modifier: bonus,
        });
        o.affects.push(crate::object::ObjectAffect {
            location: APPLY_TECHNIQUE,
            modifier: bonus,
        });
    }

    if is_good(g, ch) {
        if let Some(o) = g.get_obj_mut(obj) {
            o.extra_flags |= crate::object::ExtraFlags::from_bits_truncate(ITEM_ANTI_EVIL);
        }
        act(
            g,
            "$p glows blue.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
    } else if is_evil(g, ch) {
        if let Some(o) = g.get_obj_mut(obj) {
            o.extra_flags |= crate::object::ExtraFlags::from_bits_truncate(ITEM_ANTI_GOOD);
        }
        act(
            g,
            "$p glows red.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
    } else {
        act(
            g,
            "$p glows yellow.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
    }
}

// ===========================================================================
// spell_detect_poison (spells.c)
// ===========================================================================
pub fn spell_detect_poison(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    victim: Option<CharId>,
    obj: Option<ObjId>,
) {
    if let Some(victim) = victim {
        if victim == ch {
            if is_affected(g, victim, AFF_POISON) {
                g.send_to_char(ch, "You can sense poison in your blood.\r\n");
            } else {
                g.send_to_char(ch, "You feel healthy.\r\n");
            }
        } else if is_affected(g, victim, AFF_POISON) {
            act(
                g,
                "You sense that $E is poisoned.",
                false,
                ch,
                None,
                ActArg::Char(victim),
                To::Char,
            );
        } else {
            act(
                g,
                "You sense that $E is healthy.",
                false,
                ch,
                None,
                ActArg::Char(victim),
                To::Char,
            );
        }
    }

    if let Some(obj) = obj {
        let (otype, poisoned) = g
            .get_obj(obj)
            .map(|o| (o.obj_type, o.values[3] != 0))
            .unwrap_or((ObjectType::Other, false));
        match otype {
            ObjectType::LiqContainer | ObjectType::Fountain | ObjectType::Food => {
                if poisoned {
                    act(
                        g,
                        "You sense that $p has been contaminated.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::None,
                        To::Char,
                    );
                } else {
                    act(
                        g,
                        "You sense that $p is safe for consumption.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::None,
                        To::Char,
                    );
                }
            }
            _ => g.send_to_char(ch, "You sense that it should not be consumed.\r\n"),
        }
    }
}

// ===========================================================================
// spell_fear (spells.c)
// ===========================================================================
pub fn spell_fear(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    _victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    if !g.char_exists(ch) {
        return;
    }
    g.send_to_char(ch, "You radiate an aura of fear into the room!\r\n");
    act(
        g,
        "$n is surrounded by an aura of fear!",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    for target in people {
        if target == ch {
            continue;
        }
        if get_level(g, target) >= LVL_IMMORT as i32 {
            continue;
        }
        if mag_savingthrow(g, ch, target) {
            let tname = g
                .get_char(target)
                .map(|c| c.player.name.clone())
                .unwrap_or_default();
            act(
                g,
                &format!("{} is unaffected by the fear!\r\n", tname),
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(ch, "Your victim is not afraid of the likes of you!\r\n");
            if is_npc(g, target) {
                crate::combat::hit(g, target, ch);
            }
        } else {
            let rooms_to_flee = level / 10;
            for _ in 0..rooms_to_flee {
                g.send_to_char(target, "You flee in terror!\r\n");
                crate::combat::do_flee(g, target);
            }
        }
    }
}

// ===========================================================================
// spell_recharge (spells.c)
// ===========================================================================
pub fn spell_recharge(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    _victim: Option<CharId>,
    obj: Option<ObjId>,
) {
    let obj = match obj {
        Some(o) => o,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    let otype = g.get_obj(obj).map(|o| o.obj_type);
    let (max_ch, cur_ch) = g
        .get_obj(obj)
        .map(|o| (o.values[1], o.values[2]))
        .unwrap_or((0, 0));
    let oname = g.get_obj(obj).map(|o| o.name.clone()).unwrap_or_default();
    let chname = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();

    let (verb, restore_range, explode_dice): (&str, (i32, i32), i32) = match otype {
        Some(ObjectType::Wand) => ("wand", (1, 5), 2),
        Some(ObjectType::Staff) => ("staff", (1, 3), 3),
        _ => return,
    };

    if cur_ch >= max_ch {
        g.send_to_char(ch, "That item is already at full charges!\r\n");
        return;
    }
    g.send_to_char(ch, &format!("You attempt to recharge the {}.\r\n", verb));
    let restored = g.rng.number(restore_range.0, restore_range.1);
    let new_ch = cur_ch + restored;
    if let Some(o) = g.get_obj_mut(obj) {
        o.values[2] = new_ch;
    }
    if new_ch > max_ch {
        g.send_to_char(
            ch,
            &format!("The {} is overcharged and explodes!\r\n", verb),
        );
        let line = format!("{} overcharges {} and it explodes!\r\n", chname, oname);
        act(g, &line, true, ch, None, ActArg::None, To::NotVict);
        let explode = g.rng.dice(new_ch, explode_dice);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.hit -= explode;
        }
        update_pos(g, ch);
        g.obj_from_anywhere(obj);
        g.extract_obj(obj);
    } else {
        g.send_to_char(
            ch,
            &format!("You restore {} charges to the {}.\r\n", restored, verb),
        );
    }
}

// ===========================================================================
// spell_portal (spells.c)
// ===========================================================================
pub fn spell_portal(
    g: &mut GameState,
    level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if !g.char_exists(ch) {
        return;
    }

    let ch_room = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "The magic fails.\n\r");
            return;
        }
    };
    let v_room = match g.get_char(victim).and_then(|c| c.in_room) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "The magic cannot locate the target.\n");
            return;
        }
    };

    let ch_level = get_level(g, ch);
    let ch_flags = g.room(ch_room).room_flags;
    if ch_flags.bits() & ROOM_NOMAGIC != 0 {
        g.send_to_char(ch, "Eldritch wizardry obstructs thee.\n\r");
        return;
    }
    let victim_intangible = g
        .get_char(victim)
        .map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0)
        .unwrap_or(false);
    if victim_intangible && (ch_level < LVL_IMMORT as i32 || is_npc(g, ch)) {
        g.send_to_char(ch, "Eldritch wizardry obstructs thee.\n\r");
        return;
    }
    if ch_flags.contains(RoomFlags::TUNNEL) {
        g.send_to_char(ch, "There is no room in here to summon!\n\r");
        return;
    }
    if is_npc(g, victim) && ch_level < LVL_IMMORT as i32 {
        g.send_to_char(ch, "The magic cannot locate the target.\n");
        return;
    }
    if v_room == ch_room {
        g.send_to_char(ch, "You are already at the target area.\n");
        return;
    }
    if get_level(g, victim) >= LVL_IMMORT as i32 && ch_level < LVL_IMMORT as i32 {
        g.send_to_char(ch, "You cannot travel to the gods!\n");
        return;
    }
    // Arena gates (spells.c): a portal may not bridge the arena boundary.
    let v_in_arena = crate::arena::arena_stat(victim) != crate::arena::ARENA_NOT;
    let ch_in_arena = crate::arena::arena_stat(ch) != crate::arena::ARENA_NOT;
    if v_in_arena && !ch_in_arena {
        g.send_to_char(
            ch,
            "Your target is in the arena right now.\r\nEldrich magic obstructs thee!\r\n",
        );
        return;
    }
    if !v_in_arena && ch_in_arena {
        g.send_to_char(ch, "You're in the arena right now whereas your target is not.\r\nEldrich magic obstructs thee!\r\n");
        return;
    }
    let v_flags = g.room(v_room).room_flags;
    if v_flags.bits() & ROOM_NOMAGIC != 0 {
        g.send_to_char(ch, "Your target is protected against your magic.\n\r");
        return;
    }
    const ROOM_HOUSE_CRASH: u32 = 1 << 12;
    if v_flags.contains(RoomFlags::HOUSE) || v_flags.bits() & ROOM_HOUSE_CRASH != 0 {
        let caster_idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
        let target_vnum = g.room(v_room).number;
        if !crate::house::house_owned_by(target_vnum, caster_idnum) {
            g.send_to_char(ch, "Your target is protected against your magic.\n\r");
            return;
        }
    }
    if v_flags.contains(RoomFlags::PRIVATE) {
        g.send_to_char(ch, "Your target is protected against your magic.\n\r");
        return;
    }
    if v_flags.contains(RoomFlags::TUNNEL) {
        g.send_to_char(ch, "There is not enough room there to portal!\n\r");
        return;
    }
    if is_affected(g, victim, AFF_NOPORTAL) {
        g.send_to_char(ch, "Your target's magic resists your portal attempt.\r\n");
        return;
    }

    let v_room_name = g.room(v_room).name.clone();
    let ch_room_name = g.room(ch_room).name.clone();

    // Portal at the caster's side -> leads to the victim's room.
    if let Some(p1) = g.load_object(PORTAL_VNUM) {
        let timer = if ch_level < LVL_IMMORT as i32 { 1 } else { -1 };
        if let Some(o) = g.get_obj_mut(p1) {
            o.values[0] = 1;
            o.values[1] = v_room as i32; // value[1] = destination rnum
            o.timer = timer;
            // C spells.c:873-879: the destination hint is an extra
            // description (keyword = portal name), which is what 'look
            // portal' resolves; action_description is invisible to look
            // (#247).
            // keyword = the portal object's own name (spells.c:875).
            let kw = o.name.clone();
            o.ex_descriptions.push((
                kw,
                format!(
                    "Through the mists of the portal, you can faintly see {}",
                    v_room_name
                ),
            ));
        }
        g.obj_to_room(p1, ch_room);
        act(
            g,
            "$p suddenly appears.",
            true,
            ch,
            Some(p1),
            ActArg::None,
            To::Room,
        );
        act(
            g,
            "$p suddenly appears.",
            true,
            ch,
            Some(p1),
            ActArg::None,
            To::Char,
        );
    } else {
        g.send_to_char(ch, "The magic fails.\n\r");
        return;
    }

    // Portal at the victim's side -> leads back to the caster's room.
    if let Some(p2) = g.load_object(PORTAL_VNUM) {
        let timer = if ch_level < LVL_IMMORT as i32 { 1 } else { -1 };
        if let Some(o) = g.get_obj_mut(p2) {
            o.values[0] = 1;
            o.values[1] = ch_room as i32;
            o.timer = timer;
            let kw = o.name.clone();
            o.ex_descriptions.push((
                kw,
                format!(
                    "Through the mists of the portal, you can faintly see {}",
                    ch_room_name
                ),
            ));
        }
        g.obj_to_room(p2, v_room);
        act(
            g,
            "$p suddenly appears.",
            true,
            victim,
            Some(p2),
            ActArg::None,
            To::Room,
        );
        act(
            g,
            "$p suddenly appears.",
            true,
            victim,
            Some(p2),
            ActArg::None,
            To::Char,
        );
    }
    let _ = level;
}

// ===========================================================================
// spell_home (spells.c)
// ===========================================================================
pub fn spell_home(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    // SPELL_HOME is TAR_CHAR_ROOM | TAR_SELF_ONLY, so victim == ch; fall back to
    // ch if the dispatcher passes None.
    let victim = victim.unwrap_or(ch);

    // Find the house this caster owns (house_control[i].owner == GET_IDNUM(ch),
    // last match wins — spells.c). homenum == 0 / None means "no house owned".
    let idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
    let homenum = match crate::house::house_for_owner(idnum) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "The spell fails because you don't own a house!\r\n");
            return;
        }
    };

    // Mount check (RIDING(victim) || RIDDEN_BY(victim)).
    let on_mount = g
        .get_char(victim)
        .map(|c| c.riding.is_some() || c.ridden_by.is_some())
        .unwrap_or(false);
    if on_mount {
        g.send_to_char(
            ch,
            "The spell fails because your victim is atop a mount.\r\n",
        );
        return;
    }

    // Resolve the destination room (real_room(homenum)). C computes the
    // mortal_start_room fallback too but never uses it for the destination, so
    // the owned-house room is the only teleport target.
    let dest = match g.real_room(homenum) {
        Some(r) => r,
        None => {
            // House vnum no longer maps to a real room — treat as no house.
            g.send_to_char(ch, "The spell fails because you don't own a house!\r\n");
            return;
        }
    };

    // Already home?
    if g.get_char(ch).and_then(|c| c.in_room) == Some(dest) {
        g.send_to_char(ch, "The spell fails because you're already at home!\r\n");
        return;
    }

    // Arena combatant / observer branches (spells.c): a participant casting
    // home is rebounded to the prep room / observatory via arena_recall. NOTE:
    // arena_recall emits "$n disappears." for the combatant branch, whereas
    // C's spell_home combatant branch goes straight to match_over without a
    // leave-act; the destination/concede semantics are identical.
    if crate::arena::arena_recall(g, ch) {
        return;
    }

    act(
        g,
        "$n magically teleports out.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(ch);
    g.char_to_room(ch, dest);
    act(
        g,
        "$n suddenly appears in the room.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    look_at_room(g, ch, false);
    g.send_to_char(ch, "\r\nAhhhh... Home Sweet Home!\r\n");
}

// ===========================================================================
// spell_retreat (spells.c)
// ===========================================================================
pub fn spell_retreat(
    g: &mut GameState,
    _level: i32,
    ch: CharId,
    victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
    let victim = match victim {
        Some(v) => v,
        None => return,
    };
    if is_npc(g, victim) {
        return;
    }
    // load_room (rented retreat point) is not on the Tier-0 Character; C aborts
    // when the player hasn't rented anywhere, which is the universal case here.
    g.send_to_char(ch, "You must rent somewhere before you can retreat!\r\n");
}

// ===========================================================================
// spell_control_weather (spells.c) — weather system unported; C's MAG_MANUAL
// switch has no case for this spell, so it falls through silently.
// ===========================================================================
pub fn spell_control_weather(
    _g: &mut GameState,
    _level: i32,
    _ch: CharId,
    _victim: Option<CharId>,
    _obj: Option<ObjId>,
) {
}

// ---------------------------------------------------------------------------
// Follow helpers (handler.c) — local copies (cmd_movement's are private).
// ---------------------------------------------------------------------------
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

fn stop_follower(g: &mut GameState, ch: CharId) {
    let master = match g.get_char(ch).and_then(|c| c.master) {
        Some(m) => m,
        None => return,
    };
    if let Some(l) = g.get_char_mut(master) {
        l.followers.retain(|&f| f != ch);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.master = None;
    }
}

const PK_ALLOWED: bool = false;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::object::{ExtraFlags, Object, ObjectAffect, ObjectType, WearFlags};
    use crate::room::Room;
    use crate::world::{ObjectProto, Zone};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn lock_test_houses() -> MutexGuard<'static, ()> {
        static TEST_HOUSES_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_HOUSES_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap()
    }

    fn add_portal_proto(g: &mut GameState) {
        g.obj_protos.insert(
            PORTAL_VNUM,
            ObjectProto {
                vnum: PORTAL_VNUM,
                name: "portal".to_string(),
                short_desc: "a shimmering portal".to_string(),
                description: "A shimmering portal hovers here.".to_string(),
                obj_type: ObjectType::Portal,
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
            },
        );
    }

    fn connected_player(g: &mut GameState, name: &str, conn: ConnId) -> CharId {
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player(name.into(), Class::Cleric, Race::Human);
        ch.desc = Some(conn);
        g.create_char(ch)
    }

    #[test]
    fn control_weather_is_silent() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player("Caster".into(), Class::Cleric, Race::Human);
        ch.desc = Some(conn);
        let ch = g.create_char(ch);

        spell_control_weather(&mut g, 1, ch, None, None);

        assert!(g.descriptors.get(&conn).unwrap().outbuf.is_empty());
    }

    #[test]
    fn identify_object_reports_flags_abilities_and_quality() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, "Caster", conn);
        let mut obj = Object::new(100, "armor".to_string(), "a bright cuirass".to_string());
        obj.obj_type = ObjectType::Armor;
        obj.weight = 7;
        obj.cost = 123;
        obj.rent = 4;
        obj.bitvector = AFF_SANCTUARY;
        obj.extra_flags = ExtraFlags::GLOW | ExtraFlags::MAGIC;
        obj.curr_slots = 2;
        obj.total_slots = 10;
        obj.values[0] = 5;
        obj.affects.push(ObjectAffect {
            location: APPLY_POWER,
            modifier: 3,
        });
        let obj = g.create_obj(obj);

        spell_identify(&mut g, 1, ch, None, Some(obj));

        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Object 'a bright cuirass', Item type: ARMOR\r\n"));
        assert!(out.contains("Item will give you following abilities:  SANCTUARY \r\n"));
        assert!(out.contains("Item is: GLOW MAGIC \r\n"));
        assert!(out.contains("Weight: 7, Value: 123, Rent: 4\r\n"));
        assert!(out.contains("Quality: Poor\r\n"));
        assert!(out.contains("Defense apply is 5\r\n"));
        assert!(out.contains("Can affect you as :\r\n"));
        assert!(out.contains("   Affects: POWER By 3\r\n"));
    }

    #[test]
    fn identify_pc_reports_mud_age() {
        let mut g = GameState::new(Config::default());
        let ch = connected_player(&mut g, "Caster", ConnId(1));
        let mut target = Character::new_player("Target".into(), Class::Cleric, Race::Human);
        target.player.time_birth = chrono::Utc::now().timestamp()
            - (2 * SECS_PER_MUD_YEAR)
            - (3 * SECS_PER_MUD_MONTH)
            - (4 * SECS_PER_MUD_DAY)
            - (5 * SECS_PER_MUD_HOUR);
        let target = g.create_char(target);

        spell_identify(&mut g, 1, ch, Some(target), None);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Name: Target\r\n"));
        assert!(out.contains("Target is 19 years, 3 months, 4 days and 5 hours old.\r\n"));
    }

    #[test]
    fn recall_fails_for_mounted_victim() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Stable".to_string(),
            "A stable.".to_string(),
        ));
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player("Rider".into(), Class::Cleric, Race::Human);
        ch.desc = Some(conn);
        let ch = g.create_char(ch);
        let mount = g.create_char(Character::new_npc(1));
        g.char_to_room(ch, room);
        g.char_to_room(mount, room);
        g.get_char_mut(ch).unwrap().riding = Some(mount);

        spell_recall(&mut g, 1, ch, Some(ch), None);

        assert_eq!(g.get_char(ch).unwrap().in_room, Some(room));
        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("The spell fails because your victim is atop a mount.\r\n"));
    }

    #[test]
    fn summon_fails_for_npc_from_closed_zone() {
        let mut g = GameState::new(Config::default());
        g.zones.push(Zone {
            number: 1,
            name: "Closed".to_string(),
            builders: String::new(),
            lifespan: 30,
            age: 0,
            top: 199,
            reset_mode: 2,
            min_level: 0,
            max_level: 60,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        });
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut caster = Character::new_player("Caster".into(), Class::Cleric, Race::Human);
        caster.desc = Some(conn);
        let caster = g.create_char(caster);
        let victim = g.create_char(Character::new_npc(123));

        spell_summon(&mut g, 20, caster, Some(victim), None);

        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("You failed.\r\n"));
    }

    #[test]
    fn portal_blocks_intangible_target_for_mortal_caster() {
        let _guard = lock_test_houses();
        crate::house::set_test_houses(Vec::new());
        let mut g = GameState::new(Config::default());
        add_portal_proto(&mut g);
        let caster_room = g.add_room(Room::new(
            100,
            0,
            "Caster room".to_string(),
            "A quiet room.".to_string(),
        ));
        let target_room = g.add_room(Room::new(
            101,
            0,
            "Target room".to_string(),
            "A distant room.".to_string(),
        ));
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut caster = Character::new_player("Caster".into(), Class::Cleric, Race::Human);
        caster.desc = Some(conn);
        caster.idnum = 10;
        caster.player.level = 20;
        let caster = g.create_char(caster);
        let mut target = Character::new_player("Target".into(), Class::Cleric, Race::Human);
        target.idnum = 11;
        target.prf2_flags |= PRF2_INTANGIBLE;
        let target = g.create_char(target);
        g.char_to_room(caster, caster_room);
        g.char_to_room(target, target_room);

        spell_portal(&mut g, 20, caster, Some(target), None);

        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Eldritch wizardry obstructs thee.\n\r"));
        assert!(g.room(caster_room).contents.is_empty());
        assert!(g.room(target_room).contents.is_empty());
    }

    #[test]
    fn portal_allows_target_in_casters_own_house() {
        let _guard = lock_test_houses();
        let mut g = GameState::new(Config::default());
        add_portal_proto(&mut g);
        let caster_room = g.add_room(Room::new(
            100,
            0,
            "Caster room".to_string(),
            "A quiet room.".to_string(),
        ));
        let house_room = g.add_room(Room::new(
            200,
            0,
            "House room".to_string(),
            "A private home.".to_string(),
        ));
        g.room_mut(house_room).room_flags |= RoomFlags::HOUSE;
        crate::house::set_test_houses(vec![(200, 10)]);
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut caster = Character::new_player("Caster".into(), Class::Cleric, Race::Human);
        caster.desc = Some(conn);
        caster.idnum = 10;
        caster.player.level = 20;
        let caster = g.create_char(caster);
        let mut target = Character::new_player("Target".into(), Class::Cleric, Race::Human);
        target.idnum = 11;
        let target = g.create_char(target);
        g.char_to_room(caster, caster_room);
        g.char_to_room(target, house_room);

        spell_portal(&mut g, 20, caster, Some(target), None);

        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(!out.contains("Your target is protected against your magic.\n\r"));
        assert_eq!(g.room(caster_room).contents.len(), 1);
        assert_eq!(g.room(house_room).contents.len(), 1);
    }

    #[test]
    fn portal_blocks_target_in_another_players_house() {
        let _guard = lock_test_houses();
        let mut g = GameState::new(Config::default());
        add_portal_proto(&mut g);
        let caster_room = g.add_room(Room::new(
            100,
            0,
            "Caster room".to_string(),
            "A quiet room.".to_string(),
        ));
        let house_room = g.add_room(Room::new(
            200,
            0,
            "House room".to_string(),
            "A private home.".to_string(),
        ));
        g.room_mut(house_room).room_flags |= RoomFlags::HOUSE;
        crate::house::set_test_houses(vec![(200, 99)]);
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut caster = Character::new_player("Caster".into(), Class::Cleric, Race::Human);
        caster.desc = Some(conn);
        caster.idnum = 10;
        caster.player.level = 20;
        let caster = g.create_char(caster);
        let target = g.create_char(Character::new_player(
            "Target".into(),
            Class::Cleric,
            Race::Human,
        ));
        g.char_to_room(caster, caster_room);
        g.char_to_room(target, house_room);

        spell_portal(&mut g, 20, caster, Some(target), None);

        assert!(g
            .descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("Your target is protected against your magic.\n\r"));
        assert!(g.room(caster_room).contents.is_empty());
        assert!(g.room(house_room).contents.is_empty());
    }
}
