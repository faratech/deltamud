// zedit.rs — OASIS zone editor (CircleMUD/DeltaMUD `zedit.c`), ported to the
// id-indexed GameState. This is the menu-driven online creation editor for a
// zone's reset-command list (the M/O/G/E/P/D/R commands) plus the zone header
// (name, builders, lifespan, top room, reset mode, level range, approval).
//
// Architecture (the shared OLC contract):
//   * `do_zedit(g, ch, arg, subcmd)` starts the editor on the room whose vnum
//     is given (the room the player is in, if no arg). It snapshots that
//     zone's header AND every reset command relating to that room into a
//     per-connection `ZeditState`, registers via
//     `olc::set_active(conn, EditorKind::Zedit)`, and shows the main menu.
//   * `zedit_parse(g, conn, line)` is the per-line handler the OLC router calls.
//   * On save the editor splices the edited room's commands back into the
//     zone's full command list, updates the zone header, writes the result
//     into `GameState::zones`, rewrites the on-disk `<zone>.zon` file byte-
//     faithfully, and calls `olc::clear_active(conn)`.
//
// CircleMUD's zedit edits exactly the reset commands that *relate to the room*
// being edited (load mob/obj into it, give/equip on the last-loaded mob, put
// in container, set its doors, remove an obj from it). All commands for OTHER
// rooms in the zone are left untouched. We reproduce that exactly: the scratch
// state holds only this room's commands; on save we re-read the on-disk `.zon`,
// drop the room's old commands, splice the edited ones at the right position,
// and rewrite the whole file. The Rust `Zone.reset_commands` (which we may not
// reshape) is also resynced so the live game's resets reflect the edit.
//
// All per-connection state lives in a module-static keyed by ConnId.

use crate::constants::{DIRS, EQUIPMENT_TYPES};
use crate::olc::{self, EditorKind};
use crate::state::GameState;
use crate::types::*;
use crate::world::ResetCmd;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Constants mirrored from olc.h / structs.h.
// ---------------------------------------------------------------------------
const LVL_REVIEW: Level = LVL_GRGOD;

// ---------------------------------------------------------------------------
// A raw reset command, holding the FULL field set the on-disk `.zon` line
// carries (command letter, if_flag, arg1..arg4). The Rust `ResetCmd` enum
// (world.rs) drops arg4 (the load-chance) and is variant-shaped, so for a
// lossless round-trip the editor keeps this flat representation. arg1/arg3 are
// stored as VNUMs (as written in the file), matching the on-disk form; the
// CircleMUD in-memory form uses rnums, but storing vnums keeps the file write
// trivial and avoids reshuffling on rnum changes.
#[derive(Clone, Copy)]
struct RawCmd {
    command: char, // 'M','O','G','E','P','D','R'  ('N' = freshly-created blank)
    if_flag: i32,
    arg1: i32,
    arg2: i32,
    arg3: i32,
    arg4: i32,
}

impl RawCmd {
    fn blank() -> Self {
        RawCmd {
            command: 'N',
            if_flag: 0,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Editor sub-mode (CircleMUD OLC_MODE within CON_ZEDIT).
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    MainMenu,
    ConfirmSave,
    NewEntry,
    DeleteEntry,
    ChangeEntry,
    CommandType,
    IfFlag,
    Arg1,
    Arg2,
    Arg3,
    Arg4,
    ZoneName,
    ZoneBuilders,
    ZoneTop,
    ZoneLife,
    ZoneReset,
    Levels,
    MinLvl,
    MaxLvl,
    Approve,
    Prob,  // "which command to change chance for?"
    Prob2, // "new chance"
}

// ---------------------------------------------------------------------------
// The zone header scratch (CircleMUD OLC_ZONE). `header_changed` / `cmds_changed`
// mirror C's reuse of zone->number / zone->age as dirty flags.
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct ZoneHdr {
    name: String,
    builders: String,
    lifespan: i32,
    top: RoomVnum,
    reset_mode: i32,
    lvl1: i32,
    lvl2: i32,
    status_mode: i32, // 0 = closed to mortals, 1 = approved
    header_changed: bool,
    cmds_changed: bool,
}

struct ZeditState {
    room_vnum: RoomVnum, // the room whose commands are being edited
    zone_number: i32,
    zone_index: usize, // index into GameState::zones
    hdr: ZoneHdr,
    cmds: Vec<RawCmd>, // commands relating to this room (no 'S' terminator)
    mode: Mode,
    cur: usize, // OLC_VAL — index of the command currently being changed
    level_of_editor: Level,
}

fn states() -> &'static Mutex<HashMap<ConnId, ZeditState>> {
    static S: OnceLock<Mutex<HashMap<ConnId, ZeditState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit), releasing the per-conn working copy. `olc::abort_editor` clears
/// active.
pub fn abort(conn: ConnId) {
    states().lock().unwrap().remove(&conn);
}

// ---------------------------------------------------------------------------
// Output helper.
// ---------------------------------------------------------------------------
fn send(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.outbuf.push_str(msg);
    }
}

// ===========================================================================
// Command entry — do_zedit.
// ===========================================================================

/// `zedit [<room vnum>]` — edit the reset commands relating to a room (the
/// player's current room if no vnum is given). Header fields edited here apply
/// to the whole zone.
pub fn do_zedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    if level < LVL_IMMORT {
        send(g, conn, "You do not have access to the zone editor.\r\n");
        return;
    }

    let arg = arg.trim();
    let room_vnum: RoomVnum = if arg.is_empty() {
        match g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.rooms[r].number)
        {
            Some(v) => v,
            None => {
                send(g, conn, "You are not in a room with a vnum.\r\n");
                return;
            }
        }
    } else {
        match arg.parse() {
            Ok(v) => v,
            Err(_) => {
                send(
                    g,
                    conn,
                    "Specify a room VNUM to edit (or omit to edit this room).\r\n",
                );
                return;
            }
        }
    };

    // The room must exist (zedit edits a room's resets).
    if g.real_room(room_vnum).is_none() {
        send(g, conn, "That room does not exist.\r\n");
        return;
    }

    let zone_number = match zone_for_vnum(g, room_vnum) {
        Some(z) => z,
        None => {
            send(g, conn, "That room is outside any existing zone.\r\n");
            return;
        }
    };
    let zone_index = match g.zones.iter().position(|z| z.number == zone_number) {
        Some(i) => i,
        None => return,
    };

    if level < LVL_GRGOD && !can_edit_zone(g, ch, zone_number) {
        send(g, conn, "You do not have permission to edit this zone.\r\n");
        return;
    }

    // Snapshot the zone header. We seed from the in-memory `Zone` for the
    // fields it keeps, and overlay the on-disk header for the fields the loader
    // drops (builders, the level/status line), so the round-trip is lossless.
    let path = zon_file_path(g, zone_number);
    let z = &g.zones[zone_index];
    let mut hdr = ZoneHdr {
        name: z.name.clone(),
        builders: String::new(),
        lifespan: z.lifespan,
        top: z.top,
        reset_mode: z.reset_mode,
        lvl1: z.min_level as i32,
        lvl2: z.max_level as i32,
        status_mode: 0,
        header_changed: false,
        cmds_changed: false,
    };
    if let Some(disk) = read_disk_header(&path) {
        hdr.name = disk.name;
        hdr.builders = disk.builders;
        hdr.top = disk.top;
        hdr.lifespan = disk.lifespan;
        hdr.reset_mode = disk.reset_mode;
        hdr.lvl1 = disk.lvl1;
        hdr.lvl2 = disk.lvl2;
        hdr.status_mode = disk.status_mode;
    }

    // Load this room's reset commands from the on-disk `.zon` file (full
    // fidelity incl. if_flag/arg4), falling back to the in-memory simplified
    // ResetCmd list if the file is unreadable.
    let all = read_disk_cmds(&path).unwrap_or_else(|| cmds_from_memory(g, zone_index));
    let all_copy = all.clone();
    let cmds: Vec<RawCmd> = all
        .into_iter()
        .filter(|c| belongs_to_room(&all_copy, room_vnum, c))
        .collect();

    let st = ZeditState {
        room_vnum,
        zone_number,
        zone_index,
        hdr,
        cmds,
        mode: Mode::MainMenu,
        cur: 0,
        level_of_editor: level,
    };
    // C olc.c:198-212 (#272): key on the zone being edited.
    if states().lock().unwrap().values().any(|s| s.zone_number == st.zone_number) {
        g.send_to_char(ch, "That zone is currently being edited by someone else.\r\n");
        return;
    }
    states().lock().unwrap().insert(conn, st);
    olc::set_active(conn, EditorKind::Zedit);
    disp_menu(g, conn);
}

