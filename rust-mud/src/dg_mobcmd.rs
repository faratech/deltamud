// dg_mobcmd.rs — the m* script-command set (DeltaMUD src/dg_mobcmd.c).
//
// These are the commands a *mob* trigger's command-list can run. They are NOT
// player commands; the DG script VM (`script_driver`) parses a trigger line
// such as `mteleport %actor% 3001` and dispatches the leading word here via
// `dispatch_mob_command(g, mob, "mteleport", "%actor% 3001")` AFTER the VM has
// already expanded %variables% in the argument string. So by the time control
// reaches this module, `argument` is fully literal text — except that entity
// references survive as DG *UID strings*: an ESC byte (0x1B == UID_CHAR)
// followed by the decimal arena id. `get_char()`/`get_obj()` below decode those
// exactly like dg_scripts.c's get_char()/get_obj().
//
// Faithfulness notes vs. the C authority:
//   * MOB_OR_IMPL(ch): true for a real NPC, or for a switched-in implementor.
//     We model "switched-in implementor" as a PC ch->desc whose level >= IMPL.
//   * AFF_CHARM short-circuits every command (a charmed mob obeys nobody's
//     script but its own loaders) — preserved.
//   * sub_write()/sub_write_to_char(): the DG message engine (dg_comm.c). It is
//     NOT act(): its token set is ~ @ ^ * for chars and ` for objects, resolved
//     per-recipient. Ported verbatim so masound/mecho/msend/mechoaround render
//     identically to C. Delivery still flows through send_to_char like act().
//   * mob_log(): routes to the same place script errors go (log::warn!), with
//     the C "Mob (<short>, VNum <vnum>): <msg>" prefix preserved.
//   * dg_owner_purged: the C global the VM checks after each command to learn
//     the running mob purged itself. Exposed as a module-static the VM reads
//     via `take_owner_purged()` (and the VM should `clear_owner_purged()`
//     before invoking the command, mirroring dg_scripts.c which zeroes it).
//
// Only the 16 commands present in this DeltaMUD's dg_mobcmd.c are implemented
// (masound, mkill, mjunk, mechoaround, msend, mecho, mload, mpurge, mgoto, mat,
// mteleport, mforce, mexp, mgold, mhunt, mremember, mforget, mtransform). The
// stock-Circle extras named in some docs (mzoneecho/mdamage/mfollow) are not in
// this codebase's source and are intentionally absent.

use crate::act::{act, ActArg, To};
use crate::character::Character;
use crate::flags::AFF_CHARM;
use crate::handler::isname;
use crate::interpreter::{command_interpreter, is_abbrev, one_argument};
use crate::object::{ObjLoc, WearFlags};
use crate::state::GameState;
use crate::types::*;
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// DG UID encoding: ESC byte then decimal id (dg_scripts.h: UID_CHAR '\x1b').
// ---------------------------------------------------------------------------
const UID_CHAR: char = '\x1b';

/// The C `dg_owner_purged` global. Set by `mpurge` when a mob purges itself so
/// the VM can stop running that trigger. The VM clears it before each command.
static OWNER_PURGED: AtomicBool = AtomicBool::new(false);

/// VM hook: clear the self-purge flag before running a command (C zeroes the
/// global at the top of the dispatch loop).
pub fn clear_owner_purged() {
    OWNER_PURGED.store(false, Ordering::Relaxed);
}

