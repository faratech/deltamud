// autowiz.rs — native Rust port of `src/util/autowiz.c` (the self-updating
// wizlist/immlist generator).
//
// In C, `check_autowiz()` (limits.c) shells out to a separate `../bin/autowiz`
// binary via `system()`. That helper connects to MySQL, reads every row of
// `player_main`, selects the immortals (>= LVL_HERO) that are not frozen / not
// NOWIZLIST / not deleted, groups them into named level tiers, formats two
// centered text blocks, and writes `lib/text/wizlist` + `lib/text/immlist`.
// db.c then reloads those files into the `wizlist`/`immlist` globals which the
// `wizlist`/`immlist` commands (do_gen_ps) page back to the player.
//
// This module reproduces that end to end with no subprocess: it enumerates the
// immortal roster, builds the tier list, and writes the two files byte-faithful
// to `write_wizlist()`. do_gen_ps reads the files back so the on-disk artefact
// is identical to what the C autowiz binary would have produced.
//
// ENUMERATION SOURCE: the C helper reads the full `player_main` roster directly
// from MySQL. Rust builds `GameState.player_table` from the same rows at boot
// and refreshes it on online/offline saves, so autowiz can enumerate offline
// immortals without an async DB handle inside command dispatch.

use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Constants from autowiz.c / db.h / structs.h.
// ---------------------------------------------------------------------------

/// db.h: WIZLIST_FILE / IMMLIST_FILE, relative to the lib root.
const WIZLIST_FILE_REL: &str = "text/wizlist";
const IMMLIST_FILE_REL: &str = "text/immlist";

/// autowiz.c: #define LINE_LEN 65.
const LINE_LEN: usize = 65;

/// structs.h level constants used by the tier table.
const LVL_HERO: u8 = 100;
const LVL_DEMIGOD: u8 = 102;

/// PLR_* act-flag bits (structs.h) the C roster filter rejects. These mirror
/// the C-correct values used in cmd_wizard.rs (PLR_FROZEN is 1<<2, etc.).
const PLR_FROZEN: i64 = 1 << 2;
const PLR_DELETED: i64 = 1 << 10;
const PLR_NOWIZLIST: i64 = 1 << 12;

/// config.c: `int min_wizlist_lev = LVL_IMMORT;` — immortal levels below this
/// go on the immlist instead of the wizlist. check_autowiz() passes it as the
/// wizlist minimum level (the C `system()` argv[1]).
const MIN_WIZLIST_LEV: u8 = LVL_IMMORT;

/// autowiz.c `struct control_rec level_params[]`. Each tier's display name
/// carries the literal `&Y` color prefix exactly as in C (it is part of the
/// string whose length drives the centering math). The array is ordered by
/// ASCENDING level here; the renderer walks it in DESCENDING order to match
/// the reversed linked list C builds in initialize().
const LEVEL_PARAMS: &[(u8, &str)] = &[
    (LVL_HERO, "&YHeros"),
    (LVL_IMMORT, "&YImmortals"),
    (LVL_DEMIGOD, "&YSages"),
    (LVL_GOD, "&YSeers"),
    (LVL_GRGOD, "&YProphets"),
    (LVL_IMPL, "&YImplementors"),
];

// ---------------------------------------------------------------------------
// Roster enumeration + tier grouping.
// ---------------------------------------------------------------------------

/// Collect the (level, name) of every indexed player row that the C autowiz
/// roster query would have kept: level >= MIN_LEVEL (LVL_HERO), not
/// frozen, not NOWIZLIST, not deleted, and an all-alphabetic name (autowiz.c
/// add_name() rejects any name containing a non-alpha character).
fn collect_immortals(g: &GameState) -> Vec<(u8, String)> {
    let mut out: Vec<(u8, String)> = Vec::new();
    for p in &g.player_table {
        let level = p.level;
        if level < LVL_HERO {
            continue;
        }
        let act = p.act_flags;
        if act & PLR_FROZEN != 0 || act & PLR_NOWIZLIST != 0 || act & PLR_DELETED != 0 {
            continue;
        }
        let name = p.name.clone();
        if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphabetic()) {
            continue;
        }
        out.push((level, name));
    }
    out
}