// ===========================================================================
// Zone / permission helpers.
// ===========================================================================

fn zone_for_vnum(g: &GameState, vnum: RoomVnum) -> Option<i32> {
    g.zones
        .iter()
        .find(|z| z.number * 100 <= vnum && z.top >= vnum)
        .map(|z| z.number)
}

fn can_edit_zone(g: &GameState, ch: CharId, zone_number: i32) -> bool {
    g.zones
        .iter()
        .position(|z| z.number == zone_number)
        .map(|zr| olc::can_edit_zone(g, ch, zr))
        .unwrap_or(false)
}

fn can_edit_vnum_zone(g: &GameState, ch: CharId, vnum: i32) -> bool {
    olc::real_zone(g, vnum)
        .map(|zr| olc::can_edit_zone(g, ch, zr))
        .unwrap_or(false)
}

fn char_for_conn(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors
        .get(&conn)
        .and_then(|d| d.character)
        .or_else(|| {
            g.chars
                .iter()
                .find_map(|(&id, c)| (c.desc == Some(conn)).then_some(id))
        })
}

fn zon_file_path(g: &GameState, zone_number: i32) -> std::path::PathBuf {
    std::path::Path::new(&g.config.lib_path)
        .join("world")
        .join("zon")
        .join(format!("{}.zon", zone_number))
}

/// Which room a reset command "relates to" (CircleMUD zedit_setup switch).
/// M/O target room is arg3 (a vnum, as we store vnums); G/E/P have no room (they
/// chain on the last mob); D/R target room is arg1.
fn cmd_room(c: &RawCmd) -> Option<RoomVnum> {
    match c.command {
        'M' | 'O' => Some(c.arg3),
        'D' | 'R' => Some(c.arg1),
        // C zedit.c: G/E/P hit `default: break` in the room-assignment loop
        // and INHERIT the previous command's room (cmd_room is declared
        // outside the fold). Returning None here dropped every G/E/P from
        // the isolation filters, orphaning equipment/containers on every
        // save (#261).
        'G' | 'E' | 'P' => None,
        _ => None,
    }
}

/// True if `c` belongs to `room` under C's carried-cmd_room semantics: the
/// room-relevant commands M/O set it from arg3, D/R from arg1, and G/E/P
/// inherit the room of the command before them.
fn belongs_to_room(cmds: &[RawCmd], room_vnum: RoomVnum, c: &RawCmd) -> bool {
    let mut cur: Option<RoomVnum> = None;
    for cmd in cmds {
        match cmd.command {
            'M' | 'O' => cur = Some(cmd.arg3),
            'D' | 'R' => cur = Some(cmd.arg1),
            'G' | 'E' | 'P' => {}
            _ => {}
        }
        if std::ptr::eq(cmd, c) {
            return cur == Some(room_vnum);
        }
    }
    false
}

fn filter_room_cmds(cmds: &[RawCmd], room_vnum: RoomVnum, keep: bool) -> Vec<RawCmd> {
    cmds.iter()
        .filter(|c| belongs_to_room(cmds, room_vnum, c) == keep)
        .cloned()
        .collect()
}

// ===========================================================================
// Menu rendering.
// ===========================================================================

fn disp_menu(g: &mut GameState, conn: ConnId) {
    let st = match snapshot(conn) {
        Some(s) => s,
        None => return,
    };

    let reset_desc = match st.hdr.reset_mode {
        0 => "Never reset",
        1 => "Reset only when no players are in zone.",
        _ => "Normal reset.",
    };
    let approve = if st.hdr.status_mode != 0 {
        "Approved for mortals."
    } else {
        "Closed to mortals."
    };
    let builders = if st.hdr.builders.is_empty() {
        "<NONE!>"
    } else {
        &st.hdr.builders
    };
    let name = if st.hdr.name.is_empty() {
        "<NONE!>"
    } else {
        &st.hdr.name
    };

    let mut buf = format!(
        "Room number: &c{room}&n\t\tRoom zone: &c{zone}\r\n\
         &gB&n) Builders    : &y{builders}\r\n\
         &gZ&n) Zone name   : &y{name}\r\n\
         &gL&n) Lifespan    : &y{life} minutes\r\n\
         &gT&n) Top of zone : &y{top}\r\n\
         &gR&n) Reset Mode  : &y{reset}&n\r\n\
         &gV&n) Levels      : &y{lvl1}&n to &y{lvl2}&n\r\n\
         &gA&n) Approve zone: &y{approve}&n\r\n\
         [Command list]\r\n",
        room = st.room_vnum,
        zone = st.zone_number,
        builders = builders,
        name = name,
        life = st.hdr.lifespan,
        top = st.hdr.top,
        reset = reset_desc,
        lvl1 = st.hdr.lvl1,
        lvl2 = st.hdr.lvl2,
        approve = approve,
    );

    // Render each reset command (CircleMUD zedit_disp_menu translation).
    let mut counter = 0;
    for c in &st.cmds {
        let line = describe_cmd(g, c);
        buf.push_str(&format!("&n{} - &y{}\r\n", counter, line));
        counter += 1;
    }
    buf.push_str(&format!(
        "&n{} - <END OF LIST>\r\n\
         &gN&n) New command.       &gE&n) Edit a command.\r\n\
         &gD&n) Delete a command.  &gC&n) Change a command's chance of happening.\r\n\
         &gQ&n) Quit\r\nEnter your choice : ",
        counter
    ));
    send(g, conn, &buf);
    set_mode(conn, Mode::MainMenu);
}

