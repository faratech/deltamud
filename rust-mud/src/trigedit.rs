// trigedit.rs — the DG trigger editor (port of dg_olc.c trigedit_* + the
// generic do_olc setup path for SCMD_OLC_TRIGEDIT from olc.c).
//
// A builder edits a trigger *prototype*: its name, its attach-type (mob / obj /
// room), its trigger-type bitvector, a numeric argument, a strict argument, and
// the command-list body via the multi-line text editor. On save the editor
// rewrites the whole zone's `.trg` file byte-faithfully and refreshes the live
// trig_index (crate::dg_db_scripts) by reloading prototypes from disk.
//
// The shared OLC framework (olc.rs) routes one input line at a time into
// trigedit_parse() while in_olc(conn) is true; do_trigedit() starts the editor.
// Because Character / GameState / Descriptor may not gain an OLC_DATA field, the
// per-connection edit state lives in a module-static keyed by ConnId (the same
// pattern shop.rs / quest.rs / dg_handler.rs use).
//
// C reference: /web/deltamud/src/dg_olc.c (trigedit_setup_new/existing,
// trigedit_disp_menu, trigedit_disp_types, trigedit_parse, sprintbits,
// trigedit_save) and /web/deltamud/src/olc.c (do_olc, the TRIGEDIT arm).
#![allow(dead_code)]

use crate::dg_db_scripts::{self, TrigProto};
use crate::dg_handler::{MOB_TRIGGER, OBJ_TRIGGER, WLD_TRIGGER};
use crate::olc::{self, EditorKind};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Trigger-type name tables (dg_triggers.c: trig_types/otrig_types/wtrig_types).
// Index == bit position; trailing "\n" terminates (matches sprintbit usage).
// ---------------------------------------------------------------------------
const TRIG_TYPES: &[&str] = &[
    "Global", "Random", "Command", "Speech", "Act", "Death", "Greet", "Greet-All", "Entry",
    "Receive", "Fight", "HitPrcnt", "Bribe", "Load", "Memory", "\n",
];
const OTRIG_TYPES: &[&str] = &[
    "Global", "Random", "Command", "Fight", "UNUSED", "Timer", "Get", "Drop", "Give", "Wear",
    "UNUSED", "Remove", "UNUSED", "Load", "UNUSED", "\n",
];
const WTRIG_TYPES: &[&str] = &[
    "Global", "Random", "Command", "Speech", "UNUSED", "Zone Reset", "Enter", "Drop", "UNUSED",
    "UNUSED", "UNUSED", "UNUSED", "UNUSED", "UNUSED", "UNUSED", "\n",
];

// dg_olc.h: NUM_TRIG_TYPE_FLAGS 15, MAX_CMD_LENGTH 16384.
const NUM_TRIG_TYPE_FLAGS: usize = 15;
const MAX_CMD_LENGTH: usize = 16384;

// ANSI colours (olc.c get_char_cols: builders see colour). Kept as the raw
// escape sequences C emits so the .trg menu is byte-faithful to DeltaMUD.
const GRN: &str = "\x1b[0;32m";
const NRM: &str = "\x1b[0m";
const YEL: &str = "\x1b[0;33m";
const CYN: &str = "\x1b[0;36m";

// trigedit sub-modes (dg_olc.h TRIGEDIT_*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    MainMenu,
    ConfirmSaveString,
    Name,
    Intended,
    Types,
    Commands,
    Narg,
    Argument,
}

/// Per-connection trigedit state. Mirrors C's OLC_TRIG(d) (the scratch
/// trig_data), OLC_NUM(d) (vnum being edited), OLC_ZNUM(d) (the zone rnum the
/// trigger belongs to), OLC_STORAGE(d) (the cmdlist as a flat editable string)
/// and OLC_VAL(d) (the dirty flag).
struct TrigEditState {
    mode: Mode,
    vnum: i32,    // OLC_NUM
    znum: usize,  // OLC_ZNUM (index into g.zones)
    // scratch trigger (OLC_TRIG): the prototype we're editing.
    name: String,
    attach_type: i32,
    trigger_type: i64,
    narg: i32,
    arglist: String,
    storage: String, // OLC_STORAGE: cmdlist as "line\r\nline\r\n..."
    val: i32,        // OLC_VAL: has-changed flag
}

static EDIT_STATES: OnceLock<Mutex<HashMap<ConnId, TrigEditState>>> = OnceLock::new();

