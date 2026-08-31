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
const LVL_BUILDER_LEVEL: u8 = 100; // LVL_BUILDER

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
// OLC color cols. C's get_char_cols() (olc.c:482-488) fills its globals with
// the screen.h KNRM/KGRN/KCYN/KYEL `&`-codes when the builder's colour level
// >= C_NRM (PRF_COLOR_2), else KNUL (""). We keep the same `&`-codes as the
// menu constants; the builder-facing send helpers strip them for builders
// whose colour level is below C_NRM (#306).
// ---------------------------------------------------------------------------
pub const NRM: &str = "&n";
pub const GRN: &str = "&G";
pub const CYN: &str = "&C";
pub const YEL: &str = "&Y";

/// screen.h C_NRM: the colour level OLC menus require (PRF_COLOR_2 set).
const C_NRM_LEVEL: i32 = 2;

/// screen.h _clrlevel: 0-3 from PRF_COLOR_1/2.
pub fn colour_level(g: &GameState, ch: CharId) -> i32 {
    use crate::flags::{PRF_COLOR_1, PRF_COLOR_2};
    let (p1, p2) = g
        .get_char(ch)
        .map(|c| {
            (
                c.prf_flags & PRF_COLOR_1 != 0,
                c.prf_flags & PRF_COLOR_2 != 0,
            )
        })
        .unwrap_or((false, false));
    (p1 as i32) + ((p2 as i32) * 2)
}

/// True when the builder sees OLC menu colours (clr(ch, C_NRM)).
pub fn olc_colour_on(g: &GameState, ch: CharId) -> bool {
    colour_level(g, ch) >= C_NRM_LEVEL
}

