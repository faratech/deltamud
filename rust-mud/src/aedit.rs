// aedit.rs — the OasisOLC social/action editor (C aedit.c), ported to the
// id-indexed GameState and the shared olc.rs framework contract.
//
// What this does, mirroring aedit.c:
//   * do_aedit(g, ch, arg, subcmd)  — the `aedit <action>` / `aedit save`
//     command. Resolves the named social (by abbreviation), confirms an
//     edit-or-add, snapshots it into per-connection edit state, registers
//     EditorKind::Aedit with olc, and (after confirmation) shows the menu.
//   * aedit_parse(g, conn, line)    — the per-line input router olc calls.
//
// ON-DISK FORMAT (byte-faithful inverse of cmd_social::load_socials, which is
// itself the inverse of C boot_social_messages):
//   For each social, in table order:
//       ~<command> <sort_as> <hide> <min_char_pos> <min_vict_pos> <min_level>\n
//   followed by exactly 13 message lines, each either the verbatim text or a
//   bare "#" placeholder when the field is NULL, in this order:
//       char_no_arg, others_no_arg, char_found, others_found,
//       vict_found, not_found, char_auto, others_auto,
//       char_body_found, others_body_found, vict_body_found,
//       char_obj_found, others_obj_found
//   followed by one blank separator line. The file terminates with "$\n".
//
// OWNERSHIP NOTE: the live social table in cmd_social.rs is a private static
// loaded once at boot; this editor cannot mutate it in place. So aedit owns its
// own canonical, editable copy (lazily loaded from the same misc/socials file),
// edits it, and writes the whole file back — exactly as C's aedit_save_to_disk
// rewrites SOCMESS_FILE. (A reboot re-primes cmd_social from the new file.)

use crate::constants::POSITION_TYPES;
use crate::olc::{self, EditorKind};
use crate::state::GameState;
use crate::types::*;
use std::sync::{Mutex, OnceLock};

// POS_STANDING (structs.h) — the aedit defaults.
const POS_STANDING: i32 = 9;

// CircleMUD db.h: SOCMESS_FILE "misc/socials".
const SOCMESS_REL: &str = "misc/socials";

// ---------------------------------------------------------------------------
// social_messg (structs.h) — the editable mirror. A None Option is the C NULL.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct SocialAction {
    command: String,
    sort_as: String,
    hide: bool,
    min_victim_position: i32,
    min_char_position: i32,
    min_level_char: i32,

    char_no_arg: Option<String>,
    others_no_arg: Option<String>,
    char_found: Option<String>,
    others_found: Option<String>,
    vict_found: Option<String>,
    not_found: Option<String>,
    char_auto: Option<String>,
    others_auto: Option<String>,
    char_body_found: Option<String>,
    others_body_found: Option<String>,
    vict_body_found: Option<String>,
    char_obj_found: Option<String>,
    others_obj_found: Option<String>,
}

/// The canonical, editable social table (C soc_mess_list / top_of_socialt),
/// lazily loaded from disk on first edit.
static SOC_LIST: OnceLock<Mutex<Vec<SocialAction>>> = OnceLock::new();
static SOC_LOADED: OnceLock<Mutex<bool>> = OnceLock::new();

fn soc_list() -> &'static Mutex<Vec<SocialAction>> {
    SOC_LIST.get_or_init(|| Mutex::new(Vec::new()))
}

fn ensure_loaded(lib_path: &str) {
    let flag = SOC_LOADED.get_or_init(|| Mutex::new(false));
    let mut loaded = match flag.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if *loaded {
        return;
    }
    let list = load_socials_file(lib_path);
    if let Ok(mut guard) = soc_list().lock() {
        *guard = list;
    }
    *loaded = true;
}

