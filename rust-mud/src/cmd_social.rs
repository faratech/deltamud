// cmd_social.rs — the social-message system (CircleMUD act.social.c), ported
// to the id-indexed GameState. Contains the boot-time loader for the
// `lib/misc/socials` data file plus the three ACMD handlers that live in the
// C file: do_action (the data-driven social emitter), do_insult, and do_gmote
// (the gossip-channel social variant invoked from do_gen_comm/SCMD_GMOTE).
//
// Because the Rust command table (command_table.rs / CMD_INFO) is a *static*
// 1:1 port of the C cmd_info[], the dynamically-loaded socials are NOT baked
// into it the way C's create_command_list() splices them in. Instead the
// interpreter consults find_social() for any unknown command word and, on a
// hit, calls do_action_named(). do_action() (the fixed ACMD-signature shim)
// remains for symmetry / direct invocation. See `notes` in the manifest.

use crate::act::{act_sleep, ActArg, To};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

// PRF_NOGOSS (structs.h: 1 << 19) — not yet present in flags.rs; defined here
// privately so the gemote/gossip gate matches C exactly.
const PRF_NOGOSS: i64 = 1 << 19;
// ROOM_SOUNDPROOF — matches room.rs RoomFlags::SOUNDPROOF (1 << 5).
const ROOM_SOUNDPROOF: u32 = 1 << 5;

/// One social, mirroring C `struct social_messg`. A `None` field is the C
/// NULL pointer (the loader stores `#` placeholder lines as `None`).
#[derive(Debug, Clone, Default)]
pub struct SocialMessg {
    pub command: String,
    pub sort_as: String,
    pub hide: bool,
    pub min_victim_position: i32,
    pub min_char_position: i32,
    pub min_level_char: i32,

    // No argument supplied.
    pub char_no_arg: Option<String>,
    pub others_no_arg: Option<String>,

    // Argument supplied, victim found.
    pub char_found: Option<String>,
    pub others_found: Option<String>,
    pub vict_found: Option<String>,

    // Argument + body part, victim found.
    pub char_body_found: Option<String>,
    pub others_body_found: Option<String>,
    pub vict_body_found: Option<String>,

    // Argument supplied, no victim found.
    pub not_found: Option<String>,

    // Self-target ("smile self").
    pub char_auto: Option<String>,
    pub others_auto: Option<String>,

    // Victim not found, but an object matched in inv/room.
    pub char_obj_found: Option<String>,
    pub others_obj_found: Option<String>,
}

/// Loaded social table, sorted by `sort_as` (C create_command_list() resort)
/// and indexed by `command` for O(1) lookup from the interpreter.
struct SocialTable {
    list: Vec<SocialMessg>,
    by_command: HashMap<String, usize>,
}

static SOCIALS: OnceLock<RwLock<SocialTable>> = OnceLock::new();

/// CircleMUD SOCMESS_FILE (db.h): "misc/socials" relative to the lib dir.
const SOCMESS_FILE: &str = "lib/misc/socials";

// ---------------------------------------------------------------------------
// Boot-time loading (C boot_social_messages + create_command_list resort)
// ---------------------------------------------------------------------------

/// Load the socials data file into the module static. Call once at boot,
/// before the interpreter dispatches any commands. `lib_path` is the path to
/// the `socials` file; pass `None` to use the default `lib/misc/socials`
/// (relative to the process CWD, matching the C server which chdirs to lib).
pub fn boot_socials(lib_path: Option<&str>) {
    let path = lib_path.unwrap_or(SOCMESS_FILE);
    let table = match load_socials(path) {
        Ok(t) => t,
        Err(e) => {
            // C does perror + exit(1); here we surface to stderr and install an
            // empty table so the server still boots (commands report "not
            // supported", matching C's find_action() == -1 path).
            eprintln!("SYSERR: can't open socials file '{}': {}", path, e);
            SocialTable { list: Vec::new(), by_command: HashMap::new() }
        }
    };
    install_socials(table);
}

/// Reload the live social table from disk after OLC writes `misc/socials`.
pub fn reload_socials(lib_path: Option<&str>) -> std::io::Result<()> {
    let path = lib_path.unwrap_or(SOCMESS_FILE);
    let table = load_socials(path)?;
    install_socials(table);
    Ok(())
}