fn states() -> &'static Mutex<HashMap<ConnId, TrigEditState>> {
    EDIT_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit). `olc::abort_editor` calls `olc::clear_active`.
pub fn abort(conn: ConnId) {
    states().lock().unwrap().remove(&conn);
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let mut end = 0;
    let bytes = s.as_bytes();
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    s[..end].parse().unwrap_or(0)
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

/// Send raw text to a connection's character (OLC menus go to d->character).
fn send(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(cid) = conn_char(g, conn) {
        g.send_to_char(cid, msg);
    }
}

/// real_zone(number): the zone (rnum) whose number*100..=top range covers the
/// given vnum. Mirrors db.c real_zone.
fn real_zone(g: &GameState, number: i32) -> Option<usize> {
    for (idx, z) in g.zones.iter().enumerate() {
        if number >= z.number * 100 && number <= z.top {
            return Some(idx);
        }
    }
    None
}

/// The type name table for the current attach_type (trigedit_disp_menu /
/// trigedit_disp_types switch on attach_type).
fn type_table(attach_type: i32) -> &'static [&'static str] {
    match attach_type {
        OBJ_TRIGGER => OTRIG_TYPES,
        WLD_TRIGGER => WTRIG_TYPES,
        _ => TRIG_TYPES,
    }
}

/// sprintbit (utils.c): space-separated names of set bits, from a name table
/// terminated by "\n". Empty bitvector yields "".
fn sprintbit(bits: i64, table: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in table.iter().enumerate() {
        if *name == "\n" {
            break;
        }
        if i < 32 && (bits & (1 << i)) != 0 {
            out.push_str(name);
            out.push(' ');
        }
    }
    out
}

