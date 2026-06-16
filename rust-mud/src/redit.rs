// redit.rs — the OasisOLC room editor (CircleMUD redit.c), ported to the
// single-owner GameState. Full menu-driven editor: name, multi-line
// description, room flags, sector type, the six standard exits (each with its
// own sub-menu — destination, description, keyword, key, door flags, purge),
// and the extra-descriptions menu. Save writes the room back into
// GameState.rooms and rewrites the zone's .wld file byte-faithfully (the
// inverse of file_loader::load_room_file).
//
// All per-connection edit state lives in a module-static keyed by ConnId
// (Descriptor / GameState carry no OLC field). `do_redit` snapshots the target
// room into the edit state, registers with olc::set_active, and shows the main
// menu; `redit_parse` is the per-line handler the olc router calls.

use crate::constants::{ROOM_BITS, SECTOR_TYPES};
use crate::olc::{self, EditorKind, CYN, GRN, NRM, YEL};
use crate::room::{Exit, Room, RoomFlags, SectorType, EX_ISDOOR, EX_PICKPROOF};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

// EX_HIDDEN — DeltaMUD adds a hidden flag past the standard CircleMUD set.
// (structs.h: EX_PICKPROOF = 1<<3, EX_HIDDEN = 1<<4.)
const EX_HIDDEN: i32 = 1 << 4;

// NUM_ROOM_FLAGS / NUM_ROOM_SECTORS — count of real entries (excluding the
// trailing "\n" sentinel) in the constants tables.
fn num_room_flags() -> usize {
    ROOM_BITS.iter().take_while(|&&s| s != "\n").count()
}
fn num_room_sectors() -> usize {
    SECTOR_TYPES.iter().take_while(|&&s| s != "\n").count()
}

// ---------------------------------------------------------------------------
// Per-connection edit mode (OLC_MODE for redit). Mirrors the REDIT_* enum.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReditMode {
    MainMenu,
    ConfirmSave,
    Name,
    Desc,           // multi-line description sub-editor
    Flags,
    Sector,
    ExitMenu,
    ExitNumber,
    ExitDescription, // multi-line exit-desc sub-editor
    ExitKeyword,
    ExitKey,
    ExitDoorflags,
    ExtradescMenu,
    ExtradescKey,
    ExtradescDescription, // multi-line extra-desc sub-editor
}

/// A working copy of just the editable fields of a Room.
#[derive(Clone)]
struct RoomEdit {
    number: RoomVnum,
    zone: i32,
    name: String,
    description: String,
    sector_type: SectorType,
    room_flags: RoomFlags,
    exits: [Option<Exit>; NUM_OF_DIRS],
    extra_descriptions: Vec<(String, String)>,
}

struct ReditState {
    vnum: RoomVnum,
    znum: usize,    // loaded-zone index
    room: RoomEdit,
    mode: ReditMode,
    val: i32,        // current direction being edited / "modified" flag
    modified: bool,  // OLC_VAL: something changed
    cur_exit: usize, // which direction the exit sub-menu is editing
    cur_desc: usize, // index into extra_descriptions being edited
}

fn states() -> &'static Mutex<HashMap<ConnId, ReditState>> {
    static S: OnceLock<Mutex<HashMap<ConnId, ReditState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

// Helper: run a closure with mutable access to a conn's edit state.
fn with_state<R>(conn: ConnId, f: impl FnOnce(&mut ReditState) -> R) -> Option<R> {
    states().lock().unwrap().get_mut(&conn).map(f)
}

fn send(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.outbuf.push_str(msg);
    }
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