fn install_socials(table: SocialTable) {
    if let Some(lock) = SOCIALS.get() {
        match lock.write() {
            Ok(mut guard) => *guard = table,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                *guard = table;
            }
        }
    } else {
        let _ = SOCIALS.set(RwLock::new(table));
    }
}

fn load_socials(path: &str) -> std::io::Result<SocialTable> {
    let raw = std::fs::read_to_string(path)?;
    // The C reader is line-oriented: a header line "~cmd sort hide cpos vpos
    // lvl", then exactly 13 message lines (each either a payload or a bare
    // "#" placeholder => NULL), terminated by a "$" sentinel.
    let mut lines = raw.lines();
    let mut list: Vec<SocialMessg> = Vec::new();

    loop {
        // Skip blank separator lines between socials (the file has them).
        let header = loop {
            match lines.next() {
                Some(l) if l.trim().is_empty() => continue,
                Some(l) => break l,
                None => return Ok(finalize(list)),
            }
        };

        let header = header.trim_start();
        if header.starts_with('$') {
            break;
        }
        if !header.starts_with('~') {
            // Unexpected; skip defensively rather than abort the whole boot.
            continue;
        }

        // Parse "~command sort_as hide min_char_pos min_vict_pos min_lvl".
        let mut it = header.split_whitespace();
        let command = it.next().unwrap_or("~").trim_start_matches('~').to_string();
        let sort_as = it.next().unwrap_or("").to_string();
        let hide = it.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        let min_char_pos = it.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        let min_vict_pos = it.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        let min_lvl = it.next().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);

        // Read the 13 message slots (C fread_action: a line starting with '#'
        // is the NULL/placeholder, otherwise the verbatim line is the text).
        let read = |lines: &mut std::str::Lines| -> Option<String> {
            // fread_action does NOT skip blank lines; it reads exactly one.
            let l = lines.next().unwrap_or("#");
            if l.starts_with('#') {
                None
            } else {
                Some(l.to_string())
            }
        };

        let s = SocialMessg {
            command,
            sort_as,
            hide: hide != 0,
            min_victim_position: min_vict_pos,
            min_char_position: min_char_pos,
            min_level_char: min_lvl,
            char_no_arg: read(&mut lines),
            others_no_arg: read(&mut lines),
            char_found: read(&mut lines),
            others_found: read(&mut lines),
            vict_found: read(&mut lines),
            not_found: read(&mut lines),
            char_auto: read(&mut lines),
            others_auto: read(&mut lines),
            char_body_found: read(&mut lines),
            others_body_found: read(&mut lines),
            vict_body_found: read(&mut lines),
            char_obj_found: read(&mut lines),
            others_obj_found: read(&mut lines),
        };
        list.push(s);
    }

    Ok(finalize(list))
}

/// Sort by `sort_as` (C create_command_list selection-sort) and build index.
fn finalize(mut list: Vec<SocialMessg>) -> SocialTable {
    list.sort_by(|a, b| a.sort_as.cmp(&b.sort_as));
    let mut by_command = HashMap::with_capacity(list.len());
    for (i, s) in list.iter().enumerate() {
        by_command.entry(s.command.clone()).or_insert(i);
    }
    SocialTable { list, by_command }
}

// ---------------------------------------------------------------------------
// Lookup (for the interpreter's social hook)
// ---------------------------------------------------------------------------

/// Resolve a typed command word to a social by abbreviation, exactly like the
/// interpreter walking cmd_info[]: the socials are sorted by `sort_as`, and
/// the first whose name is prefixed by `arg` wins. Returns the canonical
/// command name to pass to do_action_named().
pub fn find_social(arg: &str) -> Option<String> {
    let arg = arg.to_lowercase();
    if arg.is_empty() {
        return None;
    }
    let table = SOCIALS.get()?.read().ok()?;
    table
        .list
        .iter()
        .find(|s| s.command.starts_with(&arg))
        .map(|s| s.command.clone())
}