/// sprintbits (dg_olc.c): the letter encoding written to the .trg file. Bits
/// 0..25 => 'a'..'z', 26..51 => 'A'..'Z'.
fn sprintbits(data: i64) -> String {
    let mut out = String::new();
    for i in 0..32i64 {
        if data & (1 << i) != 0 {
            let c = if i <= 25 {
                (b'a' + i as u8) as char
            } else {
                (b'A' + (i - 26) as u8) as char
            };
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// do_trigedit — the OLC entry (olc.c do_olc, SCMD_OLC_TRIGEDIT arm).
// ---------------------------------------------------------------------------

pub fn do_trigedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // No screwing around as a mobile.
    let (is_npc, level, conn) = match g.get_char(ch) {
        Some(c) => (c.is_npc, c.player.level, c.desc),
        None => return,
    };
    if is_npc {
        return;
    }
    let conn = match conn {
        Some(c) => c,
        None => return,
    };

    // two_arguments(argument, buf1, buf2)
    let arg = arg.trim();
    let mut it = arg.split_whitespace();
    let buf1 = it.next().unwrap_or("");
    let buf2 = it.next().unwrap_or("");

    // No argument => prompt for a vnum.
    if buf1.is_empty() {
        send(g, conn, "Specify a trigger VNUM to edit.\r\n");
        return;
    }

    let _ = buf2;
    let number: i32;
    if !buf1.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        // C: only "save" is special for a non-digit arg (strn_cmp("save",buf1,4)).
        // Triggers autosave, so the save path just tells the builder there's
        // nothing to do; anything else is the "Yikes!" rejection.
        if buf1.len() >= 4 && buf1[..4].eq_ignore_ascii_case("save") {
            send(
                g,
                conn,
                "Triggers are autosaved to disk when edited, there's no need.\r\n",
            );
        } else {
            send(g, conn, "Yikes!  Stop that, someone will get hurt!\r\n");
        }
        return;
    } else {
        number = atoi(buf1);
    }

    // Find the zone for this trigger vnum.
    let znum = match real_zone(g, number) {
        Some(z) => z,
        None => {
            send(g, conn, "Sorry, there is no zone for that number!\r\n");
            return;
        }
    };

    // Everyone but IMPLs can only edit zones they have been assigned. The
    // builder-permission table is not modelled in this port (cmd_wizard.rs
    // notes the same); IMPLs and immortals edit freely. Mortals never reach
    // here (the command is LVL_IMMORT-gated in the command table).
    let _ = level;

    // Check that this trigger isn't already being edited on another connection.
    let busy_name: Option<String> = {
        let map = states().lock().unwrap();
        let mut who = None;
        for (&other_conn, st) in map.iter() {
            if other_conn != conn && st.vnum == number {
                who = g
                    .descriptors
                    .get(&other_conn)
                    .and_then(|d| d.character)
                    .and_then(|cid| g.get_char(cid))
                    .map(|c| c.player.name.clone())
                    .or(Some("someone".to_string()));
                break;
            }
        }
        who
    };
    if let Some(name) = busy_name {
        send(
            g,
            conn,
            &format!("That trigger is currently being edited by {}.\r\n", name),
        );
        return;
    }

    // Set up the scratch trigger (existing prototype or a fresh one).
    let state = match dg_db_scripts::real_trigger(number) {
        rnum if rnum >= 0 => setup_existing(rnum as usize, number, znum),
        _ => setup_new(number, znum),
    };

    states().lock().unwrap().insert(conn, state);
    olc::set_active(conn, EditorKind::Trigedit);
    disp_menu(g, conn);
}

/// trigedit_setup_existing: snapshot an existing prototype into edit state.
fn setup_existing(rnum: usize, vnum: i32, znum: usize) -> TrigEditState {
    let proto = dg_db_scripts::trig_proto(rnum).unwrap_or(TrigProto {
        vnum,
        attach_type: MOB_TRIGGER,
        name: "undefined".to_string(),
        trigger_type: 0,
        narg: 0,
        arglist: String::new(),
        cmdlist: Vec::new(),
    });

    // Convert cmdlist back into the flat editable string (C: each line + "\r\n").
    let mut storage = String::new();
    for line in &proto.cmdlist {
        storage.push_str(line);
        storage.push_str("\r\n");
    }

    TrigEditState {
        mode: Mode::MainMenu,
        vnum,
        znum,
        name: proto.name,
        attach_type: proto.attach_type,
        trigger_type: proto.trigger_type,
        narg: proto.narg,
        arglist: proto.arglist,
        storage,
        val: 0,
    }
}

/// trigedit_setup_new: a blank scratch trigger with C's defaults.
fn setup_new(vnum: i32, znum: usize) -> TrigEditState {
    TrigEditState {
        mode: Mode::MainMenu,
        vnum,
        znum,
        name: "new trigger".to_string(),
        attach_type: MOB_TRIGGER,
        trigger_type: 1 << 6, // MTRIG_GREET (C default)
        narg: 100,
        arglist: String::new(),
        storage: "say My trigger commandlist is not complete!\r\n".to_string(),
        val: 0,
    }
}

// ---------------------------------------------------------------------------
// Menu display (trigedit_disp_menu / trigedit_disp_types).
// ---------------------------------------------------------------------------

fn disp_menu(g: &mut GameState, conn: ConnId) {
    let (attach_label, trgtypes, vnum, name, narg, arglist, storage) = {
        let map = states().lock().unwrap();
        let st = match map.get(&conn) {
            Some(s) => s,
            None => return,
        };
        let label = match st.attach_type {
            OBJ_TRIGGER => "Objects",
            WLD_TRIGGER => "Rooms",
            _ => "Mobiles",
        };
        let trg = sprintbit(st.trigger_type, type_table(st.attach_type));
        (
            label,
            trg,
            st.vnum,
            st.name.clone(),
            st.narg,
            st.arglist.clone(),
            st.storage.clone(),
        )
    };

    let menu = format!(
        "Trigger Editor [{grn}{vnum}{nrm}]\r\n\r\n\
         {grn}1){nrm} Name         : {yel}{name}{nrm}\r\n\
         {grn}2){nrm} Intended for : {yel}{attach}{nrm}\r\n\
         {grn}3){nrm} Trigger types: {yel}{trg}{nrm}\r\n\
         {grn}4){nrm} Numberic Arg : {yel}{narg}{nrm}\r\n\
         {grn}5){nrm} Arguments    : {yel}{arg}{nrm}\r\n\
         {grn}6){nrm} Commands:\r\n{cyn}{cmds}{nrm}\r\n\
         {grn}Q){nrm} Quit\r\n\
         Enter Choice :",
        grn = GRN,
        nrm = NRM,
        yel = YEL,
        cyn = CYN,
        vnum = vnum,
        name = name,
        attach = attach_label,
        trg = trgtypes,
        narg = narg,
        arg = arglist,
        cmds = storage,
    );
    send(g, conn, &menu);
    set_mode(conn, Mode::MainMenu);
}

fn disp_types(g: &mut GameState, conn: ConnId) {
    let (table, cur) = {
        let map = states().lock().unwrap();
        let st = match map.get(&conn) {
            Some(s) => s,
            None => return,
        };
        (type_table(st.attach_type), st.trigger_type)
    };

    let mut out = String::new();
    let mut columns = 0;
    for i in 0..NUM_TRIG_TYPE_FLAGS {
        columns += 1;
        out.push_str(&format!(
            "{}{:2}{}) {:<20.20}  {}",
            GRN,
            i + 1,
            NRM,
            table[i],
            if columns % 2 == 0 { "\r\n" } else { "" }
        ));
    }
    let bits = sprintbit(cur, table);
    out.push_str(&format!(
        "\r\nCurrent types : {}{}{}\r\nEnter type (0 to quit) : ",
        CYN, bits, NRM
    ));
    send(g, conn, &out);
}

// ---------------------------------------------------------------------------
// State helpers
// ---------------------------------------------------------------------------

fn set_mode(conn: ConnId, mode: Mode) {
    if let Some(st) = states().lock().unwrap().get_mut(&conn) {
        st.mode = mode;
    }
}

fn get_mode(conn: ConnId) -> Option<Mode> {
    states().lock().unwrap().get(&conn).map(|s| s.mode)
}

// ---------------------------------------------------------------------------
// trigedit_parse — the per-line input handler the OLC router calls.
// ---------------------------------------------------------------------------

pub fn trigedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match get_mode(conn) {
        Some(m) => m,
        None => return,
    };

    match mode {
        Mode::Commands => {
            // The multi-line command-list editor (C drives this via d->str /
            // string_add; here it is inline so the OLC router stays the only
            // input path). Returns when the editor saves (/s) or aborts (/a).
            commands_input(g, conn, line);
            return;
        }
        Mode::MainMenu => {
            parse_main_menu(g, conn, line);
            return;
        }
        Mode::ConfirmSaveString => {
            match line.trim_start().chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('y') => {
                    save(g, conn);
                    let cname = conn_char(g, conn)
                        .and_then(|c| g.get_char(c))
                        .map(|c| c.player.name.clone())
                        .unwrap_or_default();
                    let vnum = states().lock().unwrap().get(&conn).map(|s| s.vnum).unwrap_or(0);
                    mudlog(g, &format!("OLC: {} edits trigger {}", cname, vnum));
                    cleanup(g, conn);
                }
                Some('n') => {
                    cleanup(g, conn);
                }
                Some('a') => {
                    // abort quitting — back to the main menu.
                    disp_menu(g, conn);
                }
                _ => {
                    send(g, conn, "Invalid choice!\r\n");
                    send(g, conn, "Do you wish to save the trigger? : ");
                }
            }
            return;
        }
        Mode::Name => {
            let arg = line.trim();
            if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                st.name = if arg.is_empty() { "undefined".to_string() } else { arg.to_string() };
                st.val += 1;
            }
        }
        Mode::Intended => {
            let v = atoi(line);
            if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                // C: ((atoi>=MOB_TRIGGER) || (atoi<=WLD_TRIGGER)) — that guard is
                // always true in C, so any value is accepted as the attach_type.
                st.attach_type = v;
                st.val += 1;
            }
        }
        Mode::Narg => {
            let v = atoi(line);
            if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                st.narg = v;
                st.val += 1;
            }
        }
        Mode::Argument => {
            let arg = line.trim();
            if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                st.arglist = arg.to_string();
                st.val += 1;
            }
        }
        Mode::Types => {
            let i = atoi(line);
            if i == 0 {
                // fall through to main menu
            } else {
                if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                    if i > 0 && i <= NUM_TRIG_TYPE_FLAGS as i32 {
                        st.trigger_type ^= 1 << (i - 1);
                    }
                    st.val += 1;
                }
                disp_types(g, conn);
                return;
            }
        }
    }

    // Default: return to the main menu (C: OLC_MODE = MAIN_MENU; disp_menu).
    set_mode(conn, Mode::MainMenu);
    disp_menu(g, conn);
}

