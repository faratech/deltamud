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
use crate::olc::{self, EditorKind, new_zone_unresolved_key};
use crate::state::GameState;
use crate::types::*;
use crate::world::{MAX_ZONE_NUMBER, ResetCmd, zone_vnum_bounds};
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
    authorization: olc::OlcAuthorization,
    hdr: ZoneHdr,
    cmds: Vec<RawCmd>, // commands relating to this room (no 'S' terminator)
    mode: Mode,
    cur: usize, // OLC_VAL — index of the command currently being changed
    trust_of_editor: i32,
}

fn states() -> &'static Mutex<HashMap<ConnId, ZeditState>> {
    static S: OnceLock<Mutex<HashMap<ConnId, ZeditState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit), releasing the per-conn working copy. `olc::abort_editor` clears
/// active.
pub fn abort(conn: ConnId) {
    if let Some(state) = crate::lock_ok::lock(&states()).remove(&conn) {
        olc::discard_unresolved_save(EditorKind::Zedit, state.zone_number);
    }
}

// ---------------------------------------------------------------------------
// Output helper.
// ---------------------------------------------------------------------------
fn send(g: &mut GameState, conn: ConnId, msg: &str) {
    // C get_char_cols: colour gated on the builder's colour level (#306).
    crate::olc::olc_send(g, conn, msg);
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
    let Some(authorization) = olc::capture_olc_authorization(g, ch) else {
        send(g, conn, "You do not have access to the zone editor.\r\n");
        return;
    };
    let authority = olc::validated_olc_trust(g, ch).unwrap_or(-1);
    if authority < i32::from(LVL_IMMORT) {
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
        match crate::text::parse_i32_strict(arg) {
            Ok(v) => v,
            Err(crate::text::ParseIntError::Overflow) => {
                send(
                    g,
                    conn,
                    "That room VNUM is outside the supported range.\r\n",
                );
                return;
            }
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

    if !can_edit_zone(g, ch, zone_number) {
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
    let disk = match read_disk_header(&path) {
        Ok(disk) => disk,
        Err(error) => {
            log::warn!(
                "SYSERR: OLC: cannot safely read zone {} header: {}",
                zone_number,
                error
            );
            send(
                g,
                conn,
                "The existing zone file could not be read safely; no editor was opened.\r\n",
            );
            return;
        }
    };
    hdr.name = disk.name;
    hdr.builders = disk.builders;
    hdr.top = disk.top;
    hdr.lifespan = disk.lifespan;
    hdr.reset_mode = disk.reset_mode;
    hdr.lvl1 = disk.lvl1;
    hdr.lvl2 = disk.lvl2;
    hdr.status_mode = disk.status_mode;

    // Load this room's reset commands from the on-disk `.zon` file (full
    // fidelity incl. if_flag/arg4), falling back to the in-memory simplified
    // ResetCmd list if the file is unreadable.
    let all = match read_disk_cmds(&path) {
        Ok(commands) => commands,
        Err(error) => {
            log::warn!(
                "SYSERR: OLC: cannot safely read zone {} resets: {}",
                zone_number,
                error
            );
            send(
                g,
                conn,
                "The existing zone resets could not be read safely; no editor was opened.\r\n",
            );
            return;
        }
    };
    let all_copy = all.clone();
    let cmds: Vec<RawCmd> = {
        // all_copy is a snapshot of the same list: indices line up 1:1, so
        // look each element's index up by identity.
        all.iter()
            .enumerate()
            .filter(|(i, c)| {
                belongs_to_room_at(&all_copy, room_vnum, *i)
                    && all_copy
                        .get(*i)
                        .map(|x| std::ptr::eq(x, *c))
                        .unwrap_or(false)
            })
            .map(|(_, c)| c.clone())
            .collect()
    };

    let st = ZeditState {
        room_vnum,
        zone_number,
        zone_index,
        authorization,
        hdr,
        cmds,
        mode: Mode::MainMenu,
        cur: 0,
        trust_of_editor: authority,
    };
    // C olc.c:198-212 (#272): key on the zone being edited.
    if crate::lock_ok::lock(&states())
        .values()
        .any(|s| s.zone_number == st.zone_number)
    {
        g.send_to_char(
            ch,
            "That zone is currently being edited by someone else.\r\n",
        );
        return;
    }
    crate::lock_ok::lock(&states()).insert(conn, st);
    olc::set_active(conn, EditorKind::Zedit);
    // C olc.c:381-382 (#273).
    if let Some(cid) = editor_char(g, conn) {
        if let Some(c) = g.get_char_mut(cid) {
            c.act_flags |= crate::flags::PLR_WRITING;
        }
    }
    disp_menu(g, conn);
}

// ===========================================================================
// Zone / permission helpers.
// ===========================================================================

fn zone_for_vnum(g: &GameState, vnum: RoomVnum) -> Option<i32> {
    g.zones
        .iter()
        .find(|z| z.contains_vnum(vnum))
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
fn belongs_to_room_at(cmds: &[RawCmd], room_vnum: RoomVnum, target: usize) -> bool {
    // Position-based: the OLD implementation matched by pointer identity
    // (std::ptr::eq(cmd, c)), but every caller passes a CLONE of the list, so
    // the match never fired -- zedit showed an empty command list and every
    // save appended a fresh copy of the room's resets instead of replacing
    // them (double mob loads after reboot).
    let mut cur: Option<RoomVnum> = None;
    for (i, cmd) in cmds.iter().enumerate() {
        match cmd.command {
            'M' | 'O' => cur = Some(cmd.arg3),
            'D' | 'R' => cur = Some(cmd.arg1),
            'G' | 'E' | 'P' => {}
            _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
        }
        if i == target {
            return cur == Some(room_vnum);
        }
    }
    false
}

fn filter_room_cmds(cmds: &[RawCmd], room_vnum: RoomVnum, keep: bool) -> Vec<RawCmd> {
    cmds.iter()
        .enumerate()
        .filter(|(i, _)| belongs_to_room_at(cmds, room_vnum, *i) == keep)
        .map(|(_, c)| c.clone())
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
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
    crate::lock_ok::lock(&states())
        .get(&conn)
        .map(|s| ZeditState {
            room_vnum: s.room_vnum,
            zone_number: s.zone_number,
            zone_index: s.zone_index,
            authorization: s.authorization,
            hdr: s.hdr.clone(),
            cmds: s.cmds.clone(),
            mode: s.mode,
            cur: s.cur,
            trust_of_editor: s.trust_of_editor,
        })
}

fn set_mode(conn: ConnId, mode: Mode) {
    if let Some(s) = crate::lock_ok::lock(&states()).get_mut(&conn) {
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
    let g = crate::lock_ok::lock(&states());
    g.get(&conn)
        .and_then(|s| s.cmds.get(s.cur))
        .map(|c| c.command)
        .unwrap_or('\0')
}

fn with_cur<F: FnOnce(&mut RawCmd)>(conn: ConnId, f: F) {
    let mut g = crate::lock_ok::lock(&states());
    if let Some(s) = g.get_mut(&conn) {
        let cur = s.cur;
        if let Some(c) = s.cmds.get_mut(cur) {
            f(c);
        }
    }
}

fn mark_cmds_changed(conn: ConnId) {
    if let Some(s) = crate::lock_ok::lock(&states()).get_mut(&conn) {
        s.hdr.cmds_changed = true;
    }
}
fn mark_header_changed(conn: ConnId) {
    if let Some(s) = crate::lock_ok::lock(&states()).get_mut(&conn) {
        s.hdr.header_changed = true;
    }
}

// ===========================================================================
// The line parser — zedit_parse.
// ===========================================================================

pub fn zedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match crate::lock_ok::lock(&states()).get(&conn) {
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
    let authority = states()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|s| s.trust_of_editor)
        .unwrap_or(-1);
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
            if authority != i32::from(LVL_IMPL) {
                disp_menu(g, conn);
            } else {
                send(g, conn, "Enter new top of zone : ");
                set_mode(conn, Mode::ZoneTop);
            }
        }
        'a' | 'A' => {
            if authority < i32::from(LVL_REVIEW) {
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
            if authority != i32::from(LVL_IMPL) {
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
    let mut g = crate::lock_ok::lock(&states());
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
    let mut g = crate::lock_ok::lock(&states());
    if let Some(s) = g.get_mut(&conn) {
        if pos < s.cmds.len() {
            s.cmds.remove(pos);
        }
    }
}

/// Set the "current" command index to pos if valid (CircleMUD start_change_command).
fn start_change_command(conn: ConnId, pos: usize) -> bool {
    let mut g = crate::lock_ok::lock(&states());
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
    let Some(vnum) = olc_atoi(g, conn, t) else {
        return;
    };
    let cmd = cur_cmd_letter(conn);
    let ch = char_for_conn(g, conn);
    match cmd {
        'M' => {
            // Persisted trust and builder ownership, not character level,
            // control cross-zone reset references.
            if !ch
                .map(|id| can_edit_vnum_zone(g, id, vnum))
                .unwrap_or(false)
            {
                send(
                    g,
                    conn,
                    "You don't have permissions to that zone, try again : ",
                );
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
    }
}

fn parse_arg2(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let Some(val) = olc_atoi(g, conn, t) else {
        return;
    };
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
    }
}

fn parse_arg3(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let Some(val) = olc_atoi(g, conn, t) else {
        return;
    };
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
    }
}

fn parse_arg4(g: &mut GameState, conn: ConnId, line: &str) {
    let t = line.trim();
    if !starts_digit(t) {
        send(g, conn, "Must be a numeric value, try again : ");
        return;
    }
    let Some(val) = olc_atoi(g, conn, t) else {
        return;
    };
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
        _ => log::warn!("SYSERR: OLC: zedit_parse(): invalid command state"),
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
    let Some(val) = olc_atoi(g, conn, line.trim()) else {
        return;
    };
    let val = val.clamp(0, 100);
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
    let Some(val) = olc_atoi(g, conn, line.trim()) else {
        return;
    };
    let (zone_index, zone_number) = {
        let st = crate::lock_ok::lock(&states());
        match st.get(&conn) {
            Some(s) => (s.zone_index, s.zone_number),
            None => return,
        }
    };
    // Top is clamped to >= zone*100, and (if not the last zone) below the next
    // zone's base (CircleMUD ZEDIT_ZONE_TOP).
    let Some(lower) = zone_vnum_bounds(zone_number).map(|(first, _)| first) else {
        send(
            g,
            conn,
            "That zone number is outside the supported range.\r\n",
        );
        return;
    };
    let upper = g.zones.get(zone_index + 1).and_then(|z| z.vnum_start());
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
    let Some(val) = olc_atoi(g, conn, t) else {
        return;
    };
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
    let Some(val) = olc_atoi(g, conn, t) else {
        return;
    };
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
        let Some(value) = olc_atoi(g, conn, t) else {
            return;
        };
        match value {
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
        disp_menu(g, conn); // C zedit.c:1626-1643: error, then back to the menu (#280)
        return;
    }
    let Some(pos) = olc_atoi(g, conn, t) else {
        return;
    };
    if pos > 100 {
        send(g, conn, "Value must be below 100. Minimum level? ");
        disp_menu(g, conn);
        return;
    }
    if pos < 0 {
        send(g, conn, "Value must be above 0. Minimum level? ");
        disp_menu(g, conn);
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
        disp_menu(g, conn); // #280
        return;
    }
    let Some(pos) = olc_atoi(g, conn, t) else {
        return;
    };
    if pos > 100 {
        send(g, conn, "Value must be below 100. Maximum level? ");
        disp_menu(g, conn);
        return;
    }
    if pos < 0 {
        send(g, conn, "Value must be above 0. Maximum level? ");
        disp_menu(g, conn);
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
    parse_confirm_save_with(g, conn, line, crate::olc::atomic_replace)
}

fn parse_confirm_save_with<F>(g: &mut GameState, conn: ConnId, line: &str, replace: F)
where
    F: FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
{
    match line.trim().chars().next().unwrap_or('\0') {
        'y' | 'Y' => match save_to_disk_with(g, conn, replace) {
            Ok((all_commands, None)) => {
                save_internally(g, conn, &all_commands);
                if let Some(state) = snapshot(conn) {
                    crate::olc::clear_unresolved_publication(EditorKind::Zedit, state.zone_number);
                    crate::olc::olc_remove_from_save_list(
                        state.zone_number,
                        crate::olc::OLC_SAVE_ZONE,
                    );
                }
                send(g, conn, "Zone info saved to disk and memory.\r\n");
                finish(g, conn);
            }
            Ok((all_commands, Some(err))) => {
                log::warn!("SYSERR: OLC: could not confirm zone durability: {}", err);
                // The exact validated candidate is still in scope after
                // rename, so reconciliation cannot fail on a secondary read.
                if let Some(state) = snapshot(conn) {
                    crate::olc::mark_unresolved_save_failure(
                        EditorKind::Zedit,
                        state.zone_number,
                        &err,
                    );
                    crate::olc::olc_add_to_save_list(state.zone_number, crate::olc::OLC_SAVE_ZONE);
                }
                save_internally(g, conn, &all_commands);
                send(
                    g,
                    conn,
                    "The zone file was published and live resets were reconciled, but crash durability could not be confirmed.\r\nDo you wish to retry saving the zone info? : ",
                );
            }
            Err(err) => {
                log::warn!("SYSERR: OLC: could not save zone info: {}", err);
                if let Some(state) = snapshot(conn) {
                    crate::olc::mark_unresolved_save_failure(
                        EditorKind::Zedit,
                        state.zone_number,
                        &err,
                    );
                }
                send(
                    g,
                    conn,
                    "Could not save the zone file; the live zone was not changed.\r\nDo you wish to retry saving the zone info? : ",
                );
            }
        },
        'n' | 'N' => finish(g, conn),
        _ => {
            send(g, conn, "Invalid choice!\r\n");
            send(g, conn, "Do you wish to save the zone info? : ");
        }
    }
}

fn editor_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

fn finish(g: &mut GameState, conn: ConnId) {
    if let Some(state) = crate::lock_ok::lock(&states()).remove(&conn) {
        olc::discard_unresolved_save(EditorKind::Zedit, state.zone_number);
    }
    olc::clear_active(conn);
    // C olc.c:610-613 cleanup_olc (#273).
    if let Some(cid) = editor_char(g, conn) {
        if let Some(c) = g.get_char_mut(cid) {
            c.act_flags &= !crate::flags::PLR_WRITING;
        }
        crate::act::act(
            g,
            "$n stops using OLC.",
            true,
            cid,
            None,
            crate::act::ActArg::None,
            crate::act::To::Room,
        );
    }
}

fn with_state<F: FnOnce(&mut ZeditState)>(conn: ConnId, f: F) {
    if let Some(s) = crate::lock_ok::lock(&states()).get_mut(&conn) {
        f(s);
    }
}

// ===========================================================================
// Save: splice the edited room's commands back, update header, sync the live
// Zone, and write the .zon file.
// ===========================================================================

/// Rebuild the zone's complete command list (every room) and write it into
/// GameState::zones as simplified ResetCmd entries (CircleMUD zedit_save_internally).
fn save_internally(g: &mut GameState, conn: ConnId, all: &[RawCmd]) {
    let st = match snapshot(conn) {
        Some(s) => s,
        None => return,
    };

    // Update the live Zone header + simplified reset list.
    if let Some(z) = g
        .zones
        .iter_mut()
        .find(|zone| zone.number == st.zone_number)
    {
        if st.hdr.header_changed {
            z.name = st.hdr.name.clone();
            z.builders = st.hdr.builders.clone();
            z.top = st.hdr.top;
            z.reset_mode = st.hdr.reset_mode;
            z.lifespan = st.hdr.lifespan;
            z.min_level = st.hdr.lvl1.clamp(0, 255) as Level;
            z.max_level = st.hdr.lvl2.clamp(0, 255) as Level;
            z.status_mode = st.hdr.status_mode;
        }
        z.reset_commands = all.iter().filter_map(raw_to_reset).collect();
    }
}

/// Write the zone's `.zon` file from the full command list (CircleMUD
/// zedit_save_to_disk), byte-faithfully. Each reset line is
/// `<cmd> <if_flag> <arg1> <arg2> <arg3> <arg4>`.
fn save_to_disk_with<F>(
    g: &mut GameState,
    conn: ConnId,
    replace: F,
) -> std::io::Result<(Vec<RawCmd>, Option<std::io::Error>)>
where
    F: FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
{
    let st = match snapshot(conn) {
        Some(s) => s,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "zone editor state is missing",
            ));
        }
    };
    let zone_rnum = olc::real_zone(g, st.room_vnum).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "zone editor mapping changed",
        )
    })?;
    if g.zones.get(zone_rnum).map(|zone| zone.number) != Some(st.zone_number) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "zone editor mapping changed",
        ));
    }
    olc::revalidate_olc_authorization(g, st.authorization, false, Some(zone_rnum))?;
    let path = zon_file_path(g, st.zone_number);

    // Reconstruct the full command list the same way save_internally did, so
    // the file matches memory.
    let survivors = {
        let mut all = read_disk_cmds(&path)?;
        // Drop every command that belongs to the edited room (the edited
        // scratch commands are spliced back in at the front by the caller).
        all = filter_room_cmds(&all, st.room_vnum, false);
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
        std::fs::create_dir_all(parent)?;
    }
    match replace(&path, out.as_bytes()) {
        Ok(()) => Ok((all, None)),
        Err(error) if crate::olc::replacement_was_published(&error) => Ok((all, Some(error))),
        Err(error) => Err(error),
    }
}

