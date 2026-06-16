// hedit.rs — the OasisOLC help editor (C hedit.c), ported to the id-indexed
// GameState and the shared olc.rs framework contract.
//
// What this does, mirroring hedit.c:
//   * do_hedit(g, ch, arg, subcmd)  — the `hedit <keyword>` / `hedit save`
//     command. Snapshots the matching help entry (or a fresh one) into the
//     per-connection edit state, registers EditorKind::Hedit with olc, and
//     displays the main menu.
//   * hedit_parse(g, conn, line)    — the per-line input router that olc's
//     master router calls for every line while this editor is active.
//
// ON-DISK FORMAT (byte-faithful inverse of db.c load_help, which the Rust port
// does not otherwise load):
//   For each entry, in table order:
//       <keywords>\n
//       <entry body, with every '\r' stripped — each source line is followed
//        by a single '\n'>
//       #<min_level>\n
//   The file terminates with:
//       $~\n
//   load_help reads the keyword line, then accumulates body lines (appending
//   "\r\n" to each) until a line that starts with '#', whose tail is the
//   min_level. Our writer reproduces that exactly: strip_string() drops the
//   '\r' so the on-disk body uses bare '\n', then "#<level>".
//
// OWNERSHIP NOTE: there is no shared in-memory help table in the Rust port
// (cmd_informative::do_help has no index). So this module owns the canonical
// help table in a module-static, lazily loaded from disk on first edit, and is
// the writer of record. `hedit save` and per-entry "save internally + flush"
// both persist the whole table back to text/help/help.hlp.

use crate::olc::{self, EditorKind};
use crate::state::GameState;
use crate::types::*;
use std::sync::{Mutex, OnceLock};

// olc.h limits.
const MAX_HELP_KEYWORDS: usize = 75;
const MAX_HELP_ENTRY: usize = 2048;

// CircleMUD db.h: HLP_PREFIX "world/.."? No — text/help, HELP_FILE help.hlp.
const HLP_REL_DIR: &str = "text/help";
const HELP_FILE: &str = "help.hlp";

// ---------------------------------------------------------------------------
// help_index_element (db.h) — only the fields hedit touches.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct HelpEntry {
    keywords: String,
    /// Entry body as held in memory: every line terminated by "\r\n", exactly
    /// like load_help builds it (so the menu renders identically and the save
    /// path strips the '\r' back out).
    entry: String,
    min_level: i32,
}

/// The canonical help table, lazily loaded from disk on first use. Mirrors the
/// C global `help_table` (+ top_of_helpt as the Vec length).
static HELP_TABLE: OnceLock<Mutex<Vec<HelpEntry>>> = OnceLock::new();

fn help_table() -> &'static Mutex<Vec<HelpEntry>> {
    HELP_TABLE.get_or_init(|| Mutex::new(Vec::new()))
}

/// True once the table has been populated from disk this run.
static HELP_LOADED: OnceLock<Mutex<bool>> = OnceLock::new();

fn ensure_loaded(lib_path: &str) {
    let flag = HELP_LOADED.get_or_init(|| Mutex::new(false));
    let mut loaded = match flag.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if *loaded {
        return;
    }
    let table = load_help_file(lib_path);
    if let Ok(mut guard) = help_table().lock() {
        *guard = table;
    }
    *loaded = true;
}