/// Human-readable line for one reset command (CircleMUD zedit_disp_menu cases).
fn describe_cmd(g: &GameState, c: &RawCmd) -> String {
    let then = if c.if_flag != 0 { " then " } else { "" };
    match c.command {
        'M' => {
            let chance = if c.arg4 != 0 { 101 - c.arg4 } else { 100 };
            format!(
                "{}Load {} [&c{}&y], Chance {}% , Max : {}",
                then,
                mob_short(g, c.arg1),
                c.arg1,
                chance,
                c.arg2
            )
        }
        'G' => {
            let chance = if c.arg3 != 0 { 101 - c.arg3 } else { 100 };
            format!(
                "{}Give it {} [&c{}&y], Chance {}%, Max : {}",
                then,
                obj_short(g, c.arg1),
                c.arg1,
                chance,
                c.arg2
            )
        }
        'O' => {
            let chance = if c.arg4 != 0 { 101 - c.arg4 } else { 100 };
            format!(
                "{}Load {} [&c{}&y], Chance {}%, Max : {}",
                then,
                obj_short(g, c.arg1),
                c.arg1,
                chance,
                c.arg2
            )
        }
        'E' => {
            let chance = if c.arg4 != 0 { 101 - c.arg4 } else { 100 };
            format!(
                "{}Equip with {} [&c{}&y], {}, Chance {}%, Max : {}",
                then,
                obj_short(g, c.arg1),
                c.arg1,
                equipment_name(c.arg3),
                chance,
                c.arg2
            )
        }
        'P' => {
            let chance = if c.arg4 != 0 { 101 - c.arg4 } else { 100 };
            format!(
                "{}Put {} [&c{}&y] in {} [&c{}&y], Chance {}%, Max : {}",
                then,
                obj_short(g, c.arg1),
                c.arg1,
                obj_short(g, c.arg3),
                c.arg3,
                chance,
                c.arg2
            )
        }
        'R' => format!(
            "{}Remove {} [&c{}&y] from room.",
            then,
            obj_short(g, c.arg2),
            c.arg2
        ),
        'D' => {
            let state = match c.arg3 {
                0 => "open",
                1 => "closed",
                _ => "locked",
            };
            format!("{}Set door {} as {}.", then, dir_name(c.arg2), state)
        }
        _ => "<Unknown Command>".to_string(),
    }
}

fn disp_comtype(g: &mut GameState, conn: ConnId) {
    send(
        g,
        conn,
        "&gM&n) Load Mobile to room             &gO&n) Load Object to room\r\n\
         &gE&n) Equip mobile with object        &gG&n) Give an object to a mobile\r\n\
         &gP&n) Put object in another object    &gD&n) Open/Close/Lock a Door\r\n\
         &gR&n) Remove an object from the room\r\n\
         What sort of command will this be? : ",
    );
    set_mode(conn, Mode::CommandType);
}

fn disp_levels(g: &mut GameState, conn: ConnId) {
    let st = match snapshot(conn) {
        Some(s) => s,
        None => return,
    };
    let buf = format!(
        "Set Level 1 at 91 for an immortal zone, set both at 0 for an all level zone.\r\n\
         &g1&n) Minimum level area designed for: &g{}&n\r\n\
         &g2&n) Maximum level area designed for: &g{}&n\r\n\
         Set which level? : ",
        st.hdr.lvl1, st.hdr.lvl2
    );
    send(g, conn, &buf);
    set_mode(conn, Mode::Levels);
}

// ---- arg-prompt dispatch (CircleMUD zedit_disp_arg{1,2,3,4}) --------------

fn disp_arg1(g: &mut GameState, conn: ConnId) {
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'M' => {
            send(g, conn, "Input mob's vnum : ");
            set_mode(conn, Mode::Arg1);
        }
        'O' | 'E' | 'P' | 'G' => {
            send(g, conn, "Input object vnum : ");
            set_mode(conn, Mode::Arg1);
        }
        'D' | 'R' => {
            // arg1 is the room number (this room); skip straight to arg2.
            with_cur(conn, |c| c.arg1 = state_room(conn));
            disp_arg2(g, conn);
        }
        _ => {}
    }
}

fn disp_arg2(g: &mut GameState, conn: ConnId) {
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'M' | 'O' | 'E' | 'P' | 'G' => {
            send(
                g,
                conn,
                "Input the maximum number that can exist on the mud : ",
            );
        }
        'D' => {
            let mut out = String::new();
            let mut i = 0;
            while i < DIRS.len() && DIRS[i] != "\n" {
                out.push_str(&format!("{}) Exit {}.\r\n", i, DIRS[i]));
                i += 1;
            }
            out.push_str("Enter exit number for door : ");
            send(g, conn, &out);
        }
        'R' => {
            send(g, conn, "Input object's vnum : ");
        }
        _ => {}
    }
    set_mode(conn, Mode::Arg2);
}

fn disp_arg3(g: &mut GameState, conn: ConnId) {
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'E' => {
            let mut out = String::new();
            let mut i = 0;
            while i < EQUIPMENT_TYPES.len() && EQUIPMENT_TYPES[i] != "\n" {
                let next = if i + 1 < EQUIPMENT_TYPES.len() && EQUIPMENT_TYPES[i + 1] != "\n" {
                    EQUIPMENT_TYPES[i + 1]
                } else {
                    ""
                };
                out.push_str(&format!(
                    "{:>2}) {:<26.26} {:>2}) {:<26.26}\r\n",
                    i,
                    EQUIPMENT_TYPES[i],
                    i + 1,
                    next
                ));
                if i + 1 < EQUIPMENT_TYPES.len() && EQUIPMENT_TYPES[i + 1] != "\n" {
                    i += 2;
                } else {
                    break;
                }
            }
            out.push_str("Location to equip : ");
            send(g, conn, &out);
        }
        'P' => send(g, conn, "Vnum of the container : "),
        'D' => send(
            g,
            conn,
            "0)  Door open\r\n1)  Door closed\r\n2)  Door locked\r\nEnter state of the door : ",
        ),
        'G' => send(
            g,
            conn,
            "Give the percentage chance that this event should happen: ",
        ),
        _ => {}
    }
    set_mode(conn, Mode::Arg3);
}

fn disp_arg4(g: &mut GameState, conn: ConnId) {
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'E' | 'M' | 'O' | 'P' => {
            send(
                g,
                conn,
                "Give the percentage chance that this event should happen: ",
            );
            set_mode(conn, Mode::Arg4);
        }
        _ => {}
    }
}

// ===========================================================================
// Name lookups.
// ===========================================================================

fn mob_short(g: &GameState, vnum: i32) -> String {
    g.mob_protos
        .get(&vnum)
        .map(|p| p.short_desc.clone())
        .unwrap_or_else(|| "<UNDEF>".to_string())
}
fn obj_short(g: &GameState, vnum: i32) -> String {
    g.obj_protos
        .get(&vnum)
        .map(|p| p.short_desc.clone())
        .unwrap_or_else(|| "<UNDEF>".to_string())
}
fn dir_name(i: i32) -> &'static str {
    if i >= 0 && (i as usize) < NUM_OF_DIRS {
        DIRS[i as usize]
    } else {
        "<DIR>"
    }
}
fn equipment_name(i: i32) -> &'static str {
    if i >= 0 && (i as usize) < EQUIPMENT_TYPES.len() && EQUIPMENT_TYPES[i as usize] != "\n" {
        EQUIPMENT_TYPES[i as usize]
    } else {
        "<EQ>"
    }
}

// ===========================================================================
// State helpers.
// ===========================================================================