/// zedit_save_to_disk(zone): central OLC save dispatcher entry. Writes the
/// current in-memory zone header/reset list to `<zone>.zon`.
pub fn zedit_save_to_disk(g: &mut GameState, zone_rnum: usize) -> std::io::Result<()> {
    if let Some(z) = g.zones.get(zone_rnum) {
        // C zedit.c:474: mark pending, cleared by the disk write below (#274).
        crate::olc::olc_add_to_save_list(z.number, crate::olc::OLC_SAVE_ZONE);
    }
    let z = match g.zones.get(zone_rnum) {
        Some(z) => z.clone(),
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "zone index is not loaded",
            ));
        }
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
        std::fs::create_dir_all(parent)?;
    }
    crate::olc::atomic_replace(&path, out.as_bytes())?;
    olc::olc_remove_from_save_list(z.number, olc::OLC_SAVE_ZONE);
    olc::clear_published_unresolved_numeric_range(EditorKind::Zedit, z.number, z.number);
    Ok(())
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
                // C zedit.c:550 writes -1 in the unused arg4 column.
                arg4: -1,
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
                // C zedit.c:577-578: both unused columns are -1.
                arg3: -1,
                arg4: -1,
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
                // C zedit.c:571 writes -1 in the unused arg4 column.
                arg4: -1,
            },
        })
        .collect()
}

