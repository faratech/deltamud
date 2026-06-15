// spec_procs.rs — the mobile/object/room special procedures (CircleMUD
// spec_procs.c), ported 1:1 to the id-indexed GameState.
//
// WHAT THIS PORTS (the assignment subset)
// ---------------------------------------
//   * guild          — the guildmaster: lets a PC `practice` a spell/skill,
//                      consuming a practice session and granting proficiency via
//                      set_skill, gated by the class prac_params + per-class
//                      min_level and the intelligence learn table (the key
//                      learning path).
//   * guild_guard    — blocks the wrong class (or any NPC) from moving through a
//                      guild entrance (uses class::guild_ok over guild_info).
//   * dump           — vaporises everything dropped here, paying a small bounty.
//   * pet_shops      — list / buy charmed pets from the adjacent pet room.
//   * snake          — poison-bite combat spec.
//   * thief          — npc pickpocket combat spec.
//   * magic_user     — npc spellcaster combat spec.
//   * cityguard      — protects the innocent: kills PLAYER_KILLER / PLAYER_THIEF
//                      flagged PCs and aids the most-good fighter.
//   * mayor          — the scripted patrol of Anacreon's mayor (open/close path).
//   * fido           — corpse-devouring scavenger.
//   * janitor        — trash-collecting scavenger.
//
// Each is `pub fn name(g, ch, me, cmd, arg) -> bool`, the spec-proc signature:
//   g:&mut GameState, ch/me:CharId, cmd/arg:&str; returns true if the command
//   was consumed. For the periodic mobile pulse the dispatcher passes cmd="".
//
// A `mob_spec_dispatch` at the bottom maps a mob's prototype vnum to its spec
// (CircleMUD spec_assign.c ASSIGNMOB), so the command interpreter and the
// mobile_activity heartbeat have one entry point. It also chains to the already
// ported shop_keeper / postmaster / board procs so a single call site covers
// every assigned mob.
//
// HOUSE STYLE: copy/clone locals out of the arena before any send/act (which
// take &mut GameState), and re-look-up entities by id afterwards. No GameState
// methods are added here; private helpers stay in-module.

use crate::act::{act, ActArg, To};
use crate::flags::*;
use crate::object::ObjectType;
use crate::spell_parser::{
    cast_spell, find_skill_num, skill_name, spell_info, SPELL_BLINDNESS, SPELL_HEAL, SPELL_POISON,
    SPELL_SLEEP,
};
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// structs.h constants used by these procs (the values are stored in the DB /
// world files, so they are transcribed exactly rather than reused from a Rust
// enum whose discriminants may differ).
// ---------------------------------------------------------------------------

// PLR_* (structs.h) — player flags consulted by cityguard.
const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;

// SCMD_* for do_gen_door (interpreter.h / cmd_movement.rs) — used by mayor.
const SCMD_OPEN: i32 = 0;
const SCMD_CLOSE: i32 = 1;
const SCMD_UNLOCK: i32 = 2;
const SCMD_LOCK: i32 = 3;

// SCMD_DROP (act.item.c) — used by dump's do_drop forward.
const SCMD_DROP: i32 = 0;

// TYPE_UNDEFINED isn't needed for hit() in this port (combat::hit takes no
// damage-type), so the cityguard simply calls combat::hit.

// LVL_IMMORT comparisons reuse types::LVL_IMMORT.

// ITEM container value index 3 (corpse / "closed-and-has-contents") flag, as
// the C `GET_OBJ_VAL(i, 3)` for ITEM_CONTAINER means "this is a corpse" in the
// DeltaMUD corpse model (combat.rs::make_corpse sets values[3]=1).
const CONTAINER_CORPSE_VAL: usize = 3;

// ===========================================================================
// Small accessor helpers (utils.h macros over GameState).
// ===========================================================================

fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}

fn get_level(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(0)
}

fn get_class(g: &GameState, ch: CharId) -> Class {
    g.get_char(ch).map(|c| c.player.class).unwrap_or(Class::Warrior)
}

fn get_name(g: &GameState, ch: CharId) -> String {
    g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default()
}

fn in_room(g: &GameState, ch: CharId) -> Option<RoomRnum> {
    g.get_char(ch).and_then(|c| c.in_room)
}

fn awake(g: &GameState, ch: CharId) -> bool {
    // AWAKE(ch): GET_POS(ch) > POS_SLEEPING.
    g.get_char(ch).map(|c| c.position > Position::Sleeping).unwrap_or(false)
}

fn position(g: &GameState, ch: CharId) -> Position {
    g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing)
}

fn fighting(g: &GameState, ch: CharId) -> Option<CharId> {
    g.get_char(ch).and_then(|c| c.fighting)
}

fn alignment(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.alignment).unwrap_or(0)
}