fn parse_main_menu(g: &mut GameState, conn: ConnId, line: &str) {
    let c = line.trim_start().chars().next().map(|c| c.to_ascii_lowercase());
    match c {
        Some('q') => {
            let val = states().lock().unwrap().get(&conn).map(|s| s.val).unwrap_or(0);
            if val != 0 {
                let ttype = states()
                    .lock()
                    .unwrap()
                    .get(&conn)
                    .map(|s| s.trigger_type)
                    .unwrap_or(0);
                if ttype == 0 {
                    send(
                        g,
                        conn,
                        "Invalid Trigger Type! Answer a to abort quit!\r\n",
                    );
                }
                send(
                    g,
                    conn,
                    "Do you wish to save the changes to the trigger? (y/n): ",
                );
                set_mode(conn, Mode::ConfirmSaveString);
            } else {
                cleanup(g, conn);
            }
        }
        Some('1') => {
            set_mode(conn, Mode::Name);
            send(g, conn, "Name: ");
        }
        Some('2') => {
            set_mode(conn, Mode::Intended);
            send(g, conn, "0: Mobiles, 1: Objects, 2: Rooms: ");
        }
        Some('3') => {
            set_mode(conn, Mode::Types);
            disp_types(g, conn);
        }
        Some('4') => {
            set_mode(conn, Mode::Narg);
            send(g, conn, "Numeric argument: ");
        }
        Some('5') => {
            set_mode(conn, Mode::Argument);
            send(g, conn, "Argument: ");
        }
        Some('6') => {
            set_mode(conn, Mode::Commands);
            send(
                g,
                conn,
                "Enter trigger commands: (/s saves /h for help)\r\n\r\n",
            );
            // Echo the current buffer (C sends OLC_STORAGE then dups to backstr).
            let storage = states().lock().unwrap().get(&conn).map(|s| s.storage.clone());
            if let Some(s) = storage {
                if !s.is_empty() {
                    send(g, conn, &s);
                }
            }
            if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                st.val = 1;
            }
        }
        _ => {
            disp_menu(g, conn);
        }
    }
}