// ===========================================================================
// On-disk .zon reading (full fidelity, including if_flag/arg4).
// ===========================================================================

/// The on-disk zone header (everything before the reset-command list).
#[derive(Debug)]
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

/// Parse just the header of an existing `.zon` file. Any I/O or structural
/// error is fatal to editing: silently substituting defaults here would erase
/// builders and authorization-related metadata on the next save.
fn read_disk_header(path: &std::path::Path) -> std::io::Result<DiskHeader> {
    let contents = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let number_line = lines
        .get(i)
        .ok_or_else(|| invalid_zone(path, "file is empty"))?;
    let number = number_line
        .trim()
        .strip_prefix('#')
        .ok_or_else(|| invalid_zone(path, "missing #zone header"))?;
    zone_i32(path, "zone number", number)?;
    i += 1; // skip #num

    // C db.c:1561-1567 reads exactly ONE line for name and one for builders,
    // truncating at the first '~'. A multi-line scan would shift every
    // subsequent header field up a line on save (#286).
    let name_line = lines
        .get(i)
        .ok_or_else(|| invalid_zone(path, "missing zone name"))?;
    let name_end = name_line
        .find('~')
        .ok_or_else(|| invalid_zone(path, "unterminated zone name"))?;
    let name = name_line[..name_end].to_string();
    i += 1;
    let builders_line = lines
        .get(i)
        .ok_or_else(|| invalid_zone(path, "missing builders line"))?;
    let builders_end = builders_line
        .find('~')
        .ok_or_else(|| invalid_zone(path, "unterminated builders line"))?;
    let mut builders = builders_line[..builders_end].to_string();
    i += 1;
    if builders == "<NONE!>" {
        builders.clear();
    }

    // "top lifespan reset_mode".
    let header_line = lines
        .get(i)
        .ok_or_else(|| invalid_zone(path, "missing top/lifespan/reset header"))?;
    let header_tokens: Vec<&str> = header_line.split_whitespace().collect();
    if header_tokens.len() != 3 {
        return Err(invalid_zone(
            path,
            "top/lifespan/reset header must contain exactly three integers",
        ));
    }
    let hl: Vec<i32> = header_tokens
        .iter()
        .enumerate()
        .map(|(index, token)| zone_i32(path, &format!("header field {}", index + 1), token))
        .collect::<std::io::Result<_>>()?;
    i += 1;
    let top = hl[0];
    let lifespan = hl[1];
    let reset_mode = hl[2];

    // Optional "lvl1 lvl2 status" line.
    let (mut lvl1, mut lvl2, mut status_mode) = (0, 0, 0);
    if let Some(l) = lines.get(i) {
        let toks: Vec<&str> = l.split_whitespace().collect();
        if toks.len() == 3
            && toks
                .iter()
                .all(|token| crate::text::parse_i32_strict(token).is_ok())
        {
            let nums: Vec<i32> = toks
                .iter()
                .map(|token| zone_i32(path, "level/status header", token))
                .collect::<std::io::Result<_>>()?;
            lvl1 = nums[0];
            lvl2 = nums[1];
            status_mode = nums[2];
        } else if toks.first().is_some_and(|token| {
            token
                .chars()
                .next()
                .is_some_and(|command| !"MOGEPRDS*$".contains(command))
        }) {
            return Err(invalid_zone(path, "malformed optional level/status header"));
        }
    }

    Ok(DiskHeader {
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

/// Parse an existing `.zon` file's reset-command lines without dropping
/// malformed commands. A destructive rewrite is refused unless both the `S`
/// command terminator and the final `$` record terminator are present.
fn read_disk_cmds(path: &std::path::Path) -> std::io::Result<Vec<RawCmd>> {
    let contents = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = contents.lines().collect();
    let mut i = 0;

    // Skip the header: #num / name~ / builders~ / "top life reset" / optional
    // "lvl1 lvl2 status". We advance to the first reset command line, which is
    // any line whose first token is one of M/O/G/E/P/R/D/S/* and that parses.
    // To be robust, skip lines until we have consumed: the #-line, two ~-lines,
    // and the numeric header line(s).
    // Simpler: find the #-line, then the two tilde strings, then numeric lines,
    // then commands.
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let number_line = lines
        .get(i)
        .ok_or_else(|| invalid_zone(path, "file is empty"))?;
    let number = number_line
        .trim()
        .strip_prefix('#')
        .ok_or_else(|| invalid_zone(path, "missing #zone header"))?;
    zone_i32(path, "zone number", number)?;
    i += 1; // skip #num

    // Zone names/builders are single-line tilde strings in this format.
    if !lines.get(i).is_some_and(|line| line.contains('~')) {
        return Err(invalid_zone(path, "unterminated zone name"));
    }
    i += 1;
    if !lines.get(i).is_some_and(|line| line.contains('~')) {
        return Err(invalid_zone(path, "unterminated builders line"));
    }
    i += 1;
    let header_tokens: Vec<&str> = lines
        .get(i)
        .ok_or_else(|| invalid_zone(path, "missing top/lifespan/reset header"))?
        .split_whitespace()
        .collect();
    if header_tokens.len() != 3 {
        return Err(invalid_zone(
            path,
            "top/lifespan/reset header must contain exactly three integers",
        ));
    }
    for token in header_tokens {
        zone_i32(path, "top/lifespan/reset header", token)?;
    }
    i += 1;
    // Optional "lvl1 lvl2 status" line: present iff the next line is all-numeric
    // with at least 3 tokens. Reset commands always begin with a letter, so an
    // all-numeric line here can only be the level/status header.
    if i < lines.len() {
        let toks: Vec<&str> = lines[i].split_whitespace().collect();
        let all_num = toks.len() == 3
            && toks
                .iter()
                .all(|x| crate::text::parse_i32_strict(x).is_ok());
        if all_num {
            i += 1;
        }
    }

    let mut out = Vec::new();
    let mut saw_command_terminator = false;
    let mut saw_file_terminator = false;
    while i < lines.len() {
        let t = lines[i].trim();
        i += 1;
        if t.is_empty() || t.starts_with('*') {
            continue;
        }
        let first = t.chars().next().expect("non-empty reset line");
        if first == 'S' {
            saw_command_terminator = true;
            continue;
        }
        if first == '$' {
            saw_file_terminator = true;
            break;
        }
        if saw_command_terminator {
            return Err(invalid_zone(
                path,
                &format!("content appears after the S terminator: {t:?}"),
            ));
        }
        if !"MOGEPRD".contains(first) {
            return Err(invalid_zone(
                path,
                &format!("unsupported reset command line {t:?}"),
            ));
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        if toks.len() != 6 {
            return Err(invalid_zone(
                path,
                &format!("reset command must contain six fields: {t:?}"),
            ));
        }
        let n = |k: usize| zone_i32(path, "reset command", toks[k]);
        let if_flag = n(1)?;
        let arg1 = n(2)?;
        let arg2 = n(3)?;
        let arg3 = n(4)?;
        let arg4 = n(5)?;
        out.push(RawCmd {
            command: first,
            if_flag: if_flag.max(0),
            arg1,
            arg2,
            arg3,
            arg4,
        });
    }
    if !saw_command_terminator || !saw_file_terminator {
        return Err(invalid_zone(
            path,
            "zone reset list is missing S and/or $ terminator",
        ));
    }
    Ok(out)
}

fn zone_i32(path: &std::path::Path, field: &str, raw: &str) -> std::io::Result<i32> {
    crate::text::parse_i32_strict(raw)
        .map_err(|error| invalid_zone(path, &format!("invalid {field} value {raw:?}: {error:?}")))
}

fn invalid_zone(path: &std::path::Path, message: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{} in {}; refusing destructive rewrite",
            message,
            path.display()
        ),
    )
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

fn olc_atoi(g: &mut GameState, conn: ConnId, s: &str) -> Option<i32> {
    match crate::text::parse_i32_atoi(s) {
        Ok(value) => Some(value),
        Err(crate::text::ParseIntError::Overflow) => {
            send(g, conn, "That number is outside the supported range.\r\n");
            None
        }
        Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::room::Room;
    use crate::world::{Zone, zone_vnum_bounds};

    fn zone(number: i32, builders: &str) -> Zone {
        let (_, top) = zone_vnum_bounds(number).expect("valid test zone number");
        Zone {
            number,
            name: format!("Zone {}", number),
            builders: builders.to_string(),
            lifespan: 30,
            age: 0,
            top,
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
        ch.trust = i32::from(level);
        ch.godcmds2 |= crate::gcmd::GCMD2_OLC;
        g.create_char(ch)
    }

    #[test]
    fn reset_command_vnums_must_belong_to_builders_zone() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Builder"));
        g.zones.push(zone(2, "Other"));
        let builder = player(&mut g, "Builder", LVL_IMMORT);
        let imp = player(&mut g, "Root", LVL_IMPL);
        for (ch, conn) in [(builder, ConnId(2_201)), (imp, ConnId(2_202))] {
            g.get_char_mut(ch).unwrap().desc = Some(conn);
            let mut descriptor = Descriptor::new(conn, "example.test".to_string());
            descriptor.state = crate::connection::ConState::Playing;
            descriptor.character = Some(ch);
            g.descriptors.insert(conn, descriptor);
        }

        assert!(can_edit_vnum_zone(&g, builder, 150));
        assert!(!can_edit_vnum_zone(&g, builder, 250));
        assert!(can_edit_vnum_zone(&g, imp, 250));
    }

    #[test]
    fn unused_reset_columns_write_minus_one() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "R"));
        g.zones[0].reset_commands = vec![
            ResetCmd::GiveObjToMob {
                if_flag: false,
                obj_vnum: 101,
                max_count: 1,
                load_chance: 100,
            },
            ResetCmd::RemoveObj {
                if_flag: false,
                room_vnum: 100,
                obj_vnum: 101,
            },
            ResetCmd::Door {
                if_flag: false,
                room_vnum: 100,
                direction: 0,
                state: 1,
            },
        ];
        // The in-memory fallback must reproduce the on-disk convention of -1 in
        // unused columns (C zedit.c:550/571/577-578) (#282).
        let raws = cmds_from_memory(&g, 0);
        assert_eq!(raws[0].arg4, -1, "G arg4");
        assert_eq!((raws[1].arg3, raws[1].arg4), (-1, -1), "R arg3/arg4");
        assert_eq!(raws[2].arg4, -1, "D arg4");
    }

    #[test]
    fn malformed_unterminated_header_is_rejected_before_editing() {
        let dir = std::env::temp_dir().join(format!("zedit-hdr-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("99.zon");
        std::fs::write(&path, "#99\nNameWithoutTilde\nBuilders~\n0 30 2\n$\n").unwrap();
        let error = read_disk_header(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unterminated zone name"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn editor_game(conn: ConnId) -> (GameState, CharId, RoomVnum, RoomVnum) {
        let mut config = Config::default();
        config.lib_path = std::env::temp_dir()
            .join(format!(
                "deltamud-zedit-overflow-{}-{}",
                std::process::id(),
                conn.0
            ))
            .to_string_lossy()
            .into_owned();
        let mut g = GameState::new(config);
        let zone_number = 40_402;
        let (room_vnum, _) = zone_vnum_bounds(zone_number).expect("valid test zone");
        g.zones.push(zone(zone_number, "Root"));
        g.add_room(Room::new(
            room_vnum,
            0,
            "Overflow test room".into(),
            "A room used by the zedit overflow regression.".into(),
        ));

        let mut ch = Character::new_player("Root".into(), Class::Cleric, Race::Human);
        ch.player.level = LVL_IMPL;
        ch.trust = i32::from(LVL_IMPL);
        ch.godcmds2 |= crate::gcmd::GCMD2_OLC;
        let ch = g.create_char(ch);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut descriptor = Descriptor::new(conn, "example.test".into());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(ch);
        g.descriptors.insert(conn, descriptor);
        let zon_dir = std::path::Path::new(&g.config.lib_path).join("world/zon");
        std::fs::create_dir_all(&zon_dir).unwrap();
        std::fs::write(
            zon_dir.join(format!("{zone_number}.zon")),
            format!(
                "#{zone_number}\nOverflow Zone~\nRoot~\n{} 30 2\n0 60 0\nS\n$\n",
                zone_vnum_bounds(zone_number).unwrap().1
            ),
        )
        .unwrap();
        (g, ch, room_vnum, room_vnum)
    }

    fn top_and_mode(conn: ConnId) -> (RoomVnum, Mode) {
        let map = crate::lock_ok::lock(&states());
        let state = map.get(&conn).expect("active zedit state");
        (state.hdr.top, state.mode)
    }

    #[test]
    fn post_rename_zone_failure_reconciles_from_the_candidate_and_stays_dirty() {
        let _save_guard = crate::olc::test_save_list_guard();
        let conn = ConnId(4_040_099);
        crate::olc::abort_editor(conn);
        let (mut g, ch, room_vnum, _) = editor_game(conn);
        do_zedit(&mut g, ch, &room_vnum.to_string(), 0);
        with_state(conn, |state| {
            state.hdr.name = "Published Zone Candidate".to_string();
            state.hdr.header_changed = true;
            state.mode = Mode::ConfirmSave;
        });
        let zone_number = g.zones[0].number;

        parse_confirm_save_with(&mut g, conn, "y", |path, bytes| {
            crate::olc::atomic_replace_with_hooks(
                path,
                bytes,
                |_| Ok(()),
                |_| Err(std::io::Error::other("injected directory sync failure")),
            )
        });

        assert_eq!(g.zones[0].name, "Published Zone Candidate");
        assert!(
            std::fs::read_to_string(zon_file_path(&g, zone_number))
                .unwrap()
                .contains("Published Zone Candidate~")
        );
        assert!(crate::olc::test_pending_save(
            zone_number,
            crate::olc::OLC_SAVE_ZONE
        ));
        assert!(crate::olc::test_unresolved_publication(
            EditorKind::Zedit,
            zone_number
        ));
        assert_eq!(crate::olc::active_editor(conn), Some(EditorKind::Zedit));
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("live resets were reconciled")
        );

        parse_confirm_save_with(&mut g, conn, "y", crate::olc::atomic_replace);
        assert!(!crate::olc::test_unresolved_publication(
            EditorKind::Zedit,
            zone_number
        ));
        assert!(!crate::olc::test_pending_save(
            zone_number,
            crate::olc::OLC_SAVE_ZONE
        ));
        assert!(!crate::olc::in_olc(conn));
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    #[test]
    fn zedit_save_reconciles_the_published_builder_acl_into_live_zone_state() {
        let _save_guard = crate::olc::test_save_list_guard();
        let conn = ConnId(4_040_100);
        crate::olc::abort_editor(conn);
        let (mut g, ch, room_vnum, _) = editor_game(conn);
        do_zedit(&mut g, ch, &room_vnum.to_string(), 0);
        with_state(conn, |state| {
            state.hdr.builders = "Nextbuilder".to_string();
            state.hdr.status_mode = 1;
            state.hdr.header_changed = true;
            state.mode = Mode::ConfirmSave;
        });

        parse_confirm_save_with(&mut g, conn, "y", crate::olc::atomic_replace);

        assert_eq!(g.zones[0].builders, "Nextbuilder");
        assert_eq!(g.zones[0].status_mode, 1);
        let disk = std::fs::read_to_string(zon_file_path(&g, g.zones[0].number)).unwrap();
        assert!(disk.contains("\nNextbuilder~\n"));
        assert!(!crate::olc::in_olc(conn));
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    #[test]
    fn zedit_entry_accepts_i32_edges_and_rejects_adjacent_overflow() {
        let conn = ConnId(4_040_003);
        crate::olc::abort_editor(conn);
        let (mut g, ch, room_vnum, zone_start) = editor_game(conn);

        for input in ["2147483648", "-2147483649"] {
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
            do_zedit(&mut g, ch, input, 0);
            assert_eq!(
                g.descriptors[&conn].outbuf, "That room VNUM is outside the supported range.\r\n",
                "input={input:?}"
            );
            assert!(!crate::olc::in_olc(conn));
        }

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_zedit(&mut g, ch, &room_vnum.to_string(), 0);
        assert_eq!(crate::olc::active_editor(conn), Some(EditorKind::Zedit));

        for (input, expected) in [
            ("2147483647", Some(i32::MAX)),
            ("-2147483648", Some(zone_start)),
            ("2147483648", None),
            ("-2147483649", None),
        ] {
            set_mode(conn, Mode::MainMenu);
            zedit_parse(&mut g, conn, "t");
            assert!(top_and_mode(conn).1 == Mode::ZoneTop);
            let before = top_and_mode(conn).0;
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();

            zedit_parse(&mut g, conn, input);
            let (top, mode) = top_and_mode(conn);
            match expected {
                Some(value) => {
                    assert_eq!(top, value, "input={input:?}");
                    assert!(mode == Mode::MainMenu, "input={input:?}");
                    assert!(
                        !g.descriptors[&conn]
                            .outbuf
                            .contains("outside the supported range"),
                        "input={input:?}"
                    );
                }
                None => {
                    assert_eq!(top, before, "input={input:?}");
                    assert!(mode == Mode::ZoneTop, "input={input:?}");
                    assert_eq!(
                        g.descriptors[&conn].outbuf,
                        "That number is outside the supported range.\r\n",
                        "input={input:?}"
                    );
                }
            }
        }

        set_mode(conn, Mode::MainMenu);
        zedit_parse(&mut g, conn, "q");
        zedit_parse(&mut g, conn, "n");
        assert!(!crate::olc::in_olc(conn));
        assert_eq!(
            g.get_char(ch).unwrap().act_flags & crate::flags::PLR_WRITING,
            0
        );
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    fn new_zone_game(label: &str, conn: ConnId) -> (GameState, CharId, std::path::PathBuf) {
        let lib = std::env::temp_dir().join(format!(
            "deltamud-new-zone-{label}-{}-{}",
            std::process::id(),
            conn.0
        ));
        let _ = std::fs::remove_dir_all(&lib);
        for extension in ["zon", "wld", "mob", "obj", "shp", "trg"] {
            let directory = lib.join("world").join(extension);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("index"), "$\n").unwrap();
        }

        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let mut implementor = Character::new_player("Root".into(), Class::Cleric, Race::Human);
        implementor.player.level = 1;
        implementor.trust = i32::from(LVL_IMPL);
        implementor.godcmds2 |= crate::gcmd::GCMD2_OLC;
        let implementor = g.create_char(implementor);
        g.get_char_mut(implementor).unwrap().desc = Some(conn);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(implementor);
        g.descriptors.insert(conn, descriptor);
        (g, implementor, lib)
    }

    #[test]
    fn unpublished_new_zone_failure_blocks_shutdown_until_explicit_discard() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_410;
        let (mut g, implementor, lib) = new_zone_game("unpublished-discard", ConnId(4_041_001));
        let key = new_zone_unresolved_key(zone_number);

        zedit_new_zone_with(
            &mut g,
            implementor,
            zone_number,
            |_path, _bytes| Err(std::io::Error::other("injected first-file failure")),
            |_path, _expected, _replacement| {
                unreachable!("indexes must not publish after a file failure")
            },
        );

        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());

        crate::olc::do_olc(
            &mut g,
            implementor,
            &format!("discard {zone_number}"),
            crate::olc::SCMD_OLC_ZEDIT,
        );
        assert!(!crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(
            g.descriptors[&ConnId(4_041_001)]
                .outbuf
                .contains("explicitly discarded")
        );

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn reboot_new_zone_marker_restores_exit_blocker_and_requires_exact_retry() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_420;
        let conn = ConnId(4_042_001);
        let (mut g, implementor, lib) = new_zone_game("reboot-blocker", conn);
        let key = new_zone_unresolved_key(zone_number);

        crate::olc::begin_new_zone_publication(lib.to_str().unwrap(), zone_number).unwrap();
        let pending = crate::olc::pending_new_zone_publications(lib.to_str().unwrap()).unwrap();
        crate::olc::register_pending_new_zone_publication_blockers(&pending);

        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());
        crate::olc::olc_saveinfo(&mut g, implementor);
        assert!(g.descriptors[&conn].outbuf.contains(&format!(
            "new-zone:{zone_number} has an unconfirmed published save"
        )));

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        zedit_discard_new_zone_failure(&mut g, implementor, zone_number);
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("partially published and cannot be discarded")
        );
        assert!(
            crate::olc::pending_new_zone_publications(lib.to_str().unwrap())
                .unwrap()
                .contains(&zone_number)
        );

        zedit_new_zone(&mut g, implementor, zone_number);
        assert!(!crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(
            crate::olc::pending_new_zone_publications(lib.to_str().unwrap())
                .unwrap()
                .is_empty()
        );
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_ok());

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn existing_new_zone_marker_with_uncertain_index_state_cannot_be_discarded() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_421;
        let conn = ConnId(4_042_002);
        let (mut g, implementor, lib) = new_zone_game("ambiguous-index", conn);
        let key = new_zone_unresolved_key(zone_number);

        crate::olc::begin_new_zone_publication(lib.to_str().unwrap(), zone_number).unwrap();
        let zon_index = lib.join("world/zon/index");
        std::fs::remove_file(&zon_index).unwrap();
        std::fs::create_dir(&zon_index).unwrap();

        zedit_new_zone(&mut g, implementor, zone_number);
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        zedit_discard_new_zone_failure(&mut g, implementor, zone_number);
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("partially published and cannot be discarded")
        );
        assert!(
            crate::olc::pending_new_zone_publications(lib.to_str().unwrap())
                .unwrap()
                .contains(&zone_number)
        );

        crate::olc::clear_unresolved_named_save(EditorKind::Zedit, &key);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn uncertain_new_zone_preflight_before_a_marker_remains_explicitly_discardable() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_422;
        let conn = ConnId(4_042_003);
        let (mut g, implementor, lib) = new_zone_game("unpublished-ambiguous-index", conn);
        let key = new_zone_unresolved_key(zone_number);
        let zon_index = lib.join("world/zon/index");
        std::fs::remove_file(&zon_index).unwrap();
        std::fs::create_dir(&zon_index).unwrap();

        zedit_new_zone(&mut g, implementor, zone_number);
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        zedit_discard_new_zone_failure(&mut g, implementor, zone_number);
        assert!(!crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(g.descriptors[&conn].outbuf.contains("explicitly discarded"));
        assert!(
            crate::olc::pending_new_zone_publications(lib.to_str().unwrap())
                .unwrap()
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn partial_new_zone_publication_cannot_be_discarded_and_retry_is_idempotent() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_411;
        let conn = ConnId(4_041_002);
        let (mut g, implementor, lib) = new_zone_game("partial-retry", conn);
        let key = new_zone_unresolved_key(zone_number);
        let mut file_calls = 0usize;

        zedit_new_zone_with(
            &mut g,
            implementor,
            zone_number,
            |path, bytes| {
                file_calls += 1;
                if file_calls == 2 {
                    Err(std::io::Error::other("injected second-file failure"))
                } else {
                    create_or_verify_new_zone_file(path, bytes)
                }
            },
            publish_new_zone_index,
        );

        assert!(lib.join(format!("world/zon/{zone_number}.zon")).exists());
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());

        zedit_discard_new_zone_failure(&mut g, implementor, zone_number);
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("partially published and cannot be discarded")
        );

        zedit_new_zone(&mut g, implementor, zone_number);
        assert!(!crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(g.zones.iter().any(|zone| zone.number == zone_number));
        for extension in ["zon", "wld", "mob", "obj", "shp", "trg"] {
            let index =
                std::fs::read_to_string(lib.join("world").join(extension).join("index")).unwrap();
            assert_eq!(
                index
                    .lines()
                    .filter(|line| line.trim() == format!("{zone_number}.{extension}"))
                    .count(),
                1,
                "retry must not duplicate the {extension} index row"
            );
        }

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn index_publication_failure_retains_partial_new_zone_blocker() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_412;
        let conn = ConnId(4_041_003);
        let (mut g, implementor, lib) = new_zone_game("index-failure", conn);
        let key = new_zone_unresolved_key(zone_number);

        zedit_new_zone_with(
            &mut g,
            implementor,
            zone_number,
            create_or_verify_new_zone_file,
            |_path, _expected, _replacement| Err(std::io::Error::other("injected index failure")),
        );

        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());
        for extension in ["zon", "wld", "mob", "obj", "shp", "trg"] {
            assert!(
                lib.join(format!("world/{extension}/{zone_number}.{extension}"))
                    .exists()
            );
        }

        zedit_new_zone(&mut g, implementor, zone_number);
        assert!(!crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn index_compare_and_replace_preserves_an_intervening_writer() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_413;
        let (mut g, implementor, lib) = new_zone_game("index-cas", ConnId(4_041_004));
        let key = new_zone_unresolved_key(zone_number);
        let mut injected = false;

        zedit_new_zone_with(
            &mut g,
            implementor,
            zone_number,
            create_or_verify_new_zone_file,
            |path, expected, replacement| {
                if !injected {
                    injected = true;
                    std::fs::write(path, "999.zon\n$\n").unwrap();
                }
                publish_new_zone_index(path, expected, replacement)
            },
        );

        assert!(injected);
        assert_eq!(
            std::fs::read_to_string(lib.join("world/zon/index")).unwrap(),
            "999.zon\n$\n",
            "a stale new-zone snapshot must not erase another index writer"
        );
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(g.zones.is_empty());

        crate::olc::clear_unresolved_named_save(EditorKind::Zedit, &key);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn idempotent_index_confirmation_rejects_an_intervening_row_removal() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_414;
        let (mut g, implementor, lib) = new_zone_game("index-confirm-cas", ConnId(4_041_005));
        let key = new_zone_unresolved_key(zone_number);
        let wld_index = lib.join("world/wld/index");
        std::fs::write(&wld_index, format!("{zone_number}.wld\n$\n")).unwrap();
        let mut injected = false;

        zedit_new_zone_with(
            &mut g,
            implementor,
            zone_number,
            create_or_verify_new_zone_file,
            |path, expected, replacement| {
                if path == wld_index && replacement.is_none() {
                    injected = true;
                    std::fs::write(path, "$\n").unwrap();
                }
                publish_new_zone_index(path, expected, replacement)
            },
        );

        assert!(injected);
        assert_eq!(std::fs::read_to_string(&wld_index).unwrap(), "$\n");
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(g.zones.is_empty());

        crate::olc::clear_unresolved_named_save(EditorKind::Zedit, &key);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[tokio::test]
    async fn durable_gate_hides_partial_zone_until_idempotent_crash_recovery() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 4_041;
        let conn = ConnId(4_041_006);
        let (mut g, implementor, lib) = new_zone_game("zon-commit-retry", conn);
        let key = new_zone_unresolved_key(zone_number);
        let mut order = Vec::new();

        zedit_new_zone_with(
            &mut g,
            implementor,
            zone_number,
            create_or_verify_new_zone_file,
            |path, expected, replacement| {
                let extension = path
                    .parent()
                    .and_then(std::path::Path::file_name)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                order.push(extension.clone());
                publish_new_zone_index(path, expected, replacement)?;
                if extension == "zon" {
                    return Err(crate::olc::published_but_incomplete(
                        "injected interruption after the boot-visible commit marker",
                        std::io::Error::other("injected crash boundary"),
                    ));
                }
                Ok(())
            },
        );

        assert_eq!(order, ["zon"]);
        assert!(g.zones.is_empty());
        assert!(crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(
            crate::olc::pending_new_zone_publications(lib.to_str().unwrap())
                .unwrap()
                .contains(&zone_number)
        );

        // Process-local blockers do not survive a hard crash, but the durable
        // marker does. A fresh boot must load neither the zone nor its room
        // while any of the six independent index publications is incomplete.
        crate::olc::clear_unresolved_named_save(EditorKind::Zedit, &key);
        let mut rebooted = GameState::new(g.config.clone());
        crate::file_loader::FileLoader::load_world(&mut rebooted, lib.to_str().unwrap())
            .await
            .unwrap();
        assert!(rebooted.zones.iter().all(|zone| zone.number != zone_number));
        assert!(
            rebooted
                .rooms
                .iter()
                .all(|room| room.number != zone_number * 100)
        );

        // The exact retry confirms every already-published byte, completes the
        // remaining index rows, then removes the durable gate before publishing
        // the live zone.
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        zedit_new_zone(&mut g, implementor, zone_number);

        assert_eq!(
            g.zones
                .iter()
                .filter(|zone| zone.number == zone_number)
                .count(),
            1
        );
        assert!(!crate::olc::test_unresolved_named_save(
            EditorKind::Zedit,
            &key
        ));
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("Zone created successfully")
        );
        assert!(
            crate::olc::pending_new_zone_publications(lib.to_str().unwrap())
                .unwrap()
                .is_empty()
        );

        let mut completed_boot = GameState::new(g.config.clone());
        crate::file_loader::FileLoader::load_world(&mut completed_boot, lib.to_str().unwrap())
            .await
            .unwrap();
        assert!(
            completed_boot
                .zones
                .iter()
                .any(|zone| zone.number == zone_number)
        );
        assert!(
            completed_boot
                .rooms
                .iter()
                .any(|room| room.number == zone_number * 100)
        );

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn customized_loaded_zone_is_not_misclassified_as_crash_recovery() {
        let _save_guard = crate::olc::test_save_list_guard();
        let zone_number = 40_416;
        let conn = ConnId(4_041_007);
        let (mut g, implementor, lib) = new_zone_game("custom-live-zone", conn);

        zedit_new_zone(&mut g, implementor, zone_number);
        assert_eq!(g.zones.len(), 1);
        g.zones[0].name = "Customized Zone".to_string();
        g.zones[0].max_level = 60;
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();

        zedit_new_zone(&mut g, implementor, zone_number);

        assert_eq!(g.zones.len(), 1);
        assert_eq!(g.zones[0].name, "Customized Zone");
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("A zone already covers that area")
        );

        let _ = std::fs::remove_dir_all(lib);
    }
}

/// C zedit.c:153-330 zedit_new_zone + zedit_create_index: create the six
/// stub files, append the new zone to every world index file, and insert the
/// zone into the live zone table ('olc zedit new <zone>'; issue #263).
const LVL_BUILDER_LEVEL: u8 = 100;

pub fn zedit_new_zone(g: &mut GameState, ch: CharId, vzone_num: i32) {
    zedit_new_zone_with(
        g,
        ch,
        vzone_num,
        create_or_verify_new_zone_file,
        publish_new_zone_index,
    );
}

fn zedit_new_zone_with<C, R>(
    g: &mut GameState,
    ch: CharId,
    vzone_num: i32,
    mut create_file: C,
    mut replace_index: R,
) where
    C: FnMut(&std::path::Path, &[u8]) -> std::io::Result<()>,
    R: FnMut(&std::path::Path, &[u8], Option<&[u8]>) -> std::io::Result<()>,
{
    if !olc::has_implementor_olc_authority(g, ch) {
        g.send_to_char(ch, "Only Implementors can create new zones.\r\n");
        return;
    }
    if vzone_num < 0 {
        g.send_to_char(ch, "You can't make negative zones.\r\n");
        return;
    }
    let Some((room, default_top)) = zone_vnum_bounds(vzone_num) else {
        debug_assert!(vzone_num > MAX_ZONE_NUMBER);
        g.send_to_char(ch, "That is higher then highest zone allowed.\r\n");
        return;
    };
    let lib = g.config.lib_path.clone().trim_end_matches('/').to_string();
    let files = vec![
        (
            "zon",
            format!("world/zon/{vzone_num}.zon"),
            format!("#{vzone_num}\nNew Zone~\n~\n{default_top} 30 2\n0 0 0\nS\n$\n"),
        ),
        (
            "wld",
            format!("world/wld/{vzone_num}.wld"),
            format!("#{room}\nThe Beginning~\nNot much here.\n~\n{vzone_num} 0 0\nS\n$\n"),
        ),
        (
            "mob",
            format!("world/mob/{vzone_num}.mob"),
            "$\n".to_string(),
        ),
        (
            "obj",
            format!("world/obj/{vzone_num}.obj"),
            "$\n".to_string(),
        ),
        (
            "shp",
            format!("world/shp/{vzone_num}.shp"),
            "CircleMUD v3.0 Shop File~\n$~\n".to_string(),
        ),
        (
            "trg",
            format!("world/trg/{vzone_num}.trg"),
            "$~\n".to_string(),
        ),
    ];

    // A durable transaction marker now hides partial state from boot. Retain
    // exact-stub recovery as backward compatibility for a crash produced by an
    // older binary which used the zon index itself as its commit marker.
    if g.zones
        .iter()
        .any(|zone| zone.number != vzone_num && zone.contains_vnum(room))
    {
        g.send_to_char(ch, "A zone already covers that area.\r\n");
        return;
    }
    let loaded_zone_matches = g
        .zones
        .iter()
        .filter(|zone| zone.number == vzone_num)
        .count();
    if loaded_zone_matches > 1 {
        g.send_to_char(ch, "A zone already covers that area.\r\n");
        return;
    }
    let recovering_loaded_stub = if loaded_zone_matches == 1 {
        let loaded_zone = g
            .zones
            .iter()
            .find(|zone| zone.number == vzone_num)
            .expect("the preceding count found one loaded zone");
        let (_, zon_rel, expected_zon) = &files[0];
        match std::fs::read(std::path::Path::new(&lib).join(zon_rel)) {
            Ok(existing)
                if existing == expected_zon.as_bytes()
                    && loaded_zone_is_exact_new_stub(loaded_zone, vzone_num, default_top) =>
            {
                true
            }
            _ => {
                g.send_to_char(ch, "A zone already covers that area.\r\n");
                return;
            }
        }
    } else {
        false
    };

    let unresolved_key = new_zone_unresolved_key(vzone_num);

    // A marker which predates this invocation may have survived a hard crash;
    // its former process-local phase flag did not. Treat it as potentially
    // published so an uncertain retry can never make explicit discard remove
    // the only boot gate protecting partial state.
    let marker_was_already_present = match crate::olc::pending_new_zone_publications(&lib) {
        Ok(pending) => pending.contains(&vzone_num),
        Err(error) => {
            let error = crate::olc::published_but_incomplete(
                "new-zone transaction state could not be determined",
                error,
            );
            crate::olc::mark_unresolved_named_save_failure(
                EditorKind::Zedit,
                &unresolved_key,
                &error,
            );
            crate::syslog::mudlog(
                g,
                &format!("SYSERR: OLC: new-zone transaction state is unreadable: {error}"),
                crate::syslog::BRF,
                LVL_IMPL,
            );
            g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
            return;
        }
    };

    // A prior interrupted attempt may already have published an exact stub or
    // an index row. Remember that evidence before validating the full set so a
    // later preflight error cannot be explicitly discarded as unpublished.
    let mut published_any = marker_was_already_present;
    match prior_new_zone_publication_is_visible(&lib, vzone_num, &files) {
        Ok(visible) => published_any |= visible,
        Err(error) => {
            let error = new_zone_publication_error(
                marker_was_already_present,
                "new-zone publication state could not be determined",
                error,
            );
            crate::olc::mark_unresolved_named_save_failure(
                EditorKind::Zedit,
                &unresolved_key,
                &error,
            );
            crate::syslog::mudlog(
                g,
                &format!("SYSERR: OLC: new-zone publication state is unreadable: {error}"),
                crate::syslog::BRF,
                LVL_IMPL,
            );
            g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
            return;
        }
    }

    // Read and validate every pre-existing index before publishing anything.
    // A retry after a partial prior attempt is safe: exact stub files and
    // already-present index rows are accepted, never overwritten or appended
    // twice. Any conflicting orphan fails closed for operator inspection.
    let mut index_updates = Vec::with_capacity(files.len());
    let preflight = (|| -> std::io::Result<()> {
        for (ext, rel, body) in &files {
            let file_path = std::path::Path::new(&lib).join(rel);
            match std::fs::read(&file_path) {
                Ok(existing) if existing != body.as_bytes() => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "new-zone target {} already exists with different content",
                            file_path.display()
                        ),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }

            let index_path = std::path::Path::new(&lib)
                .join(format!("world/{ext}"))
                .join("index");
            let content = std::fs::read_to_string(&index_path)?;
            let entry = format!("{vzone_num}.{ext}");
            let updated = validated_index_update(&index_path, &content, ext, &entry)?;
            index_updates.push((*ext, index_path, content, updated));
        }
        Ok(())
    })();

    if let Err(error) = preflight {
        let error = new_zone_publication_error(
            published_any,
            "new-zone preflight failed after an earlier component was published",
            error,
        );
        crate::olc::mark_unresolved_named_save_failure(EditorKind::Zedit, &unresolved_key, &error);
        crate::syslog::mudlog(
            g,
            &format!("SYSERR: OLC: new-zone preflight failed: {error}"),
            crate::syslog::BRF,
            LVL_IMPL,
        );
        g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
        return;
    }

    // Persist a boot-visible transaction gate before publishing the first
    // component. Every world loader ignores this zone while the marker is
    // present, so a hard crash between the six independent legacy index
    // updates cannot expose either an orphan component or an incomplete zone.
    if let Err(error) = crate::olc::begin_new_zone_publication(&lib, vzone_num) {
        let error = new_zone_publication_error(
            published_any,
            "new-zone transaction marker could not be made durable",
            error,
        );
        crate::olc::mark_unresolved_named_save_failure(EditorKind::Zedit, &unresolved_key, &error);
        crate::syslog::mudlog(
            g,
            &format!("SYSERR: OLC: new-zone transaction could not begin: {error}"),
            crate::syslog::BRF,
            LVL_IMPL,
        );
        g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
        return;
    }

    for (_, rel, body) in &files {
        let path = std::path::Path::new(&lib).join(rel);
        if let Err(error) = create_file(&path, body.as_bytes()) {
            let error = new_zone_publication_error(
                published_any || crate::olc::replacement_was_published(&error),
                format!(
                    "new-zone publication is incomplete after failure at {}",
                    path.display()
                ),
                error,
            );
            crate::olc::mark_unresolved_named_save_failure(
                EditorKind::Zedit,
                &unresolved_key,
                &error,
            );
            crate::syslog::mudlog(
                g,
                &format!(
                    "SYSERR: OLC: Can't publish new-zone file {}: {}",
                    path.display(),
                    error
                ),
                crate::syslog::BRF,
                LVL_IMPL,
            );
            g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
            return;
        }
        published_any = true;
    }

    // Publish zon first for compatibility with older loaders. Current loaders
    // additionally honor the durable marker and therefore expose none of the
    // six rows until this whole loop and marker removal have completed.
    index_updates.sort_by_key(|(extension, _, _, _)| *extension != "zon");
    for (_, index_path, expected, updated) in index_updates {
        // The publisher compares the exact preflight bytes under the shared
        // OLC publication lock. An intervening writer is therefore preserved
        // and turns this attempt into an unresolved partial-publication error.
        let result = replace_index(
            &index_path,
            expected.as_bytes(),
            updated.as_deref().map(str::as_bytes),
        );
        if let Err(error) = result {
            let error = new_zone_publication_error(
                published_any || crate::olc::replacement_was_published(&error),
                format!(
                    "new-zone publication is incomplete after failure at {}",
                    index_path.display()
                ),
                error,
            );
            crate::olc::mark_unresolved_named_save_failure(
                EditorKind::Zedit,
                &unresolved_key,
                &error,
            );
            crate::syslog::mudlog(
                g,
                &format!(
                    "SYSERR: OLC: Can't update {} for new zone {}: {}",
                    index_path.display(),
                    vzone_num,
                    error
                ),
                crate::syslog::BRF,
                LVL_IMPL,
            );
            g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
            return;
        }
        published_any = true;
    }

    if let Err(error) = crate::olc::complete_new_zone_publication(&lib, vzone_num) {
        let error = new_zone_publication_error(
            true,
            "new-zone components are complete but the durable boot gate could not be cleared",
            error,
        );
        crate::olc::mark_unresolved_named_save_failure(EditorKind::Zedit, &unresolved_key, &error);
        crate::syslog::mudlog(
            g,
            &format!("SYSERR: OLC: new-zone transaction could not complete: {error}"),
            crate::syslog::BRF,
            LVL_IMPL,
        );
        g.send_to_char(ch, &new_zone_failure_message(vzone_num, &error));
        return;
    }

    // Insert the live zone in number order.
    if !recovering_loaded_stub {
        let zone = crate::world::Zone {
            number: vzone_num,
            name: "New Zone".into(),
            builders: String::new(),
            lifespan: 30,
            age: 0,
            top: default_top,
            reset_mode: 2,
            min_level: 0,
            max_level: 0,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        };
        let pos = g
            .zones
            .iter()
            .position(|z| z.number > vzone_num)
            .unwrap_or(g.zones.len());
        g.zones.insert(pos, zone);
    }
    crate::olc::clear_unresolved_named_save(EditorKind::Zedit, &unresolved_key);

    crate::syslog::mudlog(
        g,
        &format!("OLC: {} creates new zone #{}", get_name(g, ch), vzone_num),
        crate::syslog::BRF,
        LVL_BUILDER_LEVEL.max(g.get_char(ch).map(|c| c.invis_level as u8).unwrap_or(0)),
    );
    g.send_to_char(ch, "Zone created successfully.\r\n");
}

fn loaded_zone_is_exact_new_stub(zone: &crate::world::Zone, number: i32, default_top: i32) -> bool {
    zone.number == number
        && zone.name == "New Zone"
        && zone.builders.is_empty()
        && zone.lifespan == 30
        && zone.top == default_top
        && zone.reset_mode == 2
        && zone.min_level == 0
        && zone.max_level == 0
        && zone.status_mode == 0
        && zone.map_x.is_none()
        && zone.map_y.is_none()
        && zone.reset_commands.is_empty()
}

fn prior_new_zone_publication_is_visible(
    lib: &str,
    vzone_num: i32,
    files: &[(&str, String, String)],
) -> std::io::Result<bool> {
    for (_, rel, body) in files {
        let path = std::path::Path::new(lib).join(rel);
        match std::fs::read(&path) {
            Ok(existing) if existing == body.as_bytes() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "cannot inspect new-zone component {}: {error}",
                        path.display()
                    ),
                ));
            }
        }
    }

    for (extension, _, _) in files {
        let index_path = std::path::Path::new(lib)
            .join(format!("world/{extension}"))
            .join("index");
        let content = std::fs::read_to_string(&index_path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "cannot inspect new-zone index {}: {error}",
                    index_path.display()
                ),
            )
        })?;
        let entry = format!("{vzone_num}.{extension}");
        if content.lines().any(|line| line.trim() == entry) {
            return Ok(true);
        }
    }

    Ok(false)
}

