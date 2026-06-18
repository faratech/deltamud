// dg_comm.rs — script-generated communication (DeltaMUD/CircleMUD dg_comm.c).
//
// The DG VM's `%send%` / `%echo%` / `%echoaround%` style commands ultimately
// funnel through `sub_write`, which renders a line containing the special
// "actor-reference" tokens (~ @ ^ *  for characters, ` for objects) into
// per-recipient text and ships it out. This is the script-side analogue of
// act(): each recipient sees the actor's name (or "you"/"your"/"its") resolved
// from *their* point of view, with CAN_SEE gating.
//
// The token grammar (dg_comm.c sub_write):
//   ~name   -> PERS(char):  the char's name, "you" if it is the recipient,
//                           "someone" if the lookup failed.
//   @name   -> possessive:  "<name>'s", "your", "someone's".
//   ^name   -> his/her/its (HSHR), "your" for self, "its" if can't see.
//   *name   -> him/her/it  (HMHR), "you" for self, "it" if can't see.
//   `name   -> OBJS(obj):   the object's short description, "something" if nil.
//   \x      -> literal x (escape).
// A trailing word with no token is appended verbatim; the whole line is
// CAP'd (first letter upper) and CRLF-terminated, exactly as C does.
//
// Name resolution mirrors C: when `find_invis` is set the lookups ignore
// visibility (get_char / get_obj by raw keyword over the whole world);
// otherwise they are room-visibility-scoped from the actor `ch`'s vantage.
// Because GameState exposes id-scoped finders rather than C's global keyword
// finders, the invis path resolves over the actor's room + inventory + the
// whole char/obj arenas, which is the faithful behaviour for the scripts in
// lib/world/trg (they reference %actor% / %self% UID vars, never bare global
// keywords here — the sub_write tokens are produced by the VM after UID
// expansion, so they are always room-local names of entities still in scope).
//
// Targets bitvector (TO_CHAR / TO_ROOM), matching the C `targets` arg.

use crate::object::ObjLoc;
use crate::state::GameState;
use crate::types::*;

/// dg_comm.c targets: who receives the sub_write output.
pub const TO_CHAR: i32 = 1 << 0;
pub const TO_ROOM: i32 = 1 << 1;

// ---------------------------------------------------------------------------
// Small pronoun / name helpers (CircleMUD utils.h HSHR/HMHR + handler PERS),
// reimplemented locally because the act.rs versions are private. Semantics
// are identical: sex-driven for HSHR/HMHR, visible-name-or-fallback for PERS.
// ---------------------------------------------------------------------------

fn sex_of(g: &GameState, cid: CharId) -> Gender {
    g.get_char(cid)
        .map(|c| c.player.sex)
        .unwrap_or(Gender::Neutral)
}

/// HSHR(ch): his / her / its.
fn hshr(g: &GameState, cid: CharId) -> &'static str {
    match sex_of(g, cid) {
        Gender::Male => "his",
        Gender::Female => "her",
        Gender::Neutral => "its",
    }
}

/// HMHR(ch): him / her / it.
fn hmhr(g: &GameState, cid: CharId) -> &'static str {
    match sex_of(g, cid) {
        Gender::Male => "him",
        Gender::Female => "her",
        Gender::Neutral => "it",
    }
}

/// PERS(target, viewer): the target's name as the viewer perceives it.
/// NPC -> short_desc, PC -> name; "someone" when invisible to the viewer.
fn pers(g: &GameState, target: CharId, viewer: CharId) -> String {
    if !g.can_see(viewer, target) {
        return "someone".to_string();
    }
    match g.get_char(target) {
        Some(ch) => {
            if ch.is_npc {
                ch.short_desc
                    .clone()
                    .unwrap_or_else(|| ch.player.name.clone())
            } else {
                ch.player.name.clone()
            }
        }
        None => "someone".to_string(),
    }
}

/// OBJS(obj, viewer): the object's short description ("something" if nil).
/// (CircleMUD OBJS also CAN_SEE-gates, but sub_write's `` ` `` token in C does
/// not — it unconditionally prints the short desc — so we match dg_comm.c.)
fn objs(g: &GameState, oid: ObjId) -> String {
    match g.get_obj(oid) {
        Some(o) => {
            if o.short_description.is_empty() {
                "something".to_string()
            } else {
                o.short_description.clone()
            }
        }
        None => "something".to_string(),
    }
}