// ===========================================================================
// do_redit — start the room editor on `vnum` (or create a new room).
// ===========================================================================
pub fn do_redit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let vnum: RoomVnum = arg.trim().parse().unwrap_or(NOWHERE);
    if vnum < 0 {
        g.send_to_char(ch, "That's not a valid room number.\r\n");
        return;
    }
    let znum = match olc::real_zone(g, vnum) {
        Some(z) => z,
        None => {
            g.send_to_char(ch, "Sorry, there is no zone for that number!\r\n");
            return;
        }
    };

    // Already-being-edited check.
    let conflict = states().lock().unwrap().values().any(|s| s.vnum == vnum);
    if conflict {
        g.send_to_char(ch, "That room is currently being edited.\r\n");
        return;
    }

    let room = if let Some(rnum) = g.real_room(vnum) {
        snapshot_room(g.room(rnum))
    } else {
        // New room (redit_setup_new).
        RoomEdit {
            number: vnum,
            zone: znum as i32,
            name: "An unfinished room".to_string(),
            description: "You are in an unfinished room.\r\n".to_string(),
            sector_type: SectorType::Inside,
            room_flags: RoomFlags::empty(),
            exits: Default::default(),
            extra_descriptions: Vec::new(),
        }
    };

    states().lock().unwrap().insert(
        conn,
        ReditState {
            vnum,
            znum,
            room,
            mode: ReditMode::MainMenu,
            val: 0,
            modified: false,
            cur_exit: 0,
            cur_desc: 0,
        },
    );
    olc::set_active(conn, EditorKind::Redit);

    // act("$n starts using OLC.", TO_ROOM)
    crate::act::act(
        g,
        "$n starts using OLC.",
        true,
        ch,
        None,
        crate::act::ActArg::None,
        crate::act::To::Room,
    );
    disp_menu(g, conn);
}

fn snapshot_room(room: &Room) -> RoomEdit {
    RoomEdit {
        number: room.number,
        zone: room.zone,
        name: room.name.clone(),
        description: room.description.clone(),
        sector_type: room.sector_type,
        room_flags: room.room_flags,
        exits: room.exits.clone(),
        extra_descriptions: room.extra_descriptions.clone(),
    }
}

// ===========================================================================
// Menu display.
// ===========================================================================
fn disp_menu(g: &mut GameState, conn: ConnId) {
    let s = match states().lock().unwrap().get(&conn).map(|s| {
        (
            s.vnum,
            g_zone_number(s),
            s.room.name.clone(),
            s.room.description.clone(),
            olc::sprintbit(s.room.room_flags.bits() as i64, ROOM_BITS),
            olc::sprinttype(s.room.sector_type as i32, SECTOR_TYPES),
            exit_to_vnum(&s.room, NORTH),
            exit_to_vnum(&s.room, EAST),
            exit_to_vnum(&s.room, SOUTH),
            exit_to_vnum(&s.room, WEST),
            exit_to_vnum(&s.room, UP),
            exit_to_vnum(&s.room, DOWN),
        )
    }) {
        Some(v) => v,
        None => return,
    };
    let (vnum, zonenum, name, desc, flags, sector, n, e, so, w, u, d) = s;

    let body = format!(
        "-- Room number : [{cyn}{vnum}{nrm}]\tRoom zone: [{cyn}{zone}{nrm}]\r\n\
         {grn}1{nrm}) Name        : {yel}{name}{nrm}\r\n\
         {grn}2{nrm}) Description :\r\n{yel}{desc}{nrm}\r\n\
         {grn}3{nrm}) Room flags  : {cyn}{flags}{nrm}\r\n\
         {grn}4{nrm}) Sector type : {cyn}{sector}{nrm}\r\n\
         {grn}5{nrm}) Exit north  : {cyn}{n}{nrm}\r\n\
         {grn}6{nrm}) Exit east   : {cyn}{e}{nrm}\r\n\
         {grn}7{nrm}) Exit south  : {cyn}{so}{nrm}\r\n\
         {grn}8{nrm}) Exit west   : {cyn}{w}{nrm}\r\n\
         {grn}9{nrm}) Exit up     : {cyn}{u}{nrm}\r\n\
         {grn}A{nrm}) Exit down   : {cyn}{d}{nrm}\r\n\
         {grn}C{nrm}) Extra descriptions menu\r\n\
         {grn}Q{nrm}) Quit\r\n\
         Enter choice : ",
        cyn = CYN, nrm = NRM, grn = GRN, yel = YEL,
        vnum = vnum, zone = zonenum, name = name, desc = desc, flags = flags,
        sector = sector, n = n, e = e, so = so, w = w, u = u, d = d,
    );
    send(g, conn, &body);
    let _ = with_state(conn, |s| s.mode = ReditMode::MainMenu);
}

fn g_zone_number(s: &ReditState) -> i32 {
    // We only stored the zone rnum; map back to the builder number lazily by
    // dividing the room vnum (matches DeltaMUD zone_number convention).
    s.vnum / 100
}

fn exit_to_vnum(room: &RoomEdit, dir: usize) -> i32 {
    match &room.exits[dir] {
        Some(e) => e.to_room,
        None => -1,
    }
}

