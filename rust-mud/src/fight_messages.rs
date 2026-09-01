// Combat hit-messages (fight.c load_messages / skill_message + lib/misc/messages).
//
// Each attack type — a skill/spell number, or a weapon type (TYPE_HIT + i) — can
// register several flavourful message sets; one is chosen at random and act()ed
// to the attacker, victim, and onlookers on a death blow, a normal hit, a miss,
// or a hit on a god. This is the 1:1 port of CircleMUD/DeltaMUD's fight_messages
// table. Without it, offensive skills (backstab, kick, bash, ...) and offensive
// spells fall back to the generic dam_message weapon verbs.

use crate::act::{ActArg, To, act};
use crate::state::GameState;
use crate::types::*;
use std::sync::OnceLock;

/// One directed message triple (to attacker / victim / onlookers). A field is
/// None when the messages file had `#` for that line (act() then prints nothing).
#[derive(Clone)]
struct MsgType {
    attacker: Option<String>,
    victim: Option<String>,
    room: Option<String>,
}

/// The four situations a single record covers (structs.h message_type).
#[derive(Clone)]
struct MessageSet {
    die: MsgType,
    miss: MsgType,
    hit: MsgType,
    god: MsgType,
}

/// All message sets registered for one attack type (structs.h message_list);
/// `messages.len()` is C's `number_of_attacks`.
struct MessageList {
    a_type: i32,
    messages: Vec<MessageSet>,
}

static FIGHT_MESSAGES: OnceLock<Vec<MessageList>> = OnceLock::new();

/// load_messages (fight.c): parse `<lib>/misc/messages` into the fight-message
/// table. Unlike C (which exit(1)s on a missing/short file) this logs and
/// disables skill messages so the server still boots without the data file.
pub fn load_messages(lib_path: &str) {
    let path = format!("{}/misc/messages", lib_path);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "combat message file {} not read: {} -- skill messages disabled",
                path,
                e
            );
            let _ = FIGHT_MESSAGES.set(Vec::new());
            return;
        }
    };

    let mut table: Vec<MessageList> = Vec::new();
    let mut lines = content.lines();

    // Skip leading comment ('*') / blank lines, then walk record by record. C
    // keeps a one-line lookahead in `chk`; `cur` mirrors it.
    let mut cur = lines.next();
    while matches!(cur, Some(l) if l.is_empty() || l.starts_with('*')) {
        cur = lines.next();
    }

    while matches!(cur, Some(l) if l.starts_with('M')) {
        // The next line is the skill / attack-type name (leading space trimmed).
        let name = lines.next().unwrap_or("").trim().to_string();
        let a_type = resolve_atype(&name);

        // Twelve message lines: die / miss / hit / god, each (attacker, victim,
        // room). fread_action: a line beginning with '#' means "no message".
        let mut f: Vec<Option<String>> = Vec::with_capacity(12);
        for _ in 0..12 {
            f.push(match lines.next() {
                Some(s) if s.starts_with('#') => None,
                Some(s) => Some(s.to_string()),
                None => None,
            });
        }

        if let Some(at) = a_type {
            let set = MessageSet {
                die: MsgType {
                    attacker: f[0].take(),
                    victim: f[1].take(),
                    room: f[2].take(),
                },
                miss: MsgType {
                    attacker: f[3].take(),
                    victim: f[4].take(),
                    room: f[5].take(),
                },
                hit: MsgType {
                    attacker: f[6].take(),
                    victim: f[7].take(),
                    room: f[8].take(),
                },
                god: MsgType {
                    attacker: f[9].take(),
                    victim: f[10].take(),
                    room: f[11].take(),
                },
            };
            if let Some(ml) = table.iter_mut().find(|m| m.a_type == at) {
                ml.messages.push(set);
            } else {
                table.push(MessageList {
                    a_type: at,
                    messages: vec![set],
                });
            }
        }

        // Advance one line past the record, then skip comments/blanks (matches
        // C's trailing fgets + comment-skip; for an unknown record this is the
        // 13th read).
        cur = lines.next();
        while matches!(cur, Some(l) if l.is_empty() || l.starts_with('*')) {
            cur = lines.next();
        }
    }

    let types = table.len();
    let total: usize = table.iter().map(|m| m.messages.len()).sum();
    let _ = FIGHT_MESSAGES.set(table);
    log::info!(
        "   {} attack-message type(s), {} message(s) loaded.",
        types,
        total
    );
}