/// Build the per-tier sorted name lists, mirroring autowiz.c add_name() +
/// sort_names(). A player lands in the highest tier whose threshold level is
/// `<= the player level` (the C walk over the descending list). Returns one
/// `Vec<String>` per entry of LEVEL_PARAMS (same index order), each sorted
/// ascending case-sensitively (C strcmp).
fn group_by_tier(roster: &[(u8, String)]) -> Vec<Vec<String>> {
    let mut tiers: Vec<Vec<String>> = vec![Vec::new(); LEVEL_PARAMS.len()];
    for (level, name) in roster {
        // C: walk the descending list; settle on the first tier whose level is
        // <= the player's level. We replicate by scanning LEVEL_PARAMS (ascending
        // by level) and picking the highest threshold that is still <= level.
        let mut chosen: Option<usize> = None;
        for (i, (tier_level, _)) in LEVEL_PARAMS.iter().enumerate() {
            if *tier_level <= *level {
                chosen = Some(i);
            }
        }
        if let Some(i) = chosen {
            tiers[i].push(name.clone());
        }
    }
    for t in &mut tiers {
        t.sort(); // C strcmp ordering (ASCII, case-sensitive).
    }
    tiers
}

// ---------------------------------------------------------------------------
// write_wizlist() — byte-faithful port of autowiz.c.
// ---------------------------------------------------------------------------

