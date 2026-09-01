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
const HEDIT_GLOBAL_SAVE_KEY: &str = "<all help>";

// ---------------------------------------------------------------------------
// help_index_element (db.h) — only the fields hedit touches.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct HelpEntry {
    pub keywords: String,
    /// Entry body as held in memory: every line terminated by "\r\n", exactly
    /// like load_help builds it (so the menu renders identically and the save
    /// path strips the '\r' back out).
    pub entry: String,
    pub min_level: i32,
}

/// The canonical help table, lazily loaded from disk on first use. Mirrors the
/// C global `help_table` (+ top_of_helpt as the Vec length).

/// Boot the help table for the live `help` command (db.c:299-300
/// index_boot(DB_BOOT_HLP)); the hedit editor loaded lazily before, but
/// nothing booted the table for the command path (#232).
pub fn boot_help_table(g: &mut GameState) -> std::io::Result<()> {
    ensure_loaded(g)
}

/// The general page: C's `help` global is the FIRST entry's body (the
/// 'help' keywords record at the top of help.hlp).
pub fn general_help_page(g: &GameState) -> Option<String> {
    g.social.help_table.first().map(|e| e.entry.clone())
}

/// find_help + min-level gate; returns the formatted page
/// "keywords\r\nentry" (act.informative.c:1620-1654).
pub fn lookup_help(g: &mut GameState, keyword: &str, level: i32) -> Option<String> {
    ensure_loaded(g).ok()?;
    let rnum = find_help_rnum(g, keyword)?;
    let e = g.social.help_table.get(rnum)?;
    if e.min_level > level {
        return None;
    }
    Some(format!("{}\r\n{}", e.keywords, e.entry))
}

fn ensure_loaded(g: &mut GameState) -> std::io::Result<()> {
    if g.social.help_loaded {
        return Ok(());
    }
    let table = load_help_file(&g.config.lib_path)?;
    g.social.help_table = table;
    g.social.help_loaded = true;
    Ok(())
}