fn snapshot(conn: ConnId) -> Option<ZeditState> {
    states().lock().unwrap().get(&conn).map(|s| ZeditState {
        room_vnum: s.room_vnum,
        zone_number: s.zone_number,
        zone_index: s.zone_index,
        hdr: s.hdr.clone(),
        cmds: s.cmds.clone(),
        mode: s.mode,
        cur: s.cur,
        level_of_editor: s.level_of_editor,
    })
}

fn set_mode(conn: ConnId, mode: Mode) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        s.mode = mode;
    }
}

fn state_room(conn: ConnId) -> RoomVnum {
    states()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|s| s.room_vnum)
        .unwrap_or(0)
}

fn cur_cmd_letter(conn: ConnId) -> char {
    let g = states().lock().unwrap();
    g.get(&conn)
        .and_then(|s| s.cmds.get(s.cur))
        .map(|c| c.command)
        .unwrap_or('\0')
}

fn with_cur<F: FnOnce(&mut RawCmd)>(conn: ConnId, f: F) {
    let mut g = states().lock().unwrap();
    if let Some(s) = g.get_mut(&conn) {
        let cur = s.cur;
        if let Some(c) = s.cmds.get_mut(cur) {
            f(c);
        }
    }
}

fn mark_cmds_changed(conn: ConnId) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        s.hdr.cmds_changed = true;
    }
}
fn mark_header_changed(conn: ConnId) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        s.hdr.header_changed = true;
    }
}

// ===========================================================================
// The line parser — zedit_parse.
// ===========================================================================

pub fn zedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match states().lock().unwrap().get(&conn) {
        Some(s) => s.mode,
        None => {
            olc::clear_active(conn);
            return;
        }
    };

    match mode {
        Mode::ConfirmSave => parse_confirm_save(g, conn, line),
        Mode::MainMenu => parse_main_menu(g, conn, line),
        Mode::NewEntry => parse_new_entry(g, conn, line),
        Mode::DeleteEntry => parse_delete_entry(g, conn, line),
        Mode::ChangeEntry => parse_change_entry(g, conn, line),
        Mode::CommandType => parse_command_type(g, conn, line),
        Mode::IfFlag => parse_if_flag(g, conn, line),
        Mode::Arg1 => parse_arg1(g, conn, line),
        Mode::Arg2 => parse_arg2(g, conn, line),
        Mode::Arg3 => parse_arg3(g, conn, line),
        Mode::Arg4 => parse_arg4(g, conn, line),
        Mode::ZoneName => parse_zone_name(g, conn, line),
        Mode::ZoneBuilders => parse_zone_builders(g, conn, line),
        Mode::ZoneTop => parse_zone_top(g, conn, line),
        Mode::ZoneLife => parse_zone_life(g, conn, line),
        Mode::ZoneReset => parse_zone_reset(g, conn, line),
        Mode::Levels => parse_levels(g, conn, line),
        Mode::MinLvl => parse_min_lvl(g, conn, line),
        Mode::MaxLvl => parse_max_lvl(g, conn, line),
        Mode::Approve => parse_approve(g, conn, line),
        Mode::Prob => parse_prob(g, conn, line),
        Mode::Prob2 => parse_prob2(g, conn, line),
    }
}

// ---- Main menu ------------------------------------------------------------

fn parse_main_menu(g: &mut GameState, conn: ConnId, line: &str) {
    let level = states()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|s| s.level_of_editor)
        .unwrap_or(0);
    match line.trim().chars().next().unwrap_or('\0') {
        'q' | 'Q' => {
            let (cmds_changed, header_changed) = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| (s.hdr.cmds_changed, s.hdr.header_changed))
                .unwrap_or((false, false));
            if cmds_changed || header_changed {
                send(
                    g,
                    conn,
                    "Do you wish to save the changes to the zone info? (y/n) : ",
                );
                set_mode(conn, Mode::ConfirmSave);
            } else {
                send(g, conn, "No changes made.\r\n");
                finish(g, conn);
            }
        }
        'n' | 'N' => {
            send(
                g,
                conn,
                "What number in the list should the new command be? : ",
            );
            set_mode(conn, Mode::NewEntry);
        }
        'e' | 'E' => {
            send(g, conn, "Which command do you wish to change? : ");
            set_mode(conn, Mode::ChangeEntry);
        }
        'd' | 'D' => {
            send(g, conn, "Which command do you wish to delete? : ");
            set_mode(conn, Mode::DeleteEntry);
        }
        'z' | 'Z' => {
            send(g, conn, "Enter new zone name : ");
            set_mode(conn, Mode::ZoneName);
        }
        't' | 'T' => {
            if level < LVL_IMPL {
                disp_menu(g, conn);
            } else {
                send(g, conn, "Enter new top of zone : ");
                set_mode(conn, Mode::ZoneTop);
            }
        }
        'a' | 'A' => {
            if level < LVL_REVIEW {
                disp_menu(g, conn);
            } else {
                send(g, conn, "Approve this zone? ");
                set_mode(conn, Mode::Approve);
            }
        }
        'v' | 'V' => disp_levels(g, conn),
        'l' | 'L' => {
            send(g, conn, "Enter new zone lifespan : ");
            set_mode(conn, Mode::ZoneLife);
        }
        'r' | 'R' => {
            send(
                g,
                conn,
                "\r\n0) Never reset\r\n1) Reset only when no players in zone\r\n2) Normal reset\r\nEnter new zone reset type : ",
            );
            set_mode(conn, Mode::ZoneReset);
        }
        'b' | 'B' => {
            if level < LVL_IMPL {
                send(
                    g,
                    conn,
                    "Only Implementors can modify the builder list.\r\n",
                );
                disp_menu(g, conn);
            } else {
                send(g, conn, "Enter new zone builders : ");
                set_mode(conn, Mode::ZoneBuilders);
            }
        }
        'c' | 'C' => {
            send(g, conn, "Which command? ");
            set_mode(conn, Mode::Prob);
        }
        _ => disp_menu(g, conn),
    }
}

// ---- New / Delete / Change ------------------------------------------------

fn parse_new_entry(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if let Ok(pos) = t.parse::<usize>() {
        if new_command(conn, pos) {
            // start change at pos
            with_state(conn, |s| s.cur = pos);
            mark_cmds_changed(conn);
            disp_comtype(g, conn);
            return;
        }
    }
    disp_menu(g, conn);
}

fn parse_delete_entry(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if let Ok(pos) = t.parse::<usize>() {
        delete_command(conn, pos);
        mark_cmds_changed(conn);
    }
    disp_menu(g, conn);
}

fn parse_change_entry(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if let Ok(pos) = t.parse::<usize>() {
        if start_change_command(conn, pos) {
            mark_cmds_changed(conn);
            disp_comtype(g, conn);
            return;
        }
    }
    disp_menu(g, conn);
}

/// Insert a new blank command at `pos` (CircleMUD new_command). Returns false
/// if pos is out of range.
fn new_command(conn: ConnId, pos: usize) -> bool {
    let mut g = states().lock().unwrap();
    let s = match g.get_mut(&conn) {
        Some(s) => s,
        None => return false,
    };
    if pos > s.cmds.len() {
        return false;
    }
    s.cmds.insert(pos, RawCmd::blank());
    true
}