/// VM hook: read-and-clear whether the just-run command purged its owning mob.
pub fn take_owner_purged() -> bool {
    OWNER_PURGED.swap(false, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Logging — C mob_log(): "Mob (<short>, VNum <vnum>): <msg>".
// ---------------------------------------------------------------------------
fn mob_log(g: &GameState, mob: CharId, msg: &str) {
    let (short, vnum) = match g.get_char(mob) {
        Some(c) => (
            c.short_desc.clone().unwrap_or_else(|| c.player.name.clone()),
            c.nr,
        ),
        None => ("<extracted>".to_string(), NOBODY),
    };
    log::warn!("Mob ({}, VNum {}): {}", short, vnum, msg);
}

// ---------------------------------------------------------------------------
// MOB_OR_IMPL(ch): permitted to run m* commands?  (dg_mobcmd.c macro)
//   IS_NPC(ch) && (!ch->desc || GET_LEVEL(ch->desc->original) >= LVL_IMPL)
// In the Rust port a switched immortal keeps is_npc=false on their PC record
// and possesses the mob; we model the relevant case directly: a real NPC is
// always allowed, and a connected PC is allowed only at implementor level.
// ---------------------------------------------------------------------------
fn mob_or_impl(g: &GameState, ch: CharId) -> bool {
    match g.get_char(ch) {
        Some(c) => {
            if c.is_npc {
                // A real mob: allowed unless a (switched) descriptor below IMPL
                // is driving it. NPC records never carry a PC desc here, so a
                // bare NPC qualifies.
                c.desc.is_none() || c.player.level >= LVL_IMPL
            } else {
                // A switched-in PC body: only implementors may script.
                c.player.level >= LVL_IMPL
            }
        }
        None => false,
    }
}

/// C: `if (AFF_FLAGGED(ch, AFF_CHARM)) return;`
fn is_charmed(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.affect_flags & AFF_CHARM != 0).unwrap_or(false)
}

/// C: the extra implementor gate several commands carry —
/// `if (ch->desc && GET_LEVEL(ch->desc->original) < LVL_IMPL) return;`
/// True means "blocked" (a switched mortal cannot run this privileged command).
fn blocked_by_desc_level(g: &GameState, ch: CharId) -> bool {
    match g.get_char(ch) {
        Some(c) => c.desc.is_some() && c.player.level < LVL_IMPL,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// DG entity resolution by UID-or-name (dg_scripts.c get_char/get_obj).
// ---------------------------------------------------------------------------

/// True if `s` is a DG UID token (`\x1b<id>`). Returns the decoded id.
fn parse_uid(s: &str) -> Option<u64> {
    let mut chars = s.chars();
    if chars.next() != Some(UID_CHAR) {
        return None;
    }
    // C uses atoi(name+1): leading digits, stop at first non-digit.
    let rest = &s[UID_CHAR.len_utf8()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok()
}

/// C get_char(): UID → arena lookup, else first char in world by name.
/// The C invis-level guard (`!GET_INVIS_LEV(i)`) is preserved: a hidden
/// immortal is not returned to a script.
fn dg_get_char(g: &GameState, name: &str) -> Option<CharId> {
    if let Some(id) = parse_uid(name) {
        let cid = CharId(id);
        return match g.get_char(cid) {
            Some(c) if c.is_npc || c.invis_level == 0 => Some(cid),
            _ => None,
        };
    }
    for &cid in &g.char_list {
        if let Some(c) = g.get_char(cid) {
            if isname(name, &c.player.name) && (c.is_npc || c.invis_level == 0) {
                return Some(cid);
            }
        }
    }
    None
}

/// C get_obj(): UID → arena lookup, else first object in world by name.
fn dg_get_obj(g: &GameState, name: &str) -> Option<ObjId> {
    if let Some(id) = parse_uid(name) {
        let oid = ObjId(id);
        return if g.get_obj(oid).is_some() { Some(oid) } else { None };
    }
    for &oid in &g.obj_list {
        if let Some(o) = g.get_obj(oid) {
            if isname(name, &o.name) {
                return Some(oid);
            }
        }
    }
    None
}

/// Visible char in the mob's room by name (get_char_room_vis) — does NOT do
/// UID; callers branch on UID first exactly as the C does.
fn get_char_room_vis(g: &GameState, mob: CharId, name: &str) -> Option<CharId> {
    g.get_char_room_vis(mob, name)
}

/// Visible char anywhere in the world by name (get_char_vis).
fn get_char_vis(g: &GameState, mob: CharId, name: &str) -> Option<CharId> {
    crate::spell_parser::get_char_world_vis(g, mob, name)
}

/// Visible object anywhere relevant to the mob (get_obj_vis): the mob's room,
/// then its equipment, then its inventory — matching the C ordering used by the
/// mob commands (do_mpurge falls through get_obj_vis on a missing victim).
fn get_obj_vis(g: &GameState, mob: CharId, name: &str) -> Option<ObjId> {
    let room = g.get_char(mob).and_then(|c| c.in_room);
    if let Some(rnum) = room {
        let contents = g.room(rnum).contents.clone();
        if let Some(o) = g.get_obj_in_list_vis(mob, name, &contents) {
            return Some(o);
        }
    }
    if let Some(c) = g.get_char(mob) {
        let eq: Vec<ObjId> = c.equipment.iter().flatten().copied().collect();
        if let Some(o) = g.get_obj_in_list_vis(mob, name, &eq) {
            return Some(o);
        }
        let inv = c.carrying.clone();
        if let Some(o) = g.get_obj_in_list_vis(mob, name, &inv) {
            return Some(o);
        }
    }
    crate::spell_parser::get_obj_world_vis(g, mob, name)
}

/// DG find_target_room (act.wizard.c) for mob scripts: resolve a room arg to an
/// rnum. The C version messages `ch` on failure; mobs have no descriptor so the
/// message is a no-op — we simply return None and let the caller mob_log().
/// Order matches C: numeric vnum, else a visible creature's room, else a
/// visible object's room.
fn find_target_room(g: &GameState, mob: CharId, raw: &str) -> Option<RoomRnum> {
    let (roomstr, _) = one_argument(raw);
    if roomstr.is_empty() {
        return None;
    }
    let first_is_digit = roomstr.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
    if first_is_digit && !roomstr.contains('.') {
        let vnum: RoomVnum = roomstr.parse().unwrap_or(NOWHERE);
        return g.real_room(vnum);
    }
    // UID or name → creature.
    if let Some(cid) = if roomstr.starts_with(UID_CHAR) {
        dg_get_char(g, &roomstr)
    } else {
        get_char_vis(g, mob, &roomstr)
    } {
        return g.get_char(cid).and_then(|c| c.in_room);
    }
    // UID or name → object's room.
    if let Some(oid) = if roomstr.starts_with(UID_CHAR) {
        dg_get_obj(g, &roomstr)
    } else {
        get_obj_vis(g, mob, &roomstr)
    } {
        if let Some(ObjLoc::Room(r)) = g.get_obj(oid).map(|o| o.loc) {
            return Some(r);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// sub_write / sub_write_to_char — the DG message engine (dg_comm.c).
//
// `arg` is rendered per recipient: ~ @ ^ * pull a *char* token (resolved by
// UID/visibility), ` pulls an *object* token, and `\` escapes the next char.
//   ~  visible name  ("you" if self, "someone" if unseen)
//   @  possessive    ("your"/"someone's")
//   ^  his/her/its possessive (CAN_SEE-gated; "its" if unseen)
//   *  him/her/it    (CAN_SEE-gated; "it" if unseen)
//   `  object short  ("something" if unseen)
// The first character of the finished line is capitalised and "\r\n" appended
// (C used "\n\r"; the rest of this port uses "\r\n", so we match the port).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Tok {
    Char(char),     // a token whose substitution char is `kind`
    Obj,            // a ` object token
}

/// A parsed sub_write template: literal text segments interleaved with tokens.
/// `literals` has N+1 entries when there are N tokens (matching the C layout
/// `tokens[0] tok0 tokens[1] tok1 ... tokens[N]`).
struct SubTemplate {
    literals: Vec<String>,
    toks: Vec<(Tok, String)>, // (kind, name-after-token)
}

/// C any_one_name: lowercase first word, stopping at whitespace or punctuation
/// (but '#' and '-' are allowed inside a word — needed for negative ids/UIDs).
fn any_one_name(argument: &str) -> (String, &str) {
    let bytes = argument.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    let start = i;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            break;
        }
        if c.is_ascii_punctuation() && c != '#' && c != '-' {
            break;
        }
        i += 1;
    }
    (argument[start..i].to_ascii_lowercase(), &argument[i..])
}

/// Parse the sub_write template (C: the big tokenising loop in sub_write).
fn parse_sub_template(arg: &str) -> SubTemplate {
    let mut literals: Vec<String> = Vec::new();
    let mut toks: Vec<(Tok, String)> = Vec::new();
    let mut cur = String::new();
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' | '@' | '^' | '*' => {
                literals.push(std::mem::take(&mut cur));
                // Remaining string after this control char.
                let rest: String = chars.clone().collect();
                let (name, tail) = any_one_name(&rest);
                // Advance the iterator past what any_one_name consumed.
                let consumed = rest.len() - tail.len();
                for _ in 0..consumed {
                    chars.next();
                }
                toks.push((Tok::Char(c), name));
            }
            '`' => {
                literals.push(std::mem::take(&mut cur));
                let rest: String = chars.clone().collect();
                let (name, tail) = any_one_name(&rest);
                let consumed = rest.len() - tail.len();
                for _ in 0..consumed {
                    chars.next();
                }
                toks.push((Tok::Obj, name));
            }
            '\\' => {
                // Escape: drop the backslash, take the next char literally.
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            _ => cur.push(c),
        }
    }
    literals.push(cur);
    SubTemplate { literals, toks }
}

/// HSHR(ch): his/her/its.
fn hshr(g: &GameState, cid: CharId) -> &'static str {
    match g.get_char(cid).map(|c| c.player.sex) {
        Some(Gender::Male) => "his",
        Some(Gender::Female) => "her",
        _ => "its",
    }
}
/// HMHR(ch): him/her/it.
fn hmhr(g: &GameState, cid: CharId) -> &'static str {
    match g.get_char(cid).map(|c| c.player.sex) {
        Some(Gender::Male) => "him",
        Some(Gender::Female) => "her",
        _ => "it",
    }
}
/// PERS(target, viewer): visible name, else "someone" (act.rs pers() is private;
/// reproduce its logic to keep sub_write self-contained).
fn pers(g: &GameState, viewer: CharId, target: CharId) -> String {
    if g.can_see(viewer, target) {
        if let Some(c) = g.get_char(target) {
            return if c.is_npc {
                c.short_desc.clone().unwrap_or_else(|| c.player.name.clone())
            } else {
                c.player.name.clone()
            };
        }
    }
    let imm = g
        .get_char(target)
        .map(|c| !c.is_npc && c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    if imm { "A Mystical Being".to_string() } else { "someone".to_string() }
}
/// OBJS(obj, viewer): object short description, else "something".
fn objs_short(g: &GameState, oid: ObjId) -> String {
    g.get_obj(oid).map(|o| o.short_description.clone()).unwrap_or_else(|| "something".to_string())
}

/// C sub_write_to_char: render the template for one recipient and send it.
/// `ctoks`/`otoks` are the pre-resolved token targets (resolved once in
/// sub_write, NOT per recipient — matching the C, where get_char/… run before
/// the per-recipient loop).
fn sub_write_to_char(
    g: &mut GameState,
    to: CharId,
    tmpl: &SubTemplate,
    ctoks: &[Option<CharId>],
    otoks: &[Option<ObjId>],
) {
    let mut sb = String::new();
    for i in 0..tmpl.toks.len() {
        sb.push_str(&tmpl.literals[i]);
        match tmpl.toks[i].0 {
            Tok::Char(kind) => {
                let tgt = ctoks[i];
                match kind {
                    '~' => {
                        match tgt {
                            None => sb.push_str("someone"),
                            Some(t) if t == to => sb.push_str("you"),
                            Some(t) => sb.push_str(&pers(g, to, t)),
                        }
                    }
                    '@' => match tgt {
                        None => sb.push_str("someone's"),
                        Some(t) if t == to => sb.push_str("your"),
                        Some(t) => {
                            sb.push_str(&pers(g, to, t));
                            sb.push_str("'s");
                        }
                    },
                    '^' => match tgt {
                        Some(t) if g.can_see(to, t) => {
                            if t == to {
                                sb.push_str("your");
                            } else {
                                sb.push_str(hshr(g, t));
                            }
                        }
                        _ => sb.push_str("its"),
                    },
                    '*' => match tgt {
                        Some(t) if g.can_see(to, t) => {
                            if t == to {
                                sb.push_str("you");
                            } else {
                                sb.push_str(hmhr(g, t));
                            }
                        }
                        _ => sb.push_str("it"),
                    },
                    _ => {}
                }
            }
            Tok::Obj => match otoks[i] {
                None => sb.push_str("something"),
                Some(o) => sb.push_str(&objs_short(g, o)),
            },
        }
    }
    // Trailing literal segment.
    if let Some(last) = tmpl.literals.last() {
        sb.push_str(last);
    }
    // Capitalise first char, append CRLF.
    if let Some(first) = sb.chars().next() {
        if first.is_ascii_lowercase() {
            let up = first.to_ascii_uppercase().to_string();
            sb.replace_range(0..first.len_utf8(), &up);
        }
    }
    sb.push_str("\r\n");
    g.send_to_char(to, &sb);
}

const TO_CHAR: i32 = 1 << 0;
const TO_ROOM: i32 = 1 << 1;

/// C sub_write(arg, ch, find_invis, targets). `find_invis` true means token
/// names resolve via get_char/get_obj (UID-or-world, invis-ignoring); false
/// means room-visible only. Every DG mob command passes find_invis=TRUE.
fn sub_write(g: &mut GameState, arg: &str, ch: CharId, find_invis: bool, targets: i32) {
    let tmpl = parse_sub_template(arg);

    // Resolve each token once (C resolves before the recipient loop).
    let mut ctoks: Vec<Option<CharId>> = Vec::with_capacity(tmpl.toks.len());
    let mut otoks: Vec<Option<ObjId>> = Vec::with_capacity(tmpl.toks.len());
    for (kind, name) in &tmpl.toks {
        match kind {
            Tok::Char(_) => {
                let cid = if find_invis {
                    dg_get_char(g, name)
                } else {
                    get_char_room_vis(g, ch, name)
                };
                ctoks.push(cid);
                otoks.push(None);
            }
            Tok::Obj => {
                let oid = if find_invis {
                    dg_get_obj(g, name)
                } else {
                    get_obj_vis(g, ch, name)
                };
                ctoks.push(None);
                otoks.push(oid);
            }
        }
    }

    if targets & TO_CHAR != 0 && sendok(g, ch) {
        sub_write_to_char(g, ch, &tmpl, &ctoks, &otoks);
    }
    if targets & TO_ROOM != 0 {
        let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let people = g.room(rnum).people.clone();
        for to in people {
            if to != ch && sendok(g, to) {
                sub_write_to_char(g, to, &tmpl, &ctoks, &otoks);
            }
        }
    }
}

/// SENDOK(ch): has a connection and is awake (dg_scripts.h SENDOK macro
/// reduces to a descriptor + AWAKE check for our purposes).
fn sendok(g: &GameState, cid: CharId) -> bool {
    match g.get_char(cid) {
        Some(c) => c.desc.is_some() && c.position > Position::Sleeping,
        None => false,
    }
}

// ===========================================================================
// The 16 mob commands.
// ===========================================================================

/// do_masound — echo `argument` into every adjacent room (not the mob's room).
fn do_masound(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let arg = argument.trim_start();
    if arg.is_empty() {
        mob_log(g, ch, "masound called with no argument");
        return;
    }
    let was_in_room = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    // Snapshot adjacent rooms first; we briefly relocate the mob into each so
    // sub_write(..., TO_ROOM) reaches that room's occupants (C moves IN_ROOM
    // and restores it after).
    let mut targets: Vec<RoomRnum> = Vec::new();
    for door in 0..NUM_OF_DIRS {
        if let Some(exit) = g.room(was_in_room).get_exit(door) {
            if let Some(to_rnum) = g.real_room(exit.to_room) {
                if to_rnum != was_in_room {
                    targets.push(to_rnum);
                }
            }
        }
    }
    for to_rnum in targets {
        // Relocate without using char_to/from_room (we must not disturb the
        // mob's position in its real room's people list). Directly retarget
        // in_room, render, then restore — matching the C `IN_ROOM(ch) = ...`.
        set_in_room_field(g, ch, Some(to_rnum));
        // The mob is not actually in that room's people list, so sub_write's
        // TO_ROOM loop (which reads room.people) would miss it being excluded —
        // but it also isn't present to be excluded, which is correct.
        push_into_room_temp(g, ch, to_rnum);
        sub_write(g, arg, ch, true, TO_ROOM);
        pop_from_room_temp(g, ch, to_rnum);
    }
    set_in_room_field(g, ch, Some(was_in_room));
}

// masound helpers: temporarily present the mob in an adjacent room so the
// TO_ROOM recipients include that room (and the mob is excluded as `ch`).
fn set_in_room_field(g: &mut GameState, ch: CharId, rnum: Option<RoomRnum>) {
    if let Some(c) = g.get_char_mut(ch) {
        c.in_room = rnum;
    }
}
fn push_into_room_temp(g: &mut GameState, ch: CharId, rnum: RoomRnum) {
    if let Some(r) = g.rooms.get_mut(rnum) {
        if !r.people.contains(&ch) {
            r.people.push(ch);
        }
    }
}
fn pop_from_room_temp(g: &mut GameState, ch: CharId, rnum: RoomRnum) {
    if let Some(r) = g.rooms.get_mut(rnum) {
        r.people.retain(|&c| c != ch);
    }
}

/// do_mkill — start a fight with a player or mob (no murder check).
fn do_mkill(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        mob_log(g, ch, "mkill called with no argument");
        return;
    }
    let victim = if arg.starts_with(UID_CHAR) {
        match dg_get_char(g, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mkill: victim ({}) not found", display_arg(&arg)));
                return;
            }
        }
    } else {
        match get_char_room_vis(g, ch, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mkill: victim ({}) not found", arg));
                return;
            }
        }
    };
    if victim == ch {
        mob_log(g, ch, "mkill: victim is self");
        return;
    }
    // Charmed mob attacking its master — already guarded by is_charmed above,
    // but C re-checks against ch->master for the AFF set directly.
    let master = g.get_char(ch).and_then(|c| c.master);
    if g.get_char(ch).map(|c| c.affect_flags & AFF_CHARM != 0).unwrap_or(false)
        && master == Some(victim)
    {
        mob_log(g, ch, "mkill: charmed mob attacking master");
        return;
    }
    if g.get_char(ch).and_then(|c| c.fighting).is_some() {
        mob_log(g, ch, "mkill: already fighting");
        return;
    }
    crate::combat::hit(g, ch, victim);
}