/// Load misc/socials (inverse of save). Header line "~cmd sort hide cpos vpos
/// lvl", then exactly 13 message slots (a line starting with '#' is NULL),
/// terminated by a "$" sentinel. Mirrors cmd_social::load_socials exactly.
fn load_socials_file(lib_path: &str) -> Vec<SocialAction> {
    let path = format!("{}/{}", lib_path.trim_end_matches('/'), SOCMESS_REL);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut lines = raw.lines();
    let mut list: Vec<SocialAction> = Vec::new();

    loop {
        // Skip blank separator lines between socials.
        let header = loop {
            match lines.next() {
                Some(l) if l.trim().is_empty() => continue,
                Some(l) => break l,
                None => return list,
            }
        };
        let header = header.trim_start();
        if header.starts_with('$') {
            break;
        }
        if !header.starts_with('~') {
            continue;
        }
        let mut it = header.split_whitespace();
        let command = it.next().unwrap_or("~").trim_start_matches('~').to_string();
        let sort_as = it.next().unwrap_or("").to_string();
        let loader_i32 = |raw: Option<&str>, field: &str| match raw {
            Some(raw) => match crate::text::parse_i32_strict(raw) {
                Ok(value) => value,
                Err(crate::text::ParseIntError::Overflow) => {
                    let clamped = if raw.trim_start().starts_with('-') {
                        i32::MIN
                    } else {
                        i32::MAX
                    };
                    log::warn!(
                        "SYSERR: social {command} {field} overflow in {path}; clamped to {clamped}"
                    );
                    clamped
                }
                Err(_) => 0,
            },
            None => 0,
        };
        let hide = loader_i32(it.next(), "hide");
        let min_char_pos = loader_i32(it.next(), "minimum character position");
        let min_vict_pos = loader_i32(it.next(), "minimum victim position");
        let min_lvl = loader_i32(it.next(), "minimum level");

        let read = |lines: &mut std::str::Lines| -> Option<String> {
            let l = lines.next().unwrap_or("#");
            if l.starts_with('#') {
                None
            } else {
                Some(l.to_string())
            }
        };

        list.push(SocialAction {
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
        });
    }
    list
}

// ---------------------------------------------------------------------------
// Per-connection editor state.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct AeditState {
    ch: CharId,
    /// The scratch action (C OLC_ACTION).
    action: SocialAction,
    /// Real index into SOC_LIST of the action being edited, or None for a new
    /// action (C OLC_ZNUM: index, or > top_of_socialt == new).
    rnum: Option<usize>,
    /// The originally-typed action word (C OLC_STORAGE) — used by the
    /// edit-or-add confirmation walk.
    storage: String,
    /// "Anything changed?" flag (C OLC_VAL).
    changed: bool,
    mode: AeditMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AeditMode {
    ConfirmEdit,
    ConfirmAdd,
    ConfirmSave,
    MainMenu,
    ActionName,
    SortAs,
    MinCharPos,
    MinVictPos,
    MinCharLevel,
    NovictChar,
    NovictOthers,
    VictNotFound,
    SelfChar,
    SelfOthers,
    VictCharFound,
    VictOthersFound,
    VictVictFound,
    VictCharBodyFound,
    VictOthersBodyFound,
    VictVictBodyFound,
    ObjCharFound,
    ObjOthersFound,
}

static STATES: OnceLock<Mutex<std::collections::HashMap<ConnId, AeditState>>> = OnceLock::new();

fn states() -> &'static Mutex<std::collections::HashMap<ConnId, AeditState>> {
    STATES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn with_state<R>(conn: ConnId, f: impl FnOnce(&mut AeditState) -> R) -> Option<R> {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.get_mut(&conn).map(f)
}