fn disp_flag_menu(g: &mut GameState, conn: ConnId) {
    let cur = with_state(conn, |s| s.room.room_flags.bits() as i64).unwrap_or(0);
    let mut out = String::new();
    let mut columns = 0;
    for (i, name) in ROOM_BITS.iter().take(num_room_flags()).enumerate() {
        columns += 1;
        out.push_str(&format!(
            "{grn}{n:2}{nrm}) {name:<20.20} {sep}",
            grn = GRN, nrm = NRM, n = i + 1, name = name,
            sep = if columns % 2 == 0 { "\r\n" } else { "" },
        ));
    }
    out.push_str(&format!(
        "\r\nRoom flags: {cyn}{flags}{nrm}\r\nEnter room flags, 0 to quit : ",
        cyn = CYN, nrm = NRM, flags = olc::sprintbit(cur, ROOM_BITS),
    ));
    send(g, conn, &out);
    let _ = with_state(conn, |s| s.mode = ReditMode::Flags);
}

fn disp_sector_menu(g: &mut GameState, conn: ConnId) {
    let mut out = String::new();
    let mut columns = 0;
    for (i, name) in SECTOR_TYPES.iter().take(num_room_sectors()).enumerate() {
        columns += 1;
        out.push_str(&format!(
            "{grn}{n:2}{nrm}) {name:<20.20} {sep}",
            grn = GRN, nrm = NRM, n = i, name = name,
            sep = if columns % 2 == 0 { "\r\n" } else { "" },
        ));
    }
    out.push_str("\r\nEnter sector type : ");
    send(g, conn, &out);
    let _ = with_state(conn, |s| s.mode = ReditMode::Sector);
}

fn disp_exit_menu(g: &mut GameState, conn: ConnId) {
    // Ensure the exit exists.
    with_state(conn, |s| {
        let dir = s.cur_exit;
        if s.room.exits[dir].is_none() {
            s.room.exits[dir] = Some(Exit {
                description: None,
                keyword: None,
                exit_info: 0,
                key: NOTHING,
                to_room: NOWHERE,
            });
        }
    });
    let (to, desc, kw, key, doorstr) = match with_state(conn, |s| {
        let e = s.room.exits[s.cur_exit].as_ref().unwrap();
        (
            e.to_room,
            e.description.clone().unwrap_or_else(|| "<NONE>".to_string()),
            e.keyword.clone().unwrap_or_else(|| "<NONE>".to_string()),
            e.key,
            door_flag_str(e.exit_info),
        )
    }) {
        Some(v) => v,
        None => return,
    };
    let body = format!(
        "{grn}1{nrm}) Exit to     : {cyn}{to}\r\n\
         {grn}2{nrm}) Description :-\r\n{yel}{desc}{nrm}\r\n\
         {grn}3{nrm}) Door name   : {yel}{kw}{nrm}\r\n\
         {grn}4{nrm}) Key         : {cyn}{key}{nrm}\r\n\
         {grn}5{nrm}) Door flags  : {cyn}{door}{nrm}\r\n\
         {grn}6{nrm}) Purge exit.\r\n\
         Enter choice, 0 to quit : ",
        grn = GRN, nrm = NRM, cyn = CYN, yel = YEL,
        to = to, desc = desc, kw = kw, key = key, door = doorstr,
    );
    send(g, conn, &body);
    let _ = with_state(conn, |s| s.mode = ReditMode::ExitMenu);
}

fn door_flag_str(exit_info: i32) -> String {
    let mut s = if exit_info & EX_ISDOOR != 0 {
        if exit_info & EX_PICKPROOF != 0 {
            "Pickproof".to_string()
        } else {
            "Is a door".to_string()
        }
    } else {
        "No door".to_string()
    };
    if exit_info & EX_HIDDEN != 0 {
        s.push_str(" (Hidden)");
    }
    s
}

fn disp_exit_flag_menu(g: &mut GameState, conn: ConnId) {
    let body = format!(
        "{grn}0{nrm}) No door\r\n\
         {grn}1{nrm}) Closeable door\r\n\
         {grn}2{nrm}) Pickproof\r\n\
         {grn}3{nrm}) Hidden\r\n\
         Enter choice : ",
        grn = GRN, nrm = NRM,
    );
    send(g, conn, &body);
}