/// Commands visible in the `socials` listing for a character level.
pub fn social_commands_for_level(level: Level) -> Vec<String> {
    let Some(lock) = SOCIALS.get() else {
        return Vec::new();
    };
    let Ok(table) = lock.read() else {
        return Vec::new();
    };
    table
        .list
        .iter()
        .filter(|s| level as i32 >= s.min_level_char)
        .map(|s| s.command.clone())
        .collect()
}

/// Look up a social by its exact command name (post-resolution).
fn lookup<'a>(table: &'a SocialTable, command: &str) -> Option<&'a SocialMessg> {
    table.by_command.get(command).map(|&i| &table.list[i])
}

// ---------------------------------------------------------------------------
// do_action — the data-driven social emitter (C do_action)
// ---------------------------------------------------------------------------

/// Fixed ACMD-signature entry. The static command table carries no per-social
/// command name, so this shim resolves the social from the *argument's* first
/// word only when invoked directly; normal play routes through
/// do_action_named() from the interpreter's social hook. Matches C's
/// "That action is not supported." for an unresolved action.
pub fn do_action(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // Without a command word we cannot identify the social (C uses `cmd`).
    g.send_to_char(ch, "That action is not supported.\r\n");
}

/// The real social emitter: `command` is the resolved social name, `argument`
/// is everything the player typed after it. 1:1 with C do_action's body.
pub fn do_action_named(g: &mut GameState, ch: CharId, command: &str, argument: &str) {
    // Snapshot the social (clone out of the static so we hold no borrow on the
    // table while mutating GameState).
    let action = match SOCIALS
        .get()
        .and_then(|lock| lock.read().ok().and_then(|table| lookup(&table, command).cloned()))
    {
        Some(a) => a.clone(),
        None => {
            g.send_to_char(ch, "That action is not supported.\r\n");
            return;
        }
    };

    // Actor min-position gate. In C the social's min_char_position is copied into
    // complete_cmd_info[].minimum_position (act.social.c:361/376), so the command
    // interpreter's `GET_POS(ch) < minimum_position` check (interpreter.c:828)
    // refuses a sleeping/resting/fighting actor BEFORE do_action runs. The Rust
    // port dispatches socials past that gate, so enforce it here with the same
    // refusal text.
    let ch_pos = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing);
    if (ch_pos as i32) < action.min_char_position {
        let msg = crate::interpreter::position_refusal_msg(ch_pos);
        if !msg.is_empty() {
            g.send_to_char(ch, msg);
        }
        return;
    }

    // two_arguments(argument, buf, buf2): first arg => target, second => body.
    let (mut buf, buf2) = two_arguments(argument);

    if action.char_body_found.is_none() && !buf2.is_empty() {
        g.send_to_char(ch, "Sorry, this social does not support body parts.\r\n");
        return;
    }

    // C: if no char_found message, ignore any argument (force the no-arg path).
    if action.char_found.is_none() {
        buf.clear();
    } else {
        // one_argument(argument, buf): re-read the first word as the target.
        let (first, _rest) = crate::interpreter::one_argument(argument);
        buf = first;
    }

    // --- No argument: the actor performs the social with no target. --------
    if buf.is_empty() {
        if let Some(m) = action.char_no_arg.as_deref() {
            g.send_to_char(ch, m);
        }
        g.send_to_char(ch, "\r\n");
        if let Some(m) = action.others_no_arg.as_deref() {
            ac_to_room(g, m, action.hide, ch);
        }
        return;
    }

    // --- Find the victim in the room. --------------------------------------
    let vict = g.get_char_room_vis(ch, &buf);
    if vict.is_none() {
        // Fall back to an object in inventory or the room (char_obj_found).
        if action.char_obj_found.is_some() {
            let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
            let mut targ = g.get_obj_in_list_vis(ch, &buf, &inv);
            if targ.is_none() {
                if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
                    let contents = g.room(rnum).contents.clone();
                    targ = g.get_obj_in_list_vis(ch, &buf, &contents);
                }
            }
            if let Some(oid) = targ {
                if let Some(m) = action.char_obj_found.as_deref() {
                    act_sleep(g, m, action.hide, ch, Some(oid), ActArg::None, To::Char, false);
                }
                if let Some(m) = action.others_obj_found.as_deref() {
                    act_sleep(g, m, action.hide, ch, Some(oid), ActArg::None, To::Room, false);
                }
                return;
            }
        }
        if let Some(m) = action.not_found.as_deref() {
            g.send_to_char(ch, m);
        } else {
            g.send_to_char(ch, "I don't see anything by that name here.");
        }
        g.send_to_char(ch, "\r\n");
        return;
    }

    let vict = vict.unwrap();

    // --- Self-target: "smile self". ----------------------------------------
    if vict == ch {
        if let Some(m) = action.char_auto.as_deref() {
            g.send_to_char(ch, m);
        } else {
            g.send_to_char(ch, "Erm, no.");
        }
        g.send_to_char(ch, "\r\n");
        if let Some(m) = action.others_auto.as_deref() {
            ac_to_room(g, m, action.hide, ch);
        }
        return;
    }

    // --- Victim found and is someone else. ---------------------------------
    // C: isignoresend(vict, ch, IGNORE_EMOTE) — if the victim ignores ch's
    // emotes, notify ch and abort the social entirely.
    if crate::cmd_comm::isignoresend(g, vict, ch, crate::cmd_comm::IGNORE_EMOTE) {
        return;
    }

    let vict_pos = g.get_char(vict).map(|c| c.position).unwrap_or(Position::Standing);
    if (vict_pos as i32) < action.min_victim_position {
        act_sleep(
            g,
            "$N is not in a proper position for that.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
            true,
        );
        return;
    }

    if !buf2.is_empty() {
        // Body-part social. C passes the body-part text in act()'s obj slot and
        // the victim in vict_obj, which leaves $t reading the (mis-cast) victim
        // pointer — effectively garbage in the live C MUD. The data file's
        // clear intent (e.g. "You tickle $S $t.") is $t == the body part, so we
        // pre-substitute $t with buf2 and pass the victim as vict_obj for the
        // $N/$S/$M codes. This renders the feature as authored. See notes.
        emit_body(
            g,
            action.char_body_found.as_deref(),
            false,
            ch,
            vict,
            &buf2,
            To::Char,
            true,
        );
        emit_body(
            g,
            action.others_body_found.as_deref(),
            action.hide,
            ch,
            vict,
            &buf2,
            To::NotVict,
            false,
        );
        emit_body(
            g,
            action.vict_body_found.as_deref(),
            action.hide,
            ch,
            vict,
            &buf2,
            To::Vict,
            false,
        );
    } else {
        if let Some(m) = action.char_found.as_deref() {
            act_sleep(g, m, false, ch, None, ActArg::Char(vict), To::Char, true);
        }
        if let Some(m) = action.others_found.as_deref() {
            act_sleep(g, m, action.hide, ch, None, ActArg::Char(vict), To::NotVict, false);
        }
        if let Some(m) = action.vict_found.as_deref() {
            act_sleep(g, m, action.hide, ch, None, ActArg::Char(vict), To::Vict, false);
        }
    }
}

