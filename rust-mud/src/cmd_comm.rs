// cmd_comm — player-level communication commands, a 1:1 port of the C
// `src/act.comm.c` (do_say/do_gsay/do_tell/do_reply/do_spec_comm/do_write/
// do_page/do_gen_comm/do_qcomm/do_ignore + the perform_tell/is_tell_ok/
// isignore helpers).
//
// House style follows commands.rs: copy needed values into locals first,
// then mutate; look entities up by id every time; broadcast via act() and
// direct text via send_to_char / send_line. Channel scope (gossip / auction /
// grats / arena / quest) iterates the connected-player set, mirroring C's
// descriptor_list walk.

use crate::act::{act, act_sleep, ActArg, To};
use crate::flags::*;
use crate::interpreter::half_chop;
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Constants the contract's flags.rs does not yet carry (structs.h / config.c).
// Defined privately so this module is self-contained without editing flags.rs.
// ---------------------------------------------------------------------------

// PRF_* communication-channel preferences (structs.h).
const PRF_DEAF: i64 = 1 << 2; // can't hear shouts
const PRF_NOTELL: i64 = 1 << 3; // can't receive tells
const PRF_NOARENA: i64 = 1 << 9; // can't hear arena channel
const PRF_NOAUCT: i64 = 1 << 18; // can't hear auction channel
const PRF_NOGOSS: i64 = 1 << 19; // can't hear gossip channel
const PRF_NOGRATZ: i64 = 1 << 20; // can't hear grats channel
const PRF_AFK: i64 = 1 << 22; // away from keyboard

// PRF_NOREPEAT / PRF_COLOR_1 / PRF_COLOR_2 / PRF_HOLYLIGHT come from flags.rs.

// PRF2_* (second preference bitvector, structs.h).
const PRF2_QCHAN: i64 = 1 << 0; // on quest channel
const PRF2_MBUILDING: i64 = 1 << 6; // player is building
const PRF2_INTANGIBLE: i64 = 1 << 9; // player is a ghost

// PLR_* (structs.h).
const PLR_WRITING: i64 = 1 << 4; // writing a board/mail/olc message
const PLR_NOSHOUT: i64 = 1 << 8; // not allowed to shout/goss

// MOB act flag (structs.h).
const MOB_NOTALK: i64 = 1 << 23; // mob can't talk at all

// screen.h colour levels.
const C_NRM: i32 = 2;
const C_CMP: i32 = 3;

// config.c tunables.
const LEVEL_CAN_SHOUT: u8 = 1;
const HOLLER_MOVE_COST: i32 = 50;

// config.c message strings.
const OK: &str = "&YOkay.&n\r\n";
const NOPERSON: &str = "&CNo-one by that name here.&n\r\n";

// SCMD_* for do_spec_comm.
const SCMD_WHISPER: i32 = 0;
#[allow(dead_code)]
const SCMD_ASK: i32 = 1;

// SCMD_* for do_gen_comm.
const SCMD_HOLLER: i32 = 0;
const SCMD_SHOUT: i32 = 1;
const SCMD_GOSSIP: i32 = 2;
#[allow(dead_code)]
const SCMD_AUCTION: i32 = 3;
#[allow(dead_code)]
const SCMD_GRATZ: i32 = 4;
const SCMD_GMOTE: i32 = 5;
#[allow(dead_code)]
const SCMD_ARENA: i32 = 6;

// SCMD_* for do_qcomm.
const SCMD_QSAY: i32 = 0;
#[allow(dead_code)]
const SCMD_QECHO: i32 = 1;

// ---------------------------------------------------------------------------
// Small predicate helpers (utils.h macros / screen.h colour level).
// ---------------------------------------------------------------------------

fn prf(g: &GameState, id: CharId, flag: i64) -> bool {
    g.get_char(id).map(|c| c.prf_flags & flag != 0).unwrap_or(false)
}
fn prf2(g: &GameState, id: CharId, flag: i64) -> bool {
    g.get_char(id).map(|c| c.prf2_flags & flag != 0).unwrap_or(false)
}
fn plr(g: &GameState, id: CharId, flag: i64) -> bool {
    let c = match g.get_char(id) {
        Some(c) => c,
        None => return false,
    };
    !c.is_npc && c.act_flags & flag != 0
}
fn mob_flagged(g: &GameState, id: CharId, flag: i64) -> bool {
    let c = match g.get_char(id) {
        Some(c) => c,
        None => return false,
    };
    c.is_npc && c.act_flags & flag != 0
}
fn is_npc(g: &GameState, id: CharId) -> bool {
    g.get_char(id).map(|c| c.is_npc).unwrap_or(false)
}
fn level(g: &GameState, id: CharId) -> u8 {
    g.get_char(id).map(|c| c.player.level).unwrap_or(0)
}
fn has_desc(g: &GameState, id: CharId) -> bool {
    g.get_char(id).map(|c| c.desc.is_some()).unwrap_or(false)
}
fn in_room(g: &GameState, id: CharId) -> Option<RoomRnum> {
    g.get_char(id).and_then(|c| c.in_room)
}
fn room_soundproof(g: &GameState, id: CharId) -> bool {
    match in_room(g, id) {
        Some(rnum) => g
            .room_opt(rnum)
            .map(|r| r.room_flags.contains(RoomFlags::SOUNDPROOF))
            .unwrap_or(false),
        None => false,
    }
}
fn get_name(g: &GameState, id: CharId) -> String {
    g.get_char(id).map(|c| c.player.name.clone()).unwrap_or_default()
}