// ---------------------------------------------------------------------------
// The command-list multi-line editor (C: d->str/string_add, with the OLC
// builder editing OLC_STORAGE). We mirror string_add's `/`-command set so the
// trigger body is edited exactly as in DeltaMUD. On /s we drop back into the
// trigedit main menu; on /a we abort the buffer change.
// ---------------------------------------------------------------------------

fn commands_input(g: &mut GameState, conn: ConnId, line: &str) {
    // delete_doubledollar: "$$" -> "$".
    let str_in = line.replace("$$", "$");

    let mut terminator = 0; // 0 keep editing, 1 save, 2 abort
    let mut action = false;

    if str_in.starts_with('/') {
        action = true;
        let actions: String = str_in.chars().skip(2).collect();
        let cmd = str_in.chars().nth(1).unwrap_or('\0');
        match cmd {
            'a' => terminator = 2, // abort
            'c' => {
                let empty = states()
                    .lock()
                    .unwrap()
                    .get(&conn)
                    .map(|s| s.storage.is_empty())
                    .unwrap_or(true);
                if !empty {
                    if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                        st.storage.clear();
                    }
                    send(g, conn, "Current buffer cleared.\r\n");
                } else {
                    send(g, conn, "Current buffer empty.\r\n");
                }
            }
            'd' => parse_action_delete(g, conn, &actions),
            'e' => parse_action_edit(g, conn, &actions),
            'f' => {
                if buffer_nonempty(conn) {
                    parse_action_format(g, conn, &actions);
                } else {
                    send(g, conn, "Current buffer empty.\r\n");
                }
            }
            'i' => {
                if buffer_nonempty(conn) {
                    parse_action_insert(g, conn, &actions);
                } else {
                    send(g, conn, "Current buffer empty.\r\n");
                }
            }
            'h' => parse_action_help(g, conn),
            'l' => {
                if buffer_nonempty(conn) {
                    parse_action_list(g, conn, &actions, false);
                } else {
                    send(g, conn, "Current buffer empty.\r\n");
                }
            }
            'n' => {
                if buffer_nonempty(conn) {
                    parse_action_list(g, conn, &actions, true);
                } else {
                    send(g, conn, "Current buffer empty.\r\n");
                }
            }
            'r' => parse_action_replace(g, conn, &actions),
            's' => terminator = 1, // save
            _ => send(g, conn, "Invalid option.\r\n"),
        }
    }

    // Append the line to the buffer (only if it was not a command).
    if !action {
        let mut buf = states().lock().unwrap().get(&conn).map(|s| s.storage.clone()).unwrap_or_default();
        let mut append = str_in.clone();
        if buf.is_empty() {
            if append.len() > MAX_CMD_LENGTH {
                send(g, conn, "String too long - Truncated.\r\n");
                append.truncate(MAX_CMD_LENGTH);
            }
            buf = append;
        } else if append.len() + buf.len() > MAX_CMD_LENGTH {
            send(
                g,
                conn,
                "String too long, limit reached on message. Last line ignored.\r\n",
            );
            return; // keep editing; line ignored
        } else {
            buf.push_str(&append);
        }
        if let Some(st) = states().lock().unwrap().get_mut(&conn) {
            st.storage = buf;
        }
    }

    if terminator == 0 {
        // Not finished: a non-command line gets a trailing CRLF appended.
        if !action {
            if let Some(st) = states().lock().unwrap().get_mut(&conn) {
                st.storage.push_str("\r\n");
            }
        }
        return;
    }

    // Editor finished. terminator 1 = keep buffer (save), 2 = abort the change.
    // Either way we return to the trigedit main menu (the C parser sets
    // OLC_MODE = TRIGEDIT_MAIN_MENU when the TRIGEDIT_COMMANDS case is reached
    // after string editing ends). The buffer is the new OLC_STORAGE on save; on
    // abort C reverts to backstr — we keep the buffer as-is only on save.
    if terminator == 2 {
        // Abort: restore from the on-disk prototype (or empty for a new trig),
        // matching C reverting d->str from d->backstr (the pre-edit text).
        let (vnum,) = {
            let map = states().lock().unwrap();
            let st = match map.get(&conn) {
                Some(s) => s,
                None => return,
            };
            (st.vnum,)
        };
        let rnum = dg_db_scripts::real_trigger(vnum);
        let restored = if rnum >= 0 {
            let mut s = String::new();
            if let Some(p) = dg_db_scripts::trig_proto(rnum as usize) {
                for l in &p.cmdlist {
                    s.push_str(l);
                    s.push_str("\r\n");
                }
            }
            s
        } else {
            "say My trigger commandlist is not complete!\r\n".to_string()
        };
        if let Some(st) = states().lock().unwrap().get_mut(&conn) {
            st.storage = restored;
        }
    }

    set_mode(conn, Mode::MainMenu);
    disp_menu(g, conn);
}