fn disp_extradesc_menu(g: &mut GameState, conn: ConnId) {
    let (kw, desc, has_next) = match with_state(conn, |s| {
        let idx = s.cur_desc;
        let (k, d) = s
            .room
            .extra_descriptions
            .get(idx)
            .cloned()
            .unwrap_or_default();
        let next = idx + 1 < s.room.extra_descriptions.len();
        (
            if k.is_empty() { "<NONE>".to_string() } else { k },
            if d.is_empty() { "<NONE>".to_string() } else { d },
            next,
        )
    }) {
        Some(v) => v,
        None => return,
    };
    let next_str = if has_next { "Set.\r\n" } else { "<NOT SET>\r\n" };
    let body = format!(
        "{grn}1{nrm}) Keyword: {yel}{kw}\r\n\
         {grn}2{nrm}) Description:\r\n{yel}{desc}\r\n\
         {grn}3{nrm}) Goto next description: {next}\
         Enter choice (0 to quit) : ",
        grn = GRN, nrm = NRM, yel = YEL, kw = kw, desc = desc, next = next_str,
    );
    send(g, conn, &body);
    let _ = with_state(conn, |s| s.mode = ReditMode::ExtradescMenu);
}

// ===========================================================================
// Multi-line text sub-editor (description / exit-desc / extra-desc). The OLC
// router always forwards input to redit_parse while in_olc; we collect lines in
// a per-conn buffer until "/s" (save) or "/a" (abort), or a lone "~".
// ===========================================================================
fn text_bufs() -> &'static Mutex<HashMap<ConnId, String>> {
    static B: OnceLock<Mutex<HashMap<ConnId, String>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

fn begin_text(g: &mut GameState, conn: ConnId, seed: &str, mode: ReditMode) {
    text_bufs().lock().unwrap().insert(conn, String::new());
    let _ = with_state(conn, |s| s.mode = mode);
    send(
        g,
        conn,
        "Enter text (terminate with '/s' on its own line, '/a' to abort):\r\n",
    );
    if !seed.is_empty() {
        send(g, conn, seed);
        if !seed.ends_with('\n') {
            send(g, conn, "\r\n");
        }
    }
}

/// Feed one line to the active text sub-editor. Returns Some(Some(text)) on
/// save, Some(None) on abort, None while still collecting.
fn text_input(conn: ConnId, line: &str) -> Option<Option<String>> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed == "/s" || trimmed == "~" {
        return Some(Some(text_bufs().lock().unwrap().remove(&conn).unwrap_or_default()));
    }
    if trimmed == "/a" {
        text_bufs().lock().unwrap().remove(&conn);
        return Some(None);
    }
    let mut bufs = text_bufs().lock().unwrap();
    let buf = bufs.entry(conn).or_default();
    buf.push_str(trimmed);
    buf.push_str("\r\n");
    None
}

