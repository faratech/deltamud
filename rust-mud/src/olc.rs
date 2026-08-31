// olc.rs — the OasisOLC shared framework (CircleMUD olc.c), ported to the
// id-indexed single-owner GameState.
//
// This module OWNS the cross-editor plumbing every OLC sub-editor plugs into:
//
//   * `EditorKind` — which editor a connection is in.
//   * `set_active` / `clear_active` / `in_olc` / `active_editor` — the
//     per-connection "am I in OLC?" registry (game.rs routes input here when
//     `in_olc(conn)` is true).
//   * `olc_input` — the master per-line router: it looks up the active editor
//     and forwards the line to that editor's `<kind>_parse(g, conn, line)`.
//   * the OLC save-list (`olc_add_to_save_list` / `olc_remove_from_save_list`
//     / `olc_saveinfo`) and the on-disk save dispatcher (`olc_save_to_disk`).
//   * `do_olc` — the immortal command (`olc` / `redit` / `oedit` / ... ) that
//     starts an editor or saves a zone, gated on subcmd (SCMD_OLC_*).
//   * shared menu helpers (`sprintbit` / `sprinttype` / `strip_cr`) and the
//     OLC color constants every editor renders with.
//
// The C code stashed the editor and its working data in `d->olc` /
// `STATE(d)`; here neither Descriptor nor GameState may carry an OLC field, so
// the active-editor map lives in a module-static keyed by ConnId, and each
// sub-editor (redit/oedit/...) keeps its own per-connection edit state the same
// way. `olc_input` is the only place that knows the full editor set, so the
// router stays the single source of truth for dispatch.
//
// Public-type contract shared with the sibling editors: the save-kind tags are
// plain `i32`, `real_zone` returns the loaded-zone *index* (rnum), and the
// color constants are raw ANSI (connection.rs forwards them untouched).

use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// SCMD_OLC_* — must match command_table.rs's private constants exactly.
// (redit=0 oedit=1 zedit=2 medit=3 sedit=4 trigedit=5 hedit=6 aedit=7 info=8)
// ---------------------------------------------------------------------------
pub const SCMD_OLC_REDIT: i32 = 0;
pub const SCMD_OLC_OEDIT: i32 = 1;
pub const SCMD_OLC_ZEDIT: i32 = 2;
pub const SCMD_OLC_MEDIT: i32 = 3;
pub const SCMD_OLC_SEDIT: i32 = 4;
pub const SCMD_OLC_TRIGEDIT: i32 = 5;
pub const SCMD_OLC_HEDIT: i32 = 6;
pub const SCMD_OLC_AEDIT: i32 = 7;
pub const SCMD_OLC_SAVEINFO: i32 = 8;

// ---------------------------------------------------------------------------
// OLC_SAVE_* — save-list component tags (olc.h). `SAVE_INFO_MSG[]` is indexed
// by these. Plain i32 to match the shared editor contract.
// ---------------------------------------------------------------------------
pub const OLC_SAVE_ROOM: i32 = 0;
pub const OLC_SAVE_OBJ: i32 = 1;
pub const OLC_SAVE_ZONE: i32 = 2;
pub const OLC_SAVE_MOB: i32 = 3;
pub const OLC_SAVE_SHOP: i32 = 4;
pub const OLC_SAVE_HELP: i32 = 5;
pub const OLC_SAVE_ACTION: i32 = 6;

/// `save_info_msg[]` (olc.c) — human label per OLC_SAVE_* tag.
pub const SAVE_INFO_MSG: [&str; 7] = [
    "Rooms",
    "Objects",
    "Zone info",
    "Mobiles",
    "Shops",
    "Help",
    "Actions",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DgScriptEditMode {
    Main,
    New,
    Delete,
}

// ---------------------------------------------------------------------------
// OLC color cols. The C get_char_cols() set per-char color pointers; the
// editors render with raw ANSI escapes that connection.rs forwards untouched.
// ---------------------------------------------------------------------------
pub const NRM: &str = "\x1b[0m";
pub const GRN: &str = "\x1b[0;32m";
pub const CYN: &str = "\x1b[0;36m";
pub const YEL: &str = "\x1b[0;33m";

// ---------------------------------------------------------------------------
// EditorKind — which OLC editor a connection is currently driving.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKind {
    Redit,
    Oedit,
    Medit,
    Zedit,
    Sedit,
    Aedit,
    Hedit,
    Trigedit,
}