fn buffer_nonempty(conn: ConnId) -> bool {
    states().lock().unwrap().get(&conn).map(|s| !s.storage.is_empty()).unwrap_or(false)
}

// Split the flat storage into logical lines (the cmdlist_element chain). Lines
// are CRLF-separated; a trailing empty segment is dropped.
fn buffer_lines(conn: ConnId) -> Vec<String> {
    let buf = states().lock().unwrap().get(&conn).map(|s| s.storage.clone()).unwrap_or_default();
    let mut v: Vec<String> = buf
        .split("\r\n")
        .map(|l| l.to_string())
        .collect();
    // A buffer that ends in CRLF leaves a trailing empty element; drop it.
    if v.last().map(|l| l.is_empty()).unwrap_or(false) {
        v.pop();
    }
    v
}

fn set_buffer_lines(conn: ConnId, lines: &[String]) {
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        s.push_str("\r\n");
    }
    if let Some(st) = states().lock().unwrap().get_mut(&conn) {
        st.storage = s;
    }
}

// ---- modify.c parse_action subset for the inline editor --------------------

fn parse_action_help(g: &mut GameState, conn: ConnId) {
    let help = "Editor command formats: /<letter>\r\n\r\n\
        /a         -  aborts editor\r\n\
        /c         -  clears buffer\r\n\
        /d#        -  deletes a line #\r\n\
        /e# <text> -  changes the line at # with <text>\r\n\
        /f         -  formats text\r\n\
        /i# <text> -  inserts <text> at line #\r\n\
        /l         -  lists buffer\r\n\
        /n         -  lists buffer with line numbers\r\n\
        /r 'a' 'b' -  replace 1st occurrence of text <a> in buffer with text <b>\r\n\
        /s         -  saves text\r\n";
    send(g, conn, help);
}

fn parse_action_list(g: &mut GameState, conn: ConnId, args: &str, numbered: bool) {
    let lines = buffer_lines(conn);
    let parts: Vec<i32> = args.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    let (mut start, end): (usize, usize) = match parts.len() {
        0 => (1, lines.len()),
        1 => (parts[0].max(1) as usize, parts[0].max(1) as usize),
        _ => (parts[0].max(1) as usize, parts[1].max(1) as usize),
    };
    if start < 1 {
        start = 1;
    }
    let mut out = String::new();
    let mut i = start;
    while i <= end && i <= lines.len() {
        if numbered {
            out.push_str(&format!("{:4}: ", i));
        }
        out.push_str(&lines[i - 1]);
        out.push_str("\r\n");
        i += 1;
    }
    if out.is_empty() {
        out.push_str("No lines in that range.\r\n");
    }
    send(g, conn, &out);
}