/// Render one body-part social line: substitute `$t` with the body part, then
/// hand the result to act() with the victim as vict_obj for the other codes.
fn emit_body(
    g: &mut GameState,
    msg: Option<&str>,
    hide: bool,
    ch: CharId,
    vict: CharId,
    body: &str,
    to: To,
    to_sleep: bool,
) {
    let msg = match msg {
        Some(m) => m,
        None => return,
    };
    // Replace $t but leave the act() escape $$ intact ($ -> literal $).
    let substituted = subst_body_part(msg, body);
    act_sleep(g, &substituted, hide, ch, None, ActArg::Char(vict), to, to_sleep);
}

/// Replace the `$t` code with `body`, preserving `$$` -> `$` and all other
/// `$x` codes for act() to resolve.
fn subst_body_part(msg: &str, body: &str) -> String {
    let mut out = String::with_capacity(msg.len() + body.len());
    let mut chars = msg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.peek() {
                Some('t') => {
                    chars.next();
                    out.push_str(body);
                }
                Some('$') => {
                    // Keep $$ for act() to collapse to a literal '$'.
                    chars.next();
                    out.push('$');
                    out.push('$');
                }
                _ => out.push('$'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// C ac_to_room(): send `string` to everyone in the actor's room except the
/// actor, as a TO_VICT act() so the recipient's perspective renders. Visibility
/// follows the hide flag (the ignore/PLR_WRITING gates land with those systems).
fn ac_to_room(g: &mut GameState, string: &str, hide: bool, ch: CharId) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let people = g.room(rnum).people.clone();
    for vict in people {
        if vict == ch {
            continue;
        }
        // C: send if (CAN_SEE || !hide_invis). When hide is false, always send;
        // when hide is true, only to those who can see the actor.
        if !hide || g.can_see(vict, ch) {
            act_sleep(g, string, false, ch, None, ActArg::Char(vict), To::Vict, false);
        }
    }
}

// ---------------------------------------------------------------------------
// do_insult (C do_insult)
// ---------------------------------------------------------------------------

pub fn do_insult(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _rest) = crate::interpreter::one_argument(argument);

    if arg.is_empty() {
        g.send_to_char(ch, "I'm sure you don't want to insult *everybody*...\r\n");
        return;
    }

    let victim = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "Can't hear you!\r\n");
            return;
        }
    };

    if victim == ch {
        g.send_to_char(ch, "You feel insulted.\r\n");
        return;
    }

    let vname = g.get_char(victim).map(|c| c.player.name.clone()).unwrap_or_default();
    g.send_to_char(ch, &format!("You insult {}.\r\n", vname));

    let roll = g.rng.number(0, 2);
    match roll {
        0 => {
            let ch_male = g.get_char(ch).map(|c| c.player.sex == Gender::Male).unwrap_or(false);
            let vic_male = g.get_char(victim).map(|c| c.player.sex == Gender::Male).unwrap_or(false);
            let msg = if ch_male {
                if vic_male {
                    "$n accuses you of fighting like a woman!"
                } else {
                    "$n says that women can't fight."
                }
            } else if vic_male {
                "$n accuses you of having the smallest... (brain?)"
            } else {
                "$n tells you that you'd lose a beauty contest against a troll."
            };
            act_sleep(g, msg, false, ch, None, ActArg::Char(victim), To::Vict, false);
        }
        1 => {
            act_sleep(
                g,
                "$n calls your mother a bitch!",
                false,
                ch,
                None,
                ActArg::Char(victim),
                To::Vict,
                false,
            );
        }
        _ => {
            act_sleep(
                g,
                "$n tells you to get lost!",
                false,
                ch,
                None,
                ActArg::Char(victim),
                To::Vict,
                false,
            );
        }
    }

    act_sleep(g, "$n insults $N.", true, ch, None, ActArg::Char(victim), To::NotVict, false);
}