fn new_zone_publication_error(
    published_any: bool,
    context: impl Into<String>,
    error: std::io::Error,
) -> std::io::Error {
    if published_any && !crate::olc::replacement_was_published(&error) {
        crate::olc::published_but_incomplete(context, error)
    } else {
        error
    }
}

fn new_zone_failure_message(vzone_num: i32, error: &std::io::Error) -> String {
    if crate::olc::replacement_was_published(error) {
        format!(
            "Zone creation stopped after partial publication. Retry with 'zedit new {vzone_num}'; published state cannot be discarded, and shutdown/copyover remain blocked until retry succeeds.\r\n"
        )
    } else {
        format!(
            "Zone creation failed before publication. Retry with 'zedit new {vzone_num}' or explicitly discard the unpublished blocker with 'zedit discard {vzone_num}'; shutdown/copyover remain blocked until then.\r\n"
        )
    }
}

pub fn zedit_discard_new_zone_failure(g: &mut GameState, ch: CharId, vzone_num: i32) {
    if !olc::has_implementor_olc_authority(g, ch) {
        g.send_to_char(ch, "Only Implementors can discard new-zone failures.\r\n");
        return;
    }
    if zone_vnum_bounds(vzone_num).is_none() {
        g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
        return;
    }
    let key = new_zone_unresolved_key(vzone_num);
    match crate::olc::discard_unresolved_named_save(EditorKind::Zedit, &key) {
        crate::olc::UnresolvedDiscardOutcome::Missing => {
            g.send_to_char(
                ch,
                "There is no unresolved new-zone failure to discard.\r\n",
            );
        }
        crate::olc::UnresolvedDiscardOutcome::Discarded => {
            let lib = g.config.lib_path.clone();
            match crate::olc::complete_new_zone_publication(&lib, vzone_num) {
                Ok(()) => g.send_to_char(
                    ch,
                    "The unpublished new-zone failure and its durable boot gate were explicitly discarded; no world files were removed.\r\n",
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => g.send_to_char(
                    ch,
                    "The unpublished new-zone failure was explicitly discarded; no world files were removed.\r\n",
                ),
                Err(error) => {
                    crate::olc::mark_unresolved_named_save_failure(
                        EditorKind::Zedit,
                        &key,
                        &error,
                    );
                    g.send_to_char(
                        ch,
                        &format!(
                            "The durable new-zone gate could not be discarded: {error}. Shutdown/copyover remain blocked.\r\n"
                        ),
                    );
                }
            }
        }
        crate::olc::UnresolvedDiscardOutcome::Published => {
            g.send_to_char(
                ch,
                &format!(
                    "Zone {vzone_num} was partially published and cannot be discarded. Retry with 'zedit new {vzone_num}'; shutdown/copyover remain blocked.\r\n"
                ),
            );
        }
    }
}

fn validated_index_update(
    path: &std::path::Path,
    content: &str,
    extension: &str,
    entry: &str,
) -> std::io::Result<Option<String>> {
    let mut seen = std::collections::HashSet::new();
    let mut terminator_offset = None;
    let mut offset = 0usize;
    for segment in content.split_inclusive('\n') {
        let line = segment.trim();
        if line == "$" {
            terminator_offset = Some(offset);
            offset += segment.len();
            break;
        }
        if !line.is_empty() && !line.starts_with('*') {
            let expected_suffix = format!(".{extension}");
            let stem = line.strip_suffix(&expected_suffix).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid index entry {line:?} in {}", path.display()),
                )
            })?;
            crate::text::parse_i32_strict(stem).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "invalid index entry {line:?} in {}: {error:?}",
                        path.display()
                    ),
                )
            })?;
            if !seen.insert(line.to_string()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("duplicate index entry {line:?} in {}", path.display()),
                ));
            }
        }
        offset += segment.len();
    }
    let terminator_offset = terminator_offset.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("index {} has no $ terminator", path.display()),
        )
    })?;
    if !content[offset..].trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("index {} has content after its terminator", path.display()),
        ));
    }
    if seen.contains(entry) {
        return Ok(None);
    }
    let mut updated = String::with_capacity(content.len() + entry.len() + 1);
    updated.push_str(&content[..terminator_offset]);
    updated.push_str(entry);
    updated.push('\n');
    updated.push_str(&content[terminator_offset..]);
    Ok(Some(updated))
}