// ---------------------------------------------------------------------------
// Active-editor registry (replaces STATE(d) == CON_*EDIT). Keyed by ConnId.
// ---------------------------------------------------------------------------
fn active() -> &'static Mutex<HashMap<ConnId, EditorKind>> {
    static ACTIVE: OnceLock<Mutex<HashMap<ConnId, EditorKind>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mark `conn` as actively editing in `kind`. Called by each `do_X` on entry.
pub fn set_active(conn: ConnId, kind: EditorKind) {
    active().lock().unwrap().insert(conn, kind);
}

/// Clear `conn`'s OLC editor (called on save/quit by each editor's parser).
pub fn clear_active(conn: ConnId) {
    active().lock().unwrap().remove(&conn);
}

/// Abort whatever OLC editor `conn` is in WITHOUT saving, then clear active.
/// Called when a descriptor goes away mid-edit (Game::disconnect): the C MUD's
/// `free_olc` / connection teardown drops the editor's working copy so the
/// edited vnum's lock is released and the per-conn state doesn't leak until the
/// next reboot. Dispatches to the owning editor's `abort(conn)`, which removes
/// the conn's working copy (and any text buffer) from that editor's per-conn
/// map. No-op if the conn isn't editing.
pub fn abort_editor(conn: ConnId) {
    if let Some(kind) = active_editor(conn) {
        match kind {
            EditorKind::Redit => crate::redit::abort(conn),
            EditorKind::Oedit => crate::oedit::abort(conn),
            EditorKind::Medit => crate::medit::abort(conn),
            EditorKind::Zedit => crate::zedit::abort(conn),
            EditorKind::Sedit => crate::sedit::abort(conn),
            EditorKind::Aedit => crate::aedit::abort(conn),
            EditorKind::Hedit => crate::hedit::abort(conn),
            EditorKind::Trigedit => crate::trigedit::abort(conn),
        }
    }
    clear_active(conn);
}

/// True if `conn` is currently inside any OLC editor. game.rs consults this to
/// route raw input into `olc_input` instead of the command interpreter.
pub fn in_olc(conn: ConnId) -> bool {
    active().lock().unwrap().contains_key(&conn)
}

/// The currently-active editor kind for `conn`, if any.
pub fn active_editor(conn: ConnId) -> Option<EditorKind> {
    active().lock().unwrap().get(&conn).copied()
}

/// Master input router (CircleMUD: the `case CON_*EDIT:` block of nanny()).
/// Forwards one input line to the active editor's `<kind>_parse`. Does nothing
/// if the connection is not in OLC (defensive — game.rs should gate on
/// `in_olc` first).
pub fn olc_input(g: &mut GameState, conn: ConnId, line: &str) {
    let kind = match active_editor(conn) {
        Some(k) => k,
        None => return,
    };
    match kind {
        EditorKind::Redit => crate::redit::redit_parse(g, conn, line),
        EditorKind::Oedit => crate::oedit::oedit_parse(g, conn, line),
        EditorKind::Medit => crate::medit::medit_parse(g, conn, line),
        EditorKind::Zedit => crate::zedit::zedit_parse(g, conn, line),
        EditorKind::Sedit => crate::sedit::sedit_parse(g, conn, line),
        EditorKind::Aedit => crate::aedit::aedit_parse(g, conn, line),
        EditorKind::Hedit => crate::hedit::hedit_parse(g, conn, line),
        EditorKind::Trigedit => crate::trigedit::trigedit_parse(g, conn, line),
    }
}

// ===========================================================================
// OLC save list (olc.c olc_save_list). A global list of (zone, component) pairs
// that have been edited in memory but not yet written to disk.
// ===========================================================================
fn save_list() -> &'static Mutex<Vec<(i32, i32)>> {
    static SAVE_LIST: OnceLock<Mutex<Vec<(i32, i32)>>> = OnceLock::new();
    SAVE_LIST.get_or_init(|| Mutex::new(Vec::new()))
}