/// Resolve a record's name to its attack-type number. A weapon type matches an
/// abbreviation of `attack_hit_text[i].singular` -> TYPE_HIT + i (C: i + 1100),
/// checked first; otherwise it is a skill/spell name via find_skill_num.
fn resolve_atype(name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    for (i, (singular, _plural)) in crate::constants::ATTACK_HIT_TEXT.iter().enumerate() {
        if crate::interpreter::is_abbrev(name, singular) {
            return Some(i as i32 + crate::combat::TYPE_HIT);
        }
    }
    match crate::spell_parser::find_skill_num(name) {
        -1 => None,
        n => Some(n),
    }
}

/// skill_message (fight.c 702-767): act() a random registered message set for
/// `attacktype` to attacker / victim / room, branching on god-victim / death /
/// hit / miss. Returns true iff a message existed, so the weapon path can fall
/// back to dam_message when it doesn't.
pub fn skill_message(
    g: &mut GameState,
    dam: i32,
    ch: CharId,
    vict: CharId,
    attacktype: i32,
) -> bool {
    let nmsgs = match FIGHT_MESSAGES
        .get()
        .and_then(|t| t.iter().find(|m| m.a_type == attacktype))
    {
        Some(ml) if !ml.messages.is_empty() => ml.messages.len(),
        _ => return false,
    };
    // dice(1, number_of_attacks) selects one set (1-based). Clone it out so we
    // hold no borrow of the table across the act() calls.
    let nr = g.rng.dice(1, nmsgs as i32);
    let set = {
        let ml = FIGHT_MESSAGES
            .get()
            .unwrap()
            .iter()
            .find(|m| m.a_type == attacktype)
            .unwrap();
        ml.messages[(nr - 1).clamp(0, nmsgs as i32 - 1) as usize].clone()
    };

    let weap = g.get_char(ch).and_then(|c| c.equipment[WEAR_WIELD]);
    let vict_god = g
        .get_char(vict)
        .map(|c| !c.is_npc && c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    let vict_dead = g
        .get_char(vict)
        .map(|c| c.position == Position::Dead)
        .unwrap_or(false);

    if vict_god {
        // Hit on a god: plain (uncoloured) messages; victim not forced-awake.
        emit(g, &set.god.attacker, "", ch, weap, vict, To::Char, false);
        emit(g, &set.god.victim, "", ch, weap, vict, To::Vict, false);
        emit(g, &set.god.room, "", ch, weap, vict, To::NotVict, false);
    } else if dam != 0 {
        let m = if vict_dead { &set.die } else { &set.hit };
        emit(g, &m.attacker, "&Y", ch, weap, vict, To::Char, false);
        emit(g, &m.victim, "&R", ch, weap, vict, To::Vict, true);
        emit(g, &m.room, "", ch, weap, vict, To::NotVict, false);
    } else if ch != vict {
        emit(g, &set.miss.attacker, "&Y", ch, weap, vict, To::Char, false);
        emit(g, &set.miss.victim, "&R", ch, weap, vict, To::Vict, true);
        emit(g, &set.miss.room, "", ch, weap, vict, To::NotVict, false);
    }
    true
}

/// act() one message field (None/empty is skipped, matching C act() returning on
/// a NULL string). The attacker line is wrapped yellow and the victim line red
/// (C CCYEL/CCRED + CCNRM); the room line and god messages are uncoloured.
/// `sleep` adds TO_SLEEP so a dying/sleeping victim still sees their own line.
#[allow(clippy::too_many_arguments)]
fn emit(
    g: &mut GameState,
    field: &Option<String>,
    color: &str,
    ch: CharId,
    weap: Option<ObjId>,
    vict: CharId,
    to: To,
    sleep: bool,
) {
    let body = match field {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };
    let text = if color.is_empty() {
        body.clone()
    } else {
        format!("{}{}&n", color, body)
    };
    if sleep {
        crate::act::act_sleep(g, &text, false, ch, weap, ActArg::Char(vict), to, true);
    } else {
        act(g, &text, false, ch, weap, ActArg::Char(vict), to);
    }
}