fn parse_action_delete(g: &mut GameState, conn: ConnId, args: &str) {
    let mut lines = buffer_lines(conn);
    let n: i32 = atoi(args);
    if n < 1 || (n as usize) > lines.len() {
        send(g, conn, "Line number out of range; aborting.\r\n");
        return;
    }
    lines.remove((n - 1) as usize);
    set_buffer_lines(conn, &lines);
    send(g, conn, &format!("Line {} deleted.\r\n", n));
}

fn parse_action_edit(g: &mut GameState, conn: ConnId, args: &str) {
    // /e# <text>
    let trimmed = args.trim_start();
    let mut it = trimmed.splitn(2, char::is_whitespace);
    let num_str = it.next().unwrap_or("");
    let text = it.next().unwrap_or("");
    let n: i32 = atoi(num_str);
    let mut lines = buffer_lines(conn);
    if n < 1 || (n as usize) > lines.len() {
        send(g, conn, "Line number out of range; aborting.\r\n");
        return;
    }
    lines[(n - 1) as usize] = text.to_string();
    set_buffer_lines(conn, &lines);
    send(g, conn, &format!("Line {} changed.\r\n", n));
}

fn parse_action_insert(g: &mut GameState, conn: ConnId, args: &str) {
    // /i# <text>
    let trimmed = args.trim_start();
    let mut it = trimmed.splitn(2, char::is_whitespace);
    let num_str = it.next().unwrap_or("");
    let text = it.next().unwrap_or("");
    let n: i32 = atoi(num_str);
    let mut lines = buffer_lines(conn);
    if n < 1 || (n as usize) > lines.len() + 1 {
        send(g, conn, "Line number out of range; aborting.\r\n");
        return;
    }
    lines.insert((n - 1) as usize, text.to_string());
    set_buffer_lines(conn, &lines);
    send(g, conn, &format!("Line inserted at {}.\r\n", n));
}

fn parse_action_replace(g: &mut GameState, conn: ConnId, args: &str) {
    // /r 'a' 'b' — replace first occurrence of a with b.
    let (a, b) = match parse_two_quoted(args) {
        Some(p) => p,
        None => {
            send(
                g,
                conn,
                "Invalid format.  Use: /r 'pattern' 'replacement'\r\n",
            );
            return;
        }
    };
    let mut buf = states().lock().unwrap().get(&conn).map(|s| s.storage.clone()).unwrap_or_default();
    if let Some(pos) = buf.find(&a) {
        buf.replace_range(pos..pos + a.len(), &b);
        if let Some(st) = states().lock().unwrap().get_mut(&conn) {
            st.storage = buf;
        }
        send(g, conn, "Replacement made.\r\n");
    } else {
        send(g, conn, "Search string not found.\r\n");
    }
}

fn parse_action_format(g: &mut GameState, conn: ConnId, _args: &str) {
    // Trigger command lists are not prose; "format" simply normalises trailing
    // whitespace on each line (C's format_text would re-wrap, which would break
    // script lines). We preserve line structure.
    let lines: Vec<String> = buffer_lines(conn)
        .into_iter()
        .map(|l| l.trim_end().to_string())
        .collect();
    set_buffer_lines(conn, &lines);
    send(g, conn, "Text formatted.\r\n");
}

/// Parse two single-quoted strings: 'a' 'b'.
fn parse_two_quoted(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let start_a = s.find('\'')? + 1;
    let rest = &s[start_a..];
    let end_a = rest.find('\'')?;
    let a = rest[..end_a].to_string();
    let after_a = &rest[end_a + 1..];
    let start_b = after_a.find('\'')? + 1;
    let rest_b = &after_a[start_b..];
    let end_b = rest_b.find('\'')?;
    let b = rest_b[..end_b].to_string();
    Some((a, b))
}

// ---------------------------------------------------------------------------
// trigedit_save (dg_olc.c): rewrite the whole zone's .trg file byte-faithfully
// and refresh the live trig_index by reloading prototypes from disk.
// ---------------------------------------------------------------------------