/// Read text/help/help.hlp into the in-memory table (inverse of save). Mirrors
/// db.c load_help: keyword line, body lines until a '#', min_level = tail.
fn load_help_file(lib_path: &str) -> Vec<HelpEntry> {
    let path = format!("{}/{}/{}", lib_path.trim_end_matches('/'), HLP_REL_DIR, HELP_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<HelpEntry> = Vec::new();
    // get_one_line(): strips the trailing newline; we iterate over .lines()
    // which already drops '\n' (and we additionally drop a trailing '\r').
    let mut lines = raw.lines().map(|l| l.trim_end_matches('\r'));

    // First keyword line.
    let mut key = match lines.next() {
        Some(k) => k,
        None => return out,
    };
    while !key.starts_with('$') {
        let mut entry = String::new();
        let mut line = lines.next().unwrap_or("#");
        while !line.starts_with('#') {
            entry.push_str(line);
            entry.push_str("\r\n");
            line = lines.next().unwrap_or("#");
        }
        // line starts with '#': min_level = atoi(line+1).
        let mut min_level = 0i32;
        if line.len() > 1 {
            min_level = line[1..].trim().parse::<i32>().unwrap_or(0);
        }
        min_level = min_level.clamp(0, LVL_IMPL as i32);

        out.push(HelpEntry {
            keywords: key.to_string(),
            entry,
            min_level,
        });

        key = match lines.next() {
            Some(k) => k,
            None => break,
        };
    }
    out
}

// ---------------------------------------------------------------------------
// Per-connection editor state (Character/Descriptor cannot carry OLC_DATA).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct HeditState {
    /// The character doing the editing (for log/permission/output).
    ch: CharId,
    /// The scratch help entry being edited (C OLC_HELP).
    help: HelpEntry,
    /// Real index into HELP_TABLE of the entry being edited, or None for a new
    /// entry (C OLC_ZNUM: rnum, or < 0 == new).
    rnum: Option<usize>,
    /// "Anything changed?" flag (C OLC_VAL).
    changed: bool,
    /// Current sub-menu mode (C OLC_MODE).
    mode: HeditMode,
    /// Active text-entry accumulator for the multi-line entry editor.
    text_buf: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeditMode {
    MainMenu,
    ConfirmSave,
    Keywords,
    Entry,
    MinLevel,
}

static STATES: OnceLock<Mutex<std::collections::HashMap<ConnId, HeditState>>> = OnceLock::new();

fn states() -> &'static Mutex<std::collections::HashMap<ConnId, HeditState>> {
    STATES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn with_state<R>(conn: ConnId, f: impl FnOnce(&mut HeditState) -> R) -> Option<R> {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.get_mut(&conn).map(f)
}

fn take_state(conn: ConnId) -> Option<HeditState> {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.remove(&conn)
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit). `olc::abort_editor` calls `olc::clear_active`.
pub fn abort(conn: ConnId) {
    take_state(conn);
}

fn set_state(conn: ConnId, st: HeditState) {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.insert(conn, st);
}

// ---------------------------------------------------------------------------
// isname (db.c): whole-keyword match, case-insensitive. Used by find_help_rnum.
// ---------------------------------------------------------------------------

fn find_help_rnum(keyword: &str) -> Option<usize> {
    let guard = match help_table().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    // C find_help_rnum loops `i < top_of_helpt`, excluding the last slot because
    // C's table always carries a trailing calloc-zeroed UNDEFINED sentinel. This
    // port's loader does NOT synthesize that sentinel and its save writes only
    // real entries, so a `len()-1` bound would make the genuinely-last help entry
    // unfindable (hence non-editable) after any hedit save+reload. Iterate ALL
    // entries instead: a stock file's UNDEFINED sentinel has keywords no real
    // lookup matches, so including it is harmless while no real entry is dropped.
    for (i, e) in guard.iter().enumerate() {
        if crate::handler::isname(keyword, &e.keywords) {
            return Some(i);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Command entry: do_hedit (the hedit.c-relevant slice of olc.c do_olc).
// ---------------------------------------------------------------------------

/// `hedit <keyword>` opens the editor on that help entry (creating a new one if
/// no entry matches); `hedit save` flushes the whole table to disk.
pub fn do_hedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // No screwing around as a mobile.
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        return;
    }
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };

    let lib_path = g.config.lib_path.clone();
    ensure_loaded(&lib_path);

    // two_arguments(argument, buf1, buf2): we only need the first word.
    let (buf1, _rest) = crate::interpreter::one_argument(arg);

    if buf1.is_empty() {
        g.send_to_char(ch, "Specify a help entry to edit.\r\n");
        return;
    }

    // "save" — flush the whole help table to disk (do_olc save path for HEDIT).
    if buf1.eq_ignore_ascii_case("save") || (buf1.len() <= 4 && "save".starts_with(&buf1.to_lowercase())) {
        // Match strn_cmp("save", buf1, 4): a prefix of "save" of length up to 4.
        if "save".starts_with(&buf1.to_lowercase()) && !buf1.is_empty() {
            g.send_to_char(ch, "Saving all help entries.\r\n");
            let name = g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default();
            log::info!("OLC: {} saves help entries.", name);
            hedit_save_to_disk(&lib_path);
            return;
        }
    }

    // Guard: is this entry already being edited by someone?
    let rnum = find_help_rnum(&buf1);
    let editing_keyword = rnum
        .and_then(|r| help_table().lock().ok().and_then(|t| t.get(r).map(|e| e.keywords.clone())))
        .unwrap_or_else(|| buf1.clone());
    if hedit_keyword_busy(&editing_keyword, conn) {
        g.send_to_char(ch, "Help files are already being editted by someone else.\r\n");
        return;
    }

    // Set up the scratch entry: existing or new.
    let help = match rnum {
        Some(r) => {
            let guard = match help_table().lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            guard[r].clone()
        }
        None => HelpEntry {
            keywords: buf1.clone(),
            entry: "This is an unfinished help entry.\r\n".to_string(),
            min_level: 0,
        },
    };

    set_state(
        conn,
        HeditState {
            ch,
            help,
            rnum,
            changed: false,
            mode: HeditMode::MainMenu,
            text_buf: None,
        },
    );
    olc::set_active(conn, EditorKind::Hedit);

    // act("$n starts using OLC.", TO_ROOM).
    crate::act::act(
        g,
        "$n starts using OLC.",
        true,
        ch,
        None,
        crate::act::ActArg::None,
        crate::act::To::Room,
    );

    hedit_disp_menu(g, conn);
}

/// True if another connection is currently in hedit editing an entry whose
/// keyword line matches (mirrors C's OLC_NUM collision check, keyed on keyword).
fn hedit_keyword_busy(keyword: &str, exclude: ConnId) -> bool {
    let guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard
        .iter()
        .any(|(&c, st)| c != exclude && st.help.keywords.eq_ignore_ascii_case(keyword))
}

// ---------------------------------------------------------------------------
// Menu rendering (hedit_disp_menu).
// ---------------------------------------------------------------------------

fn hedit_disp_menu(g: &mut GameState, conn: ConnId) {
    let (keywords, entry, min_level) = match with_state(conn, |st| {
        st.mode = HeditMode::MainMenu;
        (st.help.keywords.clone(), st.help.entry.clone(), st.help.min_level)
    }) {
        Some(v) => v,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };

    let menu = format!(
        "\x1b[H\x1b[J\
&g1&n) Keywords    : &y{}\r\n\
&g2&n) Entry       :\r\n&y{}\
&g3&n) Min Level   : &c{}\r\n\
&gQ&n) Quit\r\n\
Enter choice : ",
        keywords, entry, min_level
    );
    g.send_to_char(ch, &menu);
}

// ---------------------------------------------------------------------------
// The main loop (hedit_parse) — the per-line router olc calls.
// ---------------------------------------------------------------------------

pub fn hedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match with_state(conn, |st| st.mode) {
        Some(m) => m,
        None => {
            // No state — abandon (should not happen if olc routing is correct).
            olc::clear_active(conn);
            return;
        }
    };
    let arg = line.trim();
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => {
            cleanup(conn);
            return;
        }
    };

    match mode {
        HeditMode::ConfirmSave => {
            match arg.chars().next() {
                Some('y') | Some('Y') => {
                    hedit_save_internally(g, conn);
                    let (name, kw) = (
                        g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default(),
                        with_state(conn, |st| st.help.keywords.clone()).unwrap_or_default(),
                    );
                    log::info!("OLC: {} edits help for {}.", name, kw);
                    g.send_to_char(ch, "Help entry saved to memory.\r\n");
                    cleanup(conn);
                }
                Some('n') | Some('N') => {
                    cleanup(conn);
                }
                _ => {
                    g.send_to_char(
                        ch,
                        "Invalid choice!\r\nDo you wish to save this help entry internally? : ",
                    );
                }
            }
        }

        HeditMode::MainMenu => match arg.chars().next() {
            Some('q') | Some('Q') => {
                let changed = with_state(conn, |st| st.changed).unwrap_or(false);
                if changed {
                    g.send_to_char(ch, "Do you wish to save this help entry internally? : ");
                    with_state(conn, |st| st.mode = HeditMode::ConfirmSave);
                } else {
                    cleanup(conn);
                }
                g.send_to_char(ch, "\r\n");
            }
            Some('1') => {
                g.send_to_char(ch, "Enter keywords:-\r\n] ");
                with_state(conn, |st| st.mode = HeditMode::Keywords);
            }
            Some('2') => {
                with_state(conn, |st| {
                    st.mode = HeditMode::Entry;
                    // Begin the text editor with the existing entry preloaded.
                    st.text_buf = Some(st.help.entry.clone());
                    st.changed = true;
                });
                g.send_to_char(ch, "\x1b[H\x1b[J");
                g.send_to_char(ch, "Enter help entry: (/s saves /h for help)\r\n\r\n");
                let cur = with_state(conn, |st| st.help.entry.clone()).unwrap_or_default();
                if !cur.is_empty() {
                    g.send_to_char(ch, &cur);
                }
            }
            Some('3') => {
                g.send_to_char(ch, "Enter min level:-\r\n] ");
                with_state(conn, |st| st.mode = HeditMode::MinLevel);
            }
            _ => {
                g.send_to_char(ch, "Invalid choice!\r\n");
                hedit_disp_menu(g, conn);
            }
        },

        HeditMode::Keywords => {
            let mut new_kw = arg.to_string();
            if new_kw.len() > MAX_HELP_KEYWORDS {
                new_kw.truncate(MAX_HELP_KEYWORDS - 1);
            }
            let kw = if new_kw.is_empty() { "UNDEFINED".to_string() } else { new_kw };
            with_state(conn, |st| {
                st.help.keywords = kw;
                st.changed = true;
            });
            hedit_disp_menu(g, conn);
        }

        HeditMode::Entry => {
            // The multi-line text editor: accumulate lines until "/s" (save) or
            // "/a" (abort). "/h" prints help. Mirrors the modify.c string editor
            // semantics used by the C hedit (d->str / max_str path).
            hedit_text_input(g, conn, line);
        }

        HeditMode::MinLevel => {
            let number = arg.parse::<i32>().unwrap_or(-1);
            if number < 0 || number > LVL_IMPL as i32 {
                g.send_to_char(ch, "That is not a valid choice!\r\nEnter min level:-\r\n] ");
            } else {
                with_state(conn, |st| {
                    st.help.min_level = number;
                    st.changed = true;
                });
                hedit_disp_menu(g, conn);
            }
        }
    }
}

