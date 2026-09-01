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

use crate::act::{ActArg, To, act};
use crate::flags::*;
use crate::object::ObjectType;
use crate::spell_parser::{
    SPELL_BLINDNESS, SPELL_CURE_BLIND, SPELL_HEAL, SPELL_POISON, SPELL_REGEN_MANA,
    SPELL_REMOVE_CURSE, SPELL_REMOVE_POISON, SPELL_SLEEP, cast_spell, find_skill_num, skill_name,
    spell_info,
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

// AFF_WATERWALK (structs.h bit 6) — consulted by the portal's local has_boat.
const AFF_WATERWALK: i64 = 1 << 6;

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
    g.get_char(ch)
        .map(|c| c.player.class)
        .unwrap_or(Class::Warrior)
}

fn get_name(g: &GameState, ch: CharId) -> String {
    g.get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default()
}

fn in_room(g: &GameState, ch: CharId) -> Option<RoomRnum> {
    g.get_char(ch).and_then(|c| c.in_room)
}

fn awake(g: &GameState, ch: CharId) -> bool {
    // AWAKE(ch): GET_POS(ch) > POS_SLEEPING.
    g.get_char(ch)
        .map(|c| c.position > Position::Sleeping)
        .unwrap_or(false)
}

fn position(g: &GameState, ch: CharId) -> Position {
    g.get_char(ch)
        .map(|c| c.position)
        .unwrap_or(Position::Standing)
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
    g.get_char(ch)
        .map(|c| c.aff_abils.intel as i32)
        .unwrap_or(13)
}

fn room_vnum(g: &GameState, rnum: RoomRnum) -> RoomVnum {
    g.room_opt(rnum).map(|r| r.number).unwrap_or(NOWHERE)
}

fn plr_flag(g: &GameState, ch: CharId, bit: i64) -> bool {
    g.get_char(ch)
        .map(|c| !c.is_npc && c.act_flags & bit != 0)
        .unwrap_or(false)
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
    buf.push_str(&format!(
        "You know of the following {}s:\r\n",
        splskl(class)
    ));

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
        let pct = g
            .get_char(ch)
            .map(|c| c.skill(num as u16) as i32)
            .unwrap_or(0);
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
        g.send_to_char(
            ch,
            &format!("You do not know of that {}.\r\n", splskl(class)),
        );
        return true;
    }

    let learned = crate::class::prac_params(0, class); // LEARNED_LEVEL
    let cur = g
        .get_char(ch)
        .map(|c| c.skill(skill_num as u16) as i32)
        .unwrap_or(0);
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
    let guard_blind = g
        .get_char(me)
        .map(|c| c.affect_flags & AFF_BLIND != 0)
        .unwrap_or(false);
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
    act(
        g,
        "The guard humiliates $n, and blocks $s way.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    true
}

// ===========================================================================
// dump — vaporise dropped items, paying a small bounty.
// ===========================================================================