/// Read text/help/help.hlp into the in-memory table (inverse of save). Mirrors
/// db.c load_help: keyword line, body lines until a '#', min_level = tail.
fn load_help_file(lib_path: &str) -> std::io::Result<Vec<HelpEntry>> {
    let path = format!(
        "{}/{}/{}",
        lib_path.trim_end_matches('/'),
        HLP_REL_DIR,
        HELP_FILE
    );
    let raw = std::fs::read_to_string(&path)?;
    let mut out: Vec<HelpEntry> = Vec::new();
    // get_one_line(): strips the trailing newline; we iterate over .lines()
    // which already drops '\n' (and we additionally drop a trailing '\r').
    let mut lines = raw.lines().map(|l| l.trim_end_matches('\r'));

    // First keyword line.
    let mut key = lines.next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "help file is empty")
    })?;
    while !key.starts_with('$') {
        if key.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "help entry has empty keywords",
            ));
        }
        let mut entry = String::new();
        let mut line = lines.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("help entry {key:?} has no level marker"),
            )
        })?;
        while !line.starts_with('#') {
            entry.push_str(line);
            entry.push_str("\r\n");
            line = lines.next().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("help entry {key:?} has no level marker"),
                )
            })?;
        }
        // C load_help treats a bare `#` as level zero and only calls atoi
        // when at least one byte follows it. The shipped help file contains
        // legacy generated entries with that exact marker.
        let min_level = if line.len() == 1 {
            0
        } else {
            crate::text::parse_i32_strict(&line[1..])
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("help entry {key:?} has invalid level marker {line:?}: {error:?}"),
                    )
                })?
                .clamp(0, LVL_IMPL as i32)
        };

        out.push(HelpEntry {
            keywords: key.to_string(),
            entry,
            min_level,
        });

        key = lines.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "help file has no terminator",
            )
        })?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Per-connection editor state (Character/Descriptor cannot carry OLC_DATA).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct HeditState {
    /// The character doing the editing (for log/permission/output).
    ch: CharId,
    authorization: olc::OlcAuthorization,
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
    if let Some(state) = take_state(conn) {
        olc::discard_unresolved_named_save(
            EditorKind::Hedit,
            &state.help.keywords.to_ascii_lowercase(),
        );
    }
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

fn find_help_rnum(g: &GameState, keyword: &str) -> Option<usize> {
    let guard = &g.social.help_table;
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
    let Some(authorization) = olc::capture_olc_authorization(g, ch) else {
        send(g, ch, "You do not have access to the help editor.\r\n");
        return;
    };

    if let Err(error) = ensure_loaded(g) {
        log::warn!("SYSERR: OLC: cannot load help table: {}", error);
        send(
            g,
            ch,
            "The help file could not be read safely; no editor was opened.\r\n",
        );
        return;
    }

    // two_arguments(argument, buf1, buf2): we only need the first word.
    let (buf1, _rest) = crate::interpreter::one_argument(arg);

    if buf1.is_empty() {
        send(g, ch, "Specify a help entry to edit.\r\n");
        return;
    }

    // "save" — flush the whole help table to disk (do_olc save path for HEDIT).
    if buf1.eq_ignore_ascii_case("save")
        || (buf1.len() <= 4 && "save".starts_with(&buf1.to_lowercase()))
    {
        // Match strn_cmp("save", buf1, 4): a prefix of "save" of length up to 4.
        if "save".starts_with(&buf1.to_lowercase()) && !buf1.is_empty() {
            if let Err(error) = olc::revalidate_olc_authorization(g, authorization, true, None) {
                log::warn!("SYSERR: OLC: refused help publication: {error}");
                send(
                    g,
                    ch,
                    "Your OLC authorization changed; help was not saved.\r\n",
                );
                return;
            }
            let name = g
                .get_char(ch)
                .map(|c| c.player.name.clone())
                .unwrap_or_default();
            match save_all_help(g) {
                Ok(()) => {
                    send(g, ch, "Saving all help entries.\r\n");
                    log::info!("OLC: {} saves help entries.", name);
                }
                Err(err) => {
                    log::warn!("SYSERR: OLC: cannot save help entries: {}", err);
                    send(g, ch, "Could not save the help file.\r\n");
                }
            }
            return;
        }
    }

    // Every save rewrites the whole help table, and a newly inserted entry
    // shifts every numeric rnum. Serialize Hedit sessions globally so an editor
    // cannot later publish a scratch copy against a stale rnum.
    if let Some(other_conn) = other_hedit_session(conn) {
        let other = g
            .descriptors
            .get(&other_conn)
            .and_then(|d| d.character)
            .and_then(|cid| g.get_char(cid))
            .map(|c| c.player.name.clone())
            .unwrap_or_else(|| "someone".to_string());
        g.send_to_char(
            ch,
            &format!("Help files are already being editted by {}.\r\n", other),
        );
        return;
    }
    let rnum = find_help_rnum(g, &buf1);

    // Set up the scratch entry: existing or new.
    let help = match rnum {
        Some(r) => g.social.help_table[r].clone(),
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
            authorization,
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

/// Return another active Hedit connection, if any. Hedit stores table indexes,
/// and a save may insert at index zero, so distinct entries cannot be edited
/// concurrently without invalidating one session's identity.
fn other_hedit_session(exclude: ConnId) -> Option<ConnId> {
    crate::lock_ok::lock(&states())
        .keys()
        .copied()
        .find(|conn| *conn != exclude)
}

// ---------------------------------------------------------------------------
// Menu rendering (hedit_disp_menu).
// ---------------------------------------------------------------------------

fn hedit_disp_menu(g: &mut GameState, conn: ConnId) {
    let (keywords, entry, min_level) = match with_state(conn, |st| {
        st.mode = HeditMode::MainMenu;
        (
            st.help.keywords.clone(),
            st.help.entry.clone(),
            st.help.min_level,
        )
    }) {
        Some(v) => v,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };

    let menu = format!(
        "\
&g1&n) Keywords    : &y{}\r\n\
&g2&n) Entry       :\r\n&y{}\
&g3&n) Min Level   : &c{}\r\n\
&gQ&n) Quit\r\n\
Enter choice : ",
        keywords, entry, min_level
    );
    send(g, ch, &menu);
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
        HeditMode::ConfirmSave => match arg.chars().next() {
            Some('y') | Some('Y') => match hedit_save_internally(g, conn) {
                Ok(()) => {
                    let (name, kw) = (
                        g.get_char(ch)
                            .map(|c| c.player.name.clone())
                            .unwrap_or_default(),
                        with_state(conn, |st| st.help.keywords.clone()).unwrap_or_default(),
                    );
                    log::info!("OLC: {} edits help for {}.", name, kw);
                    send(g, ch, "Help entry saved to disk and memory.\r\n");
                    cleanup(conn);
                }
                Err(err) => {
                    log::warn!("SYSERR: OLC: cannot save help entry: {}", err);
                    if crate::olc::replacement_was_published(&err) {
                        send(
                            g,
                            ch,
                            "The help file was published and live help was reconciled, but crash durability could not be confirmed.\r\nDo you wish to retry saving this help entry? : ",
                        );
                    } else {
                        send(
                            g,
                            ch,
                            "Could not save the help entry to disk; the live help table was not changed.\r\nDo you wish to retry saving this help entry? : ",
                        );
                    }
                }
            },
            Some('n') | Some('N') => {
                cleanup(conn);
            }
            _ => {
                g.send_to_char(
                    ch,
                    "Invalid choice!\r\nDo you wish to save this help entry internally? : ",
                );
            }
        },

        HeditMode::MainMenu => match arg.chars().next() {
            Some('q') | Some('Q') => {
                let changed = with_state(conn, |st| st.changed).unwrap_or(false);
                if changed {
                    send(g, ch, "Do you wish to save this help entry internally? : ");
                    with_state(conn, |st| st.mode = HeditMode::ConfirmSave);
                } else {
                    cleanup(conn);
                }
                send(g, ch, "\r\n");
            }
            Some('1') => {
                send(g, ch, "Enter keywords:-\r\n] ");
                with_state(conn, |st| st.mode = HeditMode::Keywords);
            }
            Some('2') => {
                with_state(conn, |st| {
                    st.mode = HeditMode::Entry;
                    // Begin the text editor with the existing entry preloaded.
                    st.text_buf = Some(st.help.entry.clone());
                    st.changed = true;
                });
                send(g, ch, "");
                send(g, ch, "Enter help entry: (/s saves /h for help)\r\n\r\n");
                let cur = with_state(conn, |st| st.help.entry.clone()).unwrap_or_default();
                if !cur.is_empty() {
                    send(g, ch, &cur);
                }
            }
            Some('3') => {
                send(g, ch, "Enter min level:-\r\n] ");
                with_state(conn, |st| st.mode = HeditMode::MinLevel);
            }
            _ => {
                send(g, ch, "Invalid choice!\r\n");
                hedit_disp_menu(g, conn);
            }
        },

        HeditMode::Keywords => {
            let mut new_kw = arg.to_string();
            if new_kw.len() > MAX_HELP_KEYWORDS {
                crate::text::truncate_utf8_bytes(&mut new_kw, MAX_HELP_KEYWORDS - 1);
            }
            let kw = if new_kw.is_empty() {
                "UNDEFINED".to_string()
            } else {
                new_kw
            };
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
            // C hedit.c:317: atoi() semantics — a non-numeric line becomes 0,
            // which passes the range check and resets min_level to 0 (#298).
            let number = match crate::text::parse_i32_atoi(arg) {
                Ok(number) => number,
                Err(crate::text::ParseIntError::Overflow) => {
                    send(
                        g,
                        ch,
                        "That number is outside the supported range.\r\nEnter min level:-\r\n] ",
                    );
                    return;
                }
                Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
            };
            if number < 0 || number > LVL_IMPL as i32 {
                send(
                    g,
                    ch,
                    "That is not a valid choice!\r\nEnter min level:-\r\n] ",
                );
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
/// with '/' are handled by the shared modify.c-style editor command parser.
/// Everything else is appended with a trailing CRLF (matching load_help's
/// per-line "\r\n").
fn hedit_text_input(g: &mut GameState, conn: ConnId, line: &str) {
    if conn_char(g, conn).is_none() {
        cleanup(conn);
        return;
    }

    let mut buf =
        with_state(conn, |st| st.text_buf.clone().unwrap_or_default()).unwrap_or_default();
    match crate::modify::editor_buffer_input(g, conn, &mut buf, MAX_HELP_ENTRY, line) {
        crate::modify::BufferEditorResult::Continue => {
            with_state(conn, |st| st.text_buf = Some(buf));
        }
        crate::modify::BufferEditorResult::Save => {
            with_state(conn, |st| {
                st.help.entry = buf;
                st.text_buf = None;
                st.changed = true;
                st.mode = HeditMode::MainMenu;
            });
            hedit_disp_menu(g, conn);
        }
        crate::modify::BufferEditorResult::Abort => {
            with_state(conn, |st| {
                st.text_buf = None;
                st.mode = HeditMode::MainMenu;
            });
            hedit_disp_menu(g, conn);
        }
    }
}

// ---------------------------------------------------------------------------
// Internal save (hedit_save_internally) + disk save (hedit_save_to_disk).
// ---------------------------------------------------------------------------

fn hedit_save_internally(g: &mut GameState, conn: ConnId) -> std::io::Result<()> {
    hedit_save_internally_with(g, conn, crate::olc::atomic_replace)
}

fn hedit_save_internally_with<F>(g: &mut GameState, conn: ConnId, replace: F) -> std::io::Result<()>
where
    F: FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
{
    let (help, rnum, authorization) =
        match with_state(conn, |st| (st.help.clone(), st.rnum, st.authorization)) {
            Some(v) => v,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "help editor state is missing",
                ));
            }
        };
    olc::revalidate_olc_authorization(g, authorization, true, None)?;
    let unresolved_key = help.keywords.to_ascii_lowercase();
    let mut entries = g.social.help_table.clone();
    let inserted = match rnum {
        // C: rnum > 0 ⇒ replace existing. (rnum 0, the first slot, is also a
        // real entry in our Vec model; we replace whenever we have an index.)
        Some(r) if r < entries.len() => {
            entries[r] = help;
            false
        }
        _ => {
            // New entry: C inserts at the top of the table.
            entries.insert(0, help);
            true
        }
    };
    let lib_path = g.config.lib_path.clone();
    match hedit_save_to_disk_with(&lib_path, &entries, replace) {
        Ok(()) => {
            g.social.help_table = entries;
            crate::olc::olc_remove_from_save_list(0, crate::olc::OLC_SAVE_HELP);
            crate::olc::clear_unresolved_named_save(EditorKind::Hedit, &unresolved_key);
            crate::olc::clear_unresolved_named_save(EditorKind::Hedit, HEDIT_GLOBAL_SAVE_KEY);
            crate::olc::clear_published_unresolved_kind(EditorKind::Hedit);
            Ok(())
        }
        Err(error) if crate::olc::replacement_was_published(&error) => {
            g.social.help_table = entries;
            if inserted {
                // The candidate is already live at index zero. Retrying the
                // still-open editor must replace that entry, not insert a
                // duplicate and shift the table again.
                with_state(conn, |state| state.rnum = Some(0));
            }
            crate::olc::olc_add_to_save_list(0, crate::olc::OLC_SAVE_HELP);
            crate::olc::mark_unresolved_named_save_failure(
                EditorKind::Hedit,
                &unresolved_key,
                &error,
            );
            Err(error)
        }
        Err(error) => {
            crate::olc::mark_unresolved_named_save_failure(
                EditorKind::Hedit,
                &unresolved_key,
                &error,
            );
            Err(error)
        }
    }
}

/// Write the entire help table back to text/help/help.hlp in load_help format.
/// olc.rs 'olc hedit save' entry (#275).
pub fn save_all_help(g: &mut GameState) -> std::io::Result<()> {
    let result = (|| {
        let lib = g.config.lib_path.clone();
        ensure_loaded(g)?;
        let entries = g.social.help_table.clone();
        hedit_save_to_disk(&lib, &entries)
    })();
    match &result {
        Ok(()) => {
            crate::olc::olc_remove_from_save_list(0, crate::olc::OLC_SAVE_HELP);
            crate::olc::clear_unresolved_named_save(EditorKind::Hedit, HEDIT_GLOBAL_SAVE_KEY);
            crate::olc::clear_published_unresolved_kind(EditorKind::Hedit);
        }
        Err(error) => crate::olc::mark_unresolved_named_save_failure(
            EditorKind::Hedit,
            HEDIT_GLOBAL_SAVE_KEY,
            error,
        ),
    }
    result
}

fn hedit_save_to_disk(lib_path: &str, entries: &[HelpEntry]) -> std::io::Result<()> {
    hedit_save_to_disk_with(lib_path, entries, crate::olc::atomic_replace)
}

fn hedit_save_to_disk_with<F>(
    lib_path: &str,
    entries: &[HelpEntry],
    replace: F,
) -> std::io::Result<()>
where
    F: FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
{
    let dir = format!("{}/{}", lib_path.trim_end_matches('/'), HLP_REL_DIR);
    let final_path = format!("{}/{}", dir, HELP_FILE);

    let mut out = String::new();
    for help in entries {
        // strip_string(entry): remove every '\r', leaving bare '\n' line breaks.
        let stripped = strip_string(if help.entry.is_empty() {
            "Empty"
        } else {
            &help.entry
        });
        let kw = if help.keywords.is_empty() {
            "UNDEFINED"
        } else {
            &help.keywords
        };
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

    std::fs::create_dir_all(&dir)?;
    replace(std::path::Path::new(&final_path), out.as_bytes())
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
    if let Some(state) = take_state(conn) {
        olc::discard_unresolved_named_save(
            EditorKind::Hedit,
            &state.help.keywords.to_ascii_lowercase(),
        );
    }
    olc::clear_active(conn);
}

/// OLC output with C get_char_cols semantics: the &-codes in these menus are
/// stripped for builders whose colour level is below C_NRM (#306).
fn send(g: &mut GameState, ch: CharId, msg: &str) {
    if crate::olc::olc_colour_on(g, ch) {
        g.send_to_char(ch, msg);
    } else {
        g.send_to_char(ch, &crate::connection::strip_color(msg));
    }
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;

    fn test_help_lib(label: &str) -> (std::sync::MutexGuard<'static, ()>, std::path::PathBuf) {
        static TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let guard = TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Help state is per-GameState now: a fresh GameState starts unloaded,
        // so no global reset is needed here any more.
        crate::lock_ok::lock(&states()).clear();
        let lib =
            std::env::temp_dir().join(format!("deltamud-hedit-{label}-{}", std::process::id()));
        let help_dir = lib.join(HLP_REL_DIR);
        let _ = std::fs::remove_dir_all(&lib);
        std::fs::create_dir_all(&help_dir).unwrap();
        std::fs::write(help_dir.join(HELP_FILE), "$~\n").unwrap();
        (guard, lib)
    }

    #[test]
    fn min_level_uses_c_atoi_semantics() {
        let (_guard, lib) = test_help_lib("atoi");
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let mut ch = Character::new_player("Root".into(), Class::Cleric, Race::Human);
        ch.player.level = LVL_IMPL;
        let ch = g.create_char(ch);
        let conn = ConnId(91);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        do_hedit(&mut g, ch, "testkeyword", 0);
        hedit_parse(&mut g, conn, "3"); // min level prompt
        // C hedit.c:317: atoi("abc") == 0 -> passes the range check (#298).
        hedit_parse(&mut g, conn, "abc");
        assert_eq!(with_state(conn, |st| st.help.min_level), Some(0));
        // A genuine out-of-range number is still rejected.
        hedit_parse(&mut g, conn, "-4");
        assert_eq!(with_state(conn, |st| st.help.min_level), Some(0));
        cleanup(conn);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn legacy_bare_level_marker_loads_as_zero_and_rewrites_canonically() {
        let (_guard, lib) = test_help_lib("bare-level-marker");
        let path = lib.join(HLP_REL_DIR).join(HELP_FILE);
        std::fs::write(
            &path,
            "affected\naffected\n\nLegacy generated help body.\n#\n$~\n",
        )
        .unwrap();

        let entries = load_help_file(lib.to_str().unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].keywords, "affected");
        assert_eq!(entries[0].min_level, 0);

        hedit_save_to_disk(lib.to_str().unwrap(), &entries).unwrap();
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.contains("\n#0\n"));
        assert!(!rewritten.contains("\n#\n"));
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn keyword_editor_truncates_multibyte_scalars_on_character_boundaries() {
        let (_guard, lib) = test_help_lib("utf8");
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let mut ch = Character::new_player("Root".into(), Class::Cleric, Race::Human);
        ch.player.level = LVL_IMPL;
        let ch = g.create_char(ch);
        let conn = ConnId(92);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        do_hedit(&mut g, ch, "utf8boundarykeyword", 0);
        for scalar in ['é', '€', '🦀'] {
            hedit_parse(&mut g, conn, "1");
            let input = format!("{}{scalar}", "a".repeat(MAX_HELP_KEYWORDS - 1));
            hedit_parse(&mut g, conn, &input);
            let keyword = with_state(conn, |state| state.help.keywords.clone()).unwrap();
            assert_eq!(keyword.len(), MAX_HELP_KEYWORDS - 1);
            assert!(keyword.is_char_boundary(keyword.len()));
            assert!(!keyword.contains(scalar));
        }
        cleanup(conn);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn distinct_help_entries_are_serialized_to_prevent_stale_rnum_overwrite() {
        let (_guard, lib) = test_help_lib("serialized");
        std::fs::write(
            lib.join(HLP_REL_DIR).join(HELP_FILE),
            "alpha\nAlpha body.\n#0\nbeta\nBeta body.\n#0\n$~\n",
        )
        .unwrap();
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);

        let first_conn = ConnId(93);
        let mut first = Character::new_player("First".into(), Class::Cleric, Race::Human);
        first.player.level = LVL_IMPL;
        let first = g.create_char(first);
        g.get_char_mut(first).unwrap().desc = Some(first_conn);
        let mut first_descriptor = Descriptor::new(first_conn, "first.example".to_string());
        first_descriptor.character = Some(first);
        g.descriptors.insert(first_conn, first_descriptor);

        let second_conn = ConnId(94);
        let mut second = Character::new_player("Second".into(), Class::Cleric, Race::Human);
        second.player.level = LVL_IMPL;
        let second = g.create_char(second);
        g.get_char_mut(second).unwrap().desc = Some(second_conn);
        let mut second_descriptor = Descriptor::new(second_conn, "second.example".to_string());
        second_descriptor.character = Some(second);
        g.descriptors.insert(second_conn, second_descriptor);

        do_hedit(&mut g, first, "alpha", 0);
        assert_eq!(with_state(first_conn, |state| state.rnum), Some(Some(0)));

        // A distinct new entry would insert at zero and shift alpha's rnum.
        do_hedit(&mut g, second, "gamma", 0);
        assert!(with_state(second_conn, |_| ()).is_none());
        assert!(
            g.descriptors[&second_conn]
                .outbuf
                .contains("already being editted by First")
        );

        cleanup(first_conn);
        do_hedit(&mut g, second, "gamma", 0);
        assert!(with_state(second_conn, |_| ()).is_some());

        cleanup(second_conn);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn post_publication_retry_of_new_help_replaces_instead_of_inserting_twice() {
        let (_guard, lib) = test_help_lib("published-new-retry");
        let _save_guard = crate::olc::test_save_list_guard();
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let conn = ConnId(95);
        let mut editor = Character::new_player("Editor".into(), Class::Cleric, Race::Human);
        editor.player.level = LVL_IMPL;
        editor.trust = i32::from(LVL_IMPL);
        editor.godcmds3 |= crate::gcmd::GCMD3_IMPOLC;
        let editor = g.create_char(editor);
        g.get_char_mut(editor).unwrap().desc = Some(conn);
        let mut descriptor = Descriptor::new(conn, "editor.example".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(editor);
        g.descriptors.insert(conn, descriptor);

        do_hedit(&mut g, editor, "gamma", 0);
        let error = hedit_save_internally_with(&mut g, conn, |path, bytes| {
            crate::olc::atomic_replace_with_hooks(
                path,
                bytes,
                |_| Ok(()),
                |_| Err(std::io::Error::other("injected directory sync failure")),
            )
        })
        .unwrap_err();

        assert!(crate::olc::replacement_was_published(&error));
        assert_eq!(g.social.help_table.len(), 1);
        assert_eq!(with_state(conn, |state| state.rnum), Some(Some(0)));

        hedit_save_internally(&mut g, conn).unwrap();
        assert_eq!(g.social.help_table.len(), 1);
        let saved = std::fs::read_to_string(lib.join(HLP_REL_DIR).join(HELP_FILE)).unwrap();
        assert_eq!(saved.lines().filter(|line| *line == "gamma").count(), 1);

        cleanup(conn);
        let _ = std::fs::remove_dir_all(lib);
    }
}