// ===========================================================================
// redit_parse — per-line input handler (the olc router calls this).
// ===========================================================================
pub fn redit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match with_state(conn, |s| s.mode) {
        Some(m) => m,
        None => return,
    };
    let arg = line.trim();

    match mode {
        ReditMode::ConfirmSave => {
            match arg.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('y') => {
                    save_internally(g, conn);
                    send(g, conn, "Room saved to memory.\r\n");
                    finish(g, conn);
                }
                Some('n') => {
                    finish(g, conn);
                }
                _ => {
                    send(g, conn, "Invalid choice!\r\nDo you wish to save this room internally? : ");
                }
            }
        }

        ReditMode::MainMenu => parse_main_menu(g, conn, arg),

        ReditMode::Name => {
            let name = if arg.is_empty() { "undefined".to_string() } else { arg.to_string() };
            with_state(conn, |s| {
                s.room.name = name;
                s.modified = true;
            });
            disp_menu(g, conn);
        }

        ReditMode::Desc => {
            if let Some(result) = text_input(conn, line) {
                if let Some(text) = result {
                    with_state(conn, |s| {
                        s.room.description = if text.is_empty() {
                            "Empty\r\n".to_string()
                        } else {
                            text
                        };
                        s.modified = true;
                    });
                }
                disp_menu(g, conn);
            }
        }

        ReditMode::Flags => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number < 0 || number as usize > num_room_flags() {
                send(g, conn, "That is not a valid choice!\r\n");
                disp_flag_menu(g, conn);
            } else if number == 0 {
                with_state(conn, |s| s.modified = true);
                disp_menu(g, conn);
            } else {
                with_state(conn, |s| {
                    let bit = 1u32 << (number - 1);
                    s.room.room_flags ^= RoomFlags::from_bits_truncate(bit);
                    s.modified = true;
                });
                disp_flag_menu(g, conn);
            }
        }

        ReditMode::Sector => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if number < 0 || number as usize >= num_room_sectors() {
                send(g, conn, "Invalid choice!");
                disp_sector_menu(g, conn);
            } else {
                with_state(conn, |s| {
                    s.room.sector_type = SectorType::from_i32(number);
                    s.modified = true;
                });
                disp_menu(g, conn);
            }
        }

        ReditMode::ExitMenu => parse_exit_menu(g, conn, arg),

        ReditMode::ExitNumber => {
            let number: i32 = arg.parse().unwrap_or(NOWHERE);
            if number != -1 && g.real_room(number).is_none() {
                send(g, conn, "That room does not exist, try again : ");
                return;
            }
            with_state(conn, |s| {
                if let Some(e) = s.room.exits[s.cur_exit].as_mut() {
                    e.to_room = number;
                }
                s.modified = true;
            });
            disp_exit_menu(g, conn);
        }

        ReditMode::ExitDescription => {
            if let Some(result) = text_input(conn, line) {
                if let Some(text) = result {
                    with_state(conn, |s| {
                        if let Some(e) = s.room.exits[s.cur_exit].as_mut() {
                            e.description = if text.is_empty() { None } else { Some(text) };
                        }
                        s.modified = true;
                    });
                }
                disp_exit_menu(g, conn);
            }
        }

        ReditMode::ExitKeyword => {
            with_state(conn, |s| {
                if let Some(e) = s.room.exits[s.cur_exit].as_mut() {
                    e.keyword = if arg.is_empty() { None } else { Some(arg.to_string()) };
                }
                s.modified = true;
            });
            disp_exit_menu(g, conn);
        }

        ReditMode::ExitKey => {
            let key: i32 = arg.parse().unwrap_or(NOTHING);
            with_state(conn, |s| {
                if let Some(e) = s.room.exits[s.cur_exit].as_mut() {
                    e.key = key;
                }
                s.modified = true;
            });
            disp_exit_menu(g, conn);
        }

        ReditMode::ExitDoorflags => {
            let number: i32 = arg.parse().unwrap_or(-1);
            if !(0..=3).contains(&number) {
                send(g, conn, "That's not a valid choice!\r\n");
                disp_exit_flag_menu(g, conn);
            } else {
                let hidden_msg = with_state(conn, |s| {
                    if let Some(e) = s.room.exits[s.cur_exit].as_mut() {
                        let was_hidden = e.exit_info & EX_HIDDEN != 0;
                        if number == 3 {
                            if was_hidden {
                                e.exit_info &= !EX_HIDDEN;
                                return Some("Hidden flag removed from exit.\r\n");
                            } else {
                                e.exit_info |= EX_HIDDEN;
                                return Some("Exit flagged hidden.\r\n");
                            }
                        }
                        let base = match number {
                            1 => EX_ISDOOR,
                            2 => EX_ISDOOR | EX_PICKPROOF,
                            _ => 0,
                        };
                        e.exit_info = base | if was_hidden { EX_HIDDEN } else { 0 };
                    }
                    None
                })
                .flatten();
                with_state(conn, |s| s.modified = true);
                if let Some(msg) = hidden_msg {
                    send(g, conn, msg);
                    disp_exit_flag_menu(g, conn);
                } else {
                    disp_exit_menu(g, conn);
                }
            }
        }

        ReditMode::ExtradescKey => {
            with_state(conn, |s| {
                let idx = s.cur_desc;
                if let Some(ed) = s.room.extra_descriptions.get_mut(idx) {
                    ed.0 = arg.to_string();
                }
                s.modified = true;
            });
            disp_extradesc_menu(g, conn);
        }

        ReditMode::ExtradescDescription => {
            if let Some(result) = text_input(conn, line) {
                if let Some(text) = result {
                    with_state(conn, |s| {
                        let idx = s.cur_desc;
                        if let Some(ed) = s.room.extra_descriptions.get_mut(idx) {
                            ed.1 = text;
                        }
                        s.modified = true;
                    });
                }
                disp_extradesc_menu(g, conn);
            }
        }

        ReditMode::ExtradescMenu => parse_extradesc_menu(g, conn, arg),
    }
}