/// Delete command at `pos` if in range (CircleMUD delete_command).
fn delete_command(conn: ConnId, pos: usize) {
    let mut g = states().lock().unwrap();
    if let Some(s) = g.get_mut(&conn) {
        if pos < s.cmds.len() {
            s.cmds.remove(pos);
        }
    }
}

/// Set the "current" command index to pos if valid (CircleMUD start_change_command).
fn start_change_command(conn: ConnId, pos: usize) -> bool {
    let mut g = states().lock().unwrap();
    let s = match g.get_mut(&conn) {
        Some(s) => s,
        None => return false,
    };
    if pos >= s.cmds.len() {
        return false;
    }
    s.cur = pos;
    true
}

// ---- Command type / if-flag ----------------------------------------------

fn parse_command_type(g: &mut GameState, conn: ConnId, line: &str) {
    let c = line
        .trim()
        .chars()
        .next()
        .unwrap_or('\0')
        .to_ascii_uppercase();
    if c == '\0' || !"MOPEDGR".contains(c) {
        send(g, conn, "Invalid choice, try again : ");
        return;
    }
    with_cur(conn, |cmd| cmd.command = c);
    // If there is a previous command in the list, offer the if-flag chaining.
    let cur = states()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|s| s.cur)
        .unwrap_or(0);
    if cur > 0 {
        send(
            g,
            conn,
            "Is this command dependent on the success of the previous one? (y/n)\r\n",
        );
        set_mode(conn, Mode::IfFlag);
    } else {
        with_cur(conn, |cmd| cmd.if_flag = 0);
        disp_arg1(g, conn);
    }
}

fn parse_if_flag(g: &mut GameState, conn: ConnId, line: &str) {
    match line.trim().chars().next().unwrap_or('\0') {
        'y' | 'Y' => with_cur(conn, |c| c.if_flag = 1),
        'n' | 'N' => with_cur(conn, |c| c.if_flag = 0),
        _ => {
            send(g, conn, "Try again : ");
            return;
        }
    }
    disp_arg1(g, conn);
}

// ---- Args -----------------------------------------------------------------

fn parse_arg1(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let vnum: i32 = atoi(t);
    let cmd = cur_cmd_letter(conn);
    let ch = char_for_conn(g, conn);
    match cmd {
        'M' => {
            // C zedit.c:1406-1412: the permission check runs FIRST, exempts
            // level >= LVL_GRGOD (104), and uses the retry prompt (#271).
            let level = ch
                .and_then(|id| g.get_char(id))
                .map(|c| c.player.level as i32)
                .unwrap_or(LVL_IMPL as i32);
            if level < LVL_GRGOD as i32
                && !ch
                    .map(|id| can_edit_vnum_zone(g, id, vnum))
                    .unwrap_or(false)
            {
                send(g, conn, "You don't have permissions to that zone, try again : ");
                return;
            }
            if g.mob_protos.contains_key(&vnum) {
                with_cur(conn, |c| c.arg1 = vnum);
                disp_arg2(g, conn);
            } else {
                send(g, conn, "That mobile does not exist, try again : ");
            }
        }
        'O' | 'P' | 'E' | 'G' => {
            if g.obj_protos.contains_key(&vnum) {
                if !ch
                    .map(|id| can_edit_vnum_zone(g, id, vnum))
                    .unwrap_or(false)
                {
                    send(g, conn, "You do not have permission to edit this zone.\r\n");
                    return;
                }
                with_cur(conn, |c| c.arg1 = vnum);
                disp_arg2(g, conn);
            } else {
                send(g, conn, "That object does not exist, try again : ");
            }
        }
        _ => {}
    }
}

fn parse_arg2(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let val = atoi(t);
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'M' | 'O' => {
            let room = state_room(conn);
            with_cur(conn, |c| {
                c.arg2 = val;
                c.arg3 = room;
            });
            disp_arg4(g, conn);
        }
        'G' | 'P' | 'E' => {
            with_cur(conn, |c| c.arg2 = val);
            disp_arg3(g, conn);
        }
        'D' => {
            // Count directions.
            let mut maxdir = 0;
            while maxdir < DIRS.len() && DIRS[maxdir] != "\n" {
                maxdir += 1;
            }
            if val < 0 || val as usize > maxdir {
                send(g, conn, "Try again : ");
            } else {
                with_cur(conn, |c| c.arg2 = val);
                disp_arg3(g, conn);
            }
        }
        'R' => {
            if g.obj_protos.contains_key(&val) {
                with_cur(conn, |c| c.arg2 = val);
                disp_menu(g, conn);
            } else {
                send(g, conn, "That object does not exist, try again : ");
            }
        }
        _ => {}
    }
}

fn parse_arg3(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let val = atoi(t);
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'E' => {
            let mut maxpos = 0;
            while maxpos < EQUIPMENT_TYPES.len() && EQUIPMENT_TYPES[maxpos] != "\n" {
                maxpos += 1;
            }
            if val < 0 || val as usize > maxpos {
                send(g, conn, "Try again : ");
            } else {
                with_cur(conn, |c| c.arg3 = val);
                disp_menu(g, conn);
            }
        }
        'P' => {
            if g.obj_protos.contains_key(&val) {
                with_cur(conn, |c| c.arg3 = val);
                disp_arg4(g, conn);
            } else {
                send(g, conn, "That object does not exist, try again : ");
            }
        }
        'D' => {
            if !(0..=2).contains(&val) {
                send(g, conn, "Try again : ");
            } else {
                with_cur(conn, |c| c.arg3 = val);
                disp_menu(g, conn);
            }
        }
        'M' | 'O' | 'G' => {
            // Load-chance: 100 -> 0 (always), 1..99 stored directly.
            if val == 100 {
                with_cur(conn, |c| c.arg3 = 0);
                disp_menu(g, conn);
            } else if val > 0 && val < 100 {
                with_cur(conn, |c| c.arg3 = val);
                disp_menu(g, conn);
            } else {
                send(g, conn, "Give a number between 0 and 100. Try again: ");
            }
        }
        _ => {}
    }
}

fn parse_arg4(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let val = atoi(t);
    let cmd = cur_cmd_letter(conn);
    match cmd {
        'E' | 'M' | 'O' | 'P' => {
            if val == 100 {
                with_cur(conn, |c| c.arg4 = 0);
                disp_menu(g, conn);
            } else if val > 0 && val < 100 {
                with_cur(conn, |c| c.arg4 = 101 - val);
                disp_menu(g, conn);
            } else {
                send(g, conn, "Give a number between 1 and 100. Try again: ");
            }
        }
        _ => {}
    }
}

// ---- Change-chance (C/Q path) ---------------------------------------------

fn parse_prob(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if let Ok(pos) = t.parse::<usize>() {
        if start_change_command(conn, pos) {
            mark_cmds_changed(conn);
            send(g, conn, "Chance of loading (0-100) : ");
            set_mode(conn, Mode::Prob2);
            return;
        }
    }
    send(g, conn, "Invalid choice.\r\n");
    disp_menu(g, conn);
}

fn parse_prob2(g: &mut GameState, conn: ConnId, line: &str) {
    let val = atoi(line.trim()).clamp(0, 100);
    with_cur(conn, |c| c.arg4 = val);
    disp_menu(g, conn);
}

