// castle.rs — Special procedures for the King's Castle (Greenhill) zone,
// a 1:1 port of CircleMUD/DeltaMUD src/castle.c.
//
// Original design by Pjotr (d90-pem@nada.kth.se), coded by Sapowox
// (d90-jkr@nada.kth.se). This Rust port keeps every behaviour: the BANZAI
// rescue/attack logic, the King's scripted day/night path, Tim/Tom and
// Dick/David blocking the bedroom/treasury doors, James and the cleaning
// woman tidying trash, the training master sparring his pupils, Peter
// inspecting the guard, and Jerry/Michael gambling.
//
// Spec-proc contract (matching shop_keeper / postmaster / board):
//   pub fn name(g, ch, me, cmd, arg) -> bool
//     g    : &mut GameState
//     ch   : the acting character. On the periodic mob pulse the dispatcher
//            passes ch == me (the mob itself); on a player command ch is the
//            player who typed something in the mob's room.
//     me   : the CharId of the mob that owns this spec.
//     cmd  : the command word the player typed; "" on the periodic pulse.
//     arg  : the remaining argument string.
//   Returns true if the command was consumed.
//
// In the C source the spec body refers to the mob as `ch` because on the pulse
// ch == me. The blocking specs (Tim/Tom/Dick/David) additionally examine `ch`
// as the *mover* on a real movement command. We preserve that distinction:
//   * pulse / room-watch logic (banzaii, paths, sparring …) operates on `me`.
//   * block_way inspects the acting `ch` and the typed direction in `cmd`.
//
// THE CASTLE IS OPTIONAL (CONDITIONAL) CONTENT. The shipped DeltaMUD world does
// not include zone 150, so the finders resolve to "nobody here" and these procs
// are faithful no-ops on absent content — exactly CircleMUD's conditional-
// content behaviour, not a port gap. The port is complete, so the procs come
// alive the moment the zone is loaded.

use crate::act::{ActArg, To, act};
use crate::combat::{hit, set_fighting};
use crate::interpreter::is_abbrev;
use crate::spell_parser::SPELL_HEAL;
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Castle constants (castle.c #defines).
// ---------------------------------------------------------------------------

/// Zone number of the King's Castle. Change to relocate the castle.
const Z_KINGS_C: i32 = 150;

/// CASTLE_ITEM(item) — the vnum of the `item`-th mob/room/obj in the castle.
fn castle_item(item: i32) -> i32 {
    Z_KINGS_C * 100 + item
}

/// R_ROOM(zone, num) — real room number of castle room `num`, or None.
fn r_room(g: &GameState, num: i32) -> Option<RoomRnum> {
    g.real_room(Z_KINGS_C * 100 + num)
}

// ---------------------------------------------------------------------------
// Small GameState accessors mirroring the utils.h macros castle.c relies on.
// ---------------------------------------------------------------------------

fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}

/// GET_MOB_VNUM(ch) — the mob prototype vnum (Character::nr), NOBODY for PCs.
fn mob_vnum(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.nr).unwrap_or(NOBODY)
}

/// FIGHTING(ch).
fn fighting(g: &GameState, ch: CharId) -> Option<CharId> {
    g.get_char(ch).and_then(|c| c.fighting)
}

/// GET_POS(ch).
fn position(g: &GameState, ch: CharId) -> Position {
    g.get_char(ch).map(|c| c.position).unwrap_or(Position::Dead)
}

/// AWAKE(ch) — POS_SLEEPING < pos (utils.h: position > SLEEPING && != FIGHTING
/// is not the test; AWAKE only requires GET_POS > SLEEPING).
fn awake(g: &GameState, ch: CharId) -> bool {
    position(g, ch) > Position::Sleeping
}

/// IN_ROOM(ch).
fn in_room(g: &GameState, ch: CharId) -> Option<RoomRnum> {
    g.get_char(ch).and_then(|c| c.in_room)
}

/// GET_HIT(ch).
fn get_hit(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.hit).unwrap_or(0)
}

/// GET_MANA(ch).
fn get_mana(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.mana).unwrap_or(0)
}

/// ch->master (the leader this NPC follows, if any).
fn has_master(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.master.is_some()).unwrap_or(false)
}

/// The NPC's short description (player.short_descr in C); empty if none/PC.
fn short_descr(g: &GameState, ch: CharId) -> String {
    g.get_char(ch)
        .and_then(|c| c.short_desc.clone())
        .unwrap_or_default()
}