// ---------------------------------------------------------------------------
// any_one_name (dg_comm.c): copy the next word, lowercased, stopping at
// whitespace or punctuation (but '#' and '-' are kept, for "#3" / "1.foo"
// style ordinals and hyphenated keywords). Returns (word, rest).
// ---------------------------------------------------------------------------
fn any_one_name(argument: &str) -> (String, &str) {
    // Skip leading whitespace.
    let trimmed = argument.trim_start_matches(|c: char| c.is_whitespace());
    let mut word = String::new();
    let mut end = 0usize;
    for (idx, ch) in trimmed.char_indices() {
        if ch.is_whitespace() {
            end = idx;
            return finish(word, &trimmed[end..]);
        }
        // Stop at punctuation unless it's '#' or '-'.
        if ch.is_ascii_punctuation() && ch != '#' && ch != '-' {
            end = idx;
            return finish(word, &trimmed[end..]);
        }
        word.push(ch.to_ascii_lowercase());
        end = idx + ch.len_utf8();
    }
    finish(word, &trimmed[end..])
}

#[inline]
fn finish(word: String, rest: &str) -> (String, &str) {
    (word, rest)
}

// A parsed segment of the sub_write input: literal text optionally followed by
// one actor-reference token to resolve for each recipient.
enum Token {
    Char(char, Option<CharId>), // type byte (~ @ ^ *), resolved target
    Obj(Option<ObjId>),         // ` token
}

struct Segment {
    text: String,         // literal text preceding the token
    token: Option<Token>, // None for the final trailing literal
}

// ---------------------------------------------------------------------------
// Object lookup for the `` ` `` token in the non-invis path. C searches, in
// order: room contents, worn equipment, then carried inventory of `ch`.
// ---------------------------------------------------------------------------
fn find_obj_in_scope(g: &GameState, ch: CharId, name: &str) -> Option<ObjId> {
    let rnum = g.get_char(ch)?.in_room?;
    // Room contents.
    let room_contents = g.room(rnum).contents.clone();
    if let Some(o) = g.get_obj_in_list_vis(ch, name, &room_contents) {
        return Some(o);
    }
    // Worn equipment.
    if let Some(chref) = g.get_char(ch) {
        let equip: Vec<ObjId> = chref.equipment.iter().flatten().copied().collect();
        if let Some(o) = g.get_obj_in_list_vis(ch, name, &equip) {
            return Some(o);
        }
        // Carried inventory.
        let carrying = chref.carrying.clone();
        if let Some(o) = g.get_obj_in_list_vis(ch, name, &carrying) {
            return Some(o);
        }
    }
    None
}

/// Whole-world char lookup by keyword (C dg_scripts.c get_char(name), used on
/// the find_invis path). First non-keyword match over the char arena.
fn find_char_anywhere(g: &GameState, name: &str) -> Option<CharId> {
    let want = name.to_lowercase();
    for cid in g.char_ids() {
        if let Some(ch) = g.get_char(cid) {
            if crate::handler::isname(&want, &ch.player.name) {
                return Some(cid);
            }
        }
    }
    None
}