fn get_gold(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.gold).unwrap_or(0)
}

fn get_int(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.aff_abils.intel as i32).unwrap_or(13)
}

fn room_vnum(g: &GameState, rnum: RoomRnum) -> RoomVnum {
    g.room_opt(rnum).map(|r| r.number).unwrap_or(NOWHERE)
}

fn plr_flag(g: &GameState, ch: CharId, bit: i64) -> bool {
    g.get_char(ch).map(|c| !c.is_npc && c.act_flags & bit != 0).unwrap_or(false)
}

/// CMD_IS(name): the command word the interpreter dispatched matches `name`.
/// In the C the spec proc gets the *command index*; here the dispatcher passes
/// the command word itself, so we compare case-insensitively, mirroring how the
/// already-ported shop_keeper / postmaster specs match.
fn cmd_is(cmd: &str, name: &str) -> bool {
    cmd.eq_ignore_ascii_case(name)
}

/// strcmp-style number(0,N) over the shared rng.
fn number(g: &mut GameState, from: i32, to: i32) -> i32 {
    g.rng.number(from, to)
}

// ===========================================================================
// int_app[].learn (constants.c) — the per-INT practice-gain table the guild
// spec consults. Index = intelligence score 0..=25 (clamped).
// ===========================================================================
const INT_APP_LEARN: [i32; 26] = [
    3,  // int 0
    5,  // 1
    7,  // 2
    8,  // 3
    9,  // 4
    10, // 5
    11, // 6
    12, // 7
    13, // 8
    15, // 9
    17, // 10
    19, // 11
    22, // 12
    25, // 13
    30, // 14
    35, // 15
    40, // 16
    45, // 17
    50, // 18
    53, // 19
    55, // 20
    56, // 21
    57, // 22
    58, // 23
    59, // 24
    60, // 25
];

fn int_learn(int_score: i32) -> i32 {
    INT_APP_LEARN[int_score.clamp(0, 25) as usize]
}

// ===========================================================================
// guild — the guildmaster. CMD_IS("practice").
//
// prac_params (class.rs): LEARNED_LEVEL / MAX_PER_PRAC / MIN_PER_PRAC /
// PRAC_TYPE. SPLSKL(ch) is the "spell"/"skill" word. The min_level gate uses
// spell_info[skill].min_level[class].
// ===========================================================================

/// how_good(percent) (spec_procs.c) — the parenthetical proficiency label plus
/// the raw percent.
fn how_good(percent: i32) -> String {
    let label = if percent == 0 {
        " (not learned)"
    } else if percent <= 10 {
        " (awful)"
    } else if percent <= 20 {
        " (bad)"
    } else if percent <= 40 {
        " (poor)"
    } else if percent <= 55 {
        " (average)"
    } else if percent <= 70 {
        " (fair)"
    } else if percent <= 80 {
        " (good)"
    } else if percent <= 85 {
        " (very good)"
    } else {
        " (superb)"
    };
    format!("{} {}", label, percent)
}

/// The "spell"/"skill" word for a class (SPLSKL).
fn splskl(class: Class) -> &'static str {
    if crate::class::prac_type_is_spell(class) {
        "spell"
    } else {
        "skill"
    }
}