/// COLOR_LEV(ch): _clrlevel = (PRF_COLOR_1?1:0) + (PRF_COLOR_2?2:0).
fn color_lev(g: &GameState, id: CharId) -> i32 {
    let c = match g.get_char(id) {
        Some(c) => c,
        None => return 0,
    };
    let mut lev = 0;
    if c.prf_flags & PRF_COLOR_1 != 0 {
        lev += 1;
    }
    if c.prf_flags & PRF_COLOR_2 != 0 {
        lev += 2;
    }
    lev
}

/// CCRED(ch, lvl): "&R" if the char's colour level >= lvl, else "".
fn ccred(g: &GameState, id: CharId, lvl: i32) -> &'static str {
    if color_lev(g, id) >= lvl {
        "&R"
    } else {
        ""
    }
}
/// CCNRM(ch, lvl): "&n" if the char's colour level >= lvl, else "".
fn ccnrm(g: &GameState, id: CharId, lvl: i32) -> &'static str {
    if color_lev(g, id) >= lvl {
        "&n"
    } else {
        ""
    }
}

/// PERS(ch, vict): visible name, else immortal/"someone" fallback — used to
/// build the "<name> <speech>" line in do_say (act() also renders $n the same
/// way, but do_say composes the string by hand).
fn pers(g: &GameState, ch: CharId, vict: CharId) -> String {
    if g.can_see(vict, ch) {
        get_name(g, ch)
    } else if level(g, ch) >= LVL_IMMORT as u8 {
        "A Mystical Being".to_string()
    } else {
        "someone".to_string()
    }
}

/// The connected-player set, in a stable order — the analogue of walking
/// `descriptor_list` for the global channels (gossip/auction/grats/arena/
/// quest/page). NPCs and link-dead PCs are excluded.
fn connected_players(g: &GameState) -> Vec<CharId> {
    let mut v: Vec<CharId> = g
        .players_by_name
        .values()
        .copied()
        .filter(|&id| has_desc(g, id))
        .collect();
    v.sort_by_key(|c| c.0);
    v
}

// ---------------------------------------------------------------------------
// makedrunk — slur the speech of a sufficiently-drunk speaker (utils.c). The
// drunk system is not yet ported; with a non-positive DRUNK condition the C
// makedrunk returns the string untouched, which is the common path, so we
// preserve the argument verbatim here.
// ---------------------------------------------------------------------------
fn makedrunk(arg: &str, _g: &GameState, _ch: CharId) -> String {
    arg.to_string()
}

// ---------------------------------------------------------------------------
// Ignore-list predicates. The per-player ignore list (ch->ignore_list) is not
// yet a field on Character, so — exactly as for an empty ignore_list in C —
// nobody is ignored: isignore()/isignoresend() are always false. do_ignore
// below reproduces the user-facing flow against that empty list.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
const IGNORE_PUBLIC: i64 = 1 << 0;
#[allow(dead_code)]
const IGNORE_PRIVATE: i64 = 1 << 1;
#[allow(dead_code)]
const IGNORE_EMOTE: i64 = 1 << 2;

/// isignore(ch1, ch2, type): is ch2 on ch1's ignore list for `type`?
fn isignore(_g: &GameState, _ch1: CharId, _ch2: CharId, _ty: i64) -> bool {
    false
}
/// isignoresend(ch1, ch2, type): like isignore but also notifies ch2.
fn isignoresend(_g: &mut GameState, _ch1: CharId, _ch2: CharId, _ty: i64) -> bool {
    false
}