/// The list of characters in a character's room (world[in_room].people order).
fn people_in_room(g: &GameState, ch: CharId) -> Vec<CharId> {
    match in_room(g, ch) {
        Some(rnum) => g
            .room_opt(rnum)
            .map(|r| r.people.clone())
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// member_of_staff / member_of_royal_guard (castle.c).
// ---------------------------------------------------------------------------

/// Used mainly by BANZAI:ing NPC:s — is this character castle staff?
fn member_of_staff(g: &GameState, ch: CharId) -> bool {
    if !is_npc(g, ch) {
        return false;
    }
    let ch_num = mob_vnum(g, ch);
    ch_num == castle_item(1)
        || (ch_num > castle_item(2) && ch_num < castle_item(15))
        || (ch_num > castle_item(15) && ch_num < castle_item(18))
        || (ch_num > castle_item(18) && ch_num < castle_item(30))
}

/// Returns true if the character is a guard on duty. Used by Peter, the captain
/// of the royal guard.
fn member_of_royal_guard(g: &GameState, ch: CharId) -> bool {
    if !is_npc(g, ch) {
        return false;
    }
    let ch_num = mob_vnum(g, ch);
    ch_num == castle_item(3)
        || ch_num == castle_item(6)
        || (ch_num > castle_item(7) && ch_num < castle_item(12))
        || (ch_num > castle_item(23) && ch_num < castle_item(26))
}

// ---------------------------------------------------------------------------
// find_npc_by_name / find_guard / get_victim (castle.c).
// ---------------------------------------------------------------------------

/// Returns the id of an NPC in `at_char`'s room whose short description begins
/// with `name` (first `len` chars, case-sensitive strncmp). Used by Tim & Tom.
fn find_npc_by_name(g: &GameState, at_char: CharId, name: &str, len: usize) -> Option<CharId> {
    let prefix = crate::text::utf8_prefix(name, len);
    for cid in people_in_room(g, at_char) {
        if is_npc(g, cid) {
            let sd = short_descr(g, cid);
            if sd.starts_with(prefix) {
                return Some(cid);
            }
        }
    }
    None
}

/// Returns a guard on duty (a royal guard not currently fighting) in `at_char`'s
/// room. Used by Peter the Captain of the Royal Guard.
fn find_guard(g: &GameState, at_char: CharId) -> Option<CharId> {
    for cid in people_in_room(g, at_char) {
        if fighting(g, cid).is_none() && member_of_royal_guard(g, cid) {
            return Some(cid);
        }
    }
    None
}

/// Returns a randomly chosen character in the same room who is fighting someone
/// on the castle staff (gives the attacker a sporting chance via the off-by-one
/// `number(0, n)` roll). Used by BANZAII-ing characters and King Welmar.
fn get_victim(g: &mut GameState, at_char: CharId) -> Option<CharId> {
    let people = people_in_room(g, at_char);

    let mut num_bad_guys = 0;
    for &cid in &people {
        if let Some(opp) = fighting(g, cid) {
            if member_of_staff(g, opp) {
                num_bad_guys += 1;
            }
        }
    }
    if num_bad_guys == 0 {
        return None;
    }

    // How nice, we give them a chance: number(0, n) can roll 0 == "no victim".
    let victim_idx = g.rng.number(0, num_bad_guys);
    if victim_idx == 0 {
        return None;
    }

    let mut count = 0;
    for &cid in &people {
        if let Some(opp) = fighting(g, cid) {
            if member_of_staff(g, opp) {
                count += 1;
                if count == victim_idx {
                    return Some(cid);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// banzaii / do_npc_rescue / block_way (castle.c).
// ---------------------------------------------------------------------------

/// Makes `ch` BANZAII on an attacker of the castle staff. Used by Guards, Tim,
/// Tom, Dick, David, Peter, the Training Master, the King and the Guards.
fn banzaii(g: &mut GameState, ch: CharId) -> bool {
    if !awake(g, ch) || position(g, ch) == Position::Fighting {
        return false;
    }
    if let Some(opponent) = get_victim(g, ch) {
        act(
            g,
            "$n roars: 'Protect the Kingdom of Great King Welmar!  BANZAIIII!!!'",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        hit(g, ch, opponent);
        return true;
    }
    false
}

/// Makes `hero` rescue `victim` from whoever is fighting `victim`. Used by Tim
/// and Tom. Returns true if the rescue happened.
fn do_npc_rescue(g: &mut GameState, hero: CharId, victim: CharId) -> bool {
    // Find the bad guy currently fighting the victim.
    let mut bad_guy: Option<CharId> = None;
    for cid in people_in_room(g, hero) {
        if fighting(g, cid) == Some(victim) {
            bad_guy = Some(cid);
            break;
        }
    }
    let bad_guy = match bad_guy {
        Some(b) => b,
        None => return false,
    };
    // NO WAY I'll rescue the one I'm fighting!
    if bad_guy == hero {
        return false;
    }

    act(
        g,
        "You bravely rescue $N.\r\n",
        false,
        hero,
        None,
        ActArg::Char(victim),
        To::Char,
    );
    act(
        g,
        "You are rescued by $N, your loyal friend!\r\n",
        false,
        victim,
        None,
        ActArg::Char(hero),
        To::Char,
    );
    act(
        g,
        "$n heroically rescues $N.",
        false,
        hero,
        None,
        ActArg::Char(victim),
        To::NotVict,
    );

    // stop_fighting(bad_guy); stop_fighting(hero); then engage each other.
    npc_stop_fighting(g, bad_guy);
    npc_stop_fighting(g, hero);
    set_fighting(g, hero, bad_guy);
    set_fighting(g, bad_guy, hero);
    true
}

/// stop_fighting() — clear a combatant's target and drop POS_FIGHTING back to
/// POS_STANDING (fight.c). combat.rs keeps its own private copy, so the castle
/// module provides the same primitive locally to rewire rescue targets.
fn npc_stop_fighting(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
    }
}

/// Resolve a movement command word to its CircleMUD command-table index
/// (RESERVED=0, north=1, east=2, south=3, west=4, up=5, down=6). Directions sit
/// first in cmd_info[] so an abbreviation like "n"/"e"/"d" resolves to the
/// direction before any later command — exactly the index block_way compares
/// against. Returns 0 for the periodic pulse ("") or a non-direction command.
fn dir_command_num(cmd: &str) -> i32 {
    if cmd.is_empty() {
        return 0;
    }
    const DIRS: [&str; 6] = ["north", "east", "south", "west", "up", "down"];
    for (i, name) in DIRS.iter().enumerate() {
        if is_abbrev(cmd, name) {
            return (i + 1) as i32;
        }
    }
    0
}

/// Block a person trying to enter a room through a prohibited direction. Used by
/// Tim/Tom at the King's bedroom and Dick/David at the treasury. `ch` is the
/// acting (moving) character; `cmd` is the typed command word; `in_room_vnum`
/// is the guarded room; `prohibited_direction` is the pre-increment seed (1).
fn block_way(
    g: &mut GameState,
    ch: CharId,
    cmd: &str,
    _arg: &str,
    in_room_vnum: i32,
    prohibited_direction: i32,
) -> bool {
    // C: if (cmd != ++iProhibited_direction || (short_descr begins "King Welmar"))
    let prohibited = prohibited_direction + 1;
    let cmd_num = dir_command_num(cmd);

    let sd = short_descr(g, ch);
    let is_king = sd.starts_with("King Welmar");

    if cmd_num != prohibited || is_king {
        return false;
    }

    let guarded = g.real_room(in_room_vnum);
    if in_room(g, ch) == guarded && cmd_num == prohibited {
        if !member_of_staff(g, ch) {
            act(
                g,
                "The guard roars at $n and pushes $m back.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        g.send_to_char(
            ch,
            "The guard roars: 'Entrance is Prohibited!', and pushes you back.\r\n",
        );
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// is_trash / fry_victim (castle.c).
// ---------------------------------------------------------------------------

/// Is an object trash? Used by James the Butler and the Cleaning Lady. An item
/// is trash if it is takeable and either a drink container or cheap (cost <= 10).
fn is_trash(g: &GameState, oid: ObjId) -> bool {
    use crate::object::ObjectType;
    let obj = match g.get_obj(oid) {
        Some(o) => o,
        None => return false,
    };
    if !obj.wear_flags.contains(crate::object::WearFlags::TAKE) {
        return false;
    }
    obj.obj_type == ObjectType::LiqContainer || obj.cost <= 10
}

/// Find a suitable victim and cast a _NASTY_ spell on him. Used by King Welmar.
/// As in the C source the three offensive casts (color spray / harm / fireball)
/// are commented out; only the dramatic flavour text plus the occasional
/// self-heal survive. Costs 10 mana per call.
fn fry_victim(g: &mut GameState, ch: CharId) {
    if get_mana(g, ch) < 10 {
        return;
    }

    // Find someone suitable to fry!
    let tch = match get_victim(g, ch) {
        Some(t) => t,
        None => return,
    };

    let level = g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(1);

    match g.rng.number(0, 8) {
        1 | 2 | 3 => {
            act(
                g,
                "You raise your hand in a dramatical gesture.",
                true,
                ch,
                None,
                ActArg::None,
                To::Char,
            );
            act(
                g,
                "$n raises $s hand in a dramatical gesture.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            // cast_spell (ch, tch, 0, SPELL_COLOR_SPRAY) — disabled in C.
        }
        4 | 5 => {
            act(
                g,
                "You concentrate and mumble to yourself.",
                true,
                ch,
                None,
                ActArg::None,
                To::Char,
            );
            act(
                g,
                "$n concentrates, and mumbles to $mself.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            // cast_spell (ch, tch, 0, SPELL_HARM) — disabled in C.
        }
        6 | 7 => {
            act(
                g,
                "You look deeply into the eyes of $N.",
                true,
                ch,
                None,
                ActArg::Char(tch),
                To::Char,
            );
            act(
                g,
                "$n looks deeply into the eyes of $N.",
                true,
                ch,
                None,
                ActArg::Char(tch),
                To::NotVict,
            );
            act(
                g,
                "You see an ill-boding flame in the eye of $n.",
                true,
                ch,
                None,
                ActArg::Char(tch),
                To::Vict,
            );
            // cast_spell (ch, tch, 0, SPELL_FIREBALL) — disabled in C.
        }
        _ => {
            if g.rng.number(0, 1) == 0 {
                // cast_spell (ch, ch, 0, SPELL_HEAL) — the one live cast.
                crate::magic::call_magic(g, ch, Some(ch), None, SPELL_HEAL, level);
            }
        }
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.points.mana -= 10;
    }
}

// ---------------------------------------------------------------------------
// king_welmar — the King's scripted day/night routine (castle.c).
//
// The King advances along a single-character path string, one step per pulse.
// The path opcodes:
//   '0'..'5'  move that direction (digit value, NOT the command index)
//   'A'..'D'  proclaim the corresponding Latin monolog line
//   'P'       pause (do nothing this pulse)
//   'W'       wake & stand
//   'S'       sleep in bed
//   'r'       sit on throne
//   's'       stand up
//   'G'       "Good morning, trusted friends."
//   'g'       "Good morning, dear subjects."
//   'o'       unlock + open the door
//   'c'       close + lock the door
//   '.'       end of path
// ---------------------------------------------------------------------------

const MONOLOG: [&str; 4] = [
    "$n proclaims 'Primus in regnis Geticis coronam'.",
    "$n proclaims 'regiam gessi, subiique regis'.",
    "$n proclaims 'munus et mores colui sereno'.",
    "$n proclaims 'principe dignos'.",
];

const BEDROOM_PATH: &[u8] = b"s33004o1c1S.";
const THRONE_PATH: &[u8] = b"W3o3cG52211rg.";
const MONOLOG_PATH: &[u8] = b"ABCDPPPP.";

/// The King's per-mob walk state. Keyed by mob CharId so multiple incarnations
/// (the world only ever has one) keep independent cursors. Mirrors the C
/// function-static `path`/`index`/`move` — but those statics are shared across
/// every king, which only works because there is a single King; the per-id map
/// is the faithful single-king behaviour with no cross-talk risk.
pub(crate) struct KingWalk {
    pub(crate) path: &'static [u8],
    pub(crate) index: usize,
    pub(crate) moving: bool,
}

// The King's patrol state lives on GameState as `world.king_walks` (phase-1
// statics migration), keyed by the king mob's runtime CharId.
fn king_walks(g: &GameState) -> &std::collections::HashMap<CharId, KingWalk> {
    &g.world.king_walks
}

fn king_walks_mut(g: &mut GameState) -> &mut std::collections::HashMap<CharId, KingWalk> {
    &mut g.world.king_walks
}

/// Control the actions and movements of the King.
pub fn king_welmar(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me; // the spec body's `ch` is the mob on the pulse.

    // Pull (or default-initialise) this king's walk state.
    let (mut path, mut index, mut moving) = {
        let map = king_walks(g);
        match map.get(&ch) {
            Some(w) => (w.path, w.index, w.moving),
            None => (BEDROOM_PATH, 0, false),
        }
    };

    let hours = crate::weather::time_now(g).0;
    let here = in_room(g, ch);

    if !moving {
        if hours == 8 && here == r_room(g, 51) {
            moving = true;
            path = THRONE_PATH;
            index = 0;
        } else if hours == 21 && here == r_room(g, 17) {
            moving = true;
            path = BEDROOM_PATH;
            index = 0;
        } else if hours == 12 && here == r_room(g, 17) {
            moving = true;
            path = MONOLOG_PATH;
            index = 0;
        }
    }

    // A real command, or the King is too hurt / asleep-and-not-walking: bail
    // (but persist whatever scheduling decision we just made).
    let pos = position(g, ch);
    if !cmd.is_empty() || pos < Position::Sleeping || (pos == Position::Sleeping && !moving) {
        store_king_walk(g, ch, path, index, moving);
        return false;
    }

    if pos == Position::Fighting {
        fry_victim(g, ch);
        store_king_walk(g, ch, path, index, moving);
        return false;
    } else if banzaii(g, ch) {
        store_king_walk(g, ch, path, index, moving);
        return false;
    }

    if !moving {
        store_king_walk(g, ch, path, index, moving);
        return false;
    }

    // Execute the current path opcode.
    let op = path.get(index).copied().unwrap_or(b'.');
    match op {
        b'0'..=b'5' => {
            let dir = (op - b'0') as i32;
            crate::cmd_movement::perform_move(g, ch, dir, true);
        }
        b'A'..=b'D' => {
            let line = MONOLOG[(op - b'A') as usize];
            act(g, line, false, ch, None, ActArg::None, To::Room);
        }
        b'P' => {}
        b'W' => {
            if let Some(c) = g.get_char_mut(ch) {
                c.position = Position::Standing;
            }
            act(
                g,
                "$n awakens and stands up.",
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
                "$n lies down on $s beautiful bed and instantly falls asleep.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'r' => {
            if let Some(c) = g.get_char_mut(ch) {
                c.position = Position::Sitting;
            }
            act(
                g,
                "$n sits down on his great throne.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b's' => {
            if let Some(c) = g.get_char_mut(ch) {
                c.position = Position::Standing;
            }
            act(g, "$n stands up.", false, ch, None, ActArg::None, To::Room);
        }
        b'G' => {
            act(
                g,
                "$n says 'Good morning, trusted friends.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'g' => {
            act(
                g,
                "$n says 'Good morning, dear subjects.'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
        }
        b'o' => {
            crate::cmd_movement::do_gen_door(g, ch, "door", SCMD_UNLOCK);
            crate::cmd_movement::do_gen_door(g, ch, "door", SCMD_OPEN);
        }
        b'c' => {
            crate::cmd_movement::do_gen_door(g, ch, "door", SCMD_CLOSE);
            crate::cmd_movement::do_gen_door(g, ch, "door", SCMD_LOCK);
        }
        b'.' => {
            moving = false;
        }
        _ => {}
    }

    index += 1;
    store_king_walk(g, ch, path, index, moving);
    false
}

fn store_king_walk(g: &mut GameState, ch: CharId, path: &'static [u8], index: usize, moving: bool) {
    let map = king_walks_mut(g);
    map.insert(
        ch,
        KingWalk {
            path,
            index,
            moving,
        },
    );
}

// SCMD_* door subcommands (cmd_movement.rs keeps them private, so the castle
// module re-declares the same constants it passes to do_gen_door).
const SCMD_OPEN: i32 = 0;
const SCMD_CLOSE: i32 = 1;
const SCMD_UNLOCK: i32 = 2;
const SCMD_LOCK: i32 = 3;

// ---------------------------------------------------------------------------
// training_master (castle.c).
// ---------------------------------------------------------------------------

/// Acts out training-room sparring when his pupils Brian and Mick are present,
/// and BANZAIs intruders. (The C also forwards to the guild spec for warrior
/// practice; here the guild is reached through the normal command dispatch, so
/// the master just runs his ambient flavour — identical observable behaviour
/// since the C `guild` forward only triggers on an actual "practice" command,
/// which the master returns FALSE for and lets fall through anyway.)
pub fn training_master(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me;

    if !awake(g, ch) || position(g, ch) == Position::Fighting {
        return false;
    }
    if !cmd.is_empty() {
        return false;
    }

    if !banzaii(g, ch) && g.rng.number(0, 2) == 0 {
        let pupil1 = find_npc_by_name(g, ch, "Brian", 5);
        let pupil2 = find_npc_by_name(g, ch, "Mick", 4);
        if let (Some(mut p1), Some(mut p2)) = (pupil1, pupil2) {
            if fighting(g, p1).is_none() && fighting(g, p2).is_none() {
                if g.rng.number(0, 1) == 1 {
                    std::mem::swap(&mut p1, &mut p2);
                }
                match g.rng.number(0, 7) {
                    0 => {
                        act(
                            g,
                            "$n hits $N on $s head with a powerful blow.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You hit $N on $s head with a powerful blow.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n hits you on your head with a powerful blow.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Vict,
                        );
                    }
                    1 => {
                        act(
                            g,
                            "$n hits $N in $s chest with a thrust.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You manage to thrust $N in the chest.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n manages to thrust you in your chest.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Vict,
                        );
                    }
                    2 => {
                        g.send_to_char(ch, "You command your pupils to bow\r\n.");
                        act(
                            g,
                            "$n commands his pupils to bow.",
                            false,
                            ch,
                            None,
                            ActArg::None,
                            To::Room,
                        );
                        act(
                            g,
                            "$n bows before $N.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "$N bows before $n.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You bow before $N, who returns your gesture.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Char,
                        );
                        act(
                            g,
                            "You bow before $n, who returns your gesture.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Vict,
                        );
                    }
                    3 => {
                        act(
                            g,
                            "$N yells at $n, as he fumbles and drops his sword.",
                            false,
                            p1,
                            None,
                            ActArg::Char(ch),
                            To::NotVict,
                        );
                        act(
                            g,
                            "$n quickly picks up his weapon.",
                            false,
                            p1,
                            None,
                            ActArg::None,
                            To::Room,
                        );
                        act(
                            g,
                            "$N yells at you, as you fumble, losing your weapon.",
                            false,
                            p1,
                            None,
                            ActArg::Char(ch),
                            To::Char,
                        );
                        g.send_to_char(p1, "You quickly pick up your weapon again.");
                        act(
                            g,
                            "You yell at $n, as he fumbles, losing his weapon.",
                            false,
                            p1,
                            None,
                            ActArg::Char(ch),
                            To::Vict,
                        );
                    }
                    4 => {
                        act(
                            g,
                            "$N tricks $n, and slashes him across the back.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "$N tricks you, and slashes you across your back.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Char,
                        );
                        act(
                            g,
                            "You trick $n, and quickly slash him across his back.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Vict,
                        );
                    }
                    5 => {
                        act(
                            g,
                            "$n lunges a blow at $N but $N parries skillfully.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You lunge a blow at $N but $E parries skillfully.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n lunges a blow at you, but you skillfully parry it.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Vict,
                        );
                    }
                    6 => {
                        act(
                            g,
                            "$n clumsily tries to kick $N, but misses.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You clumsily miss $N with your poor excuse for a kick.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n fails an unusually clumsy attempt at kicking you.",
                            false,
                            p1,
                            None,
                            ActArg::Char(p2),
                            To::Vict,
                        );
                    }
                    _ => {
                        g.send_to_char(ch, "You show your pupils an advanced technique.");
                        act(
                            g,
                            "$n shows his pupils an advanced technique.",
                            false,
                            ch,
                            None,
                            ActArg::None,
                            To::Room,
                        );
                    }
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// tom / tim — the King's twin bodyguards (castle.c).
// ---------------------------------------------------------------------------

/// Make `guard` follow the King by feeding the King's keyword to do_follow.
/// In C the call is `do_follow(ch, "King Welmar", 0, 0)`, which resolves the
/// king in the room by name; do_follow in this port resolves by visible keyword,
/// so we hand it the king's first name-keyword (its short_descr's leading word
/// is the in-room reference, but the king's keyword list — player.name — is what
/// the finder matches). Passing the king's keyword reproduces the same follow.
fn npc_follow(g: &mut GameState, guard: CharId, king: CharId) {
    let keyword = g
        .get_char(king)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let first = keyword.split_whitespace().next().unwrap_or("").to_string();
    if !first.is_empty() {
        crate::cmd_movement::do_follow(g, guard, &first, 0);
    }
}

/// Tom, Tim's twin — follows the King, rescues him and Tim, BANZAIs, and blocks
/// the bedroom door (east into CASTLE_ITEM(49)).
pub fn tom(g: &mut GameState, ch_actor: CharId, me: CharId, cmd: &str, arg: &str) -> bool {
    let ch = me;

    if !awake(g, ch) {
        return false;
    }

    if cmd.is_empty() {
        if let Some(king) = find_npc_by_name(g, ch, "King Welmar", 11) {
            if !has_master(g, ch) {
                npc_follow(g, ch, king);
            }
            if fighting(g, king).is_some() {
                do_npc_rescue(g, ch, king);
            }
        }
    }
    if cmd.is_empty() {
        if let Some(tim) = find_npc_by_name(g, ch, "Tim", 3) {
            if fighting(g, tim).is_some() && 2 * get_hit(g, tim) < get_hit(g, ch) {
                do_npc_rescue(g, ch, tim);
            }
        }
    }

    if cmd.is_empty() && position(g, ch) != Position::Fighting {
        banzaii(g, ch);
    }

    // block_way inspects the *acting* character & their typed direction.
    block_way(g, ch_actor, cmd, arg, castle_item(49), 1)
}

/// Tim, Tom's twin — mirror of `tom` (follows the King, rescues him and Tom,
/// BANZAIs, blocks the bedroom door).
pub fn tim(g: &mut GameState, ch_actor: CharId, me: CharId, cmd: &str, arg: &str) -> bool {
    let ch = me;

    if !awake(g, ch) {
        return false;
    }

    if cmd.is_empty() {
        if let Some(king) = find_npc_by_name(g, ch, "King Welmar", 11) {
            if !has_master(g, ch) {
                npc_follow(g, ch, king);
            }
            if fighting(g, king).is_some() {
                do_npc_rescue(g, ch, king);
            }
        }
    }
    if cmd.is_empty() {
        if let Some(tom) = find_npc_by_name(g, ch, "Tom", 3) {
            if fighting(g, tom).is_some() && 2 * get_hit(g, tom) < get_hit(g, ch) {
                do_npc_rescue(g, ch, tom);
            }
        }
    }

    if cmd.is_empty() && position(g, ch) != Position::Fighting {
        banzaii(g, ch);
    }

    block_way(g, ch_actor, cmd, arg, castle_item(49), 1)
}

// ---------------------------------------------------------------------------
// James / cleaning — the housekeeping pair (castle.c).
// ---------------------------------------------------------------------------

/// James the Butler. Complains about (and pockets) any trash he finds.
pub fn james(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me;

    if !cmd.is_empty() || !awake(g, ch) || position(g, ch) == Position::Fighting {
        return false;
    }

    let contents = match in_room(g, ch) {
        Some(rnum) => g
            .room_opt(rnum)
            .map(|r| r.contents.clone())
            .unwrap_or_default(),
        None => return false,
    };

    // C: the loop checks the FIRST object only — it returns FALSE the moment it
    // sees a non-trash item (the `else return FALSE`). Faithfully replicate.
    if let Some(&first) = contents.first() {
        if is_trash(g, first) {
            act(
                g,
                "$n says: 'My oh my!  I ought to fire that lazy cleaning woman!'",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            act(
                g,
                "$n picks up a piece of trash.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.obj_from_anywhere(first);
            g.obj_to_char(first, ch);
            return true;
        } else {
            return false;
        }
    }
    false
}

/// The Cleaning Woman. Picks up the first piece of trash she finds.
pub fn cleaning(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me;

    if !cmd.is_empty() || !awake(g, ch) {
        return false;
    }

    let contents = match in_room(g, ch) {
        Some(rnum) => g
            .room_opt(rnum)
            .map(|r| r.contents.clone())
            .unwrap_or_default(),
        None => return false,
    };

    for oid in contents {
        if is_trash(g, oid) {
            act(
                g,
                "$n picks up some trash.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.obj_from_anywhere(oid);
            g.obj_to_char(oid, ch);
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// CastleGuard / DicknDavid / peter (castle.c).
// ---------------------------------------------------------------------------

/// Standard routine for ordinary castle guards: BANZAI intruders on the pulse.
pub fn castle_guard(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me;

    if !cmd.is_empty() || !awake(g, ch) || position(g, ch) == Position::Fighting {
        return false;
    }

    banzaii(g, ch)
}

/// Dick and David, guards of the Treasury. BANZAI then block the treasury door
/// (east into CASTLE_ITEM(36)).
pub fn dick_n_david(g: &mut GameState, ch_actor: CharId, me: CharId, cmd: &str, arg: &str) -> bool {
    let ch = me;

    if !awake(g, ch) {
        return false;
    }

    if cmd.is_empty() && position(g, ch) != Position::Fighting {
        banzaii(g, ch);
    }

    block_way(g, ch_actor, cmd, arg, castle_item(36), 1)
}

/// Peter, Captain of the Guards. BANZAIs intruders, then occasionally inspects a
/// guard on duty with one of six randomly chosen routines.
pub fn peter(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me;

    if !cmd.is_empty() || !awake(g, ch) || position(g, ch) == Position::Fighting {
        return false;
    }

    if banzaii(g, ch) {
        return false;
    }

    if g.rng.number(0, 3) == 0 {
        if let Some(guard) = find_guard(g, ch) {
            match g.rng.number(0, 5) {
                0 => {
                    act(
                        g,
                        "$N comes sharply into attention as $n inspects $M.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::NotVict,
                    );
                    act(
                        g,
                        "$N comes sharply into attention as you inspect $M.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Char,
                    );
                    act(
                        g,
                        "You go sharply into attention as $n inspects you.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Vict,
                    );
                }
                1 => {
                    act(
                        g,
                        "$N looks very small, as $n roars at $M.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::NotVict,
                    );
                    act(
                        g,
                        "$N looks very small as you roar at $M.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Char,
                    );
                    act(
                        g,
                        "You feel very small as $N roars at you.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Vict,
                    );
                }
                2 => {
                    act(
                        g,
                        "$n gives $N some Royal directions.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::NotVict,
                    );
                    act(
                        g,
                        "You give $N some Royal directions.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Char,
                    );
                    act(
                        g,
                        "$n gives you some Royal directions.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Vict,
                    );
                }
                3 => {
                    act(
                        g,
                        "$n looks at you.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Vict,
                    );
                    act(
                        g,
                        "$n looks at $N.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::NotVict,
                    );
                    act(
                        g,
                        "$n growls: 'Those boots need polishing!'",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Room,
                    );
                    act(
                        g,
                        "You growl at $N.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Char,
                    );
                }
                4 => {
                    act(
                        g,
                        "$n looks at you.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Vict,
                    );
                    act(
                        g,
                        "$n looks at $N.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::NotVict,
                    );
                    act(
                        g,
                        "$n growls: 'Straighten that collar!'",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Room,
                    );
                    act(
                        g,
                        "You growl at $N.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Char,
                    );
                }
                _ => {
                    act(
                        g,
                        "$n looks at you.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Vict,
                    );
                    act(
                        g,
                        "$n looks at $N.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::NotVict,
                    );
                    act(
                        g,
                        "$n growls: 'That chain mail looks rusty!  CLEAN IT !!!'",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Room,
                    );
                    act(
                        g,
                        "You growl at $N.",
                        false,
                        ch,
                        None,
                        ActArg::Char(guard),
                        To::Char,
                    );
                }
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// jerry — Jerry & Michael, the gamblers (castle.c).
// ---------------------------------------------------------------------------

/// Jerry the Gambler (with Michael) in x08 of the King's Castle. BANZAIs
/// intruders, otherwise rolls dice with Michael in a little tableau.
pub fn jerry(g: &mut GameState, _ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    let ch = me;

    if !awake(g, ch) || position(g, ch) == Position::Fighting {
        return false;
    }
    if !cmd.is_empty() {
        return false;
    }

    if !banzaii(g, ch) && g.rng.number(0, 2) == 0 {
        let mut g1 = ch;
        if let Some(mut g2) = find_npc_by_name(g, ch, "Michael", 7) {
            if fighting(g, g1).is_none() && fighting(g, g2).is_none() {
                if g.rng.number(0, 1) == 1 {
                    std::mem::swap(&mut g1, &mut g2);
                }
                match g.rng.number(0, 5) {
                    0 => {
                        act(
                            g,
                            "$n rolls the dice and cheers loudly at the result.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You roll the dice and cheer. GREAT!",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n cheers loudly as $e rolls the dice.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Vict,
                        );
                    }
                    1 => {
                        act(
                            g,
                            "$n curses the Goddess of Luck roundly as he sees $N's roll.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You curse the Goddess of Luck as $N rolls.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n swears angrily. You are in luck!",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Vict,
                        );
                    }
                    2 => {
                        act(
                            g,
                            "$n sighs loudly and gives $N some gold.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You sigh loudly at the pain of having to give $N some gold.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n sighs loudly as $e gives you your rightful win.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Vict,
                        );
                    }
                    3 => {
                        act(
                            g,
                            "$n smiles remorsefully as $N's roll tops his.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You smile sadly as you see that $N beats you. Again.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n smiles remorsefully as your roll tops his.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Vict,
                        );
                    }
                    4 => {
                        act(
                            g,
                            "$n excitedly follows the dice with his eyes.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You excitedly follow the dice with your eyes.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n excitedly follows the dice with his eyes.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Vict,
                        );
                    }
                    _ => {
                        act(
                            g,
                            "$n says 'Well, my luck has to change soon', as he shakes the dice.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::NotVict,
                        );
                        act(
                            g,
                            "You say 'Well, my luck has to change soon' and shake the dice.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Char,
                        );
                        act(
                            g,
                            "$n says 'Well, my luck has to change soon', as he shakes the dice.",
                            false,
                            g1,
                            None,
                            ActArg::Char(g2),
                            To::Vict,
                        );
                    }
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// assign_kings_castle — the spec-assignment table (castle.c).
//
// Returns the (castle-relative mob index, spec function) pairs the spec
// dispatcher should register. The caller resolves each index to a real mob via
// R_MOB(Z_KINGS_C, idx) == real_mobile(Z_KINGS_C*100 + idx) and stores the
// function pointer on that mob prototype, exactly as the C assign_kings_castle
// writes C_MOB_SPEC(Z_KINGS_C, idx). Exposed so spec_assign can call it.
// ---------------------------------------------------------------------------

/// One mob spec proc, matching the shop_keeper/postmaster shape.
pub type CastleSpec = fn(&mut GameState, CharId, CharId, &str, &str) -> bool;

/// (mob vnum, spec) pairs for every special mob in the King's Castle. The vnum
/// is the absolute prototype number (Z_KINGS_C*100 + index) so the dispatcher
/// can match it directly against `Character::nr` without re-deriving the zone.
pub fn assign_kings_castle() -> Vec<(MobVnum, CastleSpec)> {
    vec![
        (castle_item(0), castle_guard),     // Gwydion
        (castle_item(1), king_welmar),      // Our dear friend, the King
        (castle_item(3), castle_guard),     // Jim
        (castle_item(4), castle_guard),     // Brian
        (castle_item(5), castle_guard),     // Mick
        (castle_item(6), castle_guard),     // Matt
        (castle_item(7), castle_guard),     // Jochem
        (castle_item(8), castle_guard),     // Anne
        (castle_item(9), castle_guard),     // Andrew
        (castle_item(10), castle_guard),    // Bertram
        (castle_item(11), castle_guard),    // Jeanette
        (castle_item(12), peter),           // Peter
        (castle_item(13), training_master), // The training master
        (castle_item(16), james),           // James the Butler
        (castle_item(17), cleaning),        // Ze Cleaning Fomen
        (castle_item(20), tim),             // Tim, Tom's twin
        (castle_item(21), tom),             // Tom, Tim's twin
        (castle_item(24), dick_n_david),    // Dick, guard of the Treasury
        (castle_item(25), dick_n_david),    // David, Dick's brother
        (castle_item(26), jerry),           // Jerry, the Gambler
        (castle_item(27), castle_guard),    // Michael
        (castle_item(28), castle_guard),    // Hans
        (castle_item(29), castle_guard),    // Boris
    ]
}