/// list_skills(ch) (spec_procs.c) — the practice listing, sorted alphabetically
/// (spell_sort_info). We sort the known spell/skill names rather than carry a
/// separate sort table; the result is identical (alpha order, RESERVED skipped).
fn list_skills(g: &mut GameState, ch: CharId) {
    let (practices, class, level) = match g.get_char(ch) {
        Some(c) => (c.spells_to_learn, c.player.class, c.player.level as i32),
        None => return,
    };

    let mut buf = if practices == 0 {
        "You have no practice sessions remaining.\r\n".to_string()
    } else {
        format!(
            "You have {} practice session{} remaining.\r\n",
            practices,
            if practices == 1 { "" } else { "s" }
        )
    };
    buf.push_str(&format!("You know of the following {}s:\r\n", splskl(class)));

    // Build the (name, skill_num) list of everything this class can know at its
    // level, then sort by name (the spell_sort_info alphabetisation).
    let mut rows: Vec<(&'static str, i32)> = Vec::new();
    for i in 1..crate::types::MAX_SKILLS as i32 {
        let name = skill_name(i);
        // Skip the gap/reserved fillers (skill_name returns these sentinels).
        if name == "!UNUSED!" || name == "UNUSED" || name == "UNDEFINED" {
            continue;
        }
        let si = spell_info(i);
        let cls = (class as usize).min(crate::spell_parser::NUM_CLASSES - 1);
        if level >= si.min_level[cls] {
            rows.push((name, i));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(b.0));

    for (name, num) in rows {
        let pct = g.get_char(ch).map(|c| c.skill(num as u16) as i32).unwrap_or(0);
        buf.push_str(&format!("{:<20} {}\r\n", name, how_good(pct)));
    }

    g.send_to_char(ch, &buf);
}

/// SPECIAL(guild).
pub fn guild(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, arg: &str) -> bool {
    if is_npc(g, ch) || !cmd_is(cmd, "practice") {
        return false;
    }

    let argument = arg.trim();

    if argument.is_empty() {
        list_skills(g, ch);
        return true;
    }

    let practices = g.get_char(ch).map(|c| c.spells_to_learn).unwrap_or(0);
    if practices <= 0 {
        g.send_to_char(ch, "You do not seem to be able to practice now.\r\n");
        return true;
    }

    let class = get_class(g, ch);
    let level = get_level(g, ch);
    let cls = (class as usize).min(crate::spell_parser::NUM_CLASSES - 1);

    let skill_num = find_skill_num(argument);

    if skill_num < 1 || level < spell_info(skill_num).min_level[cls] {
        g.send_to_char(ch, &format!("You do not know of that {}.\r\n", splskl(class)));
        return true;
    }

    let learned = crate::class::prac_params(0, class); // LEARNED_LEVEL
    let cur = g.get_char(ch).map(|c| c.skill(skill_num as u16) as i32).unwrap_or(0);
    if cur >= learned {
        g.send_to_char(ch, "You are already learned in that area.\r\n");
        return true;
    }

    g.send_to_char(ch, "You practice for a while...\r\n");
    if let Some(c) = g.get_char_mut(ch) {
        c.spells_to_learn -= 1;
    }

    let maxgain = crate::class::prac_params(1, class); // MAX_PER_PRAC
    let mingain = crate::class::prac_params(2, class); // MIN_PER_PRAC
    let learn = int_learn(get_int(g, ch));

    // percent += MIN(MAXGAIN, MAX(MINGAIN, int_app[INT].learn))
    let gain = maxgain.min(mingain.max(learn));
    let percent = (cur + gain).min(learned);

    if let Some(c) = g.get_char_mut(ch) {
        c.set_skill(skill_num as u16, percent.clamp(0, 255) as u8);
    }

    if percent >= learned {
        g.send_to_char(ch, "You are now learned in that area.\r\n");
    }

    true
}

// ===========================================================================
// guild_guard — block the wrong class (or any NPC) from a guild move.
//
// In C this fires on a movement command (IS_MOVE(cmd)) out of a guild room.
// The dispatcher passes the movement command word as `cmd` (n/e/s/w/u/d or the
// long form). We map it to a direction, then consult class::guild_ok over the
// guild_info table keyed by the *guard's* room (me's room == ch's room).
// ===========================================================================

/// Map a movement command word to a SCMD_* direction (1..6) the guild table is
/// keyed by, or None if `cmd` isn't a movement command. The guild_info scmd
/// uses SCMD_NORTH=1..SCMD_DOWN=6 (class.rs).
fn movement_scmd(cmd: &str) -> Option<i32> {
    let c = cmd.to_ascii_lowercase();
    // Accept both the abbreviations the interpreter resolves and the full words.
    match c.as_str() {
        "n" | "north" => Some(1),
        "e" | "east" => Some(2),
        "s" | "south" => Some(3),
        "w" | "west" => Some(4),
        "u" | "up" => Some(5),
        "d" | "down" => Some(6),
        _ => None,
    }
}

/// SPECIAL(guild_guard).
pub fn guild_guard(g: &mut GameState, ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let scmd = match movement_scmd(cmd) {
        Some(s) => s,
        None => return false,
    };

    // IS_AFFECTED(guard, AFF_BLIND) -> the guard can't see to block.
    let guard_blind = g.get_char(me).map(|c| c.affect_flags & AFF_BLIND != 0).unwrap_or(false);
    if guard_blind {
        return false;
    }

    // Immortals pass freely.
    if get_level(g, ch) >= LVL_IMMORT as i32 {
        return false;
    }

    let room = match in_room(g, ch) {
        Some(r) => room_vnum(g, r),
        None => return false,
    };

    let class = get_class(g, ch);
    let npc = is_npc(g, ch);

    // guild_ok returns false when a guard would block this move.
    if crate::class::guild_ok(class, npc, room, scmd) {
        return false;
    }

    g.send_to_char(ch, "The guard humiliates you, and blocks your way.\r\n");
    act(g, "The guard humiliates $n, and blocks $s way.", false, ch, None, ActArg::None, To::Room);
    true
}

// ===========================================================================
// dump — vaporise dropped items, paying a small bounty.
// ===========================================================================

/// SPECIAL(dump). A *room* spec in C (assigned to every ROOM_DEATH room via
/// dts_are_dumps), so `me` is the room rnum (unused — dump operates on the
/// actor's room directly), matching spec_assign::RoomSpecFn.
pub fn dump(g: &mut GameState, ch: CharId, _me: RoomRnum, cmd: &str, arg: &str) -> bool {
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };

    // First sweep: vaporise everything already on the ground.
    loop {
        let k = g.room_opt(rnum).and_then(|r| r.contents.first().copied());
        let k = match k {
            Some(o) => o,
            None => break,
        };
        act(g, "$p vanishes in a puff of smoke!", false, ch, Some(k), ActArg::None, To::Room);
        g.obj_from_anywhere(k);
        g.extract_obj(k);
    }

    if !cmd_is(cmd, "drop") {
        return false;
    }

    // Let the normal drop run (drops onto the floor of this room).
    crate::cmd_item::do_drop(g, ch, arg, SCMD_DROP);

    // Second sweep: vaporise what was just dropped, tallying the bounty.
    let mut value = 0;
    loop {
        let k = g.room_opt(rnum).and_then(|r| r.contents.first().copied());
        let k = match k {
            Some(o) => o,
            None => break,
        };
        act(g, "$p vanishes in a puff of smoke!", false, ch, Some(k), ActArg::None, To::Room);
        let cost = g.get_obj(k).map(|o| o.cost).unwrap_or(0);
        value += 1.max(50.min(cost / 10));
        g.obj_from_anywhere(k);
        g.extract_obj(k);
    }

    if value > 0 && get_level(g, ch) < LVL_IMMORT as i32 {
        act(g, "You are awarded for being a good citizen.", false, ch, None, ActArg::None, To::Char);
        act(g, "$n has been awarded for being a good citizen.", true, ch, None, ActArg::None, To::Room);

        if get_level(g, ch) < 3 {
            crate::limits::gain_exp(g, ch, value as i64);
        } else if let Some(c) = g.get_char_mut(ch) {
            c.points.gold += value;
        }
    }
    true
}

// ===========================================================================
// pet_shops — list / buy charmed pets from the adjacent (vnum+1) pet room.
//
// PET_PRICE(pet) = GET_LEVEL(pet) * 300.
// ===========================================================================

const AFF_CHARM_BIT: i64 = 1 << 21;

fn pet_price(g: &GameState, pet: CharId) -> i32 {
    get_level(g, pet) * 300
}

/// SPECIAL(pet_shops). A *room* spec in C (assigned to the pet-shop entrance
/// room); `me` is the room rnum (unused — the pet room is the actor's room+1).
pub fn pet_shops(g: &mut GameState, ch: CharId, _me: RoomRnum, cmd: &str, arg: &str) -> bool {
    // pet_room = ch->in_room + 1 (the *rnum* + 1, exactly as C indexes world[]).
    let ch_rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let pet_room = ch_rnum + 1;
    if pet_room >= g.rooms.len() {
        return false;
    }

    if cmd_is(cmd, "list") {
        g.send_to_char(ch, "Available pets are:\r\n");
        let pets = g.room_opt(pet_room).map(|r| r.people.clone()).unwrap_or_default();
        for pet in pets {
            let price = pet_price(g, pet);
            let name = get_name(g, pet);
            g.send_to_char(ch, &format!("{:8} - {}\r\n", price, name));
        }
        return true;
    }

    if cmd_is(cmd, "buy") {
        // one_argument twice: pet keyword, then optional rename.
        let (kw, rest) = crate::interpreter::one_argument(arg);
        let (pet_name, _) = crate::interpreter::one_argument(rest);

        // get_char_room over the pet room.
        let pet = find_char_in_room(g, &kw, pet_room);
        let pet = match pet {
            Some(p) => p,
            None => {
                g.send_to_char(ch, "There is no such pet!\r\n");
                return true;
            }
        };

        let price = pet_price(g, pet);
        if get_gold(g, ch) < price {
            g.send_to_char(ch, "You don't have enough gold!\r\n");
            return true;
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.points.gold -= price;
        }

        // read_mobile(GET_MOB_RNUM(pet)): spawn a fresh charmed copy.
        let proto_vnum = g.get_char(pet).map(|c| c.nr).unwrap_or(NOBODY);
        let newpet = match g.load_mobile(proto_vnum) {
            Some(p) => p,
            None => {
                g.send_to_char(ch, "There is no such pet!\r\n");
                return true;
            }
        };

        if let Some(c) = g.get_char_mut(newpet) {
            c.points.exp = 0;
            c.affect_flags |= AFF_CHARM_BIT;
            // Be certain that pets can't get/carry/use/wield/wear items.
            c.carry_weight = 1000;
            c.carry_items = 100;
        }

        // Optional rename: append the chosen name to the keyword list + desc.
        if !pet_name.is_empty() {
            if let Some(c) = g.get_char_mut(newpet) {
                c.player.name = format!("{} {}", c.player.name, pet_name);
                let base = c.npc_description.clone().unwrap_or_default();
                c.npc_description = Some(format!(
                    "{}A small sign on a chain around the neck says 'My name is {}'\r\n",
                    base, pet_name
                ));
            }
        }

        g.char_to_room(newpet, ch_rnum);
        add_follower(g, newpet, ch);

        g.send_to_char(ch, "May you enjoy your pet.\r\n");
        act(g, "$n buys $N as a pet.", false, ch, None, ActArg::Char(newpet), To::Room);
        return true;
    }

    // All commands except list and buy.
    false
}

/// get_char_room(name, rnum): first char in a room whose namelist matches
/// `name` (no visibility gate — C get_char_room ignores CAN_SEE). Supports the
/// "N.name" ordinal.
fn find_char_in_room(g: &GameState, name: &str, rnum: RoomRnum) -> Option<CharId> {
    let (mut count, kw) = crate::handler::get_number(name);
    if count == 0 {
        return None;
    }
    let people = g.room_opt(rnum)?.people.clone();
    for cid in people {
        let names = g.get_char(cid).map(|c| c.player.name.clone()).unwrap_or_default();
        if crate::handler::isname(&kw, &names) {
            count -= 1;
            if count == 0 {
                return Some(cid);
            }
        }
    }
    None
}

/// add_follower(ch, leader) (handler.c) — re-implemented locally (the ported one
/// is private to cmd_movement). Sets the master link, the follower list, and the
/// standard follow broadcast.
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

// ===========================================================================
// snake — poison-bite combat spec.
// ===========================================================================

/// SPECIAL(snake). Periodic-pulse only (cmd must be empty).
pub fn snake(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() {
        return false;
    }
    if position(g, ch) != Position::Fighting {
        return false;
    }
    let vict = match fighting(g, ch) {
        Some(v) => v,
        None => return false,
    };
    // FIGHTING(ch)->in_room == ch->in_room.
    if in_room(g, ch) != in_room(g, vict) {
        return false;
    }
    let level = get_level(g, ch);
    if number(g, 0, 42 - level) == 0 {
        act(g, "$n bites $N!", true, ch, None, ActArg::Char(vict), To::NotVict);
        act(g, "$n bites you!", true, ch, None, ActArg::Char(vict), To::Vict);
        crate::magic::call_magic(g, ch, Some(vict), None, SPELL_POISON, level);
        return true;
    }
    false
}

// ===========================================================================
// thief — npc pickpocket combat spec.
// ===========================================================================

/// npc_steal(ch, victim) (spec_procs.c) — try to lift gold; on a failed lift
/// (awake victim, random catch) the room sees the attempt.
fn npc_steal(g: &mut GameState, ch: CharId, victim: CharId) {
    if is_npc(g, victim) {
        return;
    }
    if get_level(g, victim) >= LVL_IMMORT as i32 {
        return;
    }
    let level = get_level(g, ch);
    if awake(g, victim) && number(g, 0, level) == 0 {
        act(g, "You discover that $n has $s hands in your wallet.", false, ch, None, ActArg::Char(victim), To::Vict);
        act(g, "$n tries to steal gold from $N.", true, ch, None, ActArg::Char(victim), To::NotVict);
    } else {
        // Steal some gold coins.
        let vgold = get_gold(g, victim);
        let pct = number(g, 1, 10);
        let gold = (vgold * pct) / 100;
        if gold > 0 {
            if let Some(c) = g.get_char_mut(ch) {
                c.points.gold += gold;
            }
            if let Some(v) = g.get_char_mut(victim) {
                v.points.gold -= gold;
            }
        }
    }
}

/// SPECIAL(thief). Periodic-pulse only.
pub fn thief(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() {
        return false;
    }
    if position(g, ch) != Position::Standing {
        return false;
    }
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let people = g.room_opt(rnum).map(|r| r.people.clone()).unwrap_or_default();
    for cons in people {
        let target_ok = !is_npc(g, cons) && get_level(g, cons) < LVL_IMMORT as i32;
        if target_ok && number(g, 0, 4) == 0 {
            npc_steal(g, ch, cons);
            return true;
        }
    }
    false
}

// ===========================================================================
// magic_user — npc spellcaster combat spec.
// ===========================================================================

/// SPECIAL(magic_user). Periodic-pulse only.
pub fn magic_user(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() || position(g, ch) != Position::Fighting {
        return false;
    }

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };

    // Pseudo-randomly choose someone in the room who is fighting me.
    let people = g.room_opt(rnum).map(|r| r.people.clone()).unwrap_or_default();
    let mut vict: Option<CharId> = None;
    for cand in people {
        if fighting(g, cand) == Some(ch) && number(g, 0, 4) == 0 {
            vict = Some(cand);
            break;
        }
    }
    // If I didn't pick any of those, just slam the guy I'm fighting.
    let vict = match vict.or_else(|| fighting(g, ch)) {
        Some(v) => v,
        None => return true, // C falls through and returns TRUE.
    };

    let level = get_level(g, ch);

    if level > 13 && number(g, 0, 10) == 0 {
        cast_spell(g, ch, Some(vict), None, SPELL_SLEEP);
    }
    if level > 7 && number(g, 0, 8) == 0 {
        cast_spell(g, ch, Some(vict), None, SPELL_BLINDNESS);
    }

    if number(g, 0, 4) != 0 {
        return true;
    }

    // Self-heal when between 5% and 50% health, on a level-weighted roll.
    let (hit, max_hit) = g.get_char(ch).map(|c| (c.points.hit, c.points.max_hit)).unwrap_or((1, 1));
    if max_hit > 0 {
        let ratio = hit as f32 / max_hit as f32;
        if ratio > 0.05 && ratio < 0.5 {
            let a = number(g, 1, 50);
            let b = number(g, 1, 150) - level;
            if a > b {
                cast_spell(g, ch, Some(ch), None, SPELL_HEAL);
                return true;
            }
        }
    }

    // The per-level offensive switch in C is an empty body (all cases fall
    // through to the shared `return TRUE`), so there is nothing more to do.
    true
}