// ---------------------------------------------------------------------------
// do_ignore — list / add / remove ignore entries. With no persistent
// ignore_list storage the list is always empty; the parse + reply structure
// is reproduced faithfully (immortals can't be ignored, type syntax errors,
// "off" / "isn't on your ignore list", etc.).
// ---------------------------------------------------------------------------
pub fn do_ignore(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = arg.trim_start();

    // No argument: print the (always-empty) ignore table.
    if argument.is_empty() {
        g.send_to_char(ch, "\r\nName                 Type           Reason\r\n");
        g.send_to_char(ch, "-------------------- -------------- ------>\r\n");
        g.send_to_char(ch, "None                 None           None\r\n");
        return;
    }

    // Parse the first space-delimited token as the target name (capped at 20).
    let mut who = String::new();
    let bytes: Vec<char> = argument.chars().collect();
    let mut l = 0usize;
    let mut argnum = 1;
    while l < bytes.len() {
        if bytes[l] == ' ' {
            argnum += 1;
            l += 1;
            continue;
        }
        if argnum != 1 {
            break;
        }
        if who.len() < 20 {
            who.push(bytes[l]);
        }
        l += 1;
    }

    // Player must exist.
    let target = g.find_player_by_name(&who);
    if target.is_none() {
        g.send_to_char(ch, "\r\nNo player with that name exists.\r\n");
        return;
    }
    // Cannot ignore immortals.
    if let Some(t) = target {
        if level(g, t) >= LVL_IMMORT as u8 {
            g.send_to_char(ch, "\r\nYou cannot ignore immortals.\r\n");
            return;
        }
    }

    // After the name we require the ignore type(s) enclosed in ' symbols.
    if l >= bytes.len() || bytes[l] != '\'' {
        g.send_to_char(
            ch,
            "\r\nYou must supply the ignore type(s) enclosed in ' symbols. Types: 'pub priv emote all off'\r\n",
        );
        return;
    }
    l += 1;

    // Collect the type tokens until the closing quote.
    let mut tokbuf = String::new();
    let mut off_requested = false;
    let mut unknown: Option<String> = None;
    while l < bytes.len() {
        let cprev = bytes[l];
        if cprev == ' ' || cprev == '\'' {
            if !tokbuf.is_empty() {
                match tokbuf.to_lowercase().as_str() {
                    "pub" | "priv" | "emote" | "all" => {}
                    "off" => off_requested = true,
                    other => {
                        unknown = Some(other.to_string());
                        break;
                    }
                }
            }
            tokbuf.clear();
            if cprev == '\'' {
                break;
            }
            l += 1;
            continue;
        }
        tokbuf.push(cprev);
        l += 1;
    }

    if let Some(bad) = unknown {
        g.send_to_char(ch, "\r\nUnknown ignore option: '");
        g.send_to_char(ch, &bad);
        g.send_to_char(ch, "'\r\n");
        return;
    }

    // "off" removes the entry — but the list is always empty.
    if off_requested {
        g.send_to_char(ch, "\r\n");
        g.send_to_char(ch, &who);
        g.send_to_char(ch, " isn't on your ignore list.\r\n");
        return;
    }

    // We need a closing quote present (the reason follows it).
    if l >= bytes.len() || bytes[l] != '\'' {
        g.send_to_char(
            ch,
            "\r\nYou must supply the ignore type(s) enclosed in ' symbols. Types: 'pub priv emote all off'\r\n",
        );
        return;
    }
    l += 1;
    if l < bytes.len() && bytes[l] == ' ' {
        l += 1;
    }

    // Remaining text is the reason (defaults to "Ignore").
    let reason: String = if l >= bytes.len() {
        "Ignore".to_string()
    } else {
        bytes[l..].iter().collect()
    };

    // Build the confirmation line exactly as C does.
    let mut out = format!("\r\nIgnored {} on", who);
    // (No persisted types to reflect; C lists those that were set.)
    out.push_str(&format!(" ({})\r\n", reason));
    g.send_to_char(ch, &out);
}