// ---------------------------------------------------------------------------
// do_gmote — gossip-channel social (C do_gmote; called via do_gen_comm/GMOTE)
// ---------------------------------------------------------------------------

/// Gossip-emote: emit a social over the gossip channel. `command` is the
/// resolved social name (the leading word the player typed); `argument` is the
/// remainder. Invoked from do_gen_comm when SCMD_GMOTE or `.@<social>`.
pub fn do_gmote_named(g: &mut GameState, ch: CharId, command: &str, argument: &str) {
    // Meditating mortals can't gmote (C GET_POS == POS_MEDITATING gate).
    let (pos, lvl) = g
        .get_char(ch)
        .map(|c| (c.position, c.player.level))
        .unwrap_or((Position::Standing, 1));
    if pos == Position::Meditating && lvl < LVL_IMMORT {
        g.send_to_char(ch, "Not while you're meditating!\r\n");
        return;
    }

    let action = match SOCIALS
        .get()
        .and_then(|lock| lock.read().ok().and_then(|table| lookup(&table, command).cloned()))
    {
        Some(a) => a.clone(),
        None => {
            g.send_to_char(ch, "That's not a social!\r\n");
            return;
        }
    };

    // half_chop already gave us the remainder; re-read its first word as target.
    let (mut arg, _rest) = crate::interpreter::one_argument(argument);
    if action.char_found.is_none() {
        arg.clear();
    }

    let mut vict: Option<CharId> = None;
    let out: String;

    if arg.is_empty() {
        match action.others_no_arg.as_deref() {
            Some(m) if !m.is_empty() => {
                out = format!("&c[&YGOSSIP&c]&n {}", m);
            }
            _ => {
                g.send_to_char(ch, "Who are you going to do that to?\r\n");
                return;
            }
        }
    } else {
        // do_gmote uses get_char_vis (room first, then world-wide).
        match get_char_vis(g, ch, &arg) {
            None => {
                if let Some(m) = action.not_found.as_deref() {
                    g.send_to_char(ch, m);
                }
                g.send_to_char(ch, "\r\n");
                return;
            }
            Some(v) if v == ch => {
                vict = Some(v);
                match action.others_auto.as_deref() {
                    Some(m) if !m.is_empty() => {
                        out = format!("&c[&YGOSSIP&c]&n {}", m);
                    }
                    _ => {
                        if let Some(m) = action.char_auto.as_deref() {
                            g.send_to_char(ch, m);
                        }
                        g.send_to_char(ch, "\r\n");
                        return;
                    }
                }
            }
            Some(v) => {
                vict = Some(v);
                // C: isignoresend(vict, ch, IGNORE_EMOTE) — abort if the victim
                // ignores ch's emotes (notifying ch).
                if crate::cmd_comm::isignoresend(g, v, ch, crate::cmd_comm::IGNORE_EMOTE) {
                    return;
                }
                let vpos = g.get_char(v).map(|c| c.position).unwrap_or(Position::Standing);
                if (vpos as i32) < action.min_victim_position {
                    act_sleep(
                        g,
                        "$N is not in a proper position for that.",
                        false,
                        ch,
                        None,
                        ActArg::Char(v),
                        To::Char,
                        true,
                    );
                    return;
                }
                let m = action.others_found.as_deref().unwrap_or("");
                out = format!("&c[&YGOSSIP&c]&n {}", m);
            }
        }
    }

    gmote_broadcast(g, &out, ch, vict);
}