/// olc_add_to_save_list: record that `zone` (the builder zone *number*, not
/// rnum) has unsaved `kind` changes. No-op if already present.
pub fn olc_add_to_save_list(zone: i32, kind: i32) {
    let mut list = save_list().lock().unwrap();
    if !list.iter().any(|&(z, t)| z == zone && t == kind) {
        // C prepends; order only matters for olc_saveinfo display, where we
        // iterate the whole list, so prepend to mirror C exactly.
        list.insert(0, (zone, kind));
    }
}

/// olc_remove_from_save_list: drop the (zone, kind) entry once it is on disk.
pub fn olc_remove_from_save_list(zone: i32, kind: i32) {
    save_list()
        .lock()
        .unwrap()
        .retain(|&(z, t)| !(z == zone && t == kind));
}


/// C act.wizard.c:1927-1990 / comm.c:458-510: before copyover or shutdown,
/// every entry on the save list is written to disk. Wired into do_copyover
/// and the Game shutdown path; unsaved redit/oedit work would otherwise be
/// lost on a routine reboot (#262).
pub fn flush_save_list_to_disk(g: &mut GameState) {
    let entries: Vec<(i32, i32)> = save_list().lock().unwrap().clone();
    for (zone, kind) in entries {
        let zone_rnum = match real_zone(g, zone * 100) {
            Some(z) => z,
            None => continue,
        };
        match kind {
            OLC_SAVE_ROOM => crate::redit::redit_save_to_disk(g, zone_rnum),
            OLC_SAVE_OBJ => crate::oedit::oedit_save_to_disk(g, zone_rnum),
            OLC_SAVE_MOB => crate::medit::medit_save_to_disk(g, zone_rnum),
            OLC_SAVE_ZONE => crate::zedit::zedit_save_to_disk(g, zone_rnum),
            OLC_SAVE_SHOP => crate::sedit::sedit_save_zone_to_disk(g, zone_rnum),
            _ => {}
        }
        log::info!("OLC: Reboot saving for zone {}.", zone);
    }
}

/// olc_saveinfo: tell the immortal which OLC components still need saving.
pub fn olc_saveinfo(g: &mut GameState, ch: CharId) {
    let entries: Vec<(i32, i32)> = save_list().lock().unwrap().clone();
    if entries.is_empty() {
        g.send_to_char(ch, "The database is up to date.\r\n");
        return;
    }
    let mut out = String::from("The following OLC components need saving:\r\n");
    let mut any = false;
    for (zone, kind) in entries {
        let line = match kind {
            OLC_SAVE_HELP => " - Help Entries.\r\n".to_string(),
            OLC_SAVE_ACTION => " - Actions.\r\n".to_string(),
            t if (t as usize) < SAVE_INFO_MSG.len() => {
                format!(" - {} for zone {}.\r\n", SAVE_INFO_MSG[t as usize], zone)
            }
            _ => continue,
        };
        out.push_str(&line);
        any = true;
    }
    if any {
        g.send_to_char(ch, &out);
    } else {
        g.send_to_char(ch, "The database is up to date.\r\n");
    }
}

// ===========================================================================
// real_zone: find the loaded-zone *index* (rnum) owning a vnum (olc.c).
// DeltaMUD zones own the vnum band [number*100 .. top]. Returns None if no zone
// owns it.
// ===========================================================================
pub fn real_zone(g: &GameState, vnum: i32) -> Option<usize> {
    if vnum < 0 {
        return None;
    }
    g.zones
        .iter()
        .position(|z| vnum >= z.number * 100 && vnum <= z.top)
}

/// True when `ch` may edit the loaded zone at `zone_rnum`.
pub fn can_edit_zone(g: &GameState, ch: CharId, zone_rnum: usize) -> bool {
    let Some(c) = g.get_char(ch) else {
        return false;
    };
    if c.player.level >= LVL_IMPL {
        return true;
    }
    let Some(zone) = g.zones.get(zone_rnum) else {
        return false;
    };
    crate::handler::isname(&c.player.name, &zone.builders)
}