/// do_mjunk — destroy carried/worn objects (supports all and all.xxx).
fn do_mjunk(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        mob_log(g, ch, "mjunk called with no argument");
        return;
    }

    // find_all_dots: "all" -> ALL; "all.x" -> ALLDOT(x); else INDIV.
    let (is_all_dot, dot_name) = parse_all_dots(&arg);

    if !is_all_dot && !arg.eq_ignore_ascii_case("all") {
        // FIND_INDIV branch: try a worn item first (by name), else an inventory
        // item; destroy at most one.
        if let Some((oid, pos)) = find_obj_in_equip_vis(g, ch, &arg) {
            g.unequip_char(ch, pos);
            detach_and_extract_obj(g, oid);
            return;
        }
        let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
        if let Some(oid) = g.get_obj_in_list_vis(ch, &arg, &inv) {
            detach_and_extract_obj(g, oid);
        }
        return;
    }

    // FIND_ALL / FIND_ALLDOT branch: scan inventory.
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    for oid in inv {
        let matches = if is_all_dot {
            g.get_obj(oid).map(|o| isname(&dot_name, &o.name)).unwrap_or(false)
        } else {
            true // plain "all"
        };
        if matches {
            detach_and_extract_obj(g, oid);
        }
    }
    // Then strip any matching worn items.
    while let Some((oid, pos)) = find_obj_in_equip_vis(g, ch, &arg) {
        g.unequip_char(ch, pos);
        detach_and_extract_obj(g, oid);
    }
}