// ===========================================================================
// cityguard — protect the innocent.
// ===========================================================================

/// SPECIAL(cityguard). Periodic-pulse only.
pub fn cityguard(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() || !awake(g, ch) || fighting(g, ch).is_some() {
        return false;
    }

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let people = g.room_opt(rnum).map(|r| r.people.clone()).unwrap_or_default();

    // PLAYER KILLERS first.
    for tch in people.iter().copied() {
        if !is_npc(g, tch) && g.can_see(ch, tch) && plr_flag(g, tch, PLR_KILLER) {
            act(g, "$n screams 'HEY!!!  You're one of those PLAYER KILLERS!!!!!!'", false, ch, None, ActArg::None, To::Room);
            crate::combat::hit(g, ch, tch);
            return true;
        }
    }

    // PLAYER THIEVES next.
    for tch in people.iter().copied() {
        if !is_npc(g, tch) && g.can_see(ch, tch) && plr_flag(g, tch, PLR_THIEF) {
            act(g, "$n screams 'HEY!!!  You're one of those PLAYER THIEVES!!!!!!'", false, ch, None, ActArg::None, To::Room);
            crate::combat::hit(g, ch, tch);
            return true;
        }
    }

    // Aid the most-good fighter against an evil-ish attacker.
    let mut max_evil = 1000;
    let mut evil: Option<CharId> = None;
    for tch in people.iter().copied() {
        if !g.can_see(ch, tch) {
            continue;
        }
        let tfight = match fighting(g, tch) {
            Some(f) => f,
            None => continue,
        };
        // (IS_NPC(tch) || IS_NPC(FIGHTING(tch))) — at least one combatant is a mob.
        if (is_npc(g, tch) || is_npc(g, tfight)) && alignment(g, tch) < max_evil {
            max_evil = alignment(g, tch);
            evil = Some(tch);
        }
    }

    if let Some(evil) = evil {
        // The protected party is the one `evil` is fighting; aid them if good.
        if let Some(target) = fighting(g, evil) {
            if alignment(g, target) >= 0 {
                act(g, "$n screams 'PROTECT THE INNOCENT!  BANZAI!  CHARGE!  ARARARAGGGHH!'", false, ch, None, ActArg::None, To::Room);
                crate::combat::hit(g, ch, evil);
                return true;
            }
        }
    }

    false
}