/// Shared DG script-list menu used by redit/oedit/medit. This is the Rust
/// analogue of dg_olc.c `dg_script_menu`: edit a prototype entity's attached
/// trigger vnum list.
pub fn dg_script_menu(g: &mut GameState, conn: ConnId, kind: i32, entity_vnum: i32) {
    let mut out = String::from("     Script Editor\r\n\r\n     Trigger List:\r\n");
    let triggers = crate::dg_db_scripts::proto_trigger_vnums(kind, entity_vnum);
    if triggers.is_empty() {
        out.push_str("     <none>\r\n");
    } else {
        for (idx, trig_vnum) in triggers.iter().enumerate() {
            let (name, mismatch) = {
                let rnum = crate::dg_db_scripts::real_trigger(*trig_vnum);
                if rnum < 0 {
                    ("unknown trigger".to_string(), true)
                } else {
                    match crate::dg_db_scripts::trig_proto(rnum as usize) {
                        Some(proto) => (proto.name, proto.attach_type != kind),
                        None => ("unknown trigger".to_string(), true),
                    }
                }
            };
            out.push_str(&format!(
                "     {:2}) [{}{}{}] {}{}{}",
                idx + 1,
                CYN,
                trig_vnum,
                NRM,
                CYN,
                name,
                NRM
            ));
            if mismatch {
                out.push_str(&format!(
                    "   {}** Mis-matched Trigger Type **{}\r\n",
                    GRN, NRM
                ));
            } else {
                out.push_str("\r\n");
            }
        }
    }
    out.push_str(&format!(
        "\r\n {}N{})  New trigger for this script\r\n\
         {}D{})  Delete a trigger in this script\r\n\
         {}X{})  Exit Script Editor\r\n\r\n\
             Enter choice :",
        GRN, NRM, GRN, NRM, GRN, NRM
    ));
    send_to_conn(g, conn, &out);
}

/// Parse one line of the shared DG script-list editor. Returns false when the
/// user exits back to the owning editor's main menu.
pub fn dg_script_edit_parse(
    g: &mut GameState,
    conn: ConnId,
    kind: i32,
    entity_vnum: i32,
    mode: &mut DgScriptEditMode,
    line: &str,
) -> bool {
    match *mode {
        DgScriptEditMode::Main => {
            match line.trim().chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('x') => return false,
                Some('n') => {
                    send_to_conn(g, conn, "\r\nPlease enter position, vnum   (ex: 1, 200):");
                    *mode = DgScriptEditMode::New;
                }
                Some('d') => {
                    send_to_conn(g, conn, "     Which entry should be deleted?  0 to abort :");
                    *mode = DgScriptEditMode::Delete;
                }
                _ => dg_script_menu(g, conn, kind, entity_vnum),
            }
        }
        DgScriptEditMode::New => {
            let Some((pos, trig_vnum)) = parse_script_position_vnum(line) else {
                dg_script_menu(g, conn, kind, entity_vnum);
                *mode = DgScriptEditMode::Main;
                return true;
            };
            if pos == 0 || trig_vnum == 0 {
                dg_script_menu(g, conn, kind, entity_vnum);
                *mode = DgScriptEditMode::Main;
                return true;
            }
            if crate::dg_db_scripts::real_trigger(trig_vnum) < 0 {
                send_to_conn(
                    g,
                    conn,
                    "Invalid Trigger VNUM!\r\nPlease enter position, vnum   (ex: 1, 200):",
                );
                return true;
            }
            if !can_edit_trigger_zone(g, conn, trig_vnum) {
                send_to_conn(
                    g,
                    conn,
                    "You do not have permissions to that zone.\r\nPlease enter position, vnum   (ex: 1, 200):",
                );
                return true;
            }
            if crate::dg_db_scripts::insert_proto_trigger(kind, entity_vnum, trig_vnum, pos) {
                mark_dg_script_dirty(g, kind, entity_vnum);
            }
            *mode = DgScriptEditMode::Main;
            dg_script_menu(g, conn, kind, entity_vnum);
        }
        DgScriptEditMode::Delete => {
            let pos = line.trim().parse::<usize>().unwrap_or(0);
            if pos != 0 && crate::dg_db_scripts::remove_proto_trigger_at(kind, entity_vnum, pos) {
                mark_dg_script_dirty(g, kind, entity_vnum);
            }
            *mode = DgScriptEditMode::Main;
            dg_script_menu(g, conn, kind, entity_vnum);
        }
    }
    true
}