/// do_mechoaround — message everyone in the room except the mob and victim.
fn do_mechoaround(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg, rest) = one_argument(argument);
    let msg = rest.trim_start();
    if arg.is_empty() {
        mob_log(g, ch, "mechoaround called with no argument");
        return;
    }
    let victim = if arg.starts_with(UID_CHAR) {
        match dg_get_char(g, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mechoaround: victim ({}) does not exist", display_arg(&arg)));
                return;
            }
        }
    } else {
        match get_char_room_vis(g, ch, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mechoaround: victim ({}) does not exist", arg));
                return;
            }
        }
    };
    // sub_write(p, victim, TRUE, TO_ROOM): the victim is `ch` for sub_write, so
    // its TO_ROOM loop excludes the victim and sends to everyone else.
    sub_write(g, msg, victim, true, TO_ROOM);
}

/// do_msend — message only the victim.
fn do_msend(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg, rest) = one_argument(argument);
    let msg = rest.trim_start();
    if arg.is_empty() {
        mob_log(g, ch, "msend called with no argument");
        return;
    }
    let victim = if arg.starts_with(UID_CHAR) {
        match dg_get_char(g, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("msend: victim ({}) does not exist", display_arg(&arg)));
                return;
            }
        }
    } else {
        match get_char_room_vis(g, ch, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("msend: victim ({}) does not exist", arg));
                return;
            }
        }
    };
    sub_write(g, msg, victim, true, TO_CHAR);
}