// ---- Header fields --------------------------------------------------------

fn parse_zone_name(g: &mut GameState, conn: ConnId, line: &str) {
    with_state(conn, |s| s.hdr.name = line.trim().to_string());
    mark_header_changed(conn);
    disp_menu(g, conn);
}

fn parse_zone_builders(g: &mut GameState, conn: ConnId, line: &str) {
    with_state(conn, |s| s.hdr.builders = line.trim().to_string());
    mark_header_changed(conn);
    disp_menu(g, conn);
}

fn parse_zone_top(g: &mut GameState, conn: ConnId, line: &str) {
    let val = atoi(line.trim());
    let (zone_index, zone_number) = {
        let st = states().lock().unwrap();
        match st.get(&conn) {
            Some(s) => (s.zone_index, s.zone_number),
            None => return,
        }
    };
    // Top is clamped to >= zone*100, and (if not the last zone) below the next
    // zone's base (CircleMUD ZEDIT_ZONE_TOP).
    let lower = zone_number * 100;
    let upper = g.zones.get(zone_index + 1).map(|z| z.number * 100);
    let new_top = match upper {
        Some(u) => val.max(lower).min(u),
        None => val.max(lower),
    };
    with_state(conn, |s| s.hdr.top = new_top);
    mark_header_changed(conn);
    disp_menu(g, conn);
}

fn parse_zone_life(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    let val = atoi(t);
    if !starts_digit(t) || val < 0 || val > 240 {
        send(g, conn, "Try again (0-240) : ");
        return;
    }
    with_state(conn, |s| s.hdr.lifespan = val);
    mark_header_changed(conn);
    disp_menu(g, conn);
}

fn parse_zone_reset(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    let val = atoi(t);
    if !starts_digit(t) || !(0..=2).contains(&val) {
        send(g, conn, "Try again (0-2) : ");
        return;
    }
    with_state(conn, |s| s.hdr.reset_mode = val);
    mark_header_changed(conn);
    disp_menu(g, conn);
}

fn parse_levels(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if starts_digit(t) {
        match atoi(t) {
            1 => {
                send(g, conn, "Minimum level? ");
                set_mode(conn, Mode::MinLvl);
                mark_header_changed(conn);
                return;
            }
            2 => {
                send(g, conn, "Maximum level? ");
                set_mode(conn, Mode::MaxLvl);
                mark_header_changed(conn);
                return;
            }
            _ => {
                disp_levels(g, conn);
                return;
            }
        }
    }
    mark_header_changed(conn);
    disp_levels(g, conn);
}

fn parse_min_lvl(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Value must be an integer value. Minimum level? ");
        return;
    }
    let pos = atoi(t);
    if pos > 100 {
        send(g, conn, "Value must be below 100. Minimum level? ");
        return;
    }
    if pos < 0 {
        send(g, conn, "Value must be above 0. Minimum level? ");
        return;
    }
    with_state(conn, |s| {
        s.hdr.lvl1 = pos;
        s.hdr.header_changed = true;
    });
    disp_menu(g, conn);
}

fn parse_max_lvl(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Value must be an integer value. Maximum level? ");
        return;
    }
    let pos = atoi(t);
    if pos > 100 {
        send(g, conn, "Value must be below 100. Maximum level? ");
        return;
    }
    if pos < 0 {
        send(g, conn, "Value must be above 0. Maximum level? ");
        return;
    }
    with_state(conn, |s| {
        s.hdr.lvl2 = pos;
        s.hdr.header_changed = true;
    });
    disp_menu(g, conn);
}

fn parse_approve(g: &mut GameState, conn: ConnId, line: &str) {
    match line.trim().chars().next().unwrap_or('\0') {
        'y' | 'Y' => with_state(conn, |s| s.hdr.status_mode = 1),
        'n' | 'N' => with_state(conn, |s| s.hdr.status_mode = 0),
        _ => {
            send(g, conn, "Try again : ");
            return;
        }
    }
    mark_header_changed(conn);
    disp_menu(g, conn);
}

// ---- Save / quit ----------------------------------------------------------

fn parse_confirm_save(g: &mut GameState, conn: ConnId, line: &str) {
    match line.trim().chars().next().unwrap_or('\0') {
        'y' | 'Y' => {
            send(g, conn, "Saving zone info in memory.\r\n");
            save_internally(g, conn);
            save_to_disk(g, conn);
            finish(g, conn);
        }
        'n' | 'N' => finish(g, conn),
        _ => {
            send(g, conn, "Invalid choice!\r\n");
            send(g, conn, "Do you wish to save the zone info? : ");
        }
    }
}

fn finish(g: &mut GameState, conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    olc::clear_active(conn);
    send(g, conn, "Zone editor exited.\r\n");
}

fn with_state<F: FnOnce(&mut ZeditState)>(conn: ConnId, f: F) {
    if let Some(s) = states().lock().unwrap().get_mut(&conn) {
        f(s);
    }
}

// ===========================================================================
// Save: splice the edited room's commands back, update header, sync the live
// Zone, and write the .zon file.
// ===========================================================================

/// Rebuild the zone's complete command list (every room) and write it into
/// GameState::zones as simplified ResetCmd entries (CircleMUD zedit_save_internally).
fn save_internally(g: &mut GameState, conn: ConnId) {
    let st = match snapshot(conn) {
        Some(s) => s,
        None => return,
    };

    // Full on-disk command list (all rooms), minus this room's old commands,
    // plus the edited ones spliced back in at the front.
    let path = zon_file_path(g, st.zone_number);
    let survivors = {
        let mut all = read_disk_cmds(&path).unwrap_or_else(|| cmds_from_memory(g, st.zone_index));
        let all_copy = all.clone();
        all.retain(|c| !belongs_to_room(&all_copy, st.room_vnum, c));
        all
    };
    let all = splice_room_cmds(&st.cmds, survivors);

    // Update the live Zone header + simplified reset list.
    if let Some(z) = g.zones.get_mut(st.zone_index) {
        if st.hdr.header_changed {
            z.name = st.hdr.name.clone();
            z.top = st.hdr.top;
            z.reset_mode = st.hdr.reset_mode;
            z.lifespan = st.hdr.lifespan;
            z.min_level = st.hdr.lvl1.clamp(0, 255) as Level;
            z.max_level = st.hdr.lvl2.clamp(0, 255) as Level;
        }
        z.reset_commands = all.iter().filter_map(raw_to_reset).collect();
    }
}