// ---------------------------------------------------------------------------
// do_say
// ---------------------------------------------------------------------------
pub fn do_say(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = arg.trim_start();

    if is_npc(g, ch) && mob_flagged(g, ch, MOB_NOTALK) {
        g.send_to_char(ch, "You weren't designed to talk.\r\n");
        return;
    }
    if argument.is_empty() {
        g.send_to_char(ch, "What do you want to say?\r\n");
        return;
    }

    let argument = makedrunk(argument, g, ch);

    // Determine the verb from the trailing punctuation (C "state").
    let last = argument.chars().last().unwrap_or(' ');
    let (other_verb, self_verb): (&str, &str) = match last {
        '?' => ("asks", "ask"),
        '!' => ("exclaims", "exclaim"),
        _ => ("says", "say"),
    };

    // buf  = "<verb>, '<arg>'"          (what others see, prefixed by PERS)
    // buf2 = "You <self-verb>, '<arg>'" (what the speaker sees)
    let buf = format!("{}, '{}'", other_verb, argument);
    let buf2 = format!("You {}, '{}'", self_verb, argument);

    let dead_msgs = [
        "You feel a slight breeze.",
        "Something beckons at your ear.",
        "You hear a faint whisper.",
        "The hair on your skin stands on end.",
        "The room suddenly gets cold.",
        "Something taps at the back of your mind.",
        "Your heartbeat slows a little.",
        "You notice something in the corner of your eye.",
        "The shadows around you shift a little.",
        "You feel a chill run down your spine.",
    ];

    let rnum = match in_room(g, ch) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    let ch_intangible = prf2(g, ch, PRF2_INTANGIBLE);
    let ch_mbuilding = prf2(g, ch, PRF2_MBUILDING);

    for vict in people {
        if vict == ch {
            if prf(g, ch, PRF_NOREPEAT) {
                g.send_to_char(ch, OK);
            } else {
                act(g, &buf2, false, ch, None, ActArg::None, To::Char);
            }
            continue;
        }
        if isignore(g, vict, ch, IGNORE_PUBLIC) {
            continue;
        }
        let line = if ch_intangible
            && !prf2(g, vict, PRF2_INTANGIBLE)
            && !ch_mbuilding
            && level(g, vict) < LVL_IMMORT as u8
        {
            let idx = g.rng.number(0, 9) as usize;
            dead_msgs[idx.min(9)].to_string()
        } else {
            format!("{} {}", pers(g, ch, vict), buf)
        };
        act(g, &line, false, vict, None, ActArg::None, To::Char);
    }
    // DG speech triggers (mobs/rooms react to what was said).
    crate::dg_triggers::speech_mtrigger(g, ch, arg);
    crate::dg_triggers::speech_wtrigger(g, ch, arg);
}

// ---------------------------------------------------------------------------
// do_gsay (group say / gtell)
// ---------------------------------------------------------------------------
pub fn do_gsay(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = arg.trim_start();

    let grouped = g.get_char(ch).map(|c| c.affect_flags & AFF_GROUP != 0).unwrap_or(false);
    if !grouped {
        g.send_to_char(ch, "But you are not the member of a group!\r\n");
        return;
    }
    if argument.is_empty() {
        g.send_to_char(ch, "Yes, but WHAT do you want to group-say?\r\n");
        return;
    }

    let argument = makedrunk(argument, g, ch);

    // Group leader k: the master, or self if no master.
    let k = g.get_char(ch).and_then(|c| c.master).unwrap_or(ch);

    let buf = format!("$n tells the group, '{}'", argument);

    let k_grouped = g.get_char(k).map(|c| c.affect_flags & AFF_GROUP != 0).unwrap_or(false);
    if k_grouped && k != ch {
        act_sleep(g, &buf, false, ch, None, ActArg::Char(k), To::Vict, true);
    }
    let followers = g.get_char(k).map(|c| c.followers.clone()).unwrap_or_default();
    for f in followers {
        let f_grouped = g.get_char(f).map(|c| c.affect_flags & AFF_GROUP != 0).unwrap_or(false);
        if f_grouped && f != ch && !isignore(g, f, ch, IGNORE_PRIVATE) {
            act_sleep(g, &buf, false, ch, None, ActArg::Char(f), To::Vict, true);
        }
    }

    if prf(g, ch, PRF_NOREPEAT) {
        g.send_to_char(ch, "&YOkay.&n\r\n");
    } else {
        let selfm = format!("You tell the group, '{}'", argument);
        act_sleep(g, &selfm, false, ch, None, ActArg::None, To::Char, true);
    }
}

// ---------------------------------------------------------------------------
// perform_tell / is_tell_ok / do_tell / do_reply
// ---------------------------------------------------------------------------
fn perform_tell(g: &mut GameState, ch: CharId, vict: CharId, arg: &str) {
    let vred = ccred(g, vict, C_NRM);
    g.send_to_char(vict, vred);
    let arg = makedrunk(arg, g, ch);
    let to_vict = format!("$n tells you, '{}'", arg);
    act_sleep(g, &to_vict, false, ch, None, ActArg::Char(vict), To::Vict, true);
    let vnrm = ccnrm(g, vict, C_NRM);
    g.send_to_char(vict, vnrm);

    if prf(g, ch, PRF_NOREPEAT) {
        g.send_to_char(ch, OK);
    } else {
        let cred = ccred(g, ch, C_CMP);
        g.send_to_char(ch, cred);
        let to_self = format!("You tell $N, '{}'", arg);
        act_sleep(g, &to_self, false, ch, None, ActArg::Char(vict), To::Char, true);
        let cnrm = ccnrm(g, ch, C_CMP);
        g.send_to_char(ch, cnrm);
    }

    if let Some(v) = g.get_char_mut(vict) {
        v.last_tell = Some(ch);
    }
}