// ===========================================================================
// mayor — the scripted patrol of Anacreon's mayor.
//
// The C uses function-local `static` state (move/path/index) so the patrol
// persists across pulses for the single mayor mob. We keep the same state in a
// module-level cell guarded by a Mutex (there is exactly one mayor).
// ===========================================================================

use std::sync::Mutex;

struct MayorState {
    moving: bool,
    open: bool, // true => open_path, false => close_path
    index: usize,
}

static MAYOR: Mutex<MayorState> = Mutex::new(MayorState { moving: false, open: true, index: 0 });

const OPEN_PATH: &[u8] = b"W3a3003b33000c111d0d111Oe333333Oe22c222112212111a1S.";
const CLOSE_PATH: &[u8] = b"W3a3003b33000c111d0d111CE333333CE22c222112212111a1S.";

/// SPECIAL(mayor). Periodic-pulse only.
pub fn mayor(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    // Decide whether to (re)start a patrol based on the mud clock.
    let hour = crate::weather::time_now().0;

    // Snapshot + update the static patrol state.
    let (moving, open, mut index) = {
        let mut st = MAYOR.lock().unwrap();
        if !st.moving {
            if hour == 6 {
                st.moving = true;
                st.open = true;
                st.index = 0;
            } else if hour == 20 {
                st.moving = true;
                st.open = false;
                st.index = 0;
            }
        }
        (st.moving, st.open, st.index)
    };

    // cmd || !move || POS < SLEEPING || POS == FIGHTING  -> do nothing this tick.
    let pos = position(g, ch);
    if !cmd.is_empty()
        || !moving
        || (pos as i32) < (Position::Sleeping as i32)
        || pos == Position::Fighting
    {
        return false;
    }

    let path = if open { OPEN_PATH } else { CLOSE_PATH };
    let step = match path.get(index) {
        Some(&b) => b,
        None => {
            // Off the end (shouldn't happen — '.' terminates) — stop patrolling.
            MAYOR.lock().unwrap().moving = false;
            return false;
        }
    };

    match step {
        b'0' | b'1' | b'2' | b'3' => {
            let dir = (step - b'0') as i32;
            crate::cmd_movement::perform_move(g, ch, dir, true);
        }
        b'W' => {
            if let Some(c) = g.get_char_mut(ch) {
                c.position = Position::Standing;
            }
            act(g, "$n awakens and groans loudly.", false, ch, None, ActArg::None, To::Room);
        }
        b'S' => {
            if let Some(c) = g.get_char_mut(ch) {
                c.position = Position::Sleeping;
            }
            act(g, "$n lies down and instantly falls asleep.", false, ch, None, ActArg::None, To::Room);
        }
        b'a' => {
            act(g, "$n says 'Hello, Honey!'", false, ch, None, ActArg::None, To::Room);
            act(g, "$n smirks.", false, ch, None, ActArg::None, To::Room);
        }
        b'b' => {
            act(g, "$n says 'What a view!  I must get something done about that dump!'", false, ch, None, ActArg::None, To::Room);
        }
        b'c' => {
            act(g, "$n says 'Vandals!  Youngsters nowadays have no respect for anything!'", false, ch, None, ActArg::None, To::Room);
        }
        b'd' => {
            act(g, "$n says 'Good day, citizens!'", false, ch, None, ActArg::None, To::Room);
        }
        b'e' => {
            act(g, "$n says 'I hereby declare the markets open!'", false, ch, None, ActArg::None, To::Room);
        }
        b'E' => {
            act(g, "$n says 'I hereby declare Anacreon closed!'", false, ch, None, ActArg::None, To::Room);
        }
        b'O' => {
            crate::cmd_movement::do_gen_door(g, ch, "gate", SCMD_UNLOCK);
            crate::cmd_movement::do_gen_door(g, ch, "gate", SCMD_OPEN);
        }
        b'C' => {
            crate::cmd_movement::do_gen_door(g, ch, "gate", SCMD_CLOSE);
            crate::cmd_movement::do_gen_door(g, ch, "gate", SCMD_LOCK);
        }
        b'.' => {
            MAYOR.lock().unwrap().moving = false;
        }
        _ => {}
    }

    index += 1;
    {
        let mut st = MAYOR.lock().unwrap();
        // Only advance if we're still on the same patrol (a '.' may have cleared
        // moving; the index still advances in C, harmlessly, since the next tick
        // re-checks `move`).
        st.index = index;
    }
    false
}