fn parse_main_menu(g: &mut GameState, conn: ConnId, arg: &str) {
    match arg.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('q') => {
            let modified = with_state(conn, |s| s.modified).unwrap_or(false);
            if modified {
                send(g, conn, "Do you wish to save this room internally? : ");
                let _ = with_state(conn, |s| s.mode = ReditMode::ConfirmSave);
            } else {
                finish(g, conn);
            }
        }
        Some('1') => {
            send(g, conn, "Enter room name:-\r\n] ");
            let _ = with_state(conn, |s| s.mode = ReditMode::Name);
        }
        Some('2') => {
            let seed = with_state(conn, |s| s.room.description.clone()).unwrap_or_default();
            begin_text(g, conn, &seed, ReditMode::Desc);
        }
        Some('3') => disp_flag_menu(g, conn),
        Some('4') => disp_sector_menu(g, conn),
        Some('5') => { set_exit(conn, NORTH); disp_exit_menu(g, conn); }
        Some('6') => { set_exit(conn, EAST); disp_exit_menu(g, conn); }
        Some('7') => { set_exit(conn, SOUTH); disp_exit_menu(g, conn); }
        Some('8') => { set_exit(conn, WEST); disp_exit_menu(g, conn); }
        Some('9') => { set_exit(conn, UP); disp_exit_menu(g, conn); }
        Some('a') => { set_exit(conn, DOWN); disp_exit_menu(g, conn); }
        Some('c') => {
            // Ensure at least one (empty) extra desc to edit, position on the
            // first incomplete one.
            with_state(conn, |s| {
                if s.room.extra_descriptions.is_empty() {
                    s.room.extra_descriptions.push((String::new(), String::new()));
                }
                s.cur_desc = 0;
            });
            disp_extradesc_menu(g, conn);
        }
        _ => {
            send(g, conn, "Invalid choice!");
            disp_menu(g, conn);
        }
    }
}

fn set_exit(conn: ConnId, dir: usize) {
    let _ = with_state(conn, |s| {
        s.cur_exit = dir;
        s.val = dir as i32;
    });
}