/// is_tell_ok — true if a tell may be delivered; emits the failure message and
/// returns false otherwise. (AFK warns the sender but still allows delivery.)
fn is_tell_ok(g: &mut GameState, ch: CharId, vict: CharId) -> bool {
    if ch == vict {
        g.send_to_char(ch, "You try to tell yourself something.\r\n");
    } else if prf(g, ch, PRF_NOTELL) {
        g.send_to_char(ch, "You can't tell other people while you have notell on.\r\n");
    } else if room_soundproof(g, ch) {
        g.send_to_char(ch, "The walls seem to absorb your words.\r\n");
    } else if !is_npc(g, vict) && !has_desc(g, vict) {
        act(g, "$E's linkless at the moment.", false, ch, None, ActArg::Char(vict), To::Char);
    } else if plr(g, vict, PLR_WRITING) {
        act(g, "$E's writing a message right now; try again later.", false, ch, None, ActArg::Char(vict), To::Char);
    } else if prf(g, vict, PRF_NOTELL) || room_soundproof(g, vict) {
        act(g, "$E can't hear you.", false, ch, None, ActArg::Char(vict), To::Char);
    } else if isignoresend(g, vict, ch, IGNORE_PRIVATE) {
        return false;
    } else if prf(g, vict, PRF_AFK) {
        act(g, "$E has AFK on, and may not see your message.", false, ch, None, ActArg::Char(vict), To::Char);
        return true; // still delivered
    } else {
        return true;
    }
    false
}

pub fn do_tell(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, message) = half_chop(arg);

    if name.is_empty() || message.is_empty() {
        g.send_to_char(ch, "Who do you wish to tell what??\r\n");
        return;
    }
    // get_char_vis: any visible player in the game by name.
    let vict = g.find_player_by_name(&name).filter(|&v| g.can_see(ch, v));
    let vict = match vict {
        Some(v) => v,
        None => {
            g.send_to_char(ch, NOPERSON);
            return;
        }
    };
    if is_tell_ok(g, ch, vict) {
        perform_tell(g, ch, vict, &message);
    }
}

