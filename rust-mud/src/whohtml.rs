// whohtml.rs — the web who-list generator (C comm.c:2545 make_who2html).
//
// C's version wrote a standalone HTML page (immortal tier tables, a mortal
// list with status suffixes, player counts) to a hardcoded /home/mulder/...
// path and moved it into place with system("mv ... &"). The port writes the
// same page natively: configurable output directory (MUD_WWW_WHO_DIR),
// atomic .tmp + rename, driven by the `www_who` config flag from the
// heartbeat autosave block (comm.c:1049) and by the `whoupd`/`rewww`
// immortal commands. C's broken guard (`if (!(www_who) > 0) return;`) is
// repaired here (registered in COMPATIBILITY.md).

use crate::character::Character;
use crate::state::GameState;
use crate::types::*;

/// C comm.c WizLevels[] labels for L101..L105.
const WIZ_LEVELS: [&str; 5] = ["Immortal", "Sage", "Seer", "Prophet", "Implementor"];

/// race_name (C act.informative.c race_name()): the full race name for the
/// who-list line.
pub(crate) fn race_name(g: &GameState, ch: CharId) -> &'static str {
    const RACES: [&str; 14] = [
        "Human",
        "Elf",
        "Gnome",
        "Dwarf",
        "Troll",
        "Goblin",
        "Drow",
        "Orc",
        "Minotaur",
        "Half-Elf",
        "Half-Orc",
        "Half-Giant",
        "Kender",
        "Unknown",
    ];
    g.get_char(ch)
        .map(|c| {
            let idx = c.player.race as usize;
            if idx < 13 { RACES[idx] } else { RACES[13] }
        })
        .unwrap_or("Unknown")
}

/// class_name (C act.informative.c): full class name.
pub(crate) fn class_name(g: &GameState, ch: CharId) -> &'static str {
    const CLASSES: [&str; 5] = ["Magic User", "Cleric", "Thief", "Warrior", "Artisan"];
    g.get_char(ch)
        .map(|c| {
            let idx = c.player.class as usize;
            if idx < 5 { CLASSES[idx] } else { "Unknown" }
        })
        .unwrap_or("Unknown")
}

pub(crate) fn status_suffixes(c: &Character) -> String {
    // C: (mailing) (writing) (away) (deaf) (notell) (quest) (killer) (thief)
    const PLR_MAILING: i64 = 1 << 5;
    const PLR_WRITING: i64 = 1 << 4;
    let mut s = String::new();
    if c.act_flags & PLR_MAILING != 0 {
        s.push_str(" (mailing)");
    } else if c.act_flags & PLR_WRITING != 0 {
        s.push_str(" (writing)");
    }
    if c.act_flags & (1 << 22) != 0 {
        s.push_str(" (away)");
    }
    s
}

/// Build the whole HTML page and write it atomically. Returns Err with a
/// description when the output directory is unwritable.
pub fn make_who2html(g: &mut GameState) -> Result<(), String> {
    // Collect visible, playing characters.
    let mut morts: Vec<(u8, String)> = Vec::new();
    let mut imms: Vec<(u8, String)> = Vec::new();
    let ids: Vec<CharId> = g.players_by_name.values().copied().collect();
    for cid in ids {
        let Some(c) = g.get_char(cid) else { continue };
        if c.is_npc {
            continue;
        }
        // C skips invisible players (invis level or AFF_INVISIBLE).
        if c.invis_level > 0 || c.affect_flags & crate::flags::AFF_INVISIBLE != 0 {
            continue;
        }
        let line = format!(
            "[{} {} {}] {}{}",
            c.player.level,
            race_name(g, cid),
            class_name(g, cid),
            c.get_name(),
            status_suffixes(c)
        );
        if c.player.level >= LVL_IMMORT {
            imms.push((c.player.level, line));
        } else {
            morts.push((c.player.level, line));
        }
    }
    imms.sort_by(|a, b| b.0.cmp(&a.0));
    morts.sort_by(|a, b| b.0.cmp(&a.0));

    let mut html = String::with_capacity(8192);
    html.push_str("<html><head>\n");
    html.push_str("<meta name=\"DESCRIPTION\" content=\"The official website of DeltaMUD, an online medieval roleplaying game.\">\n");
    html.push_str("<title>Online Who List</title>\n");
    html.push_str("</head><font face=\"Arial\">\n");
    html.push_str("<body bgcolor=\"#FFFFFF\" text=\"#000000\">\n");
    html.push_str("<center><H3><strong>Online Who List</strong></H3></center>\n");
    html.push_str("<CENTER><TABLE BORDER=\"0\" bgcolor=\"#000000\" width=\"90%\"><TD><PRE>");
    html.push_str("<font color=\"#C0C0C0\" FACE=\"fixedsys\">Immortals Currently Online<BR>\n");
    for (_, line) in &imms {
        let lvl = line["[".len()..].split(' ').next().unwrap_or("0");
        let _ = lvl;
        html.push_str(&format!(
            "<font color=\"#FFFF00\" FACE=\"fixedsys\">{}<BR>\n",
            html_escape(line)
        ));
    }
    if imms.is_empty() {
        html.push_str("<font color=\"#C0C0C0\" FACE=\"fixedsys\">None at all!<BR>\n");
    }
    html.push_str("<font color=\"#C0C0C0\" FACE=\"fixedsys\"><BR>Mortals Currently Online<BR>\n");
    for (_, line) in &morts {
        html.push_str(&format!(
            "<font color=\"#0000FF\">[<font color=\"#C0C0C0\">{}<BR>\n",
            html_escape(line)
        ));
    }
    if morts.is_empty() {
        html.push_str("<font color=\"#C0C0C0\" FACE=\"fixedsys\">None at all!<BR>\n");
    }
    let total = imms.len() + morts.len();
    html.push_str(&format!(
        "</PRE></TD></TABLE></CENTER>\n<CENTER><B>There {} {} player{} currently online.</B></CENTER>\n",
        if total == 1 { "is" } else { "are" },
        total,
        if total == 1 { "" } else { "s" }
    ));
    html.push_str("<CENTER><small>Auto-updated every 5 minutes.</small></CENTER>\n");
    html.push_str("</body></html>\n");

    // Atomic write: who.tmp then rename (C shelled out to mv; we rename).
    let dir = g.config.www_who_dir.clone();
    let tmp = format!("{}/who.tmp", dir);
    let dst = format!("{}/who.html", dir);
    if std::fs::write(&tmp, html.as_bytes()).is_err() {
        return Err(format!("could not create {} for who2html", tmp));
    }
    std::fs::rename(&tmp, &dst).map_err(|e| format!("rename failed: {}", e))?;
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