/// Fixed ACMD-signature shim for do_gmote (parity with the C ACMD). Real play
/// routes through do_gmote_named() from do_gen_comm with the resolved social.
pub fn do_gmote(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    // half_chop(argument, buf, arg): the social word + the remainder.
    let (command, rest) = crate::interpreter::half_chop(argument);
    if command.is_empty() {
        g.send_to_char(ch, "That's not a social!\r\n");
        return;
    }
    let resolved = match find_social(&command) {
        Some(c) => c,
        None => {
            g.send_to_char(ch, "That's not a social!\r\n");
            return;
        }
    };
    do_gmote_named(g, ch, &resolved, &rest);
}

/// C act() TO_GMOTE path: render `str` (per recipient) to every connected,
/// non-NOGOSS, non-writing player whose room isn't soundproof, wrapped in
/// yellow. The leading "&c[&YGOSSIP&c]&n " prefix is already on `str`.
fn gmote_broadcast(g: &mut GameState, msg: &str, ch: CharId, vict: Option<CharId>) {
    if msg.is_empty() {
        return;
    }
    let recipients: Vec<CharId> = g.players_by_name.values().copied().collect();
    let vict_arg = match vict {
        Some(v) => ActArg::Char(v),
        None => ActArg::None,
    };
    for to_id in recipients {
        // PRF_NOGOSS gate.
        if g.get_char(to_id).map(|c| c.prf_flags & PRF_NOGOSS != 0).unwrap_or(true) {
            continue;
        }
        // Soundproof room gate.
        let soundproof = g
            .get_char(to_id)
            .and_then(|c| c.in_room)
            .map(|rnum| g.room(rnum).room_flags.bits() & ROOM_SOUNDPROOF != 0)
            .unwrap_or(false);
        if soundproof {
            continue;
        }
        // Must be connected & in the game.
        if g.get_char(to_id).map(|c| c.desc.is_none()).unwrap_or(true) {
            continue;
        }
        // act() TO_GMOTE wraps each line in yellow (CCYEL .. CCNRM). Render the
        // per-viewer substitution then frame it.
        let line = render_for(g, msg, ch, vict_arg.clone(), to_id);
        g.send_to_char(to_id, "&Y");
        g.send_to_char(to_id, &line);
        g.send_to_char(to_id, "&n");
    }
}