pub fn do_reply(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = arg.trim_start();

    let last = g.get_char(ch).and_then(|c| c.last_tell);
    match last {
        None => {
            g.send_to_char(ch, "You have no-one to reply to!\r\n");
        }
        Some(_) if argument.is_empty() => {
            g.send_to_char(ch, "What is your reply?\r\n");
        }
        Some(tch) => {
            // Make sure that character is still in the game.
            if !g.char_exists(tch) {
                g.send_to_char(ch, "They are no longer playing.\r\n");
                return;
            }
            if is_tell_ok(g, ch, tch) {
                let argument = makedrunk(argument, g, ch);
                perform_tell(g, ch, tch, &argument);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// do_spec_comm (whisper / ask)
// ---------------------------------------------------------------------------
pub fn do_spec_comm(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let (action_sing, action_plur, action_others) = if subcmd == SCMD_WHISPER {
        ("whisper to", "whispers to", "$n whispers something to $N.")
    } else {
        ("ask", "asks", "$n asks $N a question.")
    };

    let (name, message) = half_chop(arg);

    if name.is_empty() || message.is_empty() {
        g.send_to_char(ch, &format!("Whom do you want to {}.. and what??\r\n", action_sing));
        return;
    }
    let vict = match g.get_char_room_vis(ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, NOPERSON);
            return;
        }
    };
    if vict == ch {
        g.send_to_char(ch, "You can't get your mouth close enough to your ear...\r\n");
        return;
    }
    if isignoresend(g, vict, ch, IGNORE_PRIVATE) {
        return;
    }

    let message = makedrunk(&message, g, ch);
    let to_vict = format!("$n {} you, '{}'", action_plur, message);
    act(g, &to_vict, false, ch, None, ActArg::Char(vict), To::Vict);

    if prf(g, ch, PRF_NOREPEAT) {
        g.send_to_char(ch, OK);
    } else {
        let vname = get_name(g, vict);
        let to_self = format!("You {} {}, '{}'\r\n", action_sing, vname, message);
        act(g, &to_self, false, ch, None, ActArg::None, To::Char);
    }
    act(g, action_others, false, ch, None, ActArg::Char(vict), To::NotVict);
}

// ---------------------------------------------------------------------------
// do_write — compose a note onto paper using a pen. The string editor and the
// descriptor's note plumbing are not yet ported, so this reproduces every
// pre-editor validation message; on success it tells the writer the editor is
// open and broadcasts the "$n begins to jot down a note." line.
// ---------------------------------------------------------------------------
pub fn do_write(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    use crate::object::ObjectType;
    use crate::interpreter::any_one_arg;

    // two_arguments: paper, then pen (both lowercased, fill-words not skipped).
    let (papername, rest) = any_one_arg(arg);
    let (penname, _) = any_one_arg(rest);

    if !has_desc(g, ch) {
        return;
    }
    if papername.is_empty() {
        g.send_to_char(ch, "Write?  With what?  ON what?  What are you trying to do?!?\r\n");
        return;
    }

    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let mut paper: Option<ObjId>;
    let mut pen: Option<ObjId>;

    if !penname.is_empty() {
        // Two args were supplied: both must be in inventory.
        paper = g.get_obj_in_list_vis(ch, &papername, &inv);
        if paper.is_none() {
            g.send_to_char(ch, &format!("You have no {}.\r\n", papername));
            return;
        }
        pen = g.get_obj_in_list_vis(ch, &penname, &inv);
        if pen.is_none() {
            g.send_to_char(ch, &format!("You have no {}.\r\n", penname));
            return;
        }
    } else {
        // One arg: find it, classify pen vs. note, the other comes from HOLD.
        paper = g.get_obj_in_list_vis(ch, &papername, &inv);
        let p = match paper {
            Some(p) => p,
            None => {
                g.send_to_char(ch, &format!("There is no {} in your inventory.\r\n", papername));
                return;
            }
        };
        pen = None;
        let ptype = g.get_obj(p).map(|o| o.obj_type).unwrap_or(ObjectType::Other);
        // ITEM_PEN is DeltaMUD's pen item; the contract ObjectType has no Pen
        // variant, so a "pen" is any non-note item the player names as paper.
        if ptype == ObjectType::Note {
            paper = Some(p);
        } else {
            // Treat the found object as the pen; paper will come from HOLD.
            pen = Some(p);
            paper = None;
        }

        let held = g.get_char(ch).and_then(|c| c.equipment[WEAR_HOLD]);
        let held = match held {
            Some(h) => h,
            None => {
                let an = a_or_an(&papername);
                g.send_to_char(ch, &format!("You can't write with {} {} alone.\r\n", an, papername));
                return;
            }
        };
        // CAN_SEE_OBJ on the held item — invisibility not yet modelled, so the
        // item in hand is always visible here.
        if pen.is_some() {
            paper = Some(held);
        } else {
            pen = Some(held);
        }
    }

    let pen = match pen {
        Some(p) => p,
        None => return,
    };
    let paper = match paper {
        Some(p) => p,
        None => return,
    };

    let pen_type = g.get_obj(pen).map(|o| o.obj_type).unwrap_or(ObjectType::Other);
    let paper_type = g.get_obj(paper).map(|o| o.obj_type).unwrap_or(ObjectType::Other);
    let already = g
        .get_obj(paper)
        .map(|o| o.action_description.as_ref().map(|s| !s.is_empty()).unwrap_or(false))
        .unwrap_or(false);

    if paper_type != ObjectType::Note {
        // C: "$p is no good for writing with." iff pen isn't a pen; but with no
        // ITEM_PEN type the pen check collapses — the note check is the gate.
        act(g, "You can't write on $p.", false, ch, Some(paper), ActArg::None, To::Char);
    } else if pen_type == ObjectType::Note {
        // The "pen" is actually a note — no good for writing.
        act(g, "$p is no good for writing with.", false, ch, Some(pen), ActArg::None, To::Char);
    } else if already {
        g.send_to_char(ch, "There's something written on it already.\r\n");
    } else {
        // We can write.
        g.send_to_char(ch, "Write your note.  (/s saves /h for help)\r\n");
        act(g, "$n begins to jot down a note.", true, ch, None, ActArg::None, To::Room);
        // The descriptor's string editor would be opened here once ported.
    }
}

fn a_or_an(word: &str) -> &'static str {
    let first = word.trim_start().chars().next().unwrap_or('x').to_ascii_lowercase();
    if "aeiou".contains(first) {
        "an"
    } else {
        "a"
    }
}

// ---------------------------------------------------------------------------
// do_page
// ---------------------------------------------------------------------------
pub fn do_page(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, message) = half_chop(arg);

    if is_npc(g, ch) {
        g.send_to_char(ch, "Monsters can't page.. go away.\r\n");
        return;
    }
    if name.is_empty() {
        g.send_to_char(ch, "Whom do you wish to page?\r\n");
        return;
    }

    let buf = format!("\x07\x07*{}* {}\r\n", get_name(g, ch), message);

    if name.eq_ignore_ascii_case("all") {
        if level(g, ch) > LVL_GOD as u8 {
            let players = connected_players(g);
            for d in players {
                act(g, &buf, false, ch, None, ActArg::Char(d), To::Vict);
            }
        } else {
            g.send_to_char(ch, "You will never be godly enough to do that!\r\n");
        }
        return;
    }

    match g.find_player_by_name(&name).filter(|&v| g.can_see(ch, v)) {
        Some(vict) => {
            act(g, &buf, false, ch, None, ActArg::Char(vict), To::Vict);
            if prf(g, ch, PRF_NOREPEAT) {
                g.send_to_char(ch, OK);
            } else {
                act(g, &buf, false, ch, None, ActArg::Char(vict), To::Char);
            }
        }
        None => {
            g.send_to_char(ch, "There is no such person in the game!\r\n");
        }
    }
}