/// Write the zone's `.zon` file from the full command list (CircleMUD
/// zedit_save_to_disk), byte-faithfully. Each reset line is
/// `<cmd> <if_flag> <arg1> <arg2> <arg3> <arg4>`.
fn save_to_disk(g: &mut GameState, conn: ConnId) {
    let st = match snapshot(conn) {
        Some(s) => s,
        None => return,
    };
    let path = zon_file_path(g, st.zone_number);

    // Reconstruct the full command list the same way save_internally did, so
    // the file matches memory.
    let survivors = {
        let mut all = read_disk_cmds(&path).unwrap_or_else(|| cmds_from_memory(g, st.zone_index));
        let all_copy = all.clone();
        all.retain(|c| !belongs_to_room(&all_copy, st.room_vnum, c));
        all
    };
    let all = splice_room_cmds(&st.cmds, survivors);

    let name = if st.hdr.name.is_empty() {
        "undefined"
    } else {
        &st.hdr.name
    };
    let builders = if st.hdr.builders.is_empty() {
        "<NONE!>"
    } else {
        &st.hdr.builders
    };

    let mut out = String::new();
    out.push_str(&format!("#{}\n", st.zone_number));
    out.push_str(&format!("{}~\n", name));
    out.push_str(&format!("{}~\n", builders));
    out.push_str(&format!(
        "{} {} {}\n",
        st.hdr.top, st.hdr.lifespan, st.hdr.reset_mode
    ));
    out.push_str(&format!(
        "{} {} {}\n",
        st.hdr.lvl1, st.hdr.lvl2, st.hdr.status_mode
    ));

    for c in &all {
        if c.command == '*' || c.command == 'N' || c.command == 'S' {
            continue;
        }
        if !"MOGEPRD".contains(c.command) {
            continue;
        }
        out.push_str(&format!(
            "{} {} {} {} {} {}\n",
            c.command, c.if_flag, c.arg1, c.arg2, c.arg3, c.arg4
        ));
    }
    out.push_str("S\n$\n");

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, out.as_bytes()).is_err() {
        send(
            g,
            conn,
            "Warning: could not write the zone file to disk.\r\n",
        );
    }
}

/// zedit_save_to_disk(zone): central OLC save dispatcher entry. Writes the
/// current in-memory zone header/reset list to `<zone>.zon`.
pub fn zedit_save_to_disk(g: &mut GameState, zone_rnum: usize) {
    let z = match g.zones.get(zone_rnum) {
        Some(z) => z.clone(),
        None => return,
    };
    let all = cmds_from_memory(g, zone_rnum);
    let path = zon_file_path(g, z.number);

    let name = if z.name.is_empty() {
        "undefined"
    } else {
        &z.name
    };
    let builders = if z.builders.is_empty() {
        "<NONE!>"
    } else {
        &z.builders
    };

    let mut out = String::new();
    out.push_str(&format!("#{}\n", z.number));
    out.push_str(&format!("{}~\n", name));
    out.push_str(&format!("{}~\n", builders));
    out.push_str(&format!("{} {} {}\n", z.top, z.lifespan, z.reset_mode));
    out.push_str(&format!(
        "{} {} {}\n",
        z.min_level, z.max_level, z.status_mode
    ));

    for c in &all {
        if c.command == '*' || c.command == 'N' || c.command == 'S' {
            continue;
        }
        if !"MOGEPRD".contains(c.command) {
            continue;
        }
        out.push_str(&format!(
            "{} {} {} {} {} {}\n",
            c.command, c.if_flag, c.arg1, c.arg2, c.arg3, c.arg4
        ));
    }
    out.push_str("S\n$\n");

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, out.as_bytes());
    olc::olc_remove_from_save_list(z.number, olc::OLC_SAVE_ZONE);
}

/// Splice the edited room's reset commands back into the surviving (other-room)
/// command list, byte-faithfully matching CircleMUD `zedit_save_internally`.
///
/// C removes every command relating to the edited room from the live list, then
/// re-inserts the player's scratch commands with `add_cmd_to_list(list, MYCMD[i],
/// i)` for i = 0,1,2,... Each insertion places the command at absolute index `i`,
/// so the edited room's commands always end up at the FRONT of the list in their
/// scratch order, ahead of every surviving command. (Appending them at the end
/// instead reorders the file and breaks the load->save round-trip.) 'N' is the
/// freshly-created-blank sentinel and is never written.
fn splice_room_cmds(edited: &[RawCmd], survivors: Vec<RawCmd>) -> Vec<RawCmd> {
    let mut all: Vec<RawCmd> = edited
        .iter()
        .filter(|c| c.command != 'N')
        .copied()
        .collect();
    all.extend(survivors);
    all
}

/// Convert a RawCmd (vnum-based) into the simplified in-memory ResetCmd. Returns
/// None for command letters the Rust ResetCmd does not model.
fn raw_to_reset(c: &RawCmd) -> Option<ResetCmd> {
    let if_flag = c.if_flag != 0;
    match c.command {
        'M' => Some(ResetCmd::LoadMob {
            if_flag,
            mob_vnum: c.arg1,
            max_count: c.arg2,
            room_vnum: c.arg3,
            load_chance: c.arg4,
        }),
        'O' => Some(ResetCmd::LoadObjInRoom {
            if_flag,
            obj_vnum: c.arg1,
            max_count: c.arg2,
            room_vnum: c.arg3,
            load_chance: c.arg4,
        }),
        'G' => Some(ResetCmd::GiveObjToMob {
            if_flag,
            obj_vnum: c.arg1,
            max_count: c.arg2,
            load_chance: c.arg3,
        }),
        'E' => Some(ResetCmd::EquipMob {
            if_flag,
            obj_vnum: c.arg1,
            max_count: c.arg2,
            wear_pos: c.arg3.max(0) as usize,
            load_chance: c.arg4,
        }),
        'P' => Some(ResetCmd::PutObjInObj {
            if_flag,
            obj_vnum: c.arg1,
            max_count: c.arg2,
            container_vnum: c.arg3,
            load_chance: c.arg4,
        }),
        'R' => Some(ResetCmd::RemoveObj {
            if_flag,
            room_vnum: c.arg1,
            obj_vnum: c.arg2,
        }),
        'D' => Some(ResetCmd::Door {
            if_flag,
            room_vnum: c.arg1,
            direction: c.arg2.max(0) as usize,
            state: c.arg3,
        }),
        _ => None,
    }
}

/// Reconstruct RawCmds (vnum-based) from the in-memory ResetCmd list (used as a
/// fallback when the on-disk file is unreadable). The load-chance (arg4, or arg3
/// for G) now round-trips through the in-memory model.
fn cmds_from_memory(g: &GameState, zone_index: usize) -> Vec<RawCmd> {
    let z = match g.zones.get(zone_index) {
        Some(z) => z,
        None => return Vec::new(),
    };
    z.reset_commands
        .iter()
        .map(|rc| match rc {
            ResetCmd::LoadMob {
                if_flag,
                mob_vnum,
                max_count,
                room_vnum,
                load_chance,
            } => RawCmd {
                command: 'M',
                if_flag: *if_flag as i32,
                arg1: *mob_vnum,
                arg2: *max_count,
                arg3: *room_vnum,
                arg4: *load_chance,
            },
            ResetCmd::LoadObjInRoom {
                if_flag,
                obj_vnum,
                max_count,
                room_vnum,
                load_chance,
            } => RawCmd {
                command: 'O',
                if_flag: *if_flag as i32,
                arg1: *obj_vnum,
                arg2: *max_count,
                arg3: *room_vnum,
                arg4: *load_chance,
            },
            ResetCmd::GiveObjToMob {
                if_flag,
                obj_vnum,
                max_count,
                load_chance,
            } => RawCmd {
                command: 'G',
                if_flag: *if_flag as i32,
                arg1: *obj_vnum,
                arg2: *max_count,
                arg3: *load_chance,
                arg4: 0,
            },
            ResetCmd::EquipMob {
                if_flag,
                obj_vnum,
                max_count,
                wear_pos,
                load_chance,
            } => RawCmd {
                command: 'E',
                if_flag: *if_flag as i32,
                arg1: *obj_vnum,
                arg2: *max_count,
                arg3: *wear_pos as i32,
                arg4: *load_chance,
            },
            ResetCmd::PutObjInObj {
                if_flag,
                obj_vnum,
                max_count,
                container_vnum,
                load_chance,
            } => RawCmd {
                command: 'P',
                if_flag: *if_flag as i32,
                arg1: *obj_vnum,
                arg2: *max_count,
                arg3: *container_vnum,
                arg4: *load_chance,
            },
            ResetCmd::RemoveObj {
                if_flag,
                room_vnum,
                obj_vnum,
            } => RawCmd {
                command: 'R',
                if_flag: *if_flag as i32,
                arg1: *room_vnum,
                arg2: *obj_vnum,
                arg3: -1,
                arg4: 0,
            },
            ResetCmd::Door {
                if_flag,
                room_vnum,
                direction,
                state,
            } => RawCmd {
                command: 'D',
                if_flag: *if_flag as i32,
                arg1: *room_vnum,
                arg2: *direction as i32,
                arg3: *state,
                arg4: 0,
            },
        })
        .collect()
}