/// Whole-world obj lookup by keyword (C dg_scripts.c get_obj(name)).
fn find_obj_anywhere(g: &GameState, name: &str) -> Option<ObjId> {
    let want = name.to_lowercase();
    for oid in g.obj_ids() {
        if let Some(o) = g.get_obj(oid) {
            if crate::handler::isname(&want, &o.name) {
                return Some(oid);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Phase 1: parse the raw string into segments, resolving every actor-reference
// token ONCE (the resolved CharId/ObjId is then rendered per-recipient). This
// mirrors dg_comm.c sub_write, which resolves otokens[] up front and only the
// final wording varies between recipients.
// ---------------------------------------------------------------------------
fn parse(g: &GameState, arg: &str, ch: CharId, find_invis: bool) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut text = String::new();
    let mut rest = arg;

    while let Some(first) = rest.chars().next() {
        match first {
            '~' | '@' | '^' | '*' => {
                let ty = first;
                let after = &rest[first.len_utf8()..];
                let (name, remainder) = any_one_name(after);
                let target = if find_invis {
                    find_char_anywhere(g, &name)
                } else {
                    g.get_char_room_vis(ch, &name)
                };
                segments.push(Segment {
                    text: std::mem::take(&mut text),
                    token: Some(Token::Char(ty, target)),
                });
                rest = remainder;
            }
            '`' => {
                let after = &rest['`'.len_utf8()..];
                let (name, remainder) = any_one_name(after);
                let obj = if find_invis {
                    find_obj_anywhere(g, &name)
                } else {
                    find_obj_in_scope(g, ch, &name)
                };
                segments.push(Segment {
                    text: std::mem::take(&mut text),
                    token: Some(Token::Obj(obj)),
                });
                rest = remainder;
            }
            '\\' => {
                // Escape: copy the next char literally.
                let after = &rest['\\'.len_utf8()..];
                if let Some(c) = after.chars().next() {
                    text.push(c);
                    rest = &after[c.len_utf8()..];
                } else {
                    rest = after;
                }
            }
            other => {
                text.push(other);
                rest = &rest[other.len_utf8()..];
            }
        }
    }

    // Final trailing literal (no token).
    segments.push(Segment { text, token: None });
    segments
}

/// Phase 2: render all segments for one recipient `to` (CircleMUD
/// sub_write_to_char). Resolves each token token-type byte against `to`'s
/// vantage, CAPs the first character, appends CRLF.
fn render_for(g: &GameState, segments: &[Segment], to: CharId) -> String {
    let mut sb = String::new();
    for seg in segments {
        sb.push_str(&seg.text);
        match &seg.token {
            None => {}
            Some(Token::Char(ty, target)) => {
                let piece = match ty {
                    '~' => match target {
                        None => "someone".to_string(),
                        Some(t) if *t == to => "you".to_string(),
                        Some(t) => pers(g, *t, to),
                    },
                    '@' => match target {
                        None => "someone's".to_string(),
                        Some(t) if *t == to => "your".to_string(),
                        Some(t) => format!("{}'s", pers(g, *t, to)),
                    },
                    '^' => match target {
                        Some(t) if *t == to => "your".to_string(),
                        Some(t) if g.can_see(to, *t) => hshr(g, *t).to_string(),
                        _ => "its".to_string(),
                    },
                    '*' => match target {
                        Some(t) if *t == to => "you".to_string(),
                        Some(t) if g.can_see(to, *t) => hmhr(g, *t).to_string(),
                        _ => "it".to_string(),
                    },
                    _ => String::new(),
                };
                sb.push_str(&piece);
            }
            Some(Token::Obj(obj)) => {
                let piece = match obj {
                    None => "something".to_string(),
                    Some(o) => objs(g, *o),
                };
                sb.push_str(&piece);
            }
        }
    }
    cap_first(&mut sb);
    sb.push_str("\r\n");
    sb
}

/// Uppercase the first character (CircleMUD CAP).
fn cap_first(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            s.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
}

/// True if a character may receive output now (C SENDOK: has a descriptor and
/// is at least awake-ish — dg_comm gates only on SENDOK which is desc + awake).
fn sendok(g: &GameState, cid: CharId) -> bool {
    match g.get_char(cid) {
        Some(c) => c.desc.is_some() && c.position > Position::Sleeping,
        None => false,
    }
}

/// sub_write (dg_comm.c): render `arg` from actor `ch`'s vantage and deliver to
/// the requested targets. `find_invis` toggles visibility-blind name lookup.
///
/// The actor-reference tokens are resolved against `ch`'s room/inventory once;
/// each recipient then sees the line worded from their own perspective.
pub fn sub_write(g: &mut GameState, arg: &str, ch: CharId, find_invis: bool, targets: i32) {
    if arg.is_empty() {
        return;
    }

    // Parse + resolve tokens once (immutable borrow of g).
    let segments = parse(g, arg, ch, find_invis);

    // Collect recipients, render per-recipient (immutable), then send.
    let mut rendered: Vec<(CharId, String)> = Vec::new();

    if targets & TO_CHAR != 0 && sendok(g, ch) {
        rendered.push((ch, render_for(g, &segments, ch)));
    }

    if targets & TO_ROOM != 0 {
        if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
            let people = g.room(rnum).people.clone();
            for to in people {
                if to != ch && sendok(g, to) {
                    rendered.push((to, render_for(g, &segments, to)));
                }
            }
        }
    }

    for (id, text) in rendered {
        g.send_to_char(id, &text);
    }
}

/// send_to_zone (dg_comm.c): raw line to every awake player in the given zone.
/// `zone_rnum` is an index into GameState::zones (C world[room].zone). Used by
/// the VM's `%zoneecho%` analogue.
pub fn send_to_zone(g: &mut GameState, msg: &str, zone_rnum: i32) {
    if msg.is_empty() {
        return;
    }
    let ids: Vec<CharId> = g.char_ids();
    for cid in ids {
        let in_zone = g
            .get_char(cid)
            .and_then(|c| c.in_room)
            .and_then(|rnum| g.room_opt(rnum))
            .map(|r| r.zone == zone_rnum)
            .unwrap_or(false);
        let awake = g
            .get_char(cid)
            .map(|c| c.desc.is_some() && c.position > Position::Sleeping)
            .unwrap_or(false);
        if in_zone && awake {
            g.send_to_char(cid, msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Object location helpers used by the trigger layer (dg_triggers.rs) to find
// the wearer/carrier of an object for the random_otrigger %actor% var.
// ---------------------------------------------------------------------------

/// The character currently holding/wearing an object, if any (C
/// obj->worn_by ? obj->worn_by : obj->carried_by).
pub fn obj_carrier(g: &GameState, oid: ObjId) -> Option<CharId> {
    match g.get_obj(oid)?.loc {
        ObjLoc::Worn(cid, _) => Some(cid),
        ObjLoc::Carried(cid) => Some(cid),
        _ => None,
    }
}