fn parse_exit_menu(g: &mut GameState, conn: ConnId, arg: &str) {
    match arg.chars().next() {
        Some('0') => {
            // Backing out: if the exit has no destination, drop the empty slot.
            with_state(conn, |s| {
                let dir = s.cur_exit;
                if let Some(e) = &s.room.exits[dir] {
                    if e.to_room == NOWHERE
                        && e.description.is_none()
                        && e.keyword.is_none()
                        && e.exit_info == 0
                    {
                        s.room.exits[dir] = None;
                    }
                }
            });
            with_state(conn, |s| s.modified = true);
            disp_menu(g, conn);
        }
        Some('1') => {
            send(g, conn, "Exit to room number : ");
            let _ = with_state(conn, |s| s.mode = ReditMode::ExitNumber);
        }
        Some('2') => {
            let seed = with_state(conn, |s| {
                s.room.exits[s.cur_exit]
                    .as_ref()
                    .and_then(|e| e.description.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
            begin_text(g, conn, &seed, ReditMode::ExitDescription);
        }
        Some('3') => {
            send(g, conn, "Enter keywords : ");
            let _ = with_state(conn, |s| s.mode = ReditMode::ExitKeyword);
        }
        Some('4') => {
            send(g, conn, "Enter key number : ");
            let _ = with_state(conn, |s| s.mode = ReditMode::ExitKey);
        }
        Some('5') => {
            disp_exit_flag_menu(g, conn);
            let _ = with_state(conn, |s| s.mode = ReditMode::ExitDoorflags);
        }
        Some('6') => {
            with_state(conn, |s| {
                s.room.exits[s.cur_exit] = None;
                s.modified = true;
            });
            disp_menu(g, conn);
        }
        _ => send(g, conn, "Try again : "),
    }
}

fn parse_extradesc_menu(g: &mut GameState, conn: ConnId, arg: &str) {
    let number: i32 = arg.parse().unwrap_or(-1);
    match number {
        0 => {
            // Drop the current extra desc if incomplete.
            with_state(conn, |s| {
                let idx = s.cur_desc;
                if let Some(ed) = s.room.extra_descriptions.get(idx) {
                    if ed.0.is_empty() || ed.1.is_empty() {
                        s.room.extra_descriptions.remove(idx);
                    }
                }
            });
            with_state(conn, |s| s.modified = true);
            disp_menu(g, conn);
        }
        1 => {
            send(g, conn, "Enter keywords, separated by spaces : ");
            let _ = with_state(conn, |s| s.mode = ReditMode::ExtradescKey);
        }
        2 => {
            let seed = with_state(conn, |s| {
                let idx = s.cur_desc;
                s.room.extra_descriptions.get(idx).map(|e| e.1.clone()).unwrap_or_default()
            })
            .unwrap_or_default();
            begin_text(g, conn, &seed, ReditMode::ExtradescDescription);
        }
        3 => {
            let (complete, at_end) = with_state(conn, |s| {
                let idx = s.cur_desc;
                let cur = s.room.extra_descriptions.get(idx);
                let complete = cur.map(|e| !e.0.is_empty() && !e.1.is_empty()).unwrap_or(false);
                let at_end = idx + 1 >= s.room.extra_descriptions.len();
                (complete, at_end)
            })
            .unwrap_or((false, true));
            if !complete {
                send(
                    g,
                    conn,
                    "You can't edit the next extra desc without completing this one.\r\n",
                );
            } else {
                with_state(conn, |s| {
                    if at_end {
                        s.room.extra_descriptions.push((String::new(), String::new()));
                    }
                    s.cur_desc += 1;
                });
            }
            disp_extradesc_menu(g, conn);
        }
        _ => disp_extradesc_menu(g, conn),
    }
}

// ===========================================================================
// Save to memory (redit_save_internally) — write the edit copy into the live
// world. New rooms are appended (rnum reindexing is implicit via add_room).
// ===========================================================================
fn save_internally(g: &mut GameState, conn: ConnId) {
    let (vnum, znum, edit) = match with_state(conn, |s| (s.vnum, s.znum, s.room.clone())) {
        Some(v) => v,
        None => return,
    };
    if let Some(rnum) = g.real_room(vnum) {
        // Existing room: overwrite editable fields, keep occupants/contents.
        let room = g.room_mut(rnum);
        room.name = edit.name;
        room.description = edit.description;
        room.sector_type = edit.sector_type;
        room.room_flags = edit.room_flags;
        room.exits = edit.exits;
        room.extra_descriptions = edit.extra_descriptions;
        room.zone = znum as i32;
    } else {
        // New room — append. (Note: this places it at the end of the rooms
        // vec; the room_index maps vnum->rnum so lookups still work. Full
        // sorted insertion + exit re-indexing is a C nicety not required for
        // correctness here since exits are stored by vnum.)
        let mut room = Room::new(vnum, znum as i32, edit.name, edit.description);
        room.sector_type = edit.sector_type;
        room.room_flags = edit.room_flags;
        room.exits = edit.exits;
        room.extra_descriptions = edit.extra_descriptions;
        g.add_room(room);
    }
    let zone_number = g.zones.get(znum).map(|z| z.number).unwrap_or(vnum / 100);
    olc::olc_add_to_save_list(zone_number, olc::OLC_SAVE_ROOM);
}

/// abort: drop this conn's editor state without saving (used when a player
/// disconnects mid-edit, so the edited vnum's lock is released). Mirrors the
/// state-removal half of `finish`/`cleanup_olc` minus the save and the room
/// announce. `olc::abort_editor` calls `olc::clear_active` for us.
pub fn abort(conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    text_bufs().lock().unwrap().remove(&conn);
}

fn finish(g: &mut GameState, conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    text_bufs().lock().unwrap().remove(&conn);
    olc::clear_active(conn);
    if let Some(ch) = conn_char(g, conn) {
        crate::act::act(
            g,
            "$n stops using OLC.",
            true,
            ch,
            None,
            crate::act::ActArg::None,
            crate::act::To::Room,
        );
    }
}

// ===========================================================================
// redit_save_to_disk — rewrite a zone's .wld file (inverse of
// file_loader::load_room_file). Writes every room in the zone's vnum band.
// ===========================================================================
pub fn redit_save_to_disk(g: &mut GameState, zone_rnum: usize) {
    let (zone_number, top) = match g.zones.get(zone_rnum) {
        Some(z) => (z.number, z.top),
        None => return,
    };
    let start = zone_number * 100;

    let mut out = String::new();
    for vnum in start..=top {
        let rnum = match g.real_room(vnum) {
            Some(r) => r,
            None => continue,
        };
        let room = g.room(rnum);
        out.push_str(&format!("#{}\n", vnum));
        out.push_str(&room.name);
        out.push_str("~\n");
        out.push_str(&olc::strip_cr(&room.description));
        out.push_str("~\n");
        // zone-number flags sector
        out.push_str(&format!(
            "{} {} {}\n",
            zone_number,
            room.room_flags.bits(),
            room.sector_type as i32
        ));

        // Exits.
        for dir in 0..NUM_OF_DIRS {
            if let Some(e) = &room.exits[dir] {
                let gen_desc = e
                    .description
                    .as_deref()
                    .map(olc::strip_cr)
                    .unwrap_or_default();
                let kw = e.keyword.clone().unwrap_or_default();
                // Door flag: 0 none, 1 door, 2 pickproof; +3 if hidden.
                let mut temp = if e.exit_info & EX_ISDOOR != 0 {
                    if e.exit_info & EX_PICKPROOF != 0 { 2 } else { 1 }
                } else {
                    0
                };
                if e.exit_info & EX_HIDDEN != 0 {
                    temp += 3;
                }
                out.push_str(&format!("D{}\n", dir));
                out.push_str(&gen_desc);
                out.push_str("~\n");
                out.push_str(&kw);
                out.push_str("~\n");
                out.push_str(&format!("{} {} {}\n", temp, e.key, e.to_room));
            }
        }

        // Special exit (`O` block) — must precede the extra descriptions,
        // mirroring C redit_save_to_disk. Four tilde-strings (general desc,
        // keyword, ex_name, leave msg) then a door-flag/key/to_room line.
        if let Some(se) = &room.special_exit {
            let gen_desc = se
                .general_description
                .as_deref()
                .map(olc::strip_cr)
                .unwrap_or_default();
            let kw = se.keyword.clone().unwrap_or_default();
            let ex_name = se
                .ex_name
                .as_deref()
                .map(olc::strip_cr)
                .unwrap_or_default();
            let leave_msg = se
                .leave_msg
                .as_deref()
                .map(olc::strip_cr)
                .unwrap_or_default();
            // Door flag: 0 none, 1 door, 2 pickproof; +3 if hidden.
            let mut temp = if se.exit_info & EX_ISDOOR != 0 {
                if se.exit_info & EX_PICKPROOF != 0 { 2 } else { 1 }
            } else {
                0
            };
            if se.exit_info & EX_HIDDEN != 0 {
                temp += 3;
            }
            out.push_str("O\n");
            out.push_str(&gen_desc);
            out.push_str("~\n");
            out.push_str(&kw);
            out.push_str("~\n");
            out.push_str(&ex_name);
            out.push_str("~\n");
            out.push_str(&leave_msg);
            out.push_str("~\n");
            out.push_str(&format!("{} {} {}\n", temp, se.key, se.to_room));
        }

        // Extra descriptions.
        for (kw, desc) in &room.extra_descriptions {
            out.push_str("E\n");
            out.push_str(kw);
            out.push_str("~\n");
            out.push_str(&olc::strip_cr(desc));
            out.push_str("~\n");
        }

        out.push_str("S\n");
    }
    out.push_str("$~\n");

    let path = std::path::Path::new(&g.config.lib_path)
        .join("world")
        .join("wld")
        .join(format!("{}.wld", zone_number));
    write_world_file(&path, &out);

    olc::olc_remove_from_save_list(zone_number, olc::OLC_SAVE_ROOM);
}

/// Atomic-ish file write: write to <path>.new, then rename over <path>
/// (mirrors the C remove()/rename() dance).
fn write_world_file(path: &std::path::Path, contents: &str) {
    let tmp = path.with_extension("new");
    if std::fs::write(&tmp, contents).is_err() {
        log::warn!("OLC: cannot write {:?}", tmp);
        return;
    }
    let _ = std::fs::remove_file(path);
    if std::fs::rename(&tmp, path).is_err() {
        log::warn!("OLC: cannot rename {:?} -> {:?}", tmp, path);
    }
}