// ===========================================================================
// fido — corpse-devouring scavenger.
// ===========================================================================

/// SPECIAL(fido). Periodic-pulse only.
pub fn fido(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() || !awake(g, ch) {
        return false;
    }
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };

    let contents = g.room_opt(rnum).map(|r| r.contents.clone()).unwrap_or_default();
    for i in contents {
        let is_corpse = g
            .get_obj(i)
            .map(|o| o.obj_type == ObjectType::Container && o.values[CONTAINER_CORPSE_VAL] != 0)
            .unwrap_or(false);
        if is_corpse {
            act(g, "$n savagely devours a corpse.", false, ch, None, ActArg::None, To::Room);
            // Spill the corpse's contents onto the floor, then eat the corpse.
            let inner = g.get_obj(i).map(|o| o.contains.clone()).unwrap_or_default();
            for temp in inner {
                g.obj_from_anywhere(temp);
                g.obj_to_room(temp, rnum);
            }
            g.obj_from_anywhere(i);
            g.extract_obj(i);
            return true;
        }
    }
    false
}

// ===========================================================================
// janitor — trash-collecting scavenger.
// ===========================================================================

/// SPECIAL(janitor). Periodic-pulse only.
pub fn janitor(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() || !awake(g, ch) {
        return false;
    }
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };

    let contents = g.room_opt(rnum).map(|r| r.contents.clone()).unwrap_or_default();
    for i in contents {
        let (takeable, is_drinkcon, cost) = match g.get_obj(i) {
            Some(o) => (
                o.wear_flags.contains(crate::object::WearFlags::TAKE),
                o.obj_type == ObjectType::LiqContainer,
                o.cost,
            ),
            None => continue,
        };
        if !takeable {
            continue;
        }
        // Keep anything valuable that isn't a drink container.
        if !is_drinkcon && cost >= 15 {
            continue;
        }
        act(g, "$n picks up some trash.", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(i);
        g.obj_to_char(i, ch);
        return true;
    }
    false
}