/// The body-text accumulator for menu option 2 (the help entry). Lines starting
/// with '/' are editor commands: "/s" save+return to menu, "/a" abort (discard),
/// "/h" help, "/c" clear. Everything else is appended with a trailing CRLF
/// (matching load_help's per-line "\r\n").
fn hedit_text_input(g: &mut GameState, conn: ConnId, line: &str) {
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => {
            cleanup(conn);
            return;
        }
    };
    let str_in = delete_doubledollar(line);

    if let Some(rest) = str_in.strip_prefix('/') {
        let cmd = rest.chars().next().unwrap_or('\0');
        match cmd {
            's' | 'S' => {
                // Save: commit the accumulated buffer into the help entry.
                let buf = with_state(conn, |st| st.text_buf.take().unwrap_or_default())
                    .unwrap_or_default();
                with_state(conn, |st| {
                    st.help.entry = buf;
                    st.changed = true;
                    st.mode = HeditMode::MainMenu;
                });
                hedit_disp_menu(g, conn);
                return;
            }
            'a' | 'A' => {
                // Abort: discard edits to the entry text, return to menu.
                with_state(conn, |st| {
                    st.text_buf = None;
                    st.mode = HeditMode::MainMenu;
                });
                g.send_to_char(ch, "Entry aborted.\r\n");
                hedit_disp_menu(g, conn);
                return;
            }
            'c' | 'C' => {
                with_state(conn, |st| st.text_buf = Some(String::new()));
                g.send_to_char(ch, "Current buffer cleared.\r\n");
                return;
            }
            'h' | 'H' => {
                g.send_to_char(
                    ch,
                    "Editor commands:\r\n  /s  save and exit\r\n  /a  abort (discard)\r\n  \
/c  clear the buffer\r\n  /h  this help\r\n",
                );
                return;
            }
            _ => {
                g.send_to_char(ch, "Invalid option.\r\n");
                return;
            }
        }
    }

    // Append the plain line with truncation to MAX_HELP_ENTRY.
    let overflow = with_state(conn, |st| {
        let buf = st.text_buf.get_or_insert_with(String::new);
        let mut append = str_in.clone();
        if buf.is_empty() {
            if append.len() > MAX_HELP_ENTRY {
                append.truncate(MAX_HELP_ENTRY);
                return true;
            }
            buf.push_str(&append);
            buf.push_str("\r\n");
            false
        } else if append.len() + buf.len() + 2 > MAX_HELP_ENTRY {
            true
        } else {
            buf.push_str(&append);
            buf.push_str("\r\n");
            false
        }
    })
    .unwrap_or(false);

    if overflow {
        g.send_to_char(
            ch,
            "String too long, limit reached on message. Last line ignored.\r\n",
        );
    }
}