/// do_mecho — message the whole room.
fn do_mecho(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if argument.is_empty() {
        mob_log(g, ch, "mecho called with no arguments");
        return;
    }
    let p = argument.trim_start();
    sub_write(g, p, ch, true, TO_ROOM);
}

/// do_mload — load a mob or object (into the mob's room / inventory).
fn do_mload(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    // C: `if (ch->desc && GET_LEVEL(ch->desc->original) < LVL_IMPL) return;`
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (arg1, arg2) = two_arguments(argument);
    if arg1.is_empty() || arg2.is_empty() || !is_number(&arg2) {
        mob_log(g, ch, "mload: bad syntax");
        return;
    }
    let number: i32 = arg2.parse().unwrap_or(-1);
    if number < 0 {
        mob_log(g, ch, "mload: bad syntax");
        return;
    }
    let room = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    if is_abbrev(&arg1, "mob") {
        match g.load_mobile(number) {
            Some(mob) => {
                g.char_to_room(mob, room);
                // load_mtrigger(mob): fire the new mob's load trigger (the VM
                // owns the trigger tables; route through the shared hook).
                fire_load_mtrigger(g, mob);
            }
            None => mob_log(g, ch, "mload: bad mob vnum"),
        }
    } else if is_abbrev(&arg1, "obj") {
        match g.load_object(number) {
            Some(oid) => {
                let takeable = g
                    .get_obj(oid)
                    .map(|o| o.wear_flags.contains(WearFlags::TAKE))
                    .unwrap_or(false);
                if takeable {
                    g.obj_to_char(oid, ch);
                } else {
                    g.obj_to_room(oid, room);
                }
                fire_load_otrigger(g, oid);
            }
            None => mob_log(g, ch, "mload: bad object vnum"),
        }
    } else {
        mob_log(g, ch, "mload: bad type");
    }
}

/// do_mpurge — purge a target, or all NPCs+objects in the room.
fn do_mpurge(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        // 'purge' — every NPC (except self) and every object in the room.
        let room = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let people = g.room(room).people.clone();
        for victim in people {
            if victim == ch {
                continue;
            }
            if g.get_char(victim).map(|c| c.is_npc).unwrap_or(false) {
                g.extract_char(victim);
            }
        }
        let contents = g.room(room).contents.clone();
        for oid in contents {
            detach_and_extract_obj(g, oid);
        }
        return;
    }

    let victim = if arg.starts_with(UID_CHAR) {
        dg_get_char(g, &arg)
    } else {
        get_char_room_vis(g, ch, &arg)
    };

    let victim = match victim {
        Some(v) => v,
        None => {
            // No char by that name — try an object.
            if let Some(oid) = get_obj_vis(g, ch, &arg) {
                detach_and_extract_obj(g, oid);
            } else {
                mob_log(g, ch, "mpurge: bad argument");
            }
            return;
        }
    };

    if !g.get_char(victim).map(|c| c.is_npc).unwrap_or(false) {
        mob_log(g, ch, "mpurge: purging a PC");
        return;
    }
    if victim == ch {
        OWNER_PURGED.store(true, Ordering::Relaxed);
    }
    g.extract_char(victim);
}

/// do_mgoto — relocate the mob to a non-private room.
fn do_mgoto(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        mob_log(g, ch, "mgoto called with no argument");
        return;
    }
    let location = match find_target_room(g, ch, &arg) {
        Some(r) => r,
        None => {
            mob_log(g, ch, "mgoto: invalid location");
            return;
        }
    };
    if g.get_char(ch).and_then(|c| c.fighting).is_some() {
        stop_fighting(g, ch);
    }
    g.char_from_room(ch);
    g.char_to_room(ch, location);
}

/// do_mat — run a command at another location, then return.
fn do_mat(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg, rest) = one_argument(argument);
    let command = rest.trim_start();
    if arg.is_empty() || command.is_empty() {
        mob_log(g, ch, "mat: bad argument");
        return;
    }
    let location = match find_target_room(g, ch, &arg) {
        Some(r) => r,
        None => {
            mob_log(g, ch, "mat: invalid location");
            return;
        }
    };
    let original = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    g.char_from_room(ch);
    g.char_to_room(ch, location);
    command_interpreter(g, ch, command);
    // If ch still exists and is still at `location` (didn't quit/teleport away),
    // move it back. (C: `if (IN_ROOM(ch) == location)`.)
    if g.char_exists(ch) && g.get_char(ch).and_then(|c| c.in_room) == Some(location) {
        g.char_from_room(ch);
        g.char_to_room(ch, original);
    }
}