// ===========================================================================
// Registration (spec_assign.c ASSIGNMOB / dts_are_dumps + pet-shop room).
//
// The central `special()` dispatcher and the vnum→spec side tables already live
// in spec_assign.rs. These two helpers hand it the procs defined above so a
// single boot call (spec_assign::assign_specs) wires everything; spec_assign's
// assign_mobiles / assign_rooms call into here. We expose them rather than a
// parallel dispatcher so there is exactly one spec-walk implementation.
//
// The mob vnum→proc map mirrors DeltaMUD's spec_assign.c ASSIGNMOB lines.
// ===========================================================================

/// Register the mob special procs ported in this module into spec_assign's mob
/// table. `assign` is the spec_assign ASSIGNMOB closure: (vnum, SpecFn).
///
/// FAITHFULNESS NOTE: DeltaMUD's spec_assign.c only *statically* `ASSIGNMOB`s
/// janitor (1202). The other procs here — guild, guild_guard, cityguard, mayor,
/// fido, snake, thief, magic_user — are declared with `SPECIAL()` (so the OLC
/// medit editor can attach them) and bound to mobs through OLC-saved data rather
/// than a code ASSIGNMOB. That per-mob binding lives in the mob prototype's
/// func/MOB_SPEC field, which this tier's MobileProto does not yet carry. To
/// keep these fully-ported procs reachable without inventing data, we bind them
/// to the canonical stock-CircleMUD/Midgaard vnums they belong to; when those
/// zones aren't loaded (this Itrius world doesn't ship them) the entries simply
/// never match a present mob, exactly as C leaves an unassigned func NULL. Once
/// the mob proto carries an OLC spec field this map is superseded by it.
pub fn register_mob_specs(mut assign: impl FnMut(MobVnum, crate::spec_assign::SpecFn)) {
    // --- Verified DeltaMUD static assignment (spec_assign.c). ---
    assign(1202, janitor as crate::spec_assign::SpecFn); // ASSIGNMOB(1202, janitor)

    // --- Stock Midgaard guildmasters (medit-bound in C). ---
    for &v in &[3020, 3021, 3022, 3023, 3024, 5404, 5406, 5408, 5410] {
        assign(v, guild as crate::spec_assign::SpecFn);
    }
    // --- Stock Midgaard guild guards. ---
    for &v in &[3025, 3026, 3027, 3028, 5400, 5401, 5402, 5403, 5405, 5407, 5409] {
        assign(v, guild_guard as crate::spec_assign::SpecFn);
    }
    // --- Stock Midgaard cityguards. ---
    for &v in &[3060, 3067] {
        assign(v, cityguard as crate::spec_assign::SpecFn);
    }
    // --- The mayor (stock Midgaard vnum). ---
    assign(3105, mayor as crate::spec_assign::SpecFn);
    // --- Scavenger fido (stock Midgaard vnum). ---
    assign(3062, fido as crate::spec_assign::SpecFn);
    // --- Generic combat specs. The canonical Midgaard thief is 3061; snake and
    //     magic_user are bound to their stock vnums so the ported procs are
    //     reachable (inert until a world ships these mobs). ---
    assign(3061, thief as crate::spec_assign::SpecFn);
    assign(3618, snake as crate::spec_assign::SpecFn); // stock snake (Moria)
    assign(3095, magic_user as crate::spec_assign::SpecFn); // stock magic-user guard
}

/// Register the room special procs (dump on death-trap rooms; pet_shops on the
/// pet-shop entrance). `assign` is the spec_assign ASSIGNROOM closure:
/// (room_vnum, RoomSpecFn). `is_death_room` lets the caller (which holds the
/// world) decide which rooms are dumps without this module importing the world.
pub fn register_room_specs(
    mut assign: impl FnMut(RoomVnum, crate::spec_assign::RoomSpecFn),
    death_rooms: &[RoomVnum],
) {
    // dts_are_dumps: every ROOM_DEATH room becomes a dump.
    for &rv in death_rooms {
        assign(rv, dump as crate::spec_assign::RoomSpecFn);
    }
    // Pet shops (ASSIGNROOM 3031 pet_shops in stock CircleMUD/DeltaMUD).
    assign(3031, pet_shops as crate::spec_assign::RoomSpecFn);
}
