// Combat (CircleMUD fight.c), id-based. Tier-0 scope: melee rounds in the
// heartbeat, THAC0 hit resolution, damage + position/death, simple corpse
// drop. Skills/special attacks land in Batch 5.

use crate::act::{act, ActArg, To};
use crate::object::{Object, ObjectType, ObjLoc};
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;

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

/// set_fighting: attacker begins targeting victim (CircleMUD set_fighting).
pub fn set_fighting(g: &mut GameState, ch: CharId, victim: CharId) {
    if g.get_char(ch).and_then(|c| c.fighting).is_some() {
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = Some(victim);
        c.position = Position::Fighting;
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

/// Resolve one attack (CircleMUD hit()).
pub fn hit(g: &mut GameState, ch: CharId, victim: CharId) {
    let thac0 = calc_thac0(g, ch);
    let ac = g.get_char(victim).map(|c| (c.points.armor / 10) as i16).unwrap_or(10);
    let hitroll = g.get_char(ch).map(|c| c.points.hitroll).unwrap_or(0);

    let roll = g.rng.number(1, 20) as i16;
    let needed = thac0 - ac;
    let hit_landed = roll != 1 && (roll == 20 || roll >= needed - hitroll);

    if !hit_landed {
        dam_message(g, 0, ch, victim);
        return;
    }
    let dmg = calc_damage(g, ch);
    damage(g, ch, victim, dmg);
}

fn calc_thac0(g: &GameState, ch: CharId) -> i16 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 20,
    };
    let lvl = c.player.level as i16;
    let base = match c.player.class {
        Class::Warrior => 20 - lvl,
        Class::MagicUser => 20 - lvl / 3,
        _ => 20 - lvl * 2 / 3,
    };
    base.max(1)
}

fn calc_damage(g: &mut GameState, ch: CharId) -> i32 {
    // Weapon dice or bare hands.
    let wield = g.get_char(ch).and_then(|c| c.equipment[WEAR_WIELD]);
    let (n, s) = match wield.and_then(|w| g.get_obj(w)).and_then(|o| o.damage_dice()) {
        Some((n, s)) if n > 0 && s > 0 => (n, s),
        _ => (1, 2),
    };
    let mut dmg = g.rng.dice(n, s);
    if let Some(c) = g.get_char(ch) {
        dmg += c.points.damroll as i32;
        dmg += str_damage_bonus(c.real_abils.str);
    }
    dmg.max(0)
}

fn str_damage_bonus(str: i8) -> i32 {
    match str {
        i8::MIN..=5 => -4,
        6..=7 => -3,
        8..=9 => -2,
        10..=11 => -1,
        12..=15 => 0,
        16 => 1,
        17 => 2,
        18 => 3,
        19..=20 => 4,
        21..=22 => 5,
        23..=24 => 6,
        _ => 7,
    }
}

/// Apply damage, update position, handle retaliation and death.
pub fn damage(g: &mut GameState, ch: CharId, victim: CharId, dmg: i32) {
    // Apply.
    if let Some(v) = g.get_char_mut(victim) {
        v.points.hit -= dmg;
    }
    dam_message(g, dmg, ch, victim);

    // A mob remembers a PC who strikes it (mobact memory; fight.c).
    let ch_is_pc = g.get_char(ch).map(|c| !c.is_npc).unwrap_or(false);
    let vic_is_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
    if ch_is_pc && vic_is_npc {
        crate::mobact::remember(g, victim, ch);
    }

    update_position(g, victim);

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

/// Damage messages to char/vict/room (simplified CircleMUD dam_message).
fn dam_message(g: &mut GameState, dmg: i32, ch: CharId, victim: CharId) {
    let verb = severity(dmg);
    if dmg == 0 {
        act(g, "You miss $N.", false, ch, None, ActArg::Char(victim), To::Char);
        act(g, "$n misses you.", false, ch, None, ActArg::Char(victim), To::Vict);
        act(g, "$n misses $N.", false, ch, None, ActArg::Char(victim), To::NotVict);
        return;
    }
    act(g, &format!("You {} $N.", verb), false, ch, None, ActArg::Char(victim), To::Char);
    act(g, &format!("$n {}s you.", verb), false, ch, None, ActArg::Char(victim), To::Vict);
    act(g, &format!("$n {}s $N.", verb), false, ch, None, ActArg::Char(victim), To::NotVict);
}

fn severity(dmg: i32) -> &'static str {
    match dmg {
        0 => "miss",
        1..=2 => "scratch",
        3..=4 => "graze",
        5..=6 => "hit",
        7..=10 => "injure",
        11..=14 => "wound",
        15..=19 => "maul",
        20..=23 => "decimate",
        24..=27 => "devastate",
        28..=31 => "maim",
        32..=35 => "MUTILATE",
        36..=39 => "DISEMBOWEL",
        40..=43 => "MASSACRE",
        44..=47 => "MANGLE",
        _ => "* OBLITERATE *",
    }
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