/// do_mteleport — move a victim (or everyone, with "all") to a target room.
fn do_mteleport(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    let (arg1, arg2) = two_arguments(argument);
    if arg1.is_empty() || arg2.is_empty() {
        mob_log(g, ch, "mteleport: bad syntax");
        return;
    }
    let target = find_target_room(g, ch, &arg2);
    let target = match target {
        Some(t) => t,
        None => {
            mob_log(g, ch, "mteleport target is an invalid room");
            return;
        }
    };

    if arg1.eq_ignore_ascii_case("all") {
        let here = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        if target == here {
            mob_log(g, ch, "mteleport all target is itself");
            return;
        }
        let people = g.room(here).people.clone();
        for vict in people {
            if g.get_char(vict).map(|c| c.player.level < LVL_IMMORT).unwrap_or(false) {
                g.char_from_room(vict);
                g.char_to_room(vict, target);
            }
        }
    } else {
        let vict = if arg1.starts_with(UID_CHAR) {
            match dg_get_char(g, &arg1) {
                Some(v) => v,
                None => {
                    mob_log(g, ch, &format!("mteleport: victim ({}) does not exist", display_arg(&arg1)));
                    return;
                }
            }
        } else {
            match get_char_vis(g, ch, &arg1) {
                Some(v) => v,
                None => {
                    mob_log(g, ch, &format!("mteleport: victim ({}) does not exist", arg1));
                    return;
                }
            }
        };
        if g.get_char(vict).map(|c| c.player.level < LVL_IMMORT).unwrap_or(false) {
            g.char_from_room(vict);
            g.char_to_room(vict, target);
        }
    }
}

/// do_mforce — force a victim (or "all" mortals in the room) to run a command.
fn do_mforce(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (arg, rest) = one_argument(argument);
    let command = rest.trim_start();
    if arg.is_empty() || command.is_empty() {
        mob_log(g, ch, "mforce: bad syntax");
        return;
    }

    if arg.eq_ignore_ascii_case("all") {
        // Every connected, in-game player in the mob's room below the mob's
        // level (and below immort) that the mob can see.
        let here = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
        let people = g.room(here).people.clone();
        for vch in people {
            if vch == ch {
                continue;
            }
            let ok = match g.get_char(vch) {
                Some(c) => {
                    // i->character && !i->connected — a playing PC; mobs have no
                    // descriptor so this naturally restricts to players.
                    c.desc.is_some()
                        && c.player.level < ch_level
                        && c.player.level < LVL_IMMORT
                }
                None => false,
            };
            if ok && g.can_see(ch, vch) {
                command_interpreter(g, vch, command);
            }
        }
    } else {
        let victim = if arg.starts_with(UID_CHAR) {
            match dg_get_char(g, &arg) {
                Some(v) => v,
                None => {
                    mob_log(g, ch, &format!("mforce: victim ({}) does not exist", display_arg(&arg)));
                    return;
                }
            }
        } else {
            match get_char_room_vis(g, ch, &arg) {
                Some(v) => v,
                None => {
                    mob_log(g, ch, "mforce: no such victim");
                    return;
                }
            }
        };
        if victim == ch {
            mob_log(g, ch, "mforce: forcing self");
            return;
        }
        if g.get_char(victim).map(|c| c.player.level < LVL_IMMORT).unwrap_or(false) {
            command_interpreter(g, victim, command);
        }
    }
}

/// do_mexp — grant experience to a target.
fn do_mexp(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (name, amount) = two_arguments(argument);
    if name.is_empty() || amount.is_empty() {
        mob_log(g, ch, "mexp: too few arguments");
        return;
    }
    let victim = if name.starts_with(UID_CHAR) {
        match dg_get_char(g, &name) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mexp: victim ({}) does not exist", display_arg(&name)));
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mexp: victim ({}) does not exist", name));
                return;
            }
        }
    };
    let gain: i64 = amount.parse().unwrap_or(0);
    crate::limits::gain_exp(g, victim, gain);
}

/// do_mgold — grant (or remove) gold; never below zero.
fn do_mgold(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (name, amount) = two_arguments(argument);
    if name.is_empty() || amount.is_empty() {
        mob_log(g, ch, "mgold: too few arguments");
        return;
    }
    let victim = if name.starts_with(UID_CHAR) {
        match dg_get_char(g, &name) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mgold: victim ({}) does not exist", display_arg(&name)));
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mgold: victim ({}) does not exist", name));
                return;
            }
        }
    };
    let delta: i32 = amount.parse().unwrap_or(0);
    let mut underflow = false;
    if let Some(c) = g.get_char_mut(victim) {
        c.points.gold += delta;
        if c.points.gold < 0 {
            c.points.gold = 0;
            underflow = true;
        }
    }
    if underflow {
        mob_log(g, ch, "mgold subtracting more gold than character has");
    }
}

/// do_mhunt — set the mob's hunting target (unless already fighting).
fn do_mhunt(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        mob_log(g, ch, "mhunt called with no argument");
        return;
    }
    if g.get_char(ch).and_then(|c| c.fighting).is_some() {
        return;
    }
    let victim = if arg.starts_with(UID_CHAR) {
        match dg_get_char(g, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mhunt: victim ({}) does not exist", display_arg(&arg)));
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mhunt: victim ({}) does not exist", arg));
                return;
            }
        }
    };
    if let Some(c) = g.get_char_mut(ch) {
        c.hunting = Some(victim);
    }
}