fn take_state(conn: ConnId) -> Option<AeditState> {
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

fn set_state(conn: ConnId, st: AeditState) {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.insert(conn, st);
}

// ---------------------------------------------------------------------------
// is_abbrev / find lookups against the editable table.
// ---------------------------------------------------------------------------

fn is_abbrev(part: &str, full: &str) -> bool {
    !part.is_empty() && full.to_lowercase().starts_with(&part.to_lowercase())
}

/// First action whose command has `arg` as an abbreviation (C aedit walk).
fn find_action_abbrev(arg: &str) -> Option<usize> {
    let guard = match soc_list().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.iter().position(|a| is_abbrev(arg, &a.command))
}

/// Next abbreviation match strictly after `from` (the AEDIT 'n' rescan).
fn find_action_abbrev_after(arg: &str, from: usize) -> Option<usize> {
    let guard = match soc_list().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let start = from + 1;
    (start..guard.len()).find(|&i| is_abbrev(arg, &guard[i].command))
}

fn command_at(rnum: usize) -> Option<String> {
    let guard = match soc_list().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.get(rnum).map(|a| a.command.clone())
}

fn action_at(rnum: usize) -> Option<SocialAction> {
    let guard = match soc_list().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.get(rnum).cloned()
}

/// True if the typed word resolves to an existing hard command (C find_command
/// returns > NOTHING). find_command walks the command list looking for the first
/// row whose name is prefixed by `word`; we mirror that against CMD_INFO,
/// skipping the RESERVED sentinel and the "\n" terminator.
fn command_exists(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let w = word.to_lowercase();
    crate::command_table::CMD_INFO.iter().any(|cmd| {
        cmd.name != "RESERVED" && cmd.name != "\n" && cmd.name.to_lowercase().starts_with(&w)
    })
}

// ---------------------------------------------------------------------------
// Command entry: do_aedit (the aedit.c-relevant slice of olc.c do_olc).
// ---------------------------------------------------------------------------

pub fn do_aedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        return;
    }
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };

    let lib_path = g.config.lib_path.clone();
    ensure_loaded(&lib_path);

    let (buf1, _rest) = crate::interpreter::one_argument(arg);
    if buf1.is_empty() {
        send(g, ch, "Specify an action to edit.\r\n");
        return;
    }

    // "save" — flush all actions to disk (do_olc save path for AEDIT).
    if is_abbrev(&buf1, "save") {
        send(g, ch, "Saving all actions.\r\n");
        let name = g
            .get_char(ch)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        log::info!("OLC: {} saves all actions.", name);
        aedit_save_to_disk(&lib_path);
        return;
    }

    // Set up base edit state (OLC_NUM=0, OLC_STORAGE=buf1) and walk for a match.
    set_state(
        conn,
        AeditState {
            ch,
            action: SocialAction::default(),
            rnum: None,
            storage: buf1.clone(),
            changed: false,
            mode: AeditMode::MainMenu,
        },
    );
    olc::set_active(conn, EditorKind::Aedit);

    crate::act::act(
        g,
        "$n starts using OLC.",
        true,
        ch,
        None,
        crate::act::ActArg::None,
        crate::act::To::Room,
    );

    match find_action_abbrev(&buf1) {
        Some(r) => {
            with_state(conn, |st| {
                st.rnum = Some(r);
                st.mode = AeditMode::ConfirmEdit;
            });
            let cmd = command_at(r).unwrap_or_default();
            send(
                g,
                ch,
                &format!("Do you wish to edit the '{}' action? ", cmd),
            );
        }
        None => {
            if command_exists(&buf1) {
                cleanup(conn);
                send(g, ch, "That command already exists.\r\n");
                return;
            }
            with_state(conn, |st| st.mode = AeditMode::ConfirmAdd);
            send(
                g,
                ch,
                &format!("Do you wish to add the '{}' action? ", buf1),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Setup helpers (aedit_setup_new / aedit_setup_existing).
// ---------------------------------------------------------------------------

fn aedit_setup_new(g: &mut GameState, conn: ConnId) {
    let storage = with_state(conn, |st| st.storage.clone()).unwrap_or_default();
    with_state(conn, |st| {
        st.action = SocialAction {
            command: storage.clone(),
            sort_as: storage.clone(),
            hide: false,
            min_victim_position: POS_STANDING,
            min_char_position: POS_STANDING,
            min_level_char: 0,
            char_no_arg: Some("This action is unfinished.".to_string()),
            others_no_arg: Some("This action is unfinished.".to_string()),
            ..SocialAction::default()
        };
        st.rnum = None; // new
        st.changed = false;
    });
    aedit_disp_menu(g, conn);
}

fn aedit_setup_existing(g: &mut GameState, conn: ConnId, rnum: usize) {
    if let Some(action) = action_at(rnum) {
        with_state(conn, |st| {
            st.action = action;
            st.rnum = Some(rnum);
            st.changed = false;
        });
    }
    aedit_disp_menu(g, conn);
}

// ---------------------------------------------------------------------------
// Menu rendering (aedit_disp_menu).
// ---------------------------------------------------------------------------

fn pos_name(i: i32) -> &'static str {
    let idx = i.clamp(0, (POSITION_TYPES.len() - 2) as i32) as usize;
    POSITION_TYPES[idx]
}

fn fld(o: &Option<String>) -> &str {
    match o {
        Some(s) => s.as_str(),
        None => "<Null>",
    }
}

fn aedit_disp_menu(g: &mut GameState, conn: ConnId) {
    let a = match with_state(conn, |st| {
        st.mode = AeditMode::MainMenu;
        st.action.clone()
    }) {
        Some(a) => a,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };

    // Truncate command/sort_as to 15 chars like the C "%-15.15s".
    let cmd15 = trunc(&a.command, 15);
    let sort15 = trunc(&a.sort_as, 15);

    let menu = format!(
        "\
&n-- Action editor\r\n\r\n\
&gn&n) Command         : &y{:<15}&n &g1&n) Sort as Command  : &y{:<15}&n\r\n\
&g2&n) Min Position[CH]: &c{:<8}        &g3&n) Min Position [VT]: &c{:<8}\r\n\
&g4&n) Min Level   [CH]: &c{:<3}             &g5&n) Show if Invisible: &c{}\r\n\
&ga&n) Char    [NO ARG]: &c{}\r\n\
&gb&n) Others  [NO ARG]: &c{}\r\n\
&gc&n) Char [NOT FOUND]: &c{}\r\n\
&gd&n) Char  [ARG SELF]: &c{}\r\n\
&ge&n) Others[ARG SELF]: &c{}\r\n\
&gf&n) Char      [VICT]: &c{}\r\n\
&gg&n) Others    [VICT]: &c{}\r\n\
&gh&n) Victim    [VICT]: &c{}\r\n\
&gi&n) Char  [BODY PRT]: &c{}\r\n\
&gj&n) Others[BODY PRT]: &c{}\r\n\
&gk&n) Victim[BODY PRT]: &c{}\r\n\
&gl&n) Char       [OBJ]: &c{}\r\n\
&gm&n) Others     [OBJ]: &c{}\r\n\
&gq&n) Quit\r\n\r\nEnter choice: ",
        cmd15,
        sort15,
        pos_name(a.min_char_position),
        pos_name(a.min_victim_position),
        a.min_level_char,
        if a.hide { "HIDDEN" } else { "NOT HIDDEN" },
        fld(&a.char_no_arg),
        fld(&a.others_no_arg),
        fld(&a.not_found),
        fld(&a.char_auto),
        fld(&a.others_auto),
        fld(&a.char_found),
        fld(&a.others_found),
        fld(&a.vict_found),
        fld(&a.char_body_found),
        fld(&a.others_body_found),
        fld(&a.vict_body_found),
        fld(&a.char_obj_found),
        fld(&a.others_obj_found),
    );
    send(g, ch, &menu);
}

fn trunc(s: &str, n: usize) -> String {
    crate::text::utf8_prefix(s, n).to_string()
}

// ---------------------------------------------------------------------------
// The main loop (aedit_parse).
// ---------------------------------------------------------------------------

pub fn aedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match with_state(conn, |st| st.mode) {
        Some(m) => m,
        None => {
            olc::clear_active(conn);
            return;
        }
    };
    let arg = line; // aedit keeps full lines for message fields (not trimmed).
    let trimmed = line.trim();
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => {
            cleanup(conn);
            return;
        }
    };

    match mode {
        // ---- Confirmation states ---------------------------------------
        AeditMode::ConfirmSave => {
            match trimmed.chars().next() {
                Some('y') | Some('Y') => {
                    aedit_save_internally(g, conn);
                    let cmd = with_state(conn, |st| st.action.command.clone()).unwrap_or_default();
                    let name = g
                        .get_char(ch)
                        .map(|c| c.player.name.clone())
                        .unwrap_or_default();
                    log::info!("OLC: {} edits action {}", name, cmd);
                    send(g, ch, "Action saved to memory.\r\n");
                    cleanup(conn);
                }
                Some('n') | Some('N') => cleanup(conn),
                _ => g.send_to_char(
                    ch,
                    "Invalid choice!\r\nDo you wish to save this action internally? ",
                ),
            }
            return;
        }

        AeditMode::ConfirmEdit => {
            match trimmed.chars().next() {
                Some('y') | Some('Y') => {
                    let r = with_state(conn, |st| st.rnum).flatten().unwrap_or(0);
                    aedit_setup_existing(g, conn, r);
                }
                Some('q') | Some('Q') => cleanup(conn),
                Some('n') | Some('N') => {
                    // Walk to the next abbreviation match.
                    let (storage, cur) =
                        with_state(conn, |st| (st.storage.clone(), st.rnum.unwrap_or(0)))
                            .unwrap_or_default();
                    match find_action_abbrev_after(&storage, cur) {
                        Some(next) => {
                            with_state(conn, |st| {
                                st.rnum = Some(next);
                                st.mode = AeditMode::ConfirmEdit;
                            });
                            let cmd = command_at(next).unwrap_or_default();
                            g.send_to_char(
                                ch,
                                &format!("Do you wish to edit the '{}' action? ", cmd),
                            );
                        }
                        None => {
                            // Past the end: offer to add (unless a real command).
                            if command_exists(&storage) {
                                cleanup(conn);
                            } else {
                                with_state(conn, |st| st.mode = AeditMode::ConfirmAdd);
                                g.send_to_char(
                                    ch,
                                    &format!("Do you wish to add the '{}' action? ", storage),
                                );
                            }
                        }
                    }
                }
                _ => {
                    let cmd = with_state(conn, |st| st.rnum)
                        .flatten()
                        .and_then(command_at)
                        .unwrap_or_default();
                    g.send_to_char(
                        ch,
                        &format!(
                            "Invalid choice!\r\nDo you wish to edit the '{}' action? ",
                            cmd
                        ),
                    );
                }
            }
            return;
        }

        AeditMode::ConfirmAdd => {
            match trimmed.chars().next() {
                Some('y') | Some('Y') => aedit_setup_new(g, conn),
                Some('n') | Some('N') | Some('q') | Some('Q') => cleanup(conn),
                _ => {
                    let storage = with_state(conn, |st| st.storage.clone()).unwrap_or_default();
                    g.send_to_char(
                        ch,
                        &format!(
                            "Invalid choice!\r\nDo you wish to add the '{}' action? ",
                            storage
                        ),
                    );
                }
            }
            return;
        }

        // ---- Main menu --------------------------------------------------
        AeditMode::MainMenu => {
            match trimmed.chars().next() {
                Some('q') | Some('Q') => {
                    let changed = with_state(conn, |st| st.changed).unwrap_or(false);
                    if changed {
                        send(g, ch, "Do you wish to save this action internally? ");
                        with_state(conn, |st| st.mode = AeditMode::ConfirmSave);
                    } else {
                        cleanup(conn);
                    }
                }
                Some('n') => {
                    send(g, ch, "Enter action name: ");
                    with_state(conn, |st| st.mode = AeditMode::ActionName);
                }
                Some('1') => {
                    g.send_to_char(
                        ch,
                        "Enter sort info for this action (for the command listing): ",
                    );
                    with_state(conn, |st| st.mode = AeditMode::SortAs);
                }
                Some('2') => {
                    send(
                        g,
                        ch,
                        "Enter the minimum position the Character has to be in to activate social [0 - 8]: ",
                    );
                    with_state(conn, |st| st.mode = AeditMode::MinCharPos);
                }
                Some('3') => {
                    send(
                        g,
                        ch,
                        "Enter the minimum position the Victim has to be in to activate social [0 - 8]: ",
                    );
                    with_state(conn, |st| st.mode = AeditMode::MinVictPos);
                }
                Some('4') => {
                    send(g, ch, "Enter new minimum level for social: ");
                    with_state(conn, |st| st.mode = AeditMode::MinCharLevel);
                }
                Some('5') => {
                    with_state(conn, |st| {
                        st.action.hide = !st.action.hide;
                        st.changed = true;
                    });
                    aedit_disp_menu(g, conn);
                }
                Some('a') | Some('A') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::NovictChar,
                    "Enter social shown to the Character when there is no argument supplied.",
                    |a| &a.char_no_arg,
                ),
                Some('b') | Some('B') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::NovictOthers,
                    "Enter social shown to Others when there is no argument supplied.",
                    |a| &a.others_no_arg,
                ),
                Some('c') | Some('C') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictNotFound,
                    "Enter text shown to the Character when his victim isnt found.",
                    |a| &a.not_found,
                ),
                Some('d') | Some('D') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::SelfChar,
                    "Enter social shown to the Character when it is its own victim.",
                    |a| &a.char_auto,
                ),
                Some('e') | Some('E') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::SelfOthers,
                    "Enter social shown to Others when the Char is its own victim.",
                    |a| &a.others_auto,
                ),
                Some('f') | Some('F') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictCharFound,
                    "Enter normal social shown to the Character when the victim is found.",
                    |a| &a.char_found,
                ),
                Some('g') | Some('G') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictOthersFound,
                    "Enter normal social shown to Others when the victim is found.",
                    |a| &a.others_found,
                ),
                Some('h') | Some('H') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictVictFound,
                    "Enter normal social shown to the Victim when the victim is found.",
                    |a| &a.vict_found,
                ),
                Some('i') | Some('I') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictCharBodyFound,
                    "Enter 'body part' social shown to the Character when the victim is found.",
                    |a| &a.char_body_found,
                ),
                Some('j') | Some('J') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictOthersBodyFound,
                    "Enter 'body part' social shown to Others when the victim is found.",
                    |a| &a.others_body_found,
                ),
                Some('k') | Some('K') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::VictVictBodyFound,
                    "Enter 'body part' social shown to the Victim when the victim is found.",
                    |a| &a.vict_body_found,
                ),
                Some('l') | Some('L') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::ObjCharFound,
                    "Enter 'object' social shown to the Character when the object is found.",
                    |a| &a.char_obj_found,
                ),
                Some('m') | Some('M') => prompt_field(
                    g,
                    conn,
                    ch,
                    AeditMode::ObjOthersFound,
                    "Enter 'object' social shown to the Room when the object is found.",
                    |a| &a.others_obj_found,
                ),
                _ => aedit_disp_menu(g, conn),
            }
            return;
        }

        // ---- Name / sort fields ----------------------------------------
        AeditMode::ActionName => {
            if !trimmed.is_empty() {
                if trimmed.contains(' ') {
                    aedit_disp_menu(g, conn);
                    return;
                }
                with_state(conn, |st| st.action.command = trimmed.to_string());
            } else {
                aedit_disp_menu(g, conn);
                return;
            }
        }
        AeditMode::SortAs => {
            if !trimmed.is_empty() {
                if trimmed.contains(' ') {
                    aedit_disp_menu(g, conn);
                    return;
                }
                with_state(conn, |st| st.action.sort_as = trimmed.to_string());
            } else {
                aedit_disp_menu(g, conn);
                return;
            }
        }

        // ---- Numeric fields --------------------------------------------
        AeditMode::MinCharPos | AeditMode::MinVictPos => {
            if !trimmed.is_empty() {
                let Some(i) = crate::olc::parse_i32_input(g, conn, trimmed, -1) else {
                    return;
                };
                // C bug-faithful guard: `(i < 0) && (i > POS_STANDING)` is never
                // true, so any parse passes through. We keep the practical clamp
                // the data demands: 0..=POS_STANDING.
                let v = i.clamp(0, POS_STANDING);
                with_state(conn, |st| {
                    if mode == AeditMode::MinCharPos {
                        st.action.min_char_position = v;
                    } else {
                        st.action.min_victim_position = v;
                    }
                });
            } else {
                aedit_disp_menu(g, conn);
                return;
            }
        }
        AeditMode::MinCharLevel => {
            if !trimmed.is_empty() {
                let Some(i) = crate::olc::parse_i32_input(g, conn, trimmed, 0) else {
                    return;
                };
                let v = i.clamp(0, LVL_IMPL as i32);
                with_state(conn, |st| st.action.min_level_char = v);
            } else {
                aedit_disp_menu(g, conn);
                return;
            }
        }

        // ---- Message fields (delete_doubledollar, empty => NULL) --------
        AeditMode::NovictChar => set_msg(conn, arg, |a, v| a.char_no_arg = v),
        AeditMode::NovictOthers => set_msg(conn, arg, |a, v| a.others_no_arg = v),
        AeditMode::VictCharFound => set_msg(conn, arg, |a, v| a.char_found = v),
        AeditMode::VictOthersFound => set_msg(conn, arg, |a, v| a.others_found = v),
        AeditMode::VictVictFound => set_msg(conn, arg, |a, v| a.vict_found = v),
        AeditMode::VictNotFound => set_msg(conn, arg, |a, v| a.not_found = v),
        AeditMode::SelfChar => set_msg(conn, arg, |a, v| a.char_auto = v),
        AeditMode::SelfOthers => set_msg(conn, arg, |a, v| a.others_auto = v),
        AeditMode::VictCharBodyFound => set_msg(conn, arg, |a, v| a.char_body_found = v),
        AeditMode::VictOthersBodyFound => set_msg(conn, arg, |a, v| a.others_body_found = v),
        AeditMode::VictVictBodyFound => set_msg(conn, arg, |a, v| a.vict_body_found = v),
        AeditMode::ObjCharFound => set_msg(conn, arg, |a, v| a.char_obj_found = v),
        AeditMode::ObjOthersFound => set_msg(conn, arg, |a, v| a.others_obj_found = v),
    }

    // END OF CASE: something changed; return to the main menu.
    with_state(conn, |st| st.changed = true);
    aedit_disp_menu(g, conn);
}