fn parse_script_position_vnum(line: &str) -> Option<(usize, i32)> {
    let nums: Vec<i32> = line
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<i32>().ok())
        .collect();
    match nums.as_slice() {
        [vnum] => Some((999, *vnum)),
        [pos, vnum, ..] => Some(((*pos).max(0) as usize, *vnum)),
        _ => None,
    }
}

fn can_edit_trigger_zone(g: &GameState, conn: ConnId, trig_vnum: i32) -> bool {
    let Some(ch) = g.descriptors.get(&conn).and_then(|d| d.character) else {
        return false;
    };
    if g.get_char(ch)
        .map(|c| c.player.level >= LVL_IMMORT)
        .unwrap_or(false)
    {
        return true;
    }
    real_zone(g, trig_vnum)
        .map(|zr| can_edit_zone(g, ch, zr))
        .unwrap_or(false)
}

fn mark_dg_script_dirty(g: &mut GameState, kind: i32, entity_vnum: i32) {
    let Some(zr) = real_zone(g, entity_vnum) else {
        return;
    };
    let Some(zone) = g.zones.get(zr) else {
        return;
    };
    let save_kind = match kind {
        crate::dg_handler::MOB_TRIGGER => OLC_SAVE_MOB,
        crate::dg_handler::OBJ_TRIGGER => OLC_SAVE_OBJ,
        crate::dg_handler::WLD_TRIGGER => OLC_SAVE_ROOM,
        _ => return,
    };
    olc_add_to_save_list(zone.number, save_kind);
}

fn send_to_conn(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(ch) = g.descriptors.get(&conn).and_then(|d| d.character) {
        g.send_to_char(ch, msg);
    } else if let Some(d) = g.descriptors.get_mut(&conn) {
        d.write(msg);
    }
}

// ===========================================================================
// olc_save_to_disk: the per-component save dispatcher (olc.c do_olc save arm).
// Writes a single zone's component to its CircleMUD world file and removes it
// from the save list. Rooms/objects are owned here; the zone/mob/shop editors
// autosave their working copy on quit, so an explicit `olc save` for those
// components only needs to drop the save-list entry so `olc` reports the
// database as up to date.
// ===========================================================================
pub fn olc_save_to_disk(g: &mut GameState, zone_rnum: usize, kind: i32) {
    match kind {
        OLC_SAVE_ROOM => crate::redit::redit_save_to_disk(g, zone_rnum),
        OLC_SAVE_OBJ => crate::oedit::oedit_save_to_disk(g, zone_rnum),
        OLC_SAVE_ZONE => crate::zedit::zedit_save_to_disk(g, zone_rnum),
        OLC_SAVE_MOB => crate::medit::medit_save_to_disk(g, zone_rnum),
        OLC_SAVE_SHOP => crate::sedit::sedit_save_zone_to_disk(g, zone_rnum),
        _ => {}
    }
}

// ===========================================================================
// Shared menu-rendering helpers used by every editor (utils.c sprintbit /
// sprinttype). Local copies because the per-command versions are private.
// ===========================================================================