/// Render the list text for tiers whose level is within [minlev, maxlev],
/// reproducing autowiz.c write_wizlist() character-for-character (header
/// banner, centered tier title, alternating &y/&Y underline, centered name
/// columns). `tiers` is indexed parallel to LEVEL_PARAMS.
fn render(tiers: &[Vec<String>], minlev: u8, maxlev: u8) -> String {
    let mut out = String::new();

    // Banner: the "run DeltaMUD" wording for the wizlist (minlev != LVL_HERO),
    // the "achieved hero status" wording for the immlist (minlev == LVL_HERO).
    if minlev != LVL_HERO {
        out.push_str(
            "\n  &c***************************************************************\n\
             \x20 &c* &nThe following people run DeltaMUD. They are to be treated   &c*\n\
             \x20 &c* &nwith great respect. If you need technical help, these are   &c*\n\
             \x20 &c* &nthe people to contact.                                      &c*\n\
             \x20 &c***************************************************************\n\n",
        );
    } else {
        out.push_str(
            "\n  &c***************************************************************\n\
             \x20 &c* &nThe following people have achieved hero status on DeltaMUD. &c*\n\
             \x20 &c* &nThey've attained this level through hard work and hours of  &c*\n\
             \x20 &c* &ngameplay and should be respected. Look to them for advice.  &c*\n\
             \x20 &c***************************************************************\n\n",
        );
    }

    // Walk the tiers in DESCENDING level order (the reversed list C iterates).
    for idx in (0..LEVEL_PARAMS.len()).rev() {
        let (tier_level, level_name) = LEVEL_PARAMS[idx];
        if tier_level < minlev || tier_level > maxlev {
            continue;
        }

        // Centered tier title: i = (LINE_LEN - strlen(name) + 2) / 2 spaces.
        let name_len = level_name.len();
        let indent = (LINE_LEN - name_len + 2) / 2;
        for _ in 0..indent {
            out.push(' ');
        }
        out.push_str(level_name);
        out.push('\n');

        // Underline, same indent, then `strlen(name)-2` chars each emitting
        // `&` + (y if odd / Y if even) + `-`, finally `&n`.
        for _ in 0..indent {
            out.push(' ');
        }
        let dashes = name_len - 2;
        for j in 1..=dashes {
            out.push('&');
            if j % 2 == 1 {
                out.push('y');
            } else {
                out.push('Y');
            }
            out.push('-');
        }
        out.push_str("&n\n");

        // Names: accumulate into `buf`; flush centered when length exceeds
        // LINE_LEN. Between names (when a next one follows) append IMM_LMARG
        // (a single space). COL_LEVEL is 0 here so the column branch never
        // fires for these immortal tiers.
        let names = &tiers[idx];
        let mut buf = String::new();
        for (n, nm) in names.iter().enumerate() {
            buf.push_str(nm);
            if buf.len() > LINE_LEN {
                let i = (LINE_LEN.saturating_sub(buf.len())) / 2;
                for _ in 0..i {
                    out.push(' ');
                }
                out.push_str(&buf);
                out.push('\n');
                buf.clear();
            } else if n + 1 < names.len() {
                buf.push(' '); // IMM_LMARG
            }
        }
        if !buf.is_empty() {
            let i = (LINE_LEN.saturating_sub(buf.len())) / 2;
            for _ in 0..i {
                out.push(' ');
            }
            out.push_str(&buf);
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

/// Build absolute path under the lib root for a relative text file.
fn lib_file(lib_path: &str, rel: &str) -> String {
    format!("{}/{}", lib_path.trim_end_matches('/'), rel)
}

/// Regenerate `text/wizlist` and `text/immlist` from the current immortal
/// roster (the native equivalent of running the `autowiz` binary). Returns
/// false if either file could not be written.
pub fn run_autowiz(g: &GameState) -> bool {
    let roster = collect_immortals(g);
    let tiers = group_by_tier(&roster);

    // autowiz.c main():
    //   write_wizlist(wizfile, wizlevel=min_wizlist_lev, LVL_IMPL)
    //   write_wizlist(immfile, immlevel=LVL_HERO,        LVL_HERO)
    let wizlist = render(&tiers, MIN_WIZLIST_LEV, LVL_IMPL);
    let immlist = render(&tiers, LVL_HERO, LVL_HERO);

    let lib_path = g.config.lib_path.clone();
    let wiz_ok = std::fs::write(lib_file(&lib_path, WIZLIST_FILE_REL), wizlist).is_ok();
    let imm_ok = std::fs::write(lib_file(&lib_path, IMMLIST_FILE_REL), immlist).is_ok();
    wiz_ok && imm_ok
}

/// check_autowiz(ch) — limits.c. C only regenerates when use_autowiz is set and
/// the actor is >= LVL_HERO. use_autowiz is YES in config.c, so the gate here is
/// the level check on the triggering character. Regenerates both list files.
pub fn check_autowiz(g: &mut GameState, ch: CharId) {
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    if level < LVL_HERO {
        return;
    }
    run_autowiz(g);
}

/// Read a generated list file back as text (do_gen_ps wizlist/immlist path).
/// Returns the file contents, or None if the file is missing/unreadable.
pub fn read_wizlist(lib_path: &str) -> Option<String> {
    std::fs::read_to_string(lib_file(lib_path, WIZLIST_FILE_REL)).ok()
}

pub fn read_immlist(lib_path: &str) -> Option<String> {
    std::fs::read_to_string(lib_file(lib_path, IMMLIST_FILE_REL)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::{GameState, PlayerIndex};
    use crate::types::Class;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_lib(name: &str) -> String {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("deltamud-autowiz-{}-{}", name, stamp));
        std::fs::create_dir_all(path.join("text")).unwrap();
        path.to_string_lossy().to_string()
    }

    fn player_index(name: &str, level: u8, act_flags: i64) -> PlayerIndex {
        PlayerIndex {
            idnum: level as i64,
            name: name.to_string(),
            level,
            trust: i32::from(level),
            class: Class::Warrior,
            last_logon: 0,
            host: String::new(),
            act_flags,
            clan: -1,
            clan_rank: -1,
        }
    }

    #[test]
    fn autowiz_uses_indexed_offline_player_rows() {
        let lib = temp_lib("offline");
        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        g.player_table
            .push(player_index("OfflineImm", LVL_IMMORT, 0));
        g.player_table
            .push(player_index("HiddenImm", LVL_IMMORT, PLR_NOWIZLIST));
        g.player_table.push(player_index("Mortal", 10, 0));

        assert!(run_autowiz(&g));

        let wizlist = std::fs::read_to_string(format!("{}/text/wizlist", lib)).unwrap();
        assert!(wizlist.contains("OfflineImm"));
        assert!(!wizlist.contains("HiddenImm"));
        assert!(!wizlist.contains("Mortal"));
    }
}