/// Render an act()-style template for one specific viewer, mirroring the
/// per-recipient substitution the engine performs (used by the TO_GMOTE
/// broadcast, which act() can't express through the To enum). Implemented via
/// a one-shot To::Vict act() against a scratch send is not possible without a
/// connection, so we reproduce the $n/$N/$e... codes here for the common set
/// used by socials.
fn render_for(g: &GameState, template: &str, ch: CharId, vict_obj: ActArg, viewer: CharId) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let code = match chars.next() {
            Some(x) => x,
            None => break,
        };
        let repl: String = match code {
            'n' => pers(g, viewer, ch),
            'N' => match &vict_obj {
                ActArg::Char(v) => pers(g, viewer, *v),
                _ => String::new(),
            },
            'm' => hmhr(g, ch).to_string(),
            'M' => match &vict_obj {
                ActArg::Char(v) => hmhr(g, *v).to_string(),
                _ => String::new(),
            },
            's' => hshr(g, ch).to_string(),
            'S' => match &vict_obj {
                ActArg::Char(v) => hshr(g, *v).to_string(),
                _ => String::new(),
            },
            'e' => hssh(g, ch).to_string(),
            'E' => match &vict_obj {
                ActArg::Char(v) => hssh(g, *v).to_string(),
                _ => String::new(),
            },
            '$' => "$".to_string(),
            _ => String::new(),
        };
        out.push_str(&repl);
    }
    cap_first(&mut out);
    out.push_str("\r\n");
    out
}

// ---------------------------------------------------------------------------
// Local helpers (mirrors of act.rs internals + interpreter tokenizers)
// ---------------------------------------------------------------------------

/// two_arguments(argument): first word, second word (both via one_argument,
/// which skips fill words). Mirrors C two_arguments.
fn two_arguments(argument: &str) -> (String, String) {
    let (first, rest) = crate::interpreter::one_argument(argument);
    let (second, _) = crate::interpreter::one_argument(rest);
    (first, second)
}

/// get_char_vis: room first, then a world-wide name scan (C handler.c). Used
/// by do_gmote. Honors visibility from the searcher's perspective.
fn get_char_vis(g: &GameState, ch: CharId, name: &str) -> Option<CharId> {
    if let Some(found) = g.get_char_room_vis(ch, name) {
        return Some(found);
    }
    let (mut number, name) = crate::handler::get_number(name);
    if number == 0 {
        return None;
    }
    for cid in g.char_ids() {
        let target = match g.get_char(cid) {
            Some(t) => t,
            None => continue,
        };
        if crate::handler::isname(&name, &target.player.name) && g.can_see(ch, cid) {
            number -= 1;
            if number == 0 {
                return Some(cid);
            }
        }
    }
    None
}

fn sex_of(g: &GameState, cid: CharId) -> Gender {
    g.get_char(cid).map(|c| c.player.sex).unwrap_or(Gender::Neutral)
}
fn hssh(g: &GameState, cid: CharId) -> &'static str {
    match sex_of(g, cid) {
        Gender::Male => "he",
        Gender::Female => "she",
        Gender::Neutral => "it",
    }
}
fn hshr(g: &GameState, cid: CharId) -> &'static str {
    match sex_of(g, cid) {
        Gender::Male => "his",
        Gender::Female => "her",
        Gender::Neutral => "its",
    }
}
fn hmhr(g: &GameState, cid: CharId) -> &'static str {
    match sex_of(g, cid) {
        Gender::Male => "him",
        Gender::Female => "her",
        Gender::Neutral => "it",
    }
}

/// PERS(ch, vict) — visible name, else the invisible-being fallbacks. Mirrors
/// act.rs::pers (kept local since that one is private to act.rs).
fn pers(g: &GameState, viewer: CharId, cid: CharId) -> String {
    if g.can_see(viewer, cid) {
        if let Some(ch) = g.get_char(cid) {
            return if ch.is_npc {
                ch.short_desc.clone().unwrap_or_else(|| ch.player.name.clone())
            } else {
                ch.player.name.clone()
            };
        }
    }
    let imm = g
        .get_char(cid)
        .map(|c| !c.is_npc && c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    if imm {
        "A Mystical Being".to_string()
    } else {
        "someone".to_string()
    }
}

fn cap_first(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            s.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
}