/// SPECIAL(dump). A *room* spec in C (assigned to every ROOM_DEATH room via
/// dts_are_dumps), so `me` is the room rnum (unused — dump operates on the
/// actor's room directly), matching spec_assign::RoomSpecFn.
pub fn dump(g: &mut GameState, ch: CharId, _me: RoomRnum, cmd: &str, arg: &str) -> bool {
    // Room specs fire on EVERY dispatch, including the movement specials check
    // (cmd "move") and any pulse-style re-entry with an empty cmd: gate the
    // whole proc on the actual drop command so mere movement through a death
    // trap does not vaporise the room's contents.
    if !cmd_is(cmd, "drop") {
        return false;
    }

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };

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
        act(
            g,
            "$p vanishes in a puff of smoke!",
            false,
            ch,
            Some(k),
            ActArg::None,
            To::Room,
        );
        let cost = g.get_obj(k).map(|o| o.cost).unwrap_or(0);
        value += 1.max(50.min(cost / 10));
        g.extract_obj(k);
    }

    if value > 0 && get_level(g, ch) < LVL_IMMORT as i32 {
        act(
            g,
            "You are awarded for being a good citizen.",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        act(
            g,
            "$n has been awarded for being a good citizen.",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );

        if get_level(g, ch) < 3 {
            crate::limits::gain_exp(g, ch, value as i64);
        } else if let Some(c) = g.get_char_mut(ch) {
            crate::gold::credit(c, crate::gold::Account::Carried, i64::from(value));
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
        let pets = g
            .room_opt(pet_room)
            .map(|r| r.people.clone())
            .unwrap_or_default();
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
        // read_mobile(GET_MOB_RNUM(pet)): spawn a fresh charmed copy.
        let proto_vnum = g.get_char(pet).map(|c| c.nr).unwrap_or(NOBODY);
        let newpet = match g.load_mobile(proto_vnum) {
            Some(p) => p,
            None => {
                g.send_to_char(ch, "There is no such pet!\r\n");
                return true;
            }
        };
        if let Some(c) = g.get_char_mut(ch) {
            crate::gold::debit(c, crate::gold::Account::Carried, i64::from(price));
        }

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
        crate::dg_triggers::load_mtrigger(g, newpet);

        g.send_to_char(ch, "May you enjoy your pet.\r\n");
        act(
            g,
            "$n buys $N as a pet.",
            false,
            ch,
            None,
            ActArg::Char(newpet),
            To::Room,
        );
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
        let names = g
            .get_char(cid)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
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
        act(
            g,
            "$n bites $N!",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        act(
            g,
            "$n bites you!",
            true,
            ch,
            None,
            ActArg::Char(vict),
            To::Vict,
        );
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
        act(
            g,
            "You discover that $n has $s hands in your wallet.",
            false,
            ch,
            None,
            ActArg::Char(victim),
            To::Vict,
        );
        act(
            g,
            "$n tries to steal gold from $N.",
            true,
            ch,
            None,
            ActArg::Char(victim),
            To::NotVict,
        );
    } else {
        // Steal some gold coins.
        let vgold = get_gold(g, victim);
        let pct = number(g, 1, 10);
        let gold = (i64::from(vgold) * i64::from(pct)) / 100;
        if gold > 0 {
            crate::gold::transfer_between(
                g,
                victim,
                crate::gold::Account::Carried,
                ch,
                crate::gold::Account::Carried,
                gold,
            );
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
    let people = g
        .room_opt(rnum)
        .map(|r| r.people.clone())
        .unwrap_or_default();
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
    let people = g
        .room_opt(rnum)
        .map(|r| r.people.clone())
        .unwrap_or_default();
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
    let (hit, max_hit) = g
        .get_char(ch)
        .map(|c| (c.points.hit, c.points.max_hit))
        .unwrap_or((1, 1));
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
    let people = g
        .room_opt(rnum)
        .map(|r| r.people.clone())
        .unwrap_or_default();

    // PLAYER KILLERS first.
    for tch in people.iter().copied() {
        if !is_npc(g, tch) && g.can_see(ch, tch) && plr_flag(g, tch, PLR_KILLER) {
            act(
                g,
                "$n screams 'HEY!!!  You're one of those PLAYER KILLERS!!!!!!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            crate::combat::hit(g, ch, tch);
            return true;
        }
    }

    // PLAYER THIEVES next.
    for tch in people.iter().copied() {
        if !is_npc(g, tch) && g.can_see(ch, tch) && plr_flag(g, tch, PLR_THIEF) {
            act(
                g,
                "$n screams 'HEY!!!  You're one of those PLAYER THIEVES!!!!!!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
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
                act(
                    g,
                    "$n screams 'PROTECT THE INNOCENT!  BANZAI!  CHARGE!  ARARARAGGGHH!'",
                    false,
                    ch,
                    None,
                    ActArg::None,
                    To::Room,
                );
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

static MAYOR: Mutex<MayorState> = Mutex::new(MayorState {
    moving: false,
    open: true,
    index: 0,
});

const OPEN_PATH: &[u8] = b"W3a3003b33000c111d0d111Oe333333Oe22c222112212111a1S.";
const CLOSE_PATH: &[u8] = b"W3a3003b33000c111d0d111CE333333CE22c222112212111a1S.";

/// SPECIAL(mayor). Periodic-pulse only.
pub fn mayor(g: &mut GameState, _actor: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    // C castle.c convention: the spec's `ch` is the MOB THAT OWNS the spec,
    // never the caller who happened to trigger dispatch. Without this rebinding
    // the mayor drove its patrol ON whoever walked through the room (and every
    // patrol step re-entered special() -> mayor() on the mover).
    let ch = me;
    // Decide whether to (re)start a patrol based on the mud clock.
    let hour = crate::weather::time_now().0;

    // Snapshot + update the static patrol state.
    let (moving, open, mut index) = {
        let mut st = crate::lock_ok::lock(&MAYOR);
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
            crate::lock_ok::lock(&MAYOR).moving = false;
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
            act(
                g,
                "$n awakens and groans loudly.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'S' => {
            if let Some(c) = g.get_char_mut(ch) {
                c.position = Position::Sleeping;
            }
            act(
                g,
                "$n lies down and instantly falls asleep.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'a' => {
            act(
                g,
                "$n says 'Hello, Honey!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            act(g, "$n smirks.", false, ch, None, ActArg::None, To::Room);
        }
        b'b' => {
            act(
                g,
                "$n says 'What a view!  I must get something done about that dump!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'c' => {
            act(
                g,
                "$n says 'Vandals!  Youngsters nowadays have no respect for anything!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'd' => {
            act(
                g,
                "$n says 'Good day, citizens!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'e' => {
            act(
                g,
                "$n says 'I hereby declare the markets open!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'E' => {
            act(
                g,
                "$n says 'I hereby declare Anacreon closed!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
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
            crate::lock_ok::lock(&MAYOR).moving = false;
        }
        _ => {}
    }

    index += 1;
    {
        let mut st = crate::lock_ok::lock(&MAYOR);
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

    let contents = g
        .room_opt(rnum)
        .map(|r| r.contents.clone())
        .unwrap_or_default();
    for i in contents {
        let is_corpse = g
            .get_obj(i)
            .map(|o| o.obj_type == ObjectType::Container && o.values[CONTAINER_CORPSE_VAL] != 0)
            .unwrap_or(false);
        if is_corpse {
            act(
                g,
                "$n savagely devours a corpse.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            // Spill the corpse's contents onto the floor, then eat the corpse.
            let inner = g.get_obj(i).map(|o| o.contains.clone()).unwrap_or_default();
            for temp in inner {
                g.obj_from_anywhere(temp);
                g.obj_to_room(temp, rnum);
            }
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

    let contents = g
        .room_opt(rnum)
        .map(|r| r.contents.clone())
        .unwrap_or_default();
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
        act(
            g,
            "$n picks up some trash.",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        g.obj_from_anywhere(i);
        g.obj_to_char(i, ch);
        return true;
    }
    false
}

// ===========================================================================
// puff — the cosmic puff mob (Limbo vnum 1). Random philosophical one-liners on
// the periodic pulse. C SPECIAL(puff): `if (cmd) return 0;` then number(0,66)
// selects one of ten `do_say` lines (cases 10..66 fall through to `return 0`).
// ===========================================================================

/// SPECIAL(puff). Periodic-pulse only (cmd must be empty).
pub fn puff(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() {
        return false;
    }
    let say = |g: &mut GameState, text: &str| {
        crate::cmd_comm::do_say(g, ch, text, 0);
    };
    match number(g, 0, 66) {
        0 => {
            say(g, "The force is strong with this one!");
            true
        }
        1 => {
            say(g, "If you only knew the POWER of the dark side");
            true
        }
        2 => {
            say(g, "Read my lips.. no new taxes");
            true
        }
        3 => {
            say(g, "Whenever I climb I am followed by a dog called Ego");
            true
        }
        4 => {
            say(g, "I'll sleep when I'm dead");
            true
        }
        5 => {
            say(
                g,
                "My advice to you is get married: if you find a good wife you'll be happy; if not, you'll become a philosopher",
            );
            true
        }
        6 => {
            say(
                g,
                "We all agree that your theory is crazy, but is it crazy enough?",
            );
            true
        }
        7 => {
            say(g, "I loved Welmar more than any woman I had ever known");
            true
        }
        8 => {
            say(g, "The graveyards are full of indispensable men");
            true
        }
        9 => {
            say(
                g,
                "I have an existential map; it has 'you are here' written all over it",
            );
            true
        }
        _ => false,
    }
}

// ===========================================================================
// librarian — the bookstore/library NPC (Itrius vnum 102). Random library-themed
// emotes on the periodic pulse. C SPECIAL(librarian): `if (cmd) return 0;` then
// loops over the people in the room, rolling number(0,72) and emoting the first
// hit (cases 16..72 fall through to `return 0`). The loop variable `vict` is the
// suggestive-wink target (case 5); it iterates the people list so `vict` is the
// last person in the room — we mirror that by taking the room's people list.
// ===========================================================================

/// SPECIAL(librarian). Periodic-pulse only (cmd must be empty).
pub fn librarian(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() {
        return false;
    }
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    // C `for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)`
    // executes the switch for the first person. Every switch arm, including
    // default, returns from the special, so the C loop cannot advance. Preserve
    // that exact control flow while avoiding a misleading never-repeating loop.
    let people = g
        .room_opt(rnum)
        .map(|r| r.people.clone())
        .unwrap_or_default();
    let Some(vict) = people.first().copied() else {
        return false;
    };
    match number(g, 0, 72) {
        0 => {
            act(
                g,
                "$n says, 'I sell books from all over the land, why not buy one?'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        1 => {
            act(
                g,
                "$n turns a page in the book she's reading.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        2 => {
            act(
                g,
                "$n drinks a glass of wine.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        3 => {
            act(
                g,
                "$n says, 'I'm reading a book about ancient Midgaard.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        4 => {
            act(
                g,
                "$n says, 'Thanks for being quiet in the library.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        5 => {
            act(
                g,
                "$n winks at $N suggestively.",
                true,
                ch,
                None,
                ActArg::Char(vict),
                To::Room,
            );
            return true;
        }
        6 => {
            act(
                g,
                "$n starts sorting new books.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        7 => {
            act(
                g,
                "$n points at the sign on the wall.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            act(
                g,
                "$n says, 'If you're looking for books just type: list'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        8 => {
            act(
                g,
                "$n says, 'I need a vacation. I'd love to see Jhaden'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        9 => {
            act(
                g,
                "$n puts several books on a shelf.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        10 => {
            act(
                g,
                "$n snickers softly.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        11 => {
            act(
                g,
                "$n says, 'I wish people like you would write books.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        12 => {
            act(
                g,
                "$n says, 'I once met AJ Trfiante here long ago.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            act(
                g,
                "$n says, 'It was at the debute of his restraunt'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        13 => {
            act(
                g,
                "$n seems to be getting tired.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        14 => {
            act(
                g,
                "$n leaves to a back room.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            act(
                g,
                "$n returns with a new pile of books.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        15 => {
            act(
                g,
                "$n says, 'I love to read. It makes you smart, you know.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            return true;
        }
        _ => {
            // C's default returns from the whole special.
            false
        }
    }
}

// ===========================================================================
// temple_healer / temple_mana_regenerator (Battle Arena vnums 4801 / 4802).
// Periodic-pulse temple specs that scan the room and cast restorative spells on
// the neediest player. They fire only when the mud clock's hour != 0.
//
// temple_healer: cure poison, then curse, then blindness (first match each), and
// finally heal the most-wounded character (lowest HP/MAX_HIT ratio).
// temple_mana_regenerator: regen the lowest MANA/MAX_MANA character.
// ===========================================================================

/// SPECIAL(temple_healer). Periodic-pulse only (cmd must be empty).
pub fn temple_healer(g: &mut GameState, ch: CharId, _me: CharId, cmd: &str, _arg: &str) -> bool {
    if !cmd.is_empty() {
        return false;
    }
    // if (time_info.hours != 0)
    if crate::weather::time_now().0 == 0 {
        return false;
    }
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let people = g
        .room_opt(rnum)
        .map(|r| r.people.clone())
        .unwrap_or_default();

    // Poison: cure the (last) poisoned person in the room.
    let mut hitme: Option<CharId> = None;
    for vict in people.iter().copied() {
        if g.get_char(vict)
            .map(|c| c.affect_flags & AFF_POISON != 0)
            .unwrap_or(false)
        {
            hitme = Some(vict);
        }
    }
    if let Some(v) = hitme {
        cast_spell(g, ch, Some(v), None, SPELL_REMOVE_POISON);
        return true;
    }

    // Curse: remove the (last) cursed person's curse.
    hitme = None;
    for vict in people.iter().copied() {
        if g.get_char(vict)
            .map(|c| c.affect_flags & AFF_CURSE != 0)
            .unwrap_or(false)
        {
            hitme = Some(vict);
        }
    }
    if let Some(v) = hitme {
        cast_spell(g, ch, Some(v), None, SPELL_REMOVE_CURSE);
        return true;
    }

    // Blindness: cure the (last) blind person.
    hitme = None;
    for vict in people.iter().copied() {
        if g.get_char(vict)
            .map(|c| c.affect_flags & AFF_BLIND != 0)
            .unwrap_or(false)
        {
            hitme = Some(vict);
        }
    }
    if let Some(v) = hitme {
        cast_spell(g, ch, Some(v), None, SPELL_CURE_BLIND);
        return true;
    }

    // Heal the most-wounded (lowest HP/MAX_HIT ratio < 1.0).
    hitme = None;
    let mut temp2: f32 = 1.0;
    for vict in people.iter().copied() {
        let (hit, maxhit) = g
            .get_char(vict)
            .map(|c| (c.points.hit, c.points.max_hit))
            .unwrap_or((0, 0));
        if maxhit == 0 {
            continue;
        }
        let temp1 = hit as f32 / maxhit as f32;
        if temp1 < temp2 {
            temp2 = temp1;
            hitme = Some(vict);
        }
    }
    if let Some(v) = hitme {
        cast_spell(g, ch, Some(v), None, SPELL_HEAL);
        return true;
    }
    false
}

/// SPECIAL(temple_mana_regenerator). Periodic-pulse only (cmd must be empty).
pub fn temple_mana_regenerator(
    g: &mut GameState,
    ch: CharId,
    _me: CharId,
    cmd: &str,
    _arg: &str,
) -> bool {
    if !cmd.is_empty() {
        return false;
    }
    if crate::weather::time_now().0 == 0 {
        return false;
    }
    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let people = g
        .room_opt(rnum)
        .map(|r| r.people.clone())
        .unwrap_or_default();

    // Regen the lowest MANA/MAX_MANA character (ratio < 1.0).
    let mut hitme: Option<CharId> = None;
    let mut temp2: f32 = 1.0;
    for vict in people {
        let (mana, maxmana) = g
            .get_char(vict)
            .map(|c| (c.points.mana, c.points.max_mana))
            .unwrap_or((0, 0));
        if maxmana == 0 {
            continue;
        }
        let temp1 = mana as f32 / maxmana as f32;
        if temp1 < temp2 {
            temp2 = temp1;
            hitme = Some(vict);
        }
    }
    if let Some(v) = hitme {
        cast_spell(g, ch, Some(v), None, SPELL_REGEN_MANA);
        return true;
    }
    false
}

// ===========================================================================
// portal — the generic portal object (vnum 20). CMD_IS("enter <obj>"): if the
// named object in the room is THIS portal and its value[1] is a valid room rnum,
// transport the actor there. C SPECIAL(portal).
//
// In C `port->obj_flags.value[1]` is the destination *room rnum* (an index into
// world[]); we use it the same way as an index into g.rooms. The riding path
// (RIDING(ch)/has_boat(mount)) is not reachable — DeltaMUD's mount subsystem is
// not present in this port — so the no-mount water-sector branch is ported 1:1
// and the riding branch is omitted (noted in W6 report).
// ===========================================================================

/// has_boat(ch) (act.movement.c) — re-implemented locally (the ported one is
/// private to cmd_movement). AFF_WATERWALK or immortal grants water passage; a
/// carried/worn ITEM_BOAT would too, but the port can't distinguish ITEM_BOAT
/// (same caveat as cmd_movement::has_boat), so boats fall back to those two.
fn has_boat(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_WATERWALK != 0 || (!c.is_npc && c.player.level >= LVL_IMMORT))
        .unwrap_or(false)
}

/// SPECIAL(portal). `me` is the portal object.
pub fn portal(g: &mut GameState, ch: CharId, me: ObjId, cmd: &str, arg: &str) -> bool {
    if !cmd_is(cmd, "enter") {
        return false;
    }

    // one_argument(argument, obj_name).
    let (obj_name, _) = crate::interpreter::one_argument(arg);

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };
    let contents = g
        .room_opt(rnum)
        .map(|r| r.contents.clone())
        .unwrap_or_default();
    let port = match g.get_obj_in_list_vis(ch, &obj_name, &contents) {
        Some(p) => p,
        None => return false,
    };
    if port != me {
        return false;
    }

    let dest = g.get_obj(port).map(|o| o.values[1]).unwrap_or(0);
    if dest <= 0 || dest > 32000 {
        g.send_to_char(ch, "The portal leads nowhere.\n\r");
        return true;
    }
    let dest_rnum = dest as RoomRnum;

    // Water-sector boat requirement (no-mount path only — see header note).
    let here_water = g
        .room_opt(rnum)
        .map(|r| r.sector_type == crate::room::SectorType::WaterNoSwim)
        .unwrap_or(false);
    let there_water = g
        .rooms
        .get(dest_rnum)
        .map(|r| r.sector_type == crate::room::SectorType::WaterNoSwim)
        .unwrap_or(false);
    if (here_water || there_water) && !has_boat(g, ch) {
        g.send_to_char(ch, "You need a boat to go there.\r\n");
        return true;
    }

    act(
        g,
        "$n enters $p, and vanishes!",
        false,
        ch,
        Some(port),
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "You enter $p, and you are transported elsewhere.",
        false,
        ch,
        Some(port),
        ActArg::None,
        To::Char,
    );
    g.char_from_room(ch);
    g.char_to_room(ch, dest_rnum);
    crate::cmd_informative::look_at_room(g, ch, false);
    act(
        g,
        "$n appears from the glowing portal!",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    true
}

// ===========================================================================
// tent — the campsite object (vnum 500). CMD_IS("camp"): while carrying THIS
// tent on the surface map, the player saves their map coordinates, rent-saves,
// closes their other connections, and is extracted (camps out of the game).
// C SPECIAL(tent).
//
// ismap(ch->in_room)/rm2x/rm2y: the C map subsystem injects virtual map rooms
// and derives (x,y) from the room's offset. This port models map rooms instead
// by tagging real rooms with Room.map_x/map_y (maputils.rs), so `ismap` becomes
// "the room has map_x/map_y set" and rm2x/rm2y read those fields directly.
// NOTE: no world currently ships map_x/map_y room tags (the room-injection
// loader is unported), so in practice this gate is always false and the tent
// reports "surface map only" — the single noted dependency (see W6 report).
// ===========================================================================

/// SPECIAL(tent). `me` is the tent object.
pub fn tent(g: &mut GameState, ch: CharId, me: ObjId, cmd: &str, _arg: &str) -> bool {
    if !cmd_is(cmd, "camp") {
        return false;
    }

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return false,
    };

    // ismap(ch->in_room): is this a surface-map room?
    let (mapx, mapy) = match g.rooms.get(rnum).map(|r| (r.map_x, r.map_y)) {
        Some((Some(x), Some(y))) => (x as i64, y as i64),
        _ => {
            g.send_to_char(ch, "You may only setup camp on the surface map.\r\n");
            return true;
        }
    };

    // for (obj=ch->carrying; obj; ...) if (obj==me) { ... }
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    if !carrying.contains(&me) {
        g.send_to_char(ch, "You carry nothing to setup camp with.\r\n");
        return true;
    }

    // Record the camp-out coordinates on the player (saved.mapx / saved.mapy).
    if let Some(c) = g.get_char_mut(ch) {
        c.mapx = mapx;
        c.mapy = mapy;
    }
    g.send_to_char(ch, "\r\nYou setup camp and safely fall asleep...\r\n");
    act(
        g,
        "$n sets up camp and leaves the game.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    // write_aliases(ch) — alias persistence is handled by the regular save path;
    // Crash_rentsave(ch, 0) banks the inventory at zero cost.
    crate::objsave::crash_rentsave(g, ch, 0);

    let name = get_name(g, ch);
    let rcds = room_vnum(g, rnum);
    log::info!("{} has camped out of the game. ({})", name, rcds);

    // Close every *other* connection this player has open (C loops descriptor_list
    // closing duplicate descriptors that share GET_IDNUM(ch)).
    close_other_connections(g, ch);

    // extract_char(ch): camps the player out. The async save path flushes the
    // rented inventory + player row; here we drop the in-world entity.
    if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.state = crate::connection::ConState::Close;
        }
    } else {
        g.extract_char(ch);
    }
    true
}

/// Close every descriptor for the same player idnum *other* than ch's own (C
/// tent loop: for each d in descriptor_list, if d != ch->desc and d->character's
/// idnum == ch's idnum, close_socket(d)). Mirrors do_quit's duplicate-link kill.
fn close_other_connections(g: &mut GameState, ch: CharId) {
    let (my_conn, my_idnum) = match g.get_char(ch) {
        Some(c) => (c.desc, c.idnum),
        None => return,
    };
    let to_close: Vec<ConnId> = g
        .descriptors
        .iter()
        .filter_map(|(&id, d)| {
            if Some(id) == my_conn {
                return None;
            }
            let cid = d.character?;
            let idnum = g.get_char(cid).map(|c| c.idnum)?;
            if idnum == my_idnum { Some(id) } else { None }
        })
        .collect();
    for id in to_close {
        if let Some(d) = g.descriptors.get_mut(&id) {
            d.state = crate::connection::ConState::Close;
        }
    }
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
    for &v in &[
        3025, 3026, 3027, 3028, 5400, 5401, 5402, 5403, 5405, 5407, 5409,
    ] {
        assign(v, guild_guard as crate::spec_assign::SpecFn);
    }
    // --- Stock Midgaard cityguards. ---
    for &v in &[3060, 3067] {
        assign(v, cityguard as crate::spec_assign::SpecFn);
    }
    // --- The mayor (stock Midgaard vnum 3105). NOT assigned: our world has
    // no Midgaard, and zone 31 now owns vnum 3105 (drowned templar) -- the
    // patrol would walk Cloister mobs through another zone's path script.
    // Registered in COMPATIBILITY.md (spec-assignment collisions).
    // --- Scavenger fido (stock Midgaard vnum). ---
    assign(3062, fido as crate::spec_assign::SpecFn);
    // --- Finish-the-game activations (Itrius): the authored guildmasters,
    //     guild entrance guards and city guards. C's spec_assign.c never
    //     assigned guild/guild_guard anywhere, and its cityguard vnums
    //     (3060/3067) are not in the shipped world; the MOTD explicitly
    //     promises law enforcement. These registrations activate the ported
    //     procs for the authored zone-1 mobs (COMPATIBILITY.md register).
    for &v in &[115, 116, 117, 118, 119] {
        assign(v, guild as crate::spec_assign::SpecFn); // Itrius guildmasters
    }
    for &v in &[126, 127, 128, 129, 130] {
        assign(v, guild_guard as crate::spec_assign::SpecFn); // guild entrance guards
    }
    for &v in &[121, 122, 123, 124, 125] {
        assign(v, cityguard as crate::spec_assign::SpecFn); // Itrius city watch
    }
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
    // Pet shops (ASSIGNROOM 3031 in stock CircleMUD/DeltaMUD). NOT assigned
    // to 3031: zone 30 now owns that room (The Tower Magazine), and the
    // proc's pet_room = in_room + 1 arithmetic assumed Midgaard's layout.
    // Registered at vnum 34000 instead -- no shipped room uses it, so
    // production is inert while tests can still build a pet shop there.
    assign(34000, pet_shops as crate::spec_assign::RoomSpecFn);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::dg_db_scripts::TrigProto;
    use crate::dg_handler::{DG_TEST_LOCK, MOB_TRIGGER, MTRIG_LOAD, ScriptKey};
    use crate::room::Room;
    use crate::world::MobileProto;

    fn mobile_proto(vnum: MobVnum, short: &str) -> MobileProto {
        MobileProto {
            vnum,
            name: short.to_string(),
            short_desc: short.to_string(),
            long_desc: format!("{} is here.\r\n", short),
            description: String::new(),
            level: 1,
            hitpoints: 1,
            hit_dice: (0, 0, 1),
            experience: 0,
            gold: 0,
            position: Position::Standing,
            default_pos: Position::Standing,
            sex: Gender::Neutral,
            alignment: 0,
            act_flags: 0,
            affect_flags: 0,
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 1,
            power: 0,
            mpower: 0,
            defense: 0,
            mdefense: 0,
            technique: 0,
            abilities: None,
            attack_type: 0,
        }
    }

    #[test]
    fn pet_shop_purchase_fires_load_trigger_on_new_pet() {
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        crate::dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        let shop_room = g.add_room(Room::new(
            3031,
            0,
            "Pet shop".to_string(),
            "A pet shop.".to_string(),
        ));
        let pet_room = g.add_room(Room::new(
            3032,
            0,
            "Pet room".to_string(),
            "Pets wait here.".to_string(),
        ));
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut buyer = Character::new_player("Buyer".to_string(), Class::Warrior, Race::Human);
        buyer.desc = Some(conn);
        crate::gold::set(&mut buyer, crate::gold::Account::Carried, 1000);
        let buyer = g.create_char(buyer);
        g.char_to_room(buyer, shop_room);
        g.mob_protos.insert(6200, mobile_proto(6200, "puppy"));

        crate::dg_db_scripts::set_test_proto_trigger(
            MOB_TRIGGER,
            6200,
            TrigProto {
                vnum: 96200,
                attach_type: MOB_TRIGGER,
                name: "pet load marker".to_string(),
                trigger_type: MTRIG_LOAD,
                narg: 100,
                arglist: String::new(),
                cmdlist: vec![
                    "set adopted yes".to_string(),
                    "global adopted".to_string(),
                    "halt".to_string(),
                ],
            },
        );
        let display_pet = g.load_mobile(6200).unwrap();
        g.char_to_room(display_pet, pet_room);

        assert!(pet_shops(&mut g, buyer, shop_room, "buy", "puppy"));

        let bought_pet = g
            .get_char(buyer)
            .unwrap()
            .followers
            .iter()
            .copied()
            .find(|&cid| cid != display_pet)
            .unwrap();
        assert_eq!(
            crate::dg_handler::get_global_var(ScriptKey::Mob(bought_pet), "adopted").as_deref(),
            Some("yes")
        );
    }
}