fn create_or_verify_new_zone_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::read(path) {
        Ok(existing) if existing == bytes => {
            return crate::olc::confirm_publication_unchanged(path, bytes);
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists with conflicting content", path.display()),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    match crate::olc::atomic_create(path, bytes) {
        Ok(()) => Ok(()),
        // Another writer may have created the final path after our read. Exact
        // content is an idempotent retry; conflicting content is never replaced.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match std::fs::read(path) {
                Ok(existing) if existing == bytes => {
                    crate::olc::confirm_publication_unchanged(path, bytes)
                }
                Ok(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists with conflicting content", path.display()),
                )),
                Err(read_error) => Err(read_error),
            }
        }
        Err(error) => Err(error),
    }
}

fn publish_new_zone_index(
    path: &std::path::Path,
    expected: &[u8],
    replacement: Option<&[u8]>,
) -> std::io::Result<()> {
    match replacement {
        Some(replacement) => crate::olc::atomic_replace_if_unchanged(path, expected, replacement),
        // A prior attempt may have exposed this exact row but failed its
        // directory sync. An idempotent retry must revalidate and confirm
        // durability, rather than treating visibility alone as success.
        None => crate::olc::confirm_publication_unchanged(path, expected),
    }
}

fn get_name(g: &GameState, ch: CharId) -> String {
    g.get_char(ch)
        .map(|c| c.get_name().to_string())
        .unwrap_or_else(|| "someone".into())
}