// ---------------------------------------------------------------------------
// Internal save (hedit_save_internally) + disk save (hedit_save_to_disk).
// ---------------------------------------------------------------------------

fn hedit_save_internally(g: &mut GameState, conn: ConnId) {
    let (help, rnum) = match with_state(conn, |st| (st.help.clone(), st.rnum)) {
        Some(v) => v,
        None => return,
    };
    {
        let mut guard = match help_table().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match rnum {
            // C: rnum > 0 ⇒ replace existing. (rnum 0, the first slot, is also a
            // real entry in our Vec model; we replace whenever we have an index.)
            Some(r) if r < guard.len() => {
                guard[r] = help;
            }
            _ => {
                // New entry: C inserts at the top of the table.
                guard.insert(0, help);
            }
        }
    }
    // C olc_add_to_save_list(...) then the player saves with `hedit save`; we
    // also flush to disk immediately so the edit survives a crash, matching the
    // "saved to memory" + builder convention.
    let lib_path = g.config.lib_path.clone();
    hedit_save_to_disk(&lib_path);
}

/// Write the entire help table back to text/help/help.hlp in load_help format.
fn hedit_save_to_disk(lib_path: &str) {
    let dir = format!("{}/{}", lib_path.trim_end_matches('/'), HLP_REL_DIR);
    let tmp = format!("{}/{}.new", dir, HELP_FILE);
    let final_path = format!("{}/{}", dir, HELP_FILE);

    let guard = match help_table().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let mut out = String::new();
    for help in guard.iter() {
        // strip_string(entry): remove every '\r', leaving bare '\n' line breaks.
        let stripped = strip_string(if help.entry.is_empty() { "Empty" } else { &help.entry });
        let kw = if help.keywords.is_empty() { "UNDEFINED" } else { &help.keywords };
        // fprintf(fp, "%s\n%s\n#%d\n", keywords, entry, min_level).
        // The stripped entry already ends in '\n' (every line had "\r\n" ->
        // "\n"); C then adds one more '\n', giving a blank separator line, then
        // "#level". We replicate: keyword '\n', body, '\n', "#level\n".
        out.push_str(kw);
        out.push('\n');
        out.push_str(&stripped);
        out.push('\n');
        out.push('#');
        out.push_str(&help.min_level.to_string());
        out.push('\n');
    }
    out.push_str("$~\n");

    if std::fs::write(&tmp, &out).is_err() {
        log::warn!("SYSERR: OLC: Cannot open help file!");
        return;
    }
    let _ = std::fs::remove_file(&final_path);
    let _ = std::fs::rename(&tmp, &final_path);
}

/// strip_string(buffer) (olc.c): delete every '\r'. Leaves '\n' intact.
fn strip_string(s: &str) -> String {
    s.chars().filter(|&c| c != '\r').collect()
}

/// delete_doubledollar: collapse "$$" to "$" for the text editor input path.
fn delete_doubledollar(s: &str) -> String {
    s.replace("$$", "$")
}

// ---------------------------------------------------------------------------
// Cleanup.
// ---------------------------------------------------------------------------

fn cleanup(conn: ConnId) {
    take_state(conn);
    olc::clear_active(conn);
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}