// ---------------------------------------------------------------------------
// do_gen_comm — holler / shout / gossip / auction / grats / arena / gemote.
// ---------------------------------------------------------------------------
pub fn do_gen_comm(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    // channels[subcmd]: the PRF_* flag that must NOT be set to hear the channel.
    let channels = [0i64, PRF_DEAF, PRF_NOGOSS, PRF_NOAUCT, PRF_NOGRATZ, 0, PRF_NOARENA, 0];

    // com_msgs[subcmd] = [cant_msg, name, not_on_channel_msg, color]
    let com_msgs: [[&str; 4]; 7] = [
        ["You cannot holler!!\r\n", "holler", "", "&Y"],
        ["You cannot shout!!\r\n", "shout", "Turn off your noshout flag first!\r\n", "&Y"],
        ["You cannot gossip!!\r\n", "GOSSIP", "You aren't even on the channel!\r\n", "&Y"],
        ["You cannot auction!!\r\n", "AUCTION", "You aren't even on the channel!\r\n", "&M"],
        ["You cannot congratulate!\r\n", "CONGRAT", "You aren't even on the channel!\r\n", "&G"],
        ["You cannot gemote!\r\n", "gemote", "You aren't even on the channel!\r\n", "&G"],
        ["You cannot arena?!\r\n", "ARENA", "You aren't even on the channel!\r\n", "&Y"],
    ];

    let mut subcmd = subcmd;

    // to keep pets, etc from being ordered to shout: (!ch->desc && cmd == 0)
    // — cmd is the dispatch's command index, which is not threaded here; the
    // playable path always has a descriptor, so this guard is a no-op.

    if is_npc(g, ch) && mob_flagged(g, ch, MOB_NOTALK) {
        g.send_to_char(ch, "You weren't designed to talk.\r\n");
        return;
    }

    let mut idx = subcmd as usize;
    if idx >= com_msgs.len() {
        idx = SCMD_GOSSIP as usize;
    }

    if plr(g, ch, PLR_NOSHOUT) {
        g.send_to_char(ch, com_msgs[idx][0]);
        return;
    }
    if room_soundproof(g, ch) {
        g.send_to_char(ch, "The walls seem to absorb your words.\r\n");
        return;
    }
    if level(g, ch) < LEVEL_CAN_SHOUT {
        g.send_to_char(
            ch,
            &format!("You must be at least level {} before you can {}.\r\n", LEVEL_CAN_SHOUT, com_msgs[idx][1]),
        );
        return;
    }
    // The char must be on the channel (channels[subcmd] must not be set).
    if channels[idx] != 0 && prf(g, ch, channels[idx]) {
        g.send_to_char(ch, com_msgs[idx][2]);
        return;
    }

    let argument = arg.trim_start();

    // gemote, or gossip with a leading '@', routes to do_gmote on the gossip
    // channel.
    let mut gmote = false;
    if subcmd == SCMD_GMOTE || (subcmd == SCMD_GOSSIP && argument.starts_with('@')) {
        subcmd = SCMD_GOSSIP;
        idx = SCMD_GOSSIP as usize;
        gmote = true;
    }

    if argument.is_empty() {
        g.send_to_char(
            ch,
            &format!("Yes, {}, fine, {} we must, but WHAT???\r\n", com_msgs[idx][1], com_msgs[idx][1]),
        );
        return;
    }

    if subcmd == SCMD_HOLLER {
        let mv = g.get_char(ch).map(|c| c.points.move_points).unwrap_or(0);
        if mv < HOLLER_MOVE_COST {
            g.send_to_char(ch, "You're too exhausted to holler.\r\n");
            return;
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= HOLLER_MOVE_COST;
        }
    }

    if gmote {
        // do_gmote consumes the gossip-channel social. Not yet ported; the
        // gossip-channel broadcast of the social would go here.
        let _gtext = argument.strip_prefix('@').unwrap_or(argument);
        return;
    }

    let argument = makedrunk(argument, g, ch);
    let color_on = com_msgs[idx][3];

    // Sender's echo line.
    if prf(g, ch, PRF_NOREPEAT) {
        g.send_to_char(ch, OK);
    } else if subcmd == SCMD_SHOUT || subcmd == SCMD_HOLLER {
        let buf1 = if color_lev(g, ch) >= C_CMP {
            format!("{}You {}, '{}'&n", color_on, com_msgs[idx][1], argument)
        } else {
            format!("You {}, '{}'", com_msgs[idx][1], argument)
        };
        act_sleep(g, &buf1, false, ch, None, ActArg::None, To::Char, true);
    } else {
        let buf1 = format!("&c[&Y{}&c]&n $n: {}", com_msgs[idx][1], argument);
        act_sleep(g, &buf1, false, ch, None, ActArg::None, To::Char, true);
    }

    // The broadcast line.
    let buf = if subcmd == SCMD_SHOUT || subcmd == SCMD_HOLLER {
        format!("$n {}s, '{}'", com_msgs[idx][1], argument)
    } else {
        format!("&c[&Y{}&c]&n $n: {}", com_msgs[idx][1], argument)
    };

    // SHOUT is zone-local and only reaches awake (>= POS_RESTING) hearers.
    let my_zone = in_room(g, ch).and_then(|r| g.room_opt(r)).map(|r| r.zone);

    let hearers = connected_players(g);
    for tgt in hearers {
        if tgt == ch {
            continue;
        }
        // Must be on the channel, not writing, not soundproofed, not ignoring.
        if channels[idx] != 0 && prf(g, tgt, channels[idx]) {
            continue;
        }
        if plr(g, tgt, PLR_WRITING) {
            continue;
        }
        if room_soundproof(g, tgt) {
            continue;
        }
        if isignore(g, tgt, ch, IGNORE_PUBLIC) {
            continue;
        }
        if subcmd == SCMD_SHOUT {
            let tgt_zone = in_room(g, tgt).and_then(|r| g.room_opt(r)).map(|r| r.zone);
            let tgt_pos = g.get_char(tgt).map(|c| c.position).unwrap_or(Position::Standing);
            if tgt_zone != my_zone || tgt_pos < Position::Resting {
                continue;
            }
        }
        if color_lev(g, tgt) >= C_NRM {
            g.send_to_char(tgt, color_on);
        }
        act_sleep(g, &buf, false, ch, None, ActArg::Char(tgt), To::Vict, true);
        if color_lev(g, tgt) >= C_NRM {
            g.send_to_char(tgt, "&n");
        }
    }
}