fn save(g: &mut GameState, conn: ConnId) {
    // Snapshot the scratch trigger out of edit state.
    let (vnum, znum, name, attach_type, trigger_type, narg, arglist, storage) = {
        let map = states().lock().unwrap();
        let st = match map.get(&conn) {
            Some(s) => s,
            None => return,
        };
        (
            st.vnum,
            st.znum,
            st.name.clone(),
            st.attach_type,
            st.trigger_type,
            st.narg,
            st.arglist.clone(),
            st.storage.clone(),
        )
    };

    // Recompile the command list from the storage text (strtok on "\n\r" drops
    // empty lines — match that exactly).
    let cmdlist: Vec<String> = storage
        .split(['\n', '\r'])
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    let edited = TrigProto {
        vnum,
        attach_type,
        name: name.clone(),
        trigger_type,
        narg,
        arglist: arglist.clone(),
        cmdlist,
    };

    // Resolve the zone range to rewrite.
    let (zone_number, zone_top) = match g.zones.get(znum) {
        Some(z) => (z.number, z.top),
        None => return,
    };
    let lib_path = g.config.lib_path.clone();
    let trg_dir = Path::new(&lib_path).join("world").join("trg");

    // Build the full set of prototypes to write for this zone: every existing
    // prototype in [zone*100 .. top], with `edited` substituted/inserted for
    // its own vnum. We read existing protos from the live index (dg_db_scripts).
    let mut zone_protos: Vec<TrigProto> = Vec::new();
    let mut inserted = false;
    for i in (zone_number * 100)..=zone_top {
        if i == vnum {
            zone_protos.push(edited.clone());
            inserted = true;
            continue;
        }
        let rnum = dg_db_scripts::real_trigger(i);
        if rnum >= 0 {
            if let Some(p) = dg_db_scripts::trig_proto(rnum as usize) {
                zone_protos.push(p);
            }
        }
    }
    if !inserted {
        // vnum out of the iterated range (shouldn't happen since real_zone
        // matched) — append it so the edit is never lost.
        zone_protos.push(edited.clone());
        zone_protos.sort_by_key(|p| p.vnum);
    }

    // Write to "<zone>.new" then rename to "<zone>.trg" (atomic-ish, like C).
    let new_path = trg_dir.join(format!("{}.new", zone_number));
    let final_path = trg_dir.join(format!("{}.trg", zone_number));

    let mut text = String::new();
    for p in &zone_protos {
        text.push_str(&format!("#{}\n", p.vnum));
        let bit_buf = sprintbits(p.trigger_type);
        let pname = if p.name.is_empty() { "unknown trigger" } else { &p.name };
        text.push_str(&format!(
            "{}~\n{} {} {}\n{}~\n",
            pname, p.attach_type, bit_buf, p.narg, p.arglist
        ));
        // The script body.
        let mut body = String::new();
        for c in &p.cmdlist {
            body.push_str(c);
            body.push_str("\r\n");
        }
        if body.is_empty() {
            body.push_str("* Empty script");
        }
        text.push_str(&format!("{}~\n", body));
    }
    text.push_str("$~\n");

    let write_ok = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&new_path)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        Ok(())
    })();

    match write_ok {
        Ok(()) => {
            let _ = std::fs::rename(&new_path, &final_path);
        }
        Err(_) => {
            mudlog(
                g,
                &format!(
                    "SYSERR: OLC: Can't open trig file \"{}\"",
                    new_path.display()
                ),
            );
            return;
        }
    }

    // Refresh the live trig_index from disk so the new/edited prototype takes
    // effect immediately (boot_triggers clears and reloads every prototype).
    // This is the Rust equivalent of dg_olc.c rebuilding trig_index + walking
    // trigger_list, done via the canonical loader so rnum ordering stays sane.
    dg_db_scripts::boot_triggers(&lib_path);
}

// ---------------------------------------------------------------------------
// cleanup / mudlog
// ---------------------------------------------------------------------------

fn cleanup(g: &mut GameState, conn: ConnId) {
    states().lock().unwrap().remove(&conn);
    olc::clear_active(conn);
    // C cleanup_olc returns to the playing prompt; the framework restores the
    // descriptor state. Echo a blank line so the player sees the prompt return.
    send(g, conn, "\r\n");
}

/// mudlog to immortals at builder level (CMP channel in C). We reuse the same
/// immortal-broadcast shape the rest of the port uses.
fn mudlog(g: &mut GameState, line: &str) {
    let formatted = format!("[ {} ]\r\n", line);
    let imms: Vec<CharId> = g
        .players_by_name
        .values()
        .copied()
        .filter(|&id| {
            g.get_char(id)
                .map(|c| c.player.level >= LVL_IMMORT)
                .unwrap_or(false)
        })
        .collect();
    for id in imms {
        g.send_to_char(id, &formatted);
    }
}
