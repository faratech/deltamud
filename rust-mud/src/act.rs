// act() — the central message engine (CircleMUD comm.c perform_act/act),
// ported faithfully: per-recipient $-code substitution with visibility
// gating, CAP of the first character, and a trailing CRLF. Every combat,
// movement, communication, and spell message renders through this.

use crate::state::GameState;
use crate::types::*;

/// What `vict_obj` carries for a given act() call ($N / $O$P / $T).
#[derive(Clone)]
pub enum ActArg {
    None,
    Char(CharId),
    Obj(ObjId),
    Str(String),
}

/// Delivery target (CircleMUD TO_CHAR/TO_VICT/TO_ROOM/TO_NOTVICT).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum To {
    Char,
    Vict,
    Room,
    NotVict,
}

fn sex_of(g: &GameState, cid: CharId) -> Gender {
    g.get_char(cid)
        .map(|c| c.player.sex)
        .unwrap_or(Gender::Neutral)
}

fn hssh(g: &GameState, cid: CharId) -> &'static str {
    match sex_of(g, cid) {
        Gender::Male => "he",
        Gender::Female => "she",
        Gender::Neutral => "it",
    }
}
pub(crate) fn hshr(g: &GameState, cid: CharId) -> &'static str {
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

/// PERS(ch, vict): visible name, else "A Mystical Being"/"someone".
pub(crate) fn pers(g: &GameState, viewer: CharId, cid: CharId) -> String {
    if g.can_see(viewer, cid) {
        if let Some(ch) = g.get_char(cid) {
            return if ch.is_npc {
                ch.short_desc
                    .clone()
                    .unwrap_or_else(|| ch.player.name.clone())
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

/// First keyword of a namelist (CircleMUD fname).
fn fname(namelist: &str) -> String {
    namelist.split_whitespace().next().unwrap_or("").to_string()
}

fn objn(g: &GameState, _viewer: CharId, oid: ObjId) -> String {
    match g.get_obj(oid) {
        Some(o) => fname(&o.name),
        None => "something".to_string(),
    }
}
fn objs(g: &GameState, _viewer: CharId, oid: ObjId) -> String {
    match g.get_obj(oid) {
        Some(o) => o.short_description.clone(),
        None => "something".to_string(),
    }
}
fn sana(g: &GameState, oid: ObjId) -> String {
    let short = g
        .get_obj(oid)
        .map(|o| o.short_description.clone())
        .unwrap_or_default();
    let first = short
        .trim_start()
        .chars()
        .next()
        .unwrap_or('x')
        .to_ascii_lowercase();
    if "aeiou".contains(first) {
        "an".to_string()
    } else {
        "a".to_string()
    }
}

/// Render the message for one viewer (CircleMUD perform_act + CAP + CRLF).
fn perform_act(
    g: &GameState,
    orig: &str,
    ch: CharId,
    obj: Option<ObjId>,
    vict_obj: &ActArg,
    viewer: CharId,
) -> String {
    let mut out = String::with_capacity(orig.len() + 16);
    let mut chars = orig.chars().peekable();
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
            'N' => match vict_obj {
                ActArg::Char(v) => pers(g, viewer, *v),
                _ => String::new(),
            },
            'm' => hmhr(g, ch).to_string(),
            'M' => match vict_obj {
                ActArg::Char(v) => hmhr(g, *v).to_string(),
                _ => String::new(),
            },
            's' => hshr(g, ch).to_string(),
            'S' => match vict_obj {
                ActArg::Char(v) => hshr(g, *v).to_string(),
                _ => String::new(),
            },
            'e' => hssh(g, ch).to_string(),
            'E' => match vict_obj {
                ActArg::Char(v) => hssh(g, *v).to_string(),
                _ => String::new(),
            },
            'o' => match obj {
                Some(o) => objn(g, viewer, o),
                None => String::new(),
            },
            'O' => match vict_obj {
                ActArg::Obj(o) => objn(g, viewer, *o),
                _ => String::new(),
            },
            'p' => match obj {
                Some(o) => objs(g, viewer, o),
                None => String::new(),
            },
            'P' => match vict_obj {
                ActArg::Obj(o) => objs(g, viewer, *o),
                _ => String::new(),
            },
            'a' => match obj {
                Some(o) => sana(g, o),
                None => String::new(),
            },
            'A' => match vict_obj {
                ActArg::Obj(o) => sana(g, *o),
                _ => String::new(),
            },
            'T' => match vict_obj {
                ActArg::Str(s) => s.clone(),
                _ => String::new(),
            },
            'F' => match vict_obj {
                ActArg::Str(s) => fname(s),
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

/// Uppercase the first character (CircleMUD CAP).
fn cap_first(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            s.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
}

/// True if a character may receive act output now (has a connection & awake).
fn sendok(g: &GameState, cid: CharId, to_sleep: bool) -> bool {
    match g.get_char(cid) {
        Some(c) => c.desc.is_some() && (to_sleep || c.position > Position::Sleeping),
        None => false,
    }
}

/// Deliver a message. `obj` is the primary object ($o/$p/$a); `vict_obj`
/// is the secondary target ($N…/$O$P/$T). Matches C act() targeting.
pub fn act(
    g: &mut GameState,
    msg: &str,
    hide_invisible: bool,
    ch: CharId,
    obj: Option<ObjId>,
    vict_obj: ActArg,
    to: To,
) {
    act_sleep(g, msg, hide_invisible, ch, obj, vict_obj, to, false)
}

#[allow(clippy::too_many_arguments)]
pub fn act_sleep(
    g: &mut GameState,
    msg: &str,
    hide_invisible: bool,
    ch: CharId,
    obj: Option<ObjId>,
    vict_obj: ActArg,
    to: To,
    to_sleep: bool,
) {
    if msg.is_empty() {
        return;
    }

    // Determine recipients, then render (immutable), then send.
    let mut rendered: Vec<(CharId, String)> = Vec::new();

    match to {
        To::Char => {
            if sendok(g, ch, to_sleep) {
                rendered.push((ch, perform_act(g, msg, ch, obj, &vict_obj, ch)));
            }
        }
        To::Vict => {
            if let ActArg::Char(v) = vict_obj {
                if sendok(g, v, to_sleep) {
                    rendered.push((v, perform_act(g, msg, ch, obj, &vict_obj, v)));
                }
            }
        }
        To::Room | To::NotVict => {
            let rnum = g.get_char(ch).and_then(|c| c.in_room);
            let vict = match vict_obj {
                ActArg::Char(v) => Some(v),
                _ => None,
            };
            if let Some(rnum) = rnum {
                let people = g.room(rnum).people.clone();
                for to_id in people {
                    if to_id == ch {
                        continue;
                    }
                    if to == To::NotVict && Some(to_id) == vict {
                        continue;
                    }
                    if !sendok(g, to_id, to_sleep) {
                        continue;
                    }
                    if hide_invisible && !g.can_see(to_id, ch) {
                        continue;
                    }
                    rendered.push((to_id, perform_act(g, msg, ch, obj, &vict_obj, to_id)));
                    // C comm.c:2416/2436: every NPC recipient (other than the
                    // actor) gets act_mtrigger on the raw message, unless the
                    // act originated inside the DG VM (dg_act_check; #138).
                    if g.dg_act_check && g.get_char(to_id).map(|t| t.is_npc).unwrap_or(false) {
                        let vict_char = match vict_obj {
                            ActArg::Char(v) => Some(v),
                            _ => None,
                        };
                        crate::dg_triggers::act_mtrigger(
                            g,
                            to_id,
                            msg,
                            Some(ch),
                            vict_char,
                            obj,
                            None,
                            None,
                        );
                    }
                }
            }
            // C comm.c:2517-2538: arena OBSERVE_BY chains - every room message
            // also reaches the observers of the actor and of the victim,
            // rendered from the perspective of the combatant they watch
            // (#248). Otherwise arena spectators see no combat text at all.
            let observer_line_for = |g: &mut GameState, viewer: CharId| -> String {
                perform_act(g, msg, ch, obj, &vict_obj, viewer)
            };
            if crate::arena::arena_stat(&g, ch) != crate::arena::ARENA_NOT {
                crate::arena::send_to_observers_rendered(
                    g,
                    ch,
                    &mut |g: &mut GameState, viewer: CharId| observer_line_for(g, viewer),
                );
            }
            if let Some(v) = vict {
                if crate::arena::arena_stat(&g, v) != crate::arena::ARENA_NOT {
                    crate::arena::send_to_observers_rendered(
                        g,
                        v,
                        &mut |g: &mut GameState, viewer: CharId| observer_line_for(g, viewer),
                    );
                }
            }
        }
    }

    for (id, text) in rendered {
        g.send_to_char(id, &text);
    }
}