/// sprintbit: name every set bit of `bits` using `table` (a "\n"-terminated
/// name list). Stops at the sentinel; unnamed bits past the table are skipped.
pub fn sprintbit(bits: i64, table: &[&str]) -> String {
    let mut out = String::new();
    for (i, n) in table.iter().enumerate() {
        if *n == "\n" {
            break;
        }
        if bits & (1 << i) != 0 {
            out.push_str(n);
            out.push(' ');
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

/// sprinttype: ordinal lookup into a "\n"-terminated name table.
pub fn sprinttype(t: i32, table: &[&str]) -> String {
    if t >= 0 && (t as usize) < table.len() && table[t as usize] != "\n" {
        table[t as usize].to_string()
    } else {
        "UNDEFINED".to_string()
    }
}

/// strip_string (olc.c): drop '\r' so a "\r\n"-bearing buffer writes Unix-style
/// to the world file (the loader re-adds CRLF semantics on read).
pub fn strip_cr(s: &str) -> String {
    s.chars().filter(|&c| c != '\r').collect()
}

// ===========================================================================
// do_olc — the OLC command interface (olc.c do_olc). Generic parsing, then a
// hand-off to the right sub-editor's `do_X`, or a save.
// ===========================================================================
pub fn do_olc(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    // No screwing around as a mobile.
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        return;
    }

    if subcmd == SCMD_OLC_SAVEINFO {
        olc_saveinfo(g, ch);
        return;
    }

    // Two-argument parse: buf1 = first word, buf2 = second word.
    let (buf1, rest) = crate::interpreter::half_chop(arg);
    let (buf2, _) = crate::interpreter::half_chop(&rest);

    let mut number: i32 = -1;
    let mut save = false;

    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    let in_room_vnum = g.char_room_vnum(ch).unwrap_or(NOWHERE);

    if buf1.is_empty() {
        // No argument given.
        match subcmd {
            SCMD_OLC_ZEDIT | SCMD_OLC_REDIT => {
                number = in_room_vnum;
            }
            SCMD_OLC_TRIGEDIT | SCMD_OLC_OEDIT | SCMD_OLC_MEDIT | SCMD_OLC_SEDIT => {
                let t = olc_type_word(subcmd);
                g.send_to_char(ch, &format!("Specify a {} VNUM to edit.\r\n", t));
                return;
            }
            SCMD_OLC_HEDIT => {
                g.send_to_char(ch, "Specify a help entry to edit.\r\n");
                return;
            }
            SCMD_OLC_AEDIT => {
                g.send_to_char(ch, "Specify an action to edit.\r\n");
                return;
            }
            _ => {}
        }
    } else if !buf1
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        // First arg is not a number. C: strn_cmp("save", buf1, 4) == 0 — the
        // first 4 chars of buf1 are "save" (i.e. buf1 begins with "save").
        if buf1.starts_with("save") {
            if subcmd == SCMD_OLC_HEDIT || subcmd == SCMD_OLC_AEDIT {
                save = true;
                number = 0;
            } else if buf2.is_empty() {
                g.send_to_char(ch, "Save which zone?\r\n");
                return;
            } else {
                save = true;
                number = buf2.parse::<i32>().unwrap_or(0) * 100;
            }
        } else if subcmd == SCMD_OLC_HEDIT || subcmd == SCMD_OLC_AEDIT {
            number = 0;
        } else if subcmd == SCMD_OLC_ZEDIT && level >= LVL_IMPL {
            if buf1.len() >= 3 && "new".starts_with(buf1.as_str()) && !buf2.is_empty() {
                // zedit new <num> — zedit autocreates on first edit, so hand the
                // number straight to do_zedit.
                crate::zedit::do_zedit(g, ch, &buf2, 0);
            } else {
                g.send_to_char(ch, "Specify a new zone number.\r\n");
            }
            return;
        } else {
            g.send_to_char(ch, "Yikes!  Stop that, someone will get hurt!\r\n");
            return;
        }
    }

    // If a numeric argument was given, parse it.
    if number == -1 && subcmd != SCMD_OLC_AEDIT && subcmd != SCMD_OLC_HEDIT {
        number = buf1.parse::<i32>().unwrap_or(-1);
    }

    // Resolve the zone rnum (skip for AEDIT and un-saved HEDIT, which are
    // action-/keyword-keyed rather than zone-keyed).
    let znum_rnum = if subcmd != SCMD_OLC_AEDIT {
        if subcmd == SCMD_OLC_HEDIT && !save {
            None
        } else {
            match real_zone(g, number) {
                Some(z) => Some(z),
                None => {
                    g.send_to_char(ch, "Sorry, there is no zone for that number!\r\n");
                    return;
                }
            }
        }
    } else {
        None
    };

    if level < LVL_IMPL {
        if let Some(zr) = znum_rnum {
            if !can_edit_zone(g, ch, zr) && subcmd != SCMD_OLC_HEDIT {
                g.send_to_char(ch, "You do not have permission to edit this zone.\r\n");
                return;
            }
        }
    }

    if save {
        match subcmd {
            SCMD_OLC_TRIGEDIT => {
                g.send_to_char(
                    ch,
                    "Triggers are autosaved to disk when edited, there's no need.\r\n",
                );
                return;
            }
            SCMD_OLC_HEDIT => {
                g.send_to_char(ch, "Saving all help entries.\r\n");
                return;
            }
            SCMD_OLC_AEDIT => {
                g.send_to_char(ch, "Saving all actions.\r\n");
                return;
            }
            _ => {}
        }
        let zr = match znum_rnum {
            Some(z) => z,
            None => {
                g.send_to_char(ch, "Oops, I forgot what you wanted to save.\r\n");
                return;
            }
        };
        let kind = match subcmd {
            SCMD_OLC_REDIT => OLC_SAVE_ROOM,
            SCMD_OLC_ZEDIT => OLC_SAVE_ZONE,
            SCMD_OLC_OEDIT => OLC_SAVE_OBJ,
            SCMD_OLC_MEDIT => OLC_SAVE_MOB,
            SCMD_OLC_SEDIT => OLC_SAVE_SHOP,
            _ => {
                g.send_to_char(ch, "Oops, I forgot what you wanted to save.\r\n");
                return;
            }
        };
        let znumber = g.zones.get(zr).map(|z| z.number).unwrap_or(-1);
        g.send_to_char(
            ch,
            &format!(
                "Saving all {}s in zone {}.\r\n",
                olc_type_word(subcmd),
                znumber
            ),
        );
        olc_save_to_disk(g, zr, kind);
        return;
    }

    // Not a save: hand off to the right editor's do_X.
    match subcmd {
        SCMD_OLC_REDIT => crate::redit::do_redit(g, ch, &number.to_string(), 0),
        SCMD_OLC_OEDIT => crate::oedit::do_oedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_ZEDIT => crate::zedit::do_zedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_MEDIT => crate::medit::do_medit(g, ch, &number.to_string(), 0),
        SCMD_OLC_SEDIT => crate::sedit::do_sedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_TRIGEDIT => crate::trigedit::do_trigedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_HEDIT => crate::hedit::do_hedit(g, ch, &buf1, 0),
        SCMD_OLC_AEDIT => crate::aedit::do_aedit(g, ch, &buf1, 0),
        _ => {}
    }
}

/// The descriptive word for each editor (olc_scmd_info[].text).
fn olc_type_word(subcmd: i32) -> &'static str {
    match subcmd {
        SCMD_OLC_REDIT => "room",
        SCMD_OLC_OEDIT => "object",
        SCMD_OLC_ZEDIT => "room",
        SCMD_OLC_MEDIT => "mobile",
        SCMD_OLC_SEDIT => "shop",
        SCMD_OLC_TRIGEDIT => "trigger",
        SCMD_OLC_HEDIT => "help",
        SCMD_OLC_AEDIT => "action",
        _ => "thing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::dg_db_scripts::TrigProto;
    use crate::world::Zone;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn zone(number: i32, builders: &str) -> Zone {
        Zone {
            number,
            name: format!("Zone {}", number),
            builders: builders.to_string(),
            lifespan: 30,
            age: 0,
            top: number * 100 + 99,
            reset_mode: 2,
            min_level: 0,
            max_level: 60,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        }
    }

    fn player(g: &mut GameState, name: &str, level: Level) -> CharId {
        let mut ch = Character::new_player(name.into(), Class::Cleric, Race::Human);
        ch.player.level = level;
        g.create_char(ch)
    }

    fn temp_lib(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("deltamud-olc-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("world/zon")).unwrap();
        std::fs::create_dir_all(path.join("world/mob")).unwrap();
        std::fs::create_dir_all(path.join("world/shp")).unwrap();
        path
    }

    fn olc_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn can_edit_zone_uses_builder_list_below_impl() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Alice Bob"));
        let alice = player(&mut g, "Alice", LVL_IMMORT);
        let charlie = player(&mut g, "Charlie", LVL_IMMORT);
        let imp = player(&mut g, "Root", LVL_IMPL);

        assert!(can_edit_zone(&g, alice, 0));
        assert!(!can_edit_zone(&g, charlie, 0));
        assert!(can_edit_zone(&g, imp, 0));
    }

    #[test]
    fn dg_script_editor_adds_deletes_and_marks_room_dirty() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(42, "Root"));
        olc_remove_from_save_list(42, OLC_SAVE_ROOM);

        let conn = ConnId(99);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        crate::dg_db_scripts::set_test_proto_trigger(
            crate::dg_handler::WLD_TRIGGER,
            999_001,
            TrigProto {
                vnum: 4205,
                attach_type: crate::dg_handler::WLD_TRIGGER,
                name: "first room trigger".to_string(),
                trigger_type: 1,
                narg: 0,
                arglist: String::new(),
                cmdlist: vec!["say first".to_string()],
            },
        );
        crate::dg_db_scripts::set_test_proto_trigger(
            crate::dg_handler::WLD_TRIGGER,
            999_002,
            TrigProto {
                vnum: 4206,
                attach_type: crate::dg_handler::WLD_TRIGGER,
                name: "second room trigger".to_string(),
                trigger_type: 1,
                narg: 0,
                arglist: String::new(),
                cmdlist: vec!["say second".to_string()],
            },
        );

        let mut mode = DgScriptEditMode::Main;
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "n",
        ));
        assert_eq!(mode, DgScriptEditMode::New);
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "1, 4205",
        ));
        assert_eq!(
            crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::WLD_TRIGGER, 4201),
            vec![4205]
        );

        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "n",
        ));
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "4206",
        ));
        assert_eq!(
            crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::WLD_TRIGGER, 4201),
            vec![4205, 4206]
        );

        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "d",
        ));
        assert_eq!(mode, DgScriptEditMode::Delete);
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "1",
        ));
        assert_eq!(
            crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::WLD_TRIGGER, 4201),
            vec![4206]
        );

        olc_saveinfo(&mut g, ch);
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Rooms for zone 42"));
        olc_remove_from_save_list(42, OLC_SAVE_ROOM);
        crate::dg_db_scripts::clear_proto_triggers(crate::dg_handler::WLD_TRIGGER, 4201);
    }

    #[test]
    fn central_olc_save_dispatches_zone_mob_and_shop_writers() {
        let _guard = olc_test_lock();
        let lib = temp_lib("central-save");
        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        g.zones.push(zone(43, "Root"));

        olc_add_to_save_list(43, OLC_SAVE_ZONE);
        olc_add_to_save_list(43, OLC_SAVE_MOB);
        olc_add_to_save_list(43, OLC_SAVE_SHOP);

        olc_save_to_disk(&mut g, 0, OLC_SAVE_ZONE);
        olc_save_to_disk(&mut g, 0, OLC_SAVE_MOB);
        olc_save_to_disk(&mut g, 0, OLC_SAVE_SHOP);

        let zon = lib.join("world/zon/43.zon");
        let mob = lib.join("world/mob/43.mob");
        let shp = lib.join("world/shp/43.shp");
        assert!(std::fs::read_to_string(&zon).unwrap().contains("#43\n"));
        assert_eq!(std::fs::read_to_string(&mob).unwrap(), "$\n");
        assert!(std::fs::read_to_string(&shp)
            .unwrap()
            .starts_with("CircleMUD v3.0 Shop File~\n"));

        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(ConnId(100));
        let mut d = Descriptor::new(ConnId(100), "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(ConnId(100), d);
        olc_saveinfo(&mut g, ch);
        assert!(g
            .descriptors
            .get(&ConnId(100))
            .unwrap()
            .outbuf
            .contains("The database is up to date."));

        let _ = std::fs::remove_dir_all(&lib);
    }
}