/// Send OLC text to a connection, stripping the `&`-codes when the builder's
/// colour level is below C_NRM (C get_char_cols handing back KNUL) (#306).
pub fn olc_send(g: &mut GameState, conn: ConnId, msg: &str) {
    let ch = g.descriptors.get(&conn).and_then(|d| d.character);
    let keep = ch.map(|c| olc_colour_on(g, c)).unwrap_or(false);
    if keep {
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.outbuf.push_str(msg);
        }
    } else if let Some(d) = g.descriptors.get_mut(&conn) {
        d.outbuf.push_str(&crate::connection::strip_color(msg));
    }
}

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
    // C olc.c:393-408: Help/Actions lines need >= LVL_IMMORT; zone lines
    // need can_edit_zone on the listed zone (#278).
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    let mut out = String::from("The following OLC components need saving:\r\n");
    let mut any = false;
    for (zone, kind) in entries {
        if kind != OLC_SAVE_HELP && kind != OLC_SAVE_ACTION {
            let owned = real_zone(g, zone * 100)
                .map(|zr| can_edit_zone(g, ch, zr))
                .unwrap_or(false);
            if !owned && level < LVL_IMMORT {
                continue;
            }
        } else if level < LVL_IMMORT {
            continue;
        }
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
/// True when object vnum `obj_vnum` lives in a zone the builder can edit
/// (sedit.c:1181-1188 product gate; #267).
pub fn obj_proto_in_owned_zone(g: &GameState, ch: CharId, obj_vnum: i32) -> bool {
    match g.obj_protos.get(&obj_vnum) {
        Some(p) => real_zone(g, p.vnum)
            .map(|zr| can_edit_zone(g, ch, zr))
            .unwrap_or(false),
        None => false,
    }
}

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
                // C dg_olc.c:766-783: an unparseable line leaves vnum at -1 →
                // real_trigger() < 0 → "Invalid Trigger VNUM!" re-prompt (#304).
                send_to_conn(
                    g,
                    conn,
                    "Invalid Trigger VNUM!\r\nPlease enter position, vnum   (ex: 1, 200):",
                );
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
// do_copy / do_rlink (C olc.c:735 / :880) — complete in the C source but
// never registered in cmd_info, so builders could never reach them. Ported
// and registered as the "finish the game" activations (registered in
// COMPATIBILITY.md).
// ===========================================================================

const COPY_FORMAT: &str = "Usage:  copy { room | obj } <source> <target>\r\n";
const RLINK_FORMAT: &str = "Usage:  rlink <dir> <connect|disconnect> <1|2> [target]\r\n";

/// C olc.c:646 zone_number(): the builder NUMBER of the zone owning this
/// entity. Rooms resolve through real_zone; objects/mobs use vnum/100 (the
/// author's own truncation formula).
fn zone_number_of_room(g: &GameState, rnum: usize) -> i32 {
    g.rooms
        .get(rnum)
        .and_then(|r| real_zone(g, r.number))
        .and_then(|zr| g.zones.get(zr).map(|z| z.number))
        .unwrap_or(0)
}

/// C olc.c:702 copy_room: name/description/sector/flags only — the author
/// deliberately skipped extra descriptions ("I think it will stay that way.").
fn copy_room_fields(g: &mut GameState, src: usize, targ: usize) {
    let (name, description, sector_type, room_flags) = {
        let r = g.room(src);
        (
            r.name.clone(),
            r.description.clone(),
            r.sector_type,
            r.room_flags,
        )
    };
    let t = g.room_mut(targ);
    t.name = name;
    t.description = description;
    t.sector_type = sector_type;
    t.room_flags = room_flags;
}

/// C olc.c:722 copy_object: the description/flag fields of one prototype onto
/// another (worn_on copied in C is an instance artifact and meaningless on a
/// proto — skipped).
fn copy_object_fields(g: &mut GameState, src_vnum: i32, targ_vnum: i32) {
    let src = match g.obj_protos.get(&src_vnum) {
        Some(p) => p.clone(),
        None => return,
    };
    if let Some(t) = g.obj_protos.get_mut(&targ_vnum) {
        t.name = src.name;
        t.description = src.description;
        t.short_desc = src.short_desc;
        t.action_description = src.action_description;
        t.ex_descriptions = src.ex_descriptions;
        t.obj_type = src.obj_type;
        t.extra_flags = src.extra_flags;
        t.wear_flags = src.wear_flags;
        t.weight = src.weight;
        t.cost = src.cost;
        t.rent = src.rent;
        t.values = src.values;
        t.curr_slots = src.curr_slots;
        t.total_slots = src.total_slots;
        t.bitvector = src.bitvector;
        t.obj_class = src.obj_class;
        t.min_level = src.min_level;
    }
}

/// C olc.c:735 do_copy.
pub fn do_copy(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (ty, rest) = crate::interpreter::one_argument(arg);
    let (src_num, rest2) = crate::interpreter::one_argument(&rest);
    let (targ_num, _) = crate::interpreter::one_argument(rest2);

    if ty.is_empty() || src_num.is_empty() {
        g.send_to_char(ch, COPY_FORMAT);
        return;
    }
    // C olc.c:748 tests `room_or_obj == OBJECT` BEFORE the type is parsed, so
    // this guard can never fire there; the parse-aware placement here is the
    // evident intent (registered).
    let is_obj = crate::interpreter::is_abbrev(&ty, "obj");
    if targ_num.is_empty() && is_obj {
        g.send_to_char(ch, "You must specify a target when copying objects.\r\n");
        return;
    }

    let is_room = crate::interpreter::is_abbrev(&ty, "room");
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let numeric = |s: &str| numeric(&s.clone());

    let (room_or_obj, vnum_src, rnum_src, vnum_targ, rnum_targ, save_zone) = if is_room
        && numeric(&src_num)
    {
        let vnum_src: i32 = src_num.parse().unwrap_or(-1);
        let rnum_src = g.real_room(vnum_src);
        let (vnum_targ, rnum_targ) = if targ_num.is_empty() {
            match g.get_char(ch).and_then(|c| c.in_room) {
                Some(r) => (g.rooms[r].number, Some(r)),
                None => return,
            }
        } else if numeric(&targ_num) {
            let v: i32 = targ_num.parse().unwrap_or(-1);
            (v, g.real_room(v))
        } else {
            g.send_to_char(ch, COPY_FORMAT);
            return;
        };
        let save_zone = rnum_targ.map(|r| zone_number_of_room(g, r)).unwrap_or(0);
        (
            0,
            vnum_src,
            rnum_src.is_some(),
            vnum_targ,
            rnum_targ.is_some(),
            save_zone,
        )
    } else if is_obj && !targ_num.is_empty() && numeric(&src_num) && numeric(&targ_num) {
        let vnum_src: i32 = src_num.parse().unwrap_or(-1);
        let vnum_targ: i32 = targ_num.parse().unwrap_or(-1);
        let rnum_src = g.obj_protos.contains_key(&vnum_src);
        let rnum_targ = g.obj_protos.contains_key(&vnum_targ);
        (
            1,
            vnum_src,
            rnum_src,
            vnum_targ,
            rnum_targ,
            vnum_targ / 100, // C zone_number OBJECT formula
        )
    } else {
        g.send_to_char(ch, COPY_FORMAT);
        return;
    };

    let (src_ok, targ_ok) = (rnum_src, rnum_targ);
    if !src_ok || !targ_ok {
        g.send_to_char(
            ch,
            &format!(
                "The source and target {}s must both currently exist.\r\n",
                if room_or_obj == 1 { "object" } else { "room" }
            ),
        );
        return;
    }
    if !can_edit_zone(g, ch, real_zone(g, save_zone * 100).unwrap_or(usize::MAX)) {
        g.send_to_char(ch, "You cannot edit that zone.\r\n");
        return;
    }

    if room_or_obj == 0 {
        let s = g.real_room(vnum_src).unwrap();
        let t = g.real_room(vnum_targ).unwrap();
        copy_room_fields(g, s, t);
    } else {
        copy_object_fields(g, vnum_src, vnum_targ);
    }

    g.send_to_char(
        ch,
        &format!(
            "You copy {} {} to {}.\r\n",
            if room_or_obj == 0 { "room" } else { "object" },
            vnum_src,
            vnum_targ
        ),
    );
    olc_add_to_save_list(save_zone, room_or_obj); // C: ROOM==OLC_SAVE_ROOM, OBJECT==OLC_SAVE_OBJ
}

/// C olc.c:767 create_dir: an empty exit in `dir` ("No target yet").
fn create_dir(g: &mut GameState, rnum: usize, dir: usize) -> bool {
    let Some(room) = g.rooms.get_mut(rnum) else {
        return false;
    };
    if room.exits[dir].is_some() {
        return false;
    }
    room.exits[dir] = Some(crate::room::Exit {
        description: Some("You see nothing special.\r\n".to_string()),
        keyword: None,
        exit_info: 0,
        key: -1,
        to_room: NOWHERE,
    });
    true
}

/// C olc.c:785 free_dir: remove the exit entirely.
fn free_dir(g: &mut GameState, rnum: usize, dir: usize) -> bool {
    g.rooms
        .get_mut(rnum)
        .map(|room| room.exits[dir].take().is_some())
        .unwrap_or(false)
}

/// C olc.c:880 do_rlink ("The big baby").
pub fn do_rlink(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (direction, rest) = crate::interpreter::one_argument(arg);
    let (command, rest2) = crate::interpreter::one_argument(&rest);
    let (ty, rest3) = crate::interpreter::one_argument(rest2);
    let (target, _) = crate::interpreter::one_argument(rest3);

    if direction.is_empty() || command.is_empty() || ty.is_empty() {
        g.send_to_char(ch, RLINK_FORMAT);
        return;
    }
    let type_int: i32 = match ty.parse() {
        Ok(v) if v == 1 || v == 2 => v,
        _ => {
            g.send_to_char(ch, RLINK_FORMAT);
            return;
        }
    };

    let base_rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let vnum_base = g.rooms[base_rnum].number;

    let disconnect = crate::interpreter::is_abbrev(&command, "disconnect");
    let connect = crate::interpreter::is_abbrev(&command, "connect");
    let mut create_new_room = false;
    let mut vnum_targ: i32 = 0;
    let mut rnum_targ: Option<usize> = None;
    if target.is_empty() && !disconnect {
        create_new_room = true;
    } else if !target.is_empty() && target.chars().all(|c| c.is_ascii_digit()) {
        vnum_targ = target.parse().unwrap_or(-1);
        rnum_targ = g.real_room(vnum_targ);
    } else {
        g.send_to_char(ch, RLINK_FORMAT);
        return;
    }
    // C checks `rnum_targ < 0` here; a given-but-missing target is a format
    // error, matching the C flow (real_room returning < 0).
    if !create_new_room && target.is_empty() {
        g.send_to_char(ch, RLINK_FORMAT);
        return;
    }

    let save_zone_1 = zone_number_of_room(g, base_rnum);
    if !can_edit_zone(g, ch, real_zone(g, save_zone_1 * 100).unwrap_or(usize::MAX)) {
        g.send_to_char(ch, "You cannot create exits in this zone.\r\n");
        return;
    }

    let mut save_zone_2 = 0i32;
    if create_new_room {
        // C olc.c:950-970: first free vnum in the builder's zone becomes a new
        // "An unfinished room" (the redit internal path). C's unreachable
        // "no space" guard is repaired here: if no free vnum exists we say so
        // instead of falling through with target 0 (registered).
        let Some(zr) = real_zone(g, vnum_base) else {
            return;
        };
        let top_room = match g.zones.get(zr) {
            Some(z) => z.top,
            None => return,
        };
        let mut created: Option<i32> = None;
        for k in (save_zone_1 * 100)..=top_room {
            if g.real_room(k).is_none() {
                created = Some(k);
                break;
            }
        }
        let Some(k) = created else {
            g.send_to_char(ch, "Cannot create a new room in this zone!\r\n");
            return;
        };
        let room = crate::room::Room::new(
            k,
            zr as i32,
            "An unfinished room".to_string(),
            "You are in an unfinished room.\r\n".to_string(),
        );
        g.add_room(room);
        vnum_targ = k;
        rnum_targ = g.real_room(k);
        save_zone_2 = save_zone_1;
        g.send_to_char(ch, &format!("You have created new room #{}.\r\n", k));
    } else {
        let Some(rt) = rnum_targ else {
            g.send_to_char(ch, RLINK_FORMAT);
            return;
        };
        save_zone_2 = zone_number_of_room(g, rt);
    }

    if !can_edit_zone(g, ch, real_zone(g, save_zone_2 * 100).unwrap_or(usize::MAX))
        && type_int == 2
    {
        g.send_to_char(ch, "You cannot create exits in the target zone.\r\n");
        return;
    }

    let dir = match direction.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('n') => NORTH,
        Some('e') => EAST,
        Some('s') => SOUTH,
        Some('w') => WEST,
        Some('u') => UP,
        Some('d') => DOWN,
        _ => {
            g.send_to_char(ch, "No such direction!\r\n");
            return;
        }
    };

    if connect {
        if g.rooms[base_rnum].exits[dir].is_none() {
            create_dir(g, base_rnum, dir);
        }
        if let Some(room) = g.rooms.get_mut(base_rnum) {
            if let Some(e) = room.exits[dir].as_mut() {
                e.to_room = vnum_targ;
            }
        }
        if type_int == 2 {
            if let Some(rt) = rnum_targ {
                let rdir = REV_DIR[dir];
                if g.rooms[rt].exits[rdir].is_none() {
                    create_dir(g, rt, rdir);
                }
                if let Some(room) = g.rooms.get_mut(rt) {
                    if let Some(e) = room.exits[rdir].as_mut() {
                        e.to_room = vnum_base;
                    }
                }
            }
            if save_zone_2 == 0 {
                save_zone_2 = rnum_targ.map(|rt| zone_number_of_room(g, rt)).unwrap_or(0);
            }
        }
    } else if disconnect {
        // C dereferences the exit without a NULL check here (crash on a
        // missing own exit); the guard is the registered repair.
        let own_to = g.rooms[base_rnum].exits[dir].as_ref().map(|e| e.to_room);
        if type_int == 2 {
            match own_to {
                Some(to) if to > 0 => {
                    if g.real_room(to).is_some() {
                        free_dir(g, to as usize, REV_DIR[dir]);
                    }
                    if !free_dir(g, base_rnum, dir) {
                        g.send_to_char(ch, "No such exit!\r\n");
                        return;
                    }
                    if let Some(rt) = rnum_targ {
                        save_zone_2 = zone_number_of_room(g, rt);
                    }
                }
                _ => {
                    g.send_to_char(ch, "There is no reciprocol exit to remove.\r\n");
                    if own_to.is_some() {
                        free_dir(g, base_rnum, dir);
                    } else {
                        g.send_to_char(ch, "No such exit!\r\n");
                        return;
                    }
                }
            }
        } else {
            match own_to {
                Some(to) if to > 0 => {
                    free_dir(g, base_rnum, dir);
                }
                _ => {
                    g.send_to_char(ch, "No such exit!\r\n");
                    return;
                }
            }
        }
    } else {
        g.send_to_char(
            ch,
            "Invalid command type.  Valid choices are connect and disconnect.\r\n",
        );
        return;
    }

    if connect {
        g.send_to_char(
            ch,
            &format!(
                "You make an exit {} to room {}.\r\n",
                crate::constants::DIRS[dir],
                vnum_targ
            ),
        );
    } else {
        g.send_to_char(ch, "Exit deleted.\r\n");
    }

    olc_add_to_save_list(save_zone_1, OLC_SAVE_ROOM);
    if save_zone_2 != 0 {
        olc_add_to_save_list(save_zone_2, OLC_SAVE_ROOM);
    }
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
            if buf1.len() >= 3 && buf1.starts_with("new") && !buf2.is_empty() {
                // C zedit.c:153-330: 'olc zedit new <zone>' CREATES the zone
                // (six stub files + index append + zone-table insert) and
                // then exits - it does not enter the editor (also fixes the
                // strn_cmp prefix inversion, #263).
                let zone_num: i32 = buf2.trim().parse().unwrap_or(-1);
                crate::zedit::zedit_new_zone(g, ch, zone_num);
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
                // C olc.c:314-321/343-348: mudlog then dispatch
                // hedit_save_to_disk (#275).
                let name = g
                    .get_char(ch)
                    .map(|c| c.get_name().to_string())
                    .unwrap_or_default();
                crate::syslog::mudlog(
                    g,
                    &format!("OLC: {} saves help entries.", name),
                    crate::syslog::NRM,
                    LVL_GOD,
                );
                crate::hedit::save_all_help(g);
                return;
            }
            SCMD_OLC_AEDIT => {
                let name = g
                    .get_char(ch)
                    .map(|c| c.get_name().to_string())
                    .unwrap_or_default();
                crate::syslog::mudlog(
                    g,
                    &format!("OLC: {} saves all actions.", name),
                    crate::syslog::NRM,
                    LVL_GOD,
                );
                crate::aedit::save_all_actions(g);
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
        // C olc.c:283: mudlog 'OLC: %s saves %s info for zone %d.' (#276).
        {
            let name = g
                .get_char(ch)
                .map(|c| c.get_name().to_string())
                .unwrap_or_default();
            let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(LVL_BUILDER_LEVEL);
            crate::syslog::mudlog(
                g,
                &format!(
                    "OLC: {} saves {} info for zone {}.",
                    name,
                    olc_type_word(subcmd),
                    znumber
                ),
                crate::syslog::CMP,
                LVL_BUILDER_LEVEL.max(level),
            );
        }
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
    fn dg_script_editor_reprompts_on_unparseable_line() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(44, "Root"));
        let conn = ConnId(103);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        let mut mode = DgScriptEditMode::New;
        // C dg_olc.c:766-783: garbage stays in the sub-editor with the
        // "Invalid Trigger VNUM!" re-prompt (#304).
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::MOB_TRIGGER,
            4401,
            &mut mode,
            "not a vnum",
        ));
        assert_eq!(mode, DgScriptEditMode::New);
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Invalid Trigger VNUM!"));
    }

    #[test]
    fn menu_colours_follow_the_builder_colour_level() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        let colour_on = player(&mut g, "Colour", LVL_IMPL);
        let colour_off = player(&mut g, "Plain", LVL_IMPL);
        g.get_char_mut(colour_off).unwrap().prf_flags = 0;
        // Colour level from PRF_COLOR_1/2 (screen.h _clrlevel).
        assert_eq!(colour_level(&g, colour_off), 0);
        assert!(!olc_colour_on(&g, colour_off));
        g.get_char_mut(colour_on).unwrap().prf_flags =
            crate::flags::PRF_COLOR_1 | crate::flags::PRF_COLOR_2;
        assert!(olc_colour_on(&g, colour_on));

        // olc_send strips the &-codes for a colour-off builder (#306).
        let conn = ConnId(105);
        g.get_char_mut(colour_off).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(colour_off);
        g.descriptors.insert(conn, d);
        olc_send(&mut g, conn, "-- Menu [&C42&n]\r\n");
        assert_eq!(g.descriptors.get(&conn).unwrap().outbuf, "-- Menu [42]\r\n");
    }

    #[test]
    fn saveinfo_hides_zones_the_builder_cannot_edit() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(45, "Alice"));
        g.zones.push(zone(46, "Bob"));
        olc_add_to_save_list(45, OLC_SAVE_ROOM);
        olc_add_to_save_list(46, OLC_SAVE_ROOM);

        let conn = ConnId(104);
        let alice = player(&mut g, "Alice", LVL_BUILDER_LEVEL as Level);
        g.get_char_mut(alice).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(alice);
        g.descriptors.insert(conn, d);

        olc_saveinfo(&mut g, alice);
        let out = &g.descriptors.get(&conn).unwrap().outbuf.clone();
        assert!(out.contains("zone 45"));
        assert!(!out.contains("zone 46"), "Bob's zone must be hidden (#278)");
        olc_remove_from_save_list(45, OLC_SAVE_ROOM);
        olc_remove_from_save_list(46, OLC_SAVE_ROOM);
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

    #[test]
    fn do_copy_copies_room_fields_and_marks_dirty() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(45, "Root"));
        let mut src = crate::room::Room::new(4500, 0, "Source".into(), "Src desc.\r\n".into());
        src.sector_type = crate::room::SectorType::Forest;
        let targ = crate::room::Room::new(4501, 0, "Target".into(), "Tgt desc.\r\n".into());
        g.add_room(src);
        let t = g.add_room(targ);

        let conn = ConnId(120);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);
        g.get_char_mut(ch).unwrap().in_room = Some(t);

        do_copy(&mut g, ch, "room 4500", 0);

        let room = g.room(t);
        assert_eq!(room.name, "Source");
        assert_eq!(room.sector_type, crate::room::SectorType::Forest);
        assert!(g.descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("You copy room 4500 to 4501."));
        // Save-list marks the target zone dirty (C: ROOM == OLC_SAVE_ROOM).
        olc_remove_from_save_list(45, OLC_SAVE_ROOM);
    }

    #[test]
    fn do_rlink_connects_disconnects_and_autocreates() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(46, "Root"));
        let a = g.add_room(crate::room::Room::new(4600, 0, "A".into(), String::new()));
        let b = g.add_room(crate::room::Room::new(4601, 0, "B".into(), String::new()));

        let conn = ConnId(121);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        g.get_char_mut(ch).unwrap().in_room = Some(a);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        // One-way connect.
        do_rlink(&mut g, ch, "east connect 1 4601", 0);
        assert_eq!(g.room(a).exits[EAST].as_ref().unwrap().to_room, 4601);
        assert!(g.room(b).exits[WEST].is_none());
        assert!(g.descriptors
            .get(&conn)
            .unwrap()
            .outbuf
            .contains("You make an exit east to room 4601."));

        // Two-way connect builds the reciprocal exit.
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        g.get_char_mut(ch).unwrap().in_room = Some(b);
        do_rlink(&mut g, ch, "west connect 2 4600", 0);
        assert_eq!(g.room(b).exits[WEST].as_ref().unwrap().to_room, 4600);
        assert_eq!(g.room(a).exits[EAST].as_ref().unwrap().to_room, 4601);

        // Disconnect removes the own exit (stand in B, own the west exit).
        // C quirk kept: despite the usage string's "[target]", the parse
        // demands a numeric target even for disconnect (is_number("") fails).
        g.get_char_mut(ch).unwrap().in_room = Some(b);
        do_rlink(&mut g, ch, "west disconnect 1 4600", 0);
        assert!(g.room(b).exits[WEST].is_none());
        assert!(g.descriptors.get(&conn).unwrap().outbuf.contains("Exit deleted."));

        // Auto-create: omitting the target makes the first free vnum in the zone.
        g.get_char_mut(ch).unwrap().in_room = Some(a);
        do_rlink(&mut g, ch, "south connect 1", 0);
        assert_eq!(
            g.room(a).exits[SOUTH].as_ref().map(|e| e.to_room),
            Some(4602)
        );
        assert_eq!(g.room(g.real_room(4602).unwrap()).name, "An unfinished room");
        olc_remove_from_save_list(46, OLC_SAVE_ROOM);
    }
}