/// do_mremember — add a victim to the mob's script-memory list.
fn do_mremember(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (arg, rest) = one_argument(argument);
    let cmd = rest.trim_start();
    if arg.is_empty() {
        mob_log(g, ch, "mremember: bad syntax");
        return;
    }
    let victim = if arg.starts_with(UID_CHAR) {
        match dg_get_char(g, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mremember: victim ({}) does not exist", display_arg(&arg)));
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mremember: victim ({}) does not exist", arg));
                return;
            }
        }
    };
    let cmd_opt = if cmd.is_empty() { None } else { Some(cmd.to_string()) };
    script_mem_add(ch, victim, cmd_opt);
}

/// do_mforget — remove a victim from the mob's script-memory list.
fn do_mforget(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    if blocked_by_desc_level(g, ch) {
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        mob_log(g, ch, "mforget: bad syntax");
        return;
    }
    let victim = if arg.starts_with(UID_CHAR) {
        match dg_get_char(g, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mforget: victim ({}) does not exist", display_arg(&arg)));
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &arg) {
            Some(v) => v,
            None => {
                mob_log(g, ch, &format!("mforget: victim ({}) does not exist", arg));
                return;
            }
        }
    };
    script_mem_forget(ch, victim);
}

/// do_mtransform — morph the mob into a different mob prototype, preserving its
/// id, inventory/equipment, affects, hp/exp/gold/position, combat & follow
/// links (C copies the new mob's *prototype display+stats* onto the running
/// char and keeps the dynamic linkage fields). Refuses for switched players.
fn do_mtransform(g: &mut GameState, ch: CharId, argument: &str) {
    if !mob_or_impl(g, ch) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    if is_charmed(g, ch) {
        return;
    }
    // C: a descriptor means a switched immortal — they must use 'switch', not
    // mtransform (no proto vnum to return to).
    if g.get_char(ch).map(|c| c.desc.is_some()).unwrap_or(false) {
        g.send_to_char(ch, "You've got no VNUM to return to, dummy! try 'switch'\r\n");
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        mob_log(g, ch, "mtransform: missing argument");
        return;
    }
    if !arg.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        mob_log(g, ch, "mtransform: bad argument");
        return;
    }
    let vnum: i32 = arg.parse().unwrap_or(-1);

    // Load the destination prototype into the world to read its proto fields.
    let m = match g.load_mobile(vnum) {
        Some(m) => m,
        None => {
            mob_log(g, ch, "mtransform: bad mobile vnum");
            return;
        }
    };

    // Copy the proto's *appearance + base stats* off the loaded mob `m` onto
    // `ch`, keeping ch's dynamic state. (C memcpy's the whole struct then
    // restores the dynamic fields; we copy exactly that proto-derived subset to
    // avoid clobbering ch's id/links — same observable result.)
    let proto: Character = match g.get_char(m) {
        Some(c) => c.clone(),
        None => return,
    };

    if let Some(c) = g.get_char_mut(ch) {
        c.nr = proto.nr;
        c.player.name = proto.player.name.clone();
        c.player.sex = proto.player.sex;
        c.player.class = proto.player.class;
        c.player.race = proto.player.race;
        c.player.level = proto.player.level;
        c.short_desc = proto.short_desc.clone();
        c.long_desc = proto.long_desc.clone();
        c.npc_description = proto.npc_description.clone();
        c.real_abils = proto.real_abils;
        c.aff_abils = proto.aff_abils;
        c.alignment = proto.alignment;
        c.act_flags = proto.act_flags;
        // Combat-relevant proto stats (C copies these from the new proto, while
        // hp/maxhp/exp/gold/pos are explicitly carried over from the *old* mob
        // — i.e. NOT replaced — so we deliberately leave them untouched).
        c.points.hitroll = proto.points.hitroll;
        c.points.damroll = proto.points.damroll;
        c.points.armor = proto.points.armor;
        c.points.power = proto.points.power;
        c.points.mpower = proto.points.mpower;
        c.points.defense = proto.points.defense;
        c.points.mdefense = proto.points.mdefense;
        c.points.technique = proto.points.technique;
    }

    // Discard the temporary loaded prototype mob.
    g.extract_char(m);
}

// ===========================================================================
// Shared helpers: object extraction, equip lookup, all-dots, script memory.
// ===========================================================================

/// Detach an object from wherever it lives, then extract it from the arena.
/// (C extract_obj() unlinks first; GameState::extract_obj requires detachment.)
fn detach_and_extract_obj(g: &mut GameState, oid: ObjId) {
    g.obj_from_anywhere(oid);
    g.extract_obj(oid);
}

/// get_object_in_equip_vis: first worn item matching `name`, with its slot.
fn find_obj_in_equip_vis(g: &GameState, ch: CharId, name: &str) -> Option<(ObjId, usize)> {
    let c = g.get_char(ch)?;
    let (mut count, kw) = crate::handler::get_number(name);
    if count == 0 {
        return None;
    }
    for (pos, slot) in c.equipment.iter().enumerate() {
        if let Some(oid) = slot {
            if let Some(o) = g.get_obj(*oid) {
                if isname(&kw, &o.name) {
                    count -= 1;
                    if count == 0 {
                        return Some((*oid, pos));
                    }
                }
            }
        }
    }
    None
}

/// C find_all_dots, reduced to what mjunk needs:
///   "all"      -> (false, "")     handled as plain-all by the caller
///   "all.foo"  -> (true,  "foo")
///   "foo"      -> (false, "foo")  (FIND_INDIV)
fn parse_all_dots(arg: &str) -> (bool, String) {
    if let Some(rest) = arg.strip_prefix("all.") {
        (true, rest.to_string())
    } else {
        (false, String::new())
    }
}

/// CircleMUD is_number(): non-empty and every char a digit (an optional '-' is
/// allowed only as the leading sign, matching C's isdigit-after-sign loop).
fn is_number(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let body = s.strip_prefix('-').unwrap_or(s);
    !body.is_empty() && body.chars().all(|c| c.is_ascii_digit())
}

/// CircleMUD two_arguments(): two successive one_argument() calls (fill-word
/// skipping), returning the first two words.
fn two_arguments(argument: &str) -> (String, String) {
    let (a1, rest) = one_argument(argument);
    let (a2, _) = one_argument(rest);
    (a1, a2)
}