/// Print the "[OLD]: x\r\n[NEW]: " prompt for a message field and switch mode.
fn prompt_field(
    g: &mut GameState,
    conn: ConnId,
    ch: CharId,
    next: AeditMode,
    label: &str,
    get: impl Fn(&SocialAction) -> &Option<String>,
) {
    let old = with_state(conn, |st| {
        get(&st.action)
            .clone()
            .unwrap_or_else(|| "NULL".to_string())
    })
    .unwrap_or_else(|| "NULL".to_string());
    send(g, ch, &format!("{}\r\n[OLD]: {}\r\n[NEW]: ", label, old));
    with_state(conn, |st| st.mode = next);
}

/// Set a message field from the typed line: delete_doubledollar, and an empty
/// line stores NULL (None), matching the C field setters.
fn set_msg(conn: ConnId, arg: &str, apply: impl FnOnce(&mut SocialAction, Option<String>)) {
    let val = if arg.is_empty() {
        None
    } else {
        Some(delete_doubledollar(arg))
    };
    with_state(conn, |st| apply(&mut st.action, val));
}

// ---------------------------------------------------------------------------
// Internal save (aedit_save_internally) + disk save (aedit_save_to_disk).
// ---------------------------------------------------------------------------

fn aedit_save_internally(g: &mut GameState, conn: ConnId) {
    let (action, rnum) = match with_state(conn, |st| (st.action.clone(), st.rnum)) {
        Some(v) => v,
        None => return,
    };
    {
        let mut guard = match soc_list().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match rnum {
            Some(r) if r < guard.len() => {
                guard[r] = action;
            }
            _ => {
                // New action: appended (C appends at top_of_socialt+1).
                guard.push(action);
            }
        }
    }
    let lib_path = g.config.lib_path.clone();
    aedit_save_to_disk(&lib_path);
    let socials_path = format!("{}/{}", lib_path.trim_end_matches('/'), SOCMESS_REL);
    if let Err(e) = crate::cmd_social::reload_socials(Some(&socials_path)) {
        log::warn!(
            "SYSERR: can't reload socials file '{}': {}",
            socials_path,
            e
        );
    }
}