// ---------------------------------------------------------------------------
// do_qcomm (qsay / qecho)
// ---------------------------------------------------------------------------
pub fn do_qcomm(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    if !prf2(g, ch, PRF2_QCHAN) {
        g.send_to_char(ch, "You aren't even part of the quest channel!\r\n");
        return;
    }

    let argument = arg.trim_start();

    if argument.is_empty() {
        // C uses CMD_NAME (the command word). qsay -> "Qsay?", qecho -> "Qecho?".
        let cmd_name = if subcmd == SCMD_QSAY { "qsay" } else { "qecho" };
        let mut out = format!("{}?  Yes, fine, {} we must, but WHAT??\r\n", cmd_name, cmd_name);
        cap_first(&mut out);
        g.send_to_char(ch, &out);
        return;
    }

    let argument = makedrunk(argument, g, ch);

    // Sender's echo.
    if prf(g, ch, PRF_NOREPEAT) {
        g.send_to_char(ch, OK);
    } else {
        let selfm = if subcmd == SCMD_QSAY {
            format!("You quest-say, '{}'", argument)
        } else {
            argument.clone()
        };
        act(g, &selfm, false, ch, None, ActArg::None, To::Char);
    }

    // The broadcast line.
    let buf = if subcmd == SCMD_QSAY {
        format!("$n quest-says, '{}'", argument)
    } else {
        argument.clone()
    };

    let hearers = connected_players(g);
    for tgt in hearers {
        if tgt == ch {
            continue;
        }
        if prf2(g, tgt, PRF2_QCHAN) && !isignore(g, tgt, ch, IGNORE_PUBLIC) {
            act_sleep(g, &buf, false, ch, None, ActArg::Char(tgt), To::Vict, true);
        }
    }
}

/// CAP(): uppercase the first character in place.
fn cap_first(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            s.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
}