/// Human-readable rendering of a UID arg for logs (strip the ESC byte).
fn display_arg(arg: &str) -> String {
    if let Some(stripped) = arg.strip_prefix(UID_CHAR) {
        format!("UID {}", stripped)
    } else {
        arg.to_string()
    }
}

// ---------------------------------------------------------------------------
// Script memory (SCRIPT_MEM(ch)) — Character carries no field for this, so it
// lives in a module-static keyed by CharId (the shop.rs/quest.rs pattern). One
// entry remembers a victim's *id* and an optional command to run when the mob
// next sees them (the MEMORY trigger consumes this). Exposed read API lets the
// memory-trigger fire-point (in the VM) walk a mob's remembered victims.
// ---------------------------------------------------------------------------
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone)]
pub struct ScriptMemory {
    pub id: CharId,           // remembered victim's arena id
    pub cmd: Option<String>,  // optional command to mforce on the mob when seen
}

static SCRIPT_MEM: OnceLock<Mutex<HashMap<CharId, Vec<ScriptMemory>>>> = OnceLock::new();

fn script_mem() -> &'static Mutex<HashMap<CharId, Vec<ScriptMemory>>> {
    SCRIPT_MEM.get_or_init(|| Mutex::new(HashMap::new()))
}

fn script_mem_add(mob: CharId, victim: CharId, cmd: Option<String>) {
    let mut m = script_mem().lock().unwrap();
    m.entry(mob).or_default().push(ScriptMemory { id: victim, cmd });
}

fn script_mem_forget(mob: CharId, victim: CharId) {
    let mut m = script_mem().lock().unwrap();
    if let Some(list) = m.get_mut(&mob) {
        list.retain(|e| e.id != victim);
        if list.is_empty() {
            m.remove(&mob);
        }
    }
}

/// VM read API: snapshot a mob's remembered victims (for the MEMORY trigger).
pub fn script_mem_list(mob: CharId) -> Vec<ScriptMemory> {
    script_mem().lock().unwrap().get(&mob).cloned().unwrap_or_default()
}

/// Drop all script memory for a mob (call when the mob is extracted, so a
/// recycled CharId never inherits stale memory — C frees SCRIPT_MEM in
/// free_char).
pub fn script_mem_clear(mob: CharId) {
    script_mem().lock().unwrap().remove(&mob);
}

// ---------------------------------------------------------------------------
// Trigger fire-point hooks. mload calls load_mtrigger/load_otrigger on the new
// entity. The trigger tables + script_driver live in the (sibling) DG engine
// module. To avoid a hard dependency cycle and to let mobcmd compile/run before
// that module exists, these route through act() as a visible side-effect-free
// stub IF the engine isn't present — but if the engine module exposes the real
// hooks, wire them here. Currently the engine is not yet in the tree, so these
// are no-ops (the load still happens; only the freshly-loaded entity's own LOAD
// trigger doesn't fire until the engine lands and replaces these two lines).
// ---------------------------------------------------------------------------
fn fire_load_mtrigger(g: &mut GameState, mob: CharId) {
    crate::dg_triggers::load_mtrigger(g, mob);
}
fn fire_load_otrigger(g: &mut GameState, obj: ObjId) {
    crate::dg_triggers::load_otrigger(g, obj);
}

// ---------------------------------------------------------------------------
// Keep `act`/`ActArg`/`To` imported usage honest: act() is the canonical
// message path the spec says DG sub_write "goes through". sub_write needs the
// ~@^*` token semantics act() cannot express, so it renders directly; but the
// (rare) plain-string DG echo with no tokens is identical to a TO_ROOM act with
// an empty actor. We provide that bridge for the engine's `%echo%`-style calls.
// ---------------------------------------------------------------------------
/// Plain room echo with no token substitution, delivered through act() so the
/// engine has an act()-routed path (spec requirement). Not used by the token
/// commands above, which need sub_write's richer grammar.
pub fn dg_act_room(g: &mut GameState, ch: CharId, msg: &str) {
    act(g, msg, false, ch, None, ActArg::None, To::Room);
}

// ===========================================================================
// Public dispatch — the entry point the script VM calls.
// ===========================================================================

/// Dispatch a mob script command by name (the VM has already expanded
/// %variables% in `argument`). Returns true if `cmd` is a recognised m*
/// command (whether or not it had an effect), false if it is not a mob command
/// (so the VM can fall through to the normal command_interpreter, exactly as
/// dg_scripts.c does: unknown words run as the mob's own commands).
pub fn dispatch_mob_command(g: &mut GameState, mob: CharId, cmd: &str, argument: &str) -> bool {
    let lc = cmd.to_ascii_lowercase();
    match lc.as_str() {
        "masound" => do_masound(g, mob, argument),
        "mkill" => do_mkill(g, mob, argument),
        "mjunk" => do_mjunk(g, mob, argument),
        "mechoaround" => do_mechoaround(g, mob, argument),
        "msend" => do_msend(g, mob, argument),
        "mecho" => do_mecho(g, mob, argument),
        "mload" => do_mload(g, mob, argument),
        "mpurge" => do_mpurge(g, mob, argument),
        "mgoto" => do_mgoto(g, mob, argument),
        "mat" => do_mat(g, mob, argument),
        "mteleport" => do_mteleport(g, mob, argument),
        "mforce" => do_mforce(g, mob, argument),
        "mexp" => do_mexp(g, mob, argument),
        "mgold" => do_mgold(g, mob, argument),
        "mhunt" => do_mhunt(g, mob, argument),
        "mremember" => do_mremember(g, mob, argument),
        "mforget" => do_mforget(g, mob, argument),
        "mtransform" => do_mtransform(g, mob, argument),
        _ => return false,
    }
    true
}

/// Local stop_fighting (combat.rs's is private). Clears the fighting target and
/// resets a Fighting position to Standing (CircleMUD stop_fighting).
fn stop_fighting(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
    }
}