/// Write the entire social table back to misc/socials (C aedit_save_to_disk).
/// olc.rs 'olc aedit save' entry (#275).
pub fn save_all_actions(g: &mut GameState) {
    let lib = g.config.lib_path.clone();
    aedit_save_to_disk(&lib);
}

fn aedit_save_to_disk(lib_path: &str) {
    let path = format!("{}/{}", lib_path.trim_end_matches('/'), SOCMESS_REL);

    let guard = match soc_list().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let mut out = String::new();
    let h = |o: &Option<String>| -> String {
        match o {
            Some(s) => s.clone(),
            None => "#".to_string(),
        }
    };
    for a in guard.iter() {
        // "~%s %s %d %d %d %d\n" — note the field order: hide, min_char_pos,
        // min_vict_pos, min_level (exactly C aedit_save_to_disk).
        out.push_str(&format!(
            "~{} {} {} {} {} {}\n",
            a.command,
            a.sort_as,
            if a.hide { 1 } else { 0 },
            a.min_char_position,
            a.min_victim_position,
            a.min_level_char,
        ));
        // Four-then-four-then-three-then-two blocks, exactly as C.
        out.push_str(&format!(
            "{}\n{}\n{}\n{}\n",
            h(&a.char_no_arg),
            h(&a.others_no_arg),
            h(&a.char_found),
            h(&a.others_found),
        ));
        out.push_str(&format!(
            "{}\n{}\n{}\n{}\n",
            h(&a.vict_found),
            h(&a.not_found),
            h(&a.char_auto),
            h(&a.others_auto),
        ));
        out.push_str(&format!(
            "{}\n{}\n{}\n",
            h(&a.char_body_found),
            h(&a.others_body_found),
            h(&a.vict_body_found),
        ));
        out.push_str(&format!(
            "{}\n{}\n\n",
            h(&a.char_obj_found),
            h(&a.others_obj_found),
        ));
    }
    out.push_str("$\n");

    if std::fs::write(&path, &out).is_err() {
        log::warn!("SYSERR: Can't open socials file '{}'", path);
    }
}

/// delete_doubledollar: collapse "$$" to "$".
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