// ===========================================================================
// On-disk .zon reading (full fidelity, including if_flag/arg4).
// ===========================================================================

/// The on-disk zone header (everything before the reset-command list).
struct DiskHeader {
    name: String,
    builders: String,
    top: RoomVnum,
    lifespan: i32,
    reset_mode: i32,
    lvl1: i32,
    lvl2: i32,
    status_mode: i32,
}

/// Parse just the header of a `.zon` file (name, builders, top/life/reset, and
/// the optional level/status line). Returns None if the file is unreadable.
fn read_disk_header(path: &std::path::Path) -> Option<DiskHeader> {
    let contents = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;
    while i < lines.len() && !lines[i].trim_start().starts_with('#') {
        i += 1;
    }
    if i >= lines.len() {
        return None;
    }
    i += 1; // skip #num

    // name~ (may span lines).
    let mut name = String::new();
    while i < lines.len() {
        if let Some(p) = lines[i].find('~') {
            if !name.is_empty() {
                name.push('\n');
            }
            name.push_str(&lines[i][..p]);
            i += 1;
            break;
        }
        if !name.is_empty() {
            name.push('\n');
        }
        name.push_str(lines[i]);
        i += 1;
    }
    // builders~ (may span lines).
    let mut builders = String::new();
    while i < lines.len() {
        if let Some(p) = lines[i].find('~') {
            if !builders.is_empty() {
                builders.push('\n');
            }
            builders.push_str(&lines[i][..p]);
            i += 1;
            break;
        }
        if !builders.is_empty() {
            builders.push('\n');
        }
        builders.push_str(lines[i]);
        i += 1;
    }
    if builders == "<NONE!>" {
        builders.clear();
    }

    // "top lifespan reset_mode".
    let hl: Vec<i32> = lines
        .get(i)
        .map(|l| {
            l.split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    i += 1;
    let top = *hl.first().unwrap_or(&0);
    let lifespan = *hl.get(1).unwrap_or(&30);
    let reset_mode = *hl.get(2).unwrap_or(&2);

    // Optional "lvl1 lvl2 status" line.
    let (mut lvl1, mut lvl2, mut status_mode) = (0, 0, 0);
    if let Some(l) = lines.get(i) {
        let toks: Vec<&str> = l.split_whitespace().collect();
        let nums: Vec<i32> = toks.iter().filter_map(|t| t.parse().ok()).collect();
        if !toks.is_empty() && nums.len() == toks.len() && nums.len() >= 3 {
            lvl1 = nums[0];
            lvl2 = nums[1];
            status_mode = nums[2];
        }
    }

    Some(DiskHeader {
        name,
        builders,
        top,
        lifespan,
        reset_mode,
        lvl1,
        lvl2,
        status_mode,
    })
}

/// Parse a `.zon` file's reset-command lines into RawCmds. Returns None if the
/// file cannot be read at all (caller falls back to the in-memory list).
fn read_disk_cmds(path: &std::path::Path) -> Option<Vec<RawCmd>> {
    let contents = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;

    // Skip the header: #num / name~ / builders~ / "top life reset" / optional
    // "lvl1 lvl2 status". We advance to the first reset command line, which is
    // any line whose first token is one of M/O/G/E/P/R/D/S/* and that parses.
    // To be robust, skip lines until we have consumed: the #-line, two ~-lines,
    // and the numeric header line(s).
    // Simpler: find the #-line, then the two tilde strings, then numeric lines,
    // then commands.
    while i < lines.len() && !lines[i].trim_start().starts_with('#') {
        i += 1;
    }
    if i >= lines.len() {
        return Some(Vec::new());
    }
    i += 1; // skip #num

    // Skip name~ (may span lines).
    while i < lines.len() && !lines[i].contains('~') {
        i += 1;
    }
    i += 1;
    // Skip builders~ (may span lines).
    while i < lines.len() && !lines[i].contains('~') {
        i += 1;
    }
    i += 1;
    // Skip the "top lifespan reset_mode" numeric line.
    if i < lines.len() {
        i += 1;
    }
    // Optional "lvl1 lvl2 status" line: present iff the next line is all-numeric
    // with at least 3 tokens. Reset commands always begin with a letter, so an
    // all-numeric line here can only be the level/status header.
    if i < lines.len() {
        let toks: Vec<&str> = lines[i].split_whitespace().collect();
        let all_num = !toks.is_empty() && toks.iter().all(|x| x.parse::<i32>().is_ok());
        if all_num && toks.len() >= 3 {
            i += 1;
        }
    }

    let mut out = Vec::new();
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        if t.is_empty() || t.starts_with('*') {
            continue;
        }
        let first = t.chars().next().unwrap();
        if first == 'S' || first == '$' {
            break;
        }
        if !"MOGEPRD".contains(first) {
            continue;
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        let n = |k: usize| toks.get(k).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        out.push(RawCmd {
            command: first,
            if_flag: n(1).max(0),
            arg1: n(2),
            arg2: n(3),
            arg3: n(4),
            arg4: n(5),
        });
    }
    Some(out)
}

// ===========================================================================
// Tiny helpers.
// ===========================================================================

fn starts_digit(s: &str) -> bool {
    s.bytes()
        .next()
        .map(|b| b.is_ascii_digit())
        .unwrap_or(false)
}

fn atoi(s: &str) -> i32 {
    let t = s.trim();
    let mut end = 0;
    let bytes = t.as_bytes();
    if !bytes.is_empty() && (bytes[0] == b'-' || bytes[0] == b'+') {
        end = 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    t[..end].parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::world::Zone;

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

    #[test]
    fn reset_command_vnums_must_belong_to_builders_zone() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Builder"));
        g.zones.push(zone(2, "Other"));
        let builder = player(&mut g, "Builder", LVL_IMMORT);
        let imp = player(&mut g, "Root", LVL_IMPL);

        assert!(can_edit_vnum_zone(&g, builder, 150));
        assert!(!can_edit_vnum_zone(&g, builder, 250));
        assert!(can_edit_vnum_zone(&g, imp, 250));
    }
}
