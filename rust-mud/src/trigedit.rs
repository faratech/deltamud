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
use std::path::Path;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Trigger-type name tables (dg_triggers.c: trig_types/otrig_types/wtrig_types).
// Index == bit position; trailing "\n" terminates (matches sprintbit usage).
// ---------------------------------------------------------------------------
const TRIG_TYPES: &[&str] = &[
    "Global",
    "Random",
    "Command",
    "Speech",
    "Act",
    "Death",
    "Greet",
    "Greet-All",
    "Entry",
    "Receive",
    "Fight",
    "HitPrcnt",
    "Bribe",
    "Load",
    "Memory",
    "\n",
];
const OTRIG_TYPES: &[&str] = &[
    "Global", "Random", "Command", "Fight", "UNUSED", "Timer", "Get", "Drop", "Give", "Wear",
    "UNUSED", "Remove", "UNUSED", "Load", "UNUSED", "\n",
];
const WTRIG_TYPES: &[&str] = &[
    "Global",
    "Random",
    "Command",
    "Speech",
    "UNUSED",
    "Zone Reset",
    "Enter",
    "Drop",
    "UNUSED",
    "UNUSED",
    "UNUSED",
    "UNUSED",
    "UNUSED",
    "UNUSED",
    "UNUSED",
    "\n",
];

// dg_olc.h: NUM_TRIG_TYPE_FLAGS 15, MAX_CMD_LENGTH 16384.
const NUM_TRIG_TYPE_FLAGS: usize = 15;
const MAX_CMD_LENGTH: usize = 16384;

// OLC colours: the &-codes C's get_char_cols fills in (screen.h KNRM/KGRN/
// KYEL/KCYN), gated per builder by olc::olc_send (#306).
use crate::olc::{CYN, GRN, NRM, YEL};

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
    vnum: i32,        // OLC_NUM
    znum: usize,      // OLC_ZNUM (index into g.zones)
    zone_number: i32, // retained stable identity for that zone index
    authorization: olc::OlcAuthorization,
    // scratch trigger (OLC_TRIG): the prototype we're editing.
    name: String,
    attach_type: i32,
    trigger_type: i64,
    narg: i32,
    arglist: String,
    storage: String, // OLC_STORAGE: cmdlist as "line\r\nline\r\n..."
    back_storage: Option<String>,
    val: i32, // OLC_VAL: has-changed flag
}

static EDIT_STATES: OnceLock<Mutex<HashMap<ConnId, TrigEditState>>> = OnceLock::new();

fn states() -> &'static Mutex<HashMap<ConnId, TrigEditState>> {
    EDIT_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit). `olc::abort_editor` calls `olc::clear_active`.
pub fn abort(conn: ConnId) {
    if let Some(state) = crate::lock_ok::lock(&states()).remove(&conn) {
        olc::discard_unresolved_save(EditorKind::Trigedit, state.vnum);
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

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
        if z.contains_vnum(number) {
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
    let (is_npc, conn) = match g.get_char(ch) {
        Some(c) => (c.is_npc, c.desc),
        None => return,
    };
    if is_npc {
        return;
    }
    let conn = match conn {
        Some(c) => c,
        None => return,
    };
    let Some(authorization) = olc::capture_olc_authorization(g, ch) else {
        send(g, conn, "You do not have permission to edit that zone.\r\n");
        return;
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
    if !buf1
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        // C: only "save" is special for a non-digit arg (strn_cmp("save",buf1,4)).
        // Triggers autosave, so the save path just tells the builder there's
        // nothing to do; anything else is the "Yikes!" rejection.
        if buf1
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("save"))
        {
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
        let Some(parsed) = olc_atoi(g, conn, buf1) else {
            return;
        };
        number = parsed;
    }

    // Find the zone for this trigger vnum.
    let znum = match real_zone(g, number) {
        Some(z) => z,
        None => {
            send(g, conn, "Sorry, there is no zone for that number!\r\n");
            return;
        }
    };
    if !olc::can_edit_zone(g, ch, znum) {
        send(g, conn, "You do not have permission to edit that zone.\r\n");
        return;
    }

    // Check that this trigger isn't already being edited on another connection.
    let busy_name: Option<String> = {
        let map = crate::lock_ok::lock(&states());
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
    let zone_number = g.zones[znum].number;
    let state = match dg_db_scripts::real_trigger(number) {
        rnum if rnum >= 0 => {
            setup_existing(rnum as usize, number, znum, zone_number, authorization)
        }
        _ => setup_new(number, znum, zone_number, authorization),
    };

    crate::lock_ok::lock(&states()).insert(conn, state);
    olc::set_active(conn, EditorKind::Trigedit);
    disp_menu(g, conn);
}

/// trigedit_setup_existing: snapshot an existing prototype into edit state.
fn setup_existing(
    rnum: usize,
    vnum: i32,
    znum: usize,
    zone_number: i32,
    authorization: olc::OlcAuthorization,
) -> TrigEditState {
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
        zone_number,
        authorization,
        name: proto.name,
        attach_type: proto.attach_type,
        trigger_type: proto.trigger_type,
        narg: proto.narg,
        arglist: proto.arglist,
        storage,
        back_storage: None,
        val: 0,
    }
}

/// trigedit_setup_new: a blank scratch trigger with C's defaults.
fn setup_new(
    vnum: i32,
    znum: usize,
    zone_number: i32,
    authorization: olc::OlcAuthorization,
) -> TrigEditState {
    TrigEditState {
        mode: Mode::MainMenu,
        vnum,
        znum,
        zone_number,
        authorization,
        name: "new trigger".to_string(),
        attach_type: MOB_TRIGGER,
        trigger_type: 1 << 6, // MTRIG_GREET (C default)
        narg: 100,
        arglist: String::new(),
        storage: "say My trigger commandlist is not complete!\r\n".to_string(),
        back_storage: None,
        val: 0,
    }
}

// ---------------------------------------------------------------------------
// Menu display (trigedit_disp_menu / trigedit_disp_types).
// ---------------------------------------------------------------------------

fn disp_menu(g: &mut GameState, conn: ConnId) {
    let (attach_label, trgtypes, vnum, name, narg, arglist, storage) = {
        let map = crate::lock_ok::lock(&states());
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
        let map = crate::lock_ok::lock(&states());
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
    if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
        st.mode = mode;
    }
}

fn get_mode(conn: ConnId) -> Option<Mode> {
    crate::lock_ok::lock(&states()).get(&conn).map(|s| s.mode)
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
            match line
                .trim_start()
                .chars()
                .next()
                .map(|c| c.to_ascii_lowercase())
            {
                Some('y') => match save(g, conn) {
                    Ok(()) => {
                        let cname = conn_char(g, conn)
                            .and_then(|c| g.get_char(c))
                            .map(|c| c.player.name.clone())
                            .unwrap_or_default();
                        let vnum = states()
                            .lock()
                            .unwrap()
                            .get(&conn)
                            .map(|s| s.vnum)
                            .unwrap_or(0);
                        mudlog(g, &format!("OLC: {} edits trigger {}", cname, vnum));
                        cleanup(g, conn);
                    }
                    Err(err) => {
                        mudlog(g, &format!("SYSERR: OLC: could not save trigger: {}", err));
                        if olc::replacement_was_published(&err) {
                            send(
                                g,
                                conn,
                                "The trigger file was published and live triggers were reconciled, but crash durability could not be confirmed.\r\nDo you wish to retry saving the trigger? : ",
                            );
                        } else {
                            send(
                                g,
                                conn,
                                "Could not save the trigger to disk; the live trigger was not changed.\r\nDo you wish to retry saving the trigger? : ",
                            );
                        }
                    }
                },
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
            // C dg_olc.c:340 stores the raw line (str_dup); no trim (#300).
            let arg = line;
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                st.name = if arg.is_empty() {
                    "undefined".to_string()
                } else {
                    arg.to_string()
                };
                st.val += 1;
            }
        }
        Mode::Intended => {
            let Some(v) = olc_atoi(g, conn, line) else {
                return;
            };
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                // C: ((atoi>=MOB_TRIGGER) || (atoi<=WLD_TRIGGER)) — that guard is
                // always true in C, so any value is accepted, stored as
                // (byte)atoi (wraps 0-255, dg_olc.c:353) (#300).
                st.attach_type = (v as u8) as i32;
                st.val += 1;
            }
        }
        Mode::Narg => {
            let Some(v) = olc_atoi(g, conn, line) else {
                return;
            };
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                st.narg = v;
                st.val += 1;
            }
        }
        Mode::Argument => {
            // C dg_olc.c:356 stores the raw line; no trim (#300).
            let arg = line;
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                st.arglist = arg.to_string();
                st.val += 1;
            }
        }
        Mode::Types => {
            let Some(i) = olc_atoi(g, conn, line) else {
                return;
            };
            if i == 0 {
                // fall through to main menu
            } else {
                if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
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
    let c = line
        .trim_start()
        .chars()
        .next()
        .map(|c| c.to_ascii_lowercase());
    match c {
        Some('q') => {
            let val = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| s.val)
                .unwrap_or(0);
            if val != 0 {
                let ttype = states()
                    .lock()
                    .unwrap()
                    .get(&conn)
                    .map(|s| s.trigger_type)
                    .unwrap_or(0);
                if ttype == 0 {
                    send(g, conn, "Invalid Trigger Type! Answer a to abort quit!\r\n");
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
            let storage = states()
                .lock()
                .unwrap()
                .get(&conn)
                .map(|s| s.storage.clone());
            if let Some(s) = storage {
                if !s.is_empty() {
                    send(g, conn, &s);
                }
            }
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                st.back_storage = Some(st.storage.clone());
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
    let mut buf = states()
        .lock()
        .unwrap()
        .get(&conn)
        .map(|s| s.storage.clone())
        .unwrap_or_default();

    match crate::modify::editor_buffer_input(g, conn, &mut buf, MAX_CMD_LENGTH, line) {
        crate::modify::BufferEditorResult::Continue => {
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                st.storage = buf;
            }
        }
        crate::modify::BufferEditorResult::Save => {
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                st.storage = buf;
                st.back_storage = None;
            }
            set_mode(conn, Mode::MainMenu);
            disp_menu(g, conn);
        }
        crate::modify::BufferEditorResult::Abort => {
            if let Some(st) = crate::lock_ok::lock(&states()).get_mut(&conn) {
                if let Some(back) = st.back_storage.take() {
                    st.storage = back;
                }
                st.mode = Mode::MainMenu;
            }
            disp_menu(g, conn);
        }
    }
}

// ---------------------------------------------------------------------------
// trigedit_save (dg_olc.c): rewrite the whole zone's .trg file byte-faithfully,
// then publish the edited prototype into the live trigger index.
// ---------------------------------------------------------------------------

fn save(g: &mut GameState, conn: ConnId) -> std::io::Result<()> {
    save_with(g, conn, olc::atomic_replace)
}

fn save_with<F>(g: &mut GameState, conn: ConnId, replace: F) -> std::io::Result<()>
where
    F: FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
{
    // Snapshot the scratch trigger out of edit state.
    let (
        vnum,
        znum,
        zone_number,
        authorization,
        name,
        attach_type,
        trigger_type,
        narg,
        arglist,
        storage,
    ) = {
        let map = crate::lock_ok::lock(&states());
        let st = match map.get(&conn) {
            Some(s) => s,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "trigger editor state is missing",
                ));
            }
        };
        (
            st.vnum,
            st.znum,
            st.zone_number,
            st.authorization,
            st.name.clone(),
            st.attach_type,
            st.trigger_type,
            st.narg,
            st.arglist.clone(),
            st.storage.clone(),
        )
    };

    // Recheck the exact authenticated principal, zone mapping, and zone ACL at
    // the publication boundary. A descriptor handoff or builder-list change
    // while the scratch editor is open must never become a confused-deputy
    // disk write.
    if real_zone(g, vnum) != Some(znum)
        || g.zones.get(znum).map(|zone| zone.number) != Some(zone_number)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "trigger editor zone mapping changed",
        ));
    }
    olc::revalidate_olc_authorization(g, authorization, false, Some(znum))?;

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
    let (zone_start, zone_top) = match g.zones.get(znum) {
        Some(z) => match z.vnum_start() {
            Some(zone_start) => (zone_start, z.top),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "trigger zone number is outside the supported range",
                ));
            }
        },
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "trigger zone index is not loaded",
            ));
        }
    };
    let lib_path = g.config.lib_path.clone();
    let trg_dir = Path::new(&lib_path).join("world").join("trg");

    // Build the full set of prototypes to write for this zone: every existing
    // prototype in [zone*100 .. top], with `edited` substituted/inserted for
    // its own vnum. We read existing protos from the live index (dg_db_scripts).
    let mut zone_protos: Vec<TrigProto> = Vec::new();
    let mut inserted = false;
    for i in zone_start..=zone_top {
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

    let final_path = trg_dir.join(format!("{}.trg", zone_number));

    let mut text = String::new();
    for p in &zone_protos {
        text.push_str(&format!("#{}\n", p.vnum));
        let bit_buf = sprintbits(p.trigger_type);
        let pname = if p.name.is_empty() {
            "unknown trigger"
        } else {
            &p.name
        };
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

    std::fs::create_dir_all(&trg_dir)?;
    match replace(&final_path, text.as_bytes()) {
        Ok(()) => {
            // Publish the edited prototype only after its durable
            // representation is in place.
            dg_db_scripts::upsert_proto_trigger(edited);
            olc::clear_unresolved_publication(EditorKind::Trigedit, vnum);
            Ok(())
        }
        Err(error) => {
            if olc::replacement_was_published(&error) {
                // rename already exposed the candidate. Reconcile runtime
                // while returning the typed error so the editor remains open
                // for a durability-confirming retry.
                dg_db_scripts::upsert_proto_trigger(edited);
            }
            olc::mark_unresolved_save_failure(EditorKind::Trigedit, vnum, &error);
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------
// cleanup / mudlog
// ---------------------------------------------------------------------------

fn cleanup(g: &mut GameState, conn: ConnId) {
    if let Some(state) = crate::lock_ok::lock(&states()).remove(&conn) {
        olc::discard_unresolved_save(EditorKind::Trigedit, state.vnum);
    }
    olc::clear_active(conn);
    // C cleanup_olc returns to the playing prompt; the framework restores the
    // descriptor state. Echo a blank line so the player sees the prompt return.
    send(g, conn, "\r\n");
}

/// mudlog to immortals at builder level (CMP channel in C). We reuse the same
/// immortal-broadcast shape the rest of the port uses.
fn mudlog(g: &mut GameState, line: &str) {
    crate::syslog::mudlog(g, line, crate::syslog::CMP, LVL_IMMORT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::world::{Zone, zone_vnum_bounds};

    fn editor_game(conn: ConnId) -> (GameState, CharId, i32) {
        let mut g = GameState::new(Config::default());
        let zone_number = 40_401;
        let (vnum, top) = zone_vnum_bounds(zone_number).expect("valid test zone");
        g.zones.push(Zone {
            number: zone_number,
            name: "Overflow test zone".into(),
            builders: "Root".into(),
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
        });

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
        (g, ch, vnum)
    }

    fn narg_and_mode(conn: ConnId) -> (i32, Mode) {
        let map = crate::lock_ok::lock(&states());
        let state = map.get(&conn).expect("active trigedit state");
        (state.narg, state.mode)
    }

    #[test]
    fn trigedit_entry_accepts_i32_edges_and_rejects_adjacent_overflow() {
        let conn = ConnId(4_040_002);
        crate::olc::abort_editor(conn);
        let (mut g, ch, vnum) = editor_game(conn);

        do_trigedit(&mut g, ch, "2147483648", 0);
        assert_eq!(
            g.descriptors[&conn].outbuf,
            "That number is outside the supported range.\r\n"
        );
        assert!(!crate::olc::in_olc(conn));

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_trigedit(&mut g, ch, &vnum.to_string(), 0);
        assert_eq!(crate::olc::active_editor(conn), Some(EditorKind::Trigedit));

        for (input, expected) in [
            ("2147483647", Some(i32::MAX)),
            ("-2147483648", Some(i32::MIN)),
            ("2147483648", None),
            ("-2147483649", None),
        ] {
            set_mode(conn, Mode::MainMenu);
            trigedit_parse(&mut g, conn, "4");
            assert_eq!(get_mode(conn), Some(Mode::Narg));
            let before = narg_and_mode(conn).0;
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();

            trigedit_parse(&mut g, conn, input);
            let (narg, mode) = narg_and_mode(conn);
            match expected {
                Some(value) => {
                    assert_eq!(narg, value, "input={input:?}");
                    assert_eq!(mode, Mode::MainMenu, "input={input:?}");
                    assert!(
                        !g.descriptors[&conn]
                            .outbuf
                            .contains("outside the supported range"),
                        "input={input:?}"
                    );
                }
                None => {
                    assert_eq!(narg, before, "input={input:?}");
                    assert_eq!(mode, Mode::Narg, "input={input:?}");
                    assert_eq!(
                        g.descriptors[&conn].outbuf,
                        "That number is outside the supported range.\r\n",
                        "input={input:?}"
                    );
                }
            }
        }

        set_mode(conn, Mode::MainMenu);
        trigedit_parse(&mut g, conn, "q");
        trigedit_parse(&mut g, conn, "n");
        assert!(!crate::olc::in_olc(conn));
    }

    #[test]
    fn trigedit_entry_uses_authenticated_zone_acl_not_display_level() {
        let conn = ConnId(4_040_003);
        crate::olc::abort_editor(conn);
        let (mut g, ch, vnum) = editor_game(conn);
        {
            let intruder = g.get_char_mut(ch).unwrap();
            intruder.player.name = "Intruder".into();
            intruder.player.level = LVL_IMPL;
            intruder.trust = 1;
        }

        do_trigedit(&mut g, ch, &vnum.to_string(), 0);

        assert!(!crate::olc::in_olc(conn));
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("You do not have permission to edit that zone")
        );
    }

    #[test]
    fn trigedit_save_rechecks_principal_zone_ownership_before_publication() {
        let conn = ConnId(4_040_004);
        crate::olc::abort_editor(conn);
        let (mut g, ch, vnum) = editor_game(conn);
        g.get_char_mut(ch).unwrap().trust = i32::from(LVL_IMMORT);
        do_trigedit(&mut g, ch, &vnum.to_string(), 0);
        assert!(crate::olc::in_olc(conn));

        g.zones[0].builders = "SomebodyElse".into();
        let error = save_with(&mut g, conn, |_path, _bytes| {
            panic!("revoked zone ownership must be rejected before disk publication")
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(crate::olc::in_olc(conn));
        cleanup(&mut g, conn);
    }

    #[test]
    fn trigedit_publication_rechecks_every_retained_authority_component() {
        #[derive(Clone, Copy, Debug)]
        enum Revocation {
            Grant,
            Trust,
            Quarantine,
            ZoneOwnership,
            ZoneIdentity,
            DescriptorBody,
        }

        for (index, revocation) in [
            Revocation::Grant,
            Revocation::Trust,
            Revocation::Quarantine,
            Revocation::ZoneOwnership,
            Revocation::ZoneIdentity,
            Revocation::DescriptorBody,
        ]
        .into_iter()
        .enumerate()
        {
            let conn = ConnId(4_040_020 + index as u64);
            crate::olc::abort_editor(conn);
            let (mut g, ch, zone_start) = editor_game(conn);
            if matches!(revocation, Revocation::ZoneOwnership) {
                // Exact Implementors legitimately override zone.builders;
                // exercise ACL revocation with an ordinary listed builder.
                g.get_char_mut(ch).unwrap().trust = i32::from(LVL_IMMORT);
            }
            let vnum = zone_start + 20 + index as i32;
            do_trigedit(&mut g, ch, &vnum.to_string(), 0);
            assert!(crate::olc::in_olc(conn), "case={revocation:?}");

            match revocation {
                Revocation::Grant => g.get_char_mut(ch).unwrap().godcmds2 = 0,
                Revocation::Trust => g.get_char_mut(ch).unwrap().trust = 1,
                Revocation::Quarantine => {
                    let idnum = g.get_char(ch).unwrap().idnum;
                    g.authority_quarantine.insert(idnum);
                }
                Revocation::ZoneOwnership => g.zones[0].builders = "SomebodyElse".into(),
                Revocation::ZoneIdentity => g.zones[0].number -= 1,
                Revocation::DescriptorBody => {
                    let mut replacement =
                        Character::new_player("Replacement".into(), Class::Cleric, Race::Human);
                    replacement.desc = Some(conn);
                    let replacement = g.create_char(replacement);
                    g.descriptors.get_mut(&conn).unwrap().character = Some(replacement);
                }
            }

            let replacer_called = std::cell::Cell::new(false);
            let result = save_with(&mut g, conn, |_path, _bytes| {
                replacer_called.set(true);
                Ok(())
            });
            assert!(result.is_err(), "case={revocation:?}");
            let error = result.unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "case={revocation:?}"
            );
            assert!(!replacer_called.get(), "case={revocation:?}");
            cleanup(&mut g, conn);
        }
    }

    #[test]
    fn unpublished_save_failure_blocks_flush_until_retry_or_explicit_discard() {
        let _save_guard = crate::olc::test_save_list_guard();
        let conn = ConnId(4_040_098);
        crate::olc::abort_editor(conn);
        let (mut g, ch, zone_start) = editor_game(conn);
        let vnum = zone_start + 2;
        let lib = std::env::temp_dir().join(format!(
            "deltamud-trigedit-unpublished-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&lib);
        g.config.lib_path = lib.to_string_lossy().into_owned();

        do_trigedit(&mut g, ch, &vnum.to_string(), 0);
        let error = save_with(&mut g, conn, |_path, _bytes| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected pre-publication failure",
            ))
        })
        .unwrap_err();
        assert!(!crate::olc::replacement_was_published(&error));
        assert!(crate::olc::test_unresolved_publication(
            EditorKind::Trigedit,
            vnum
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());

        // Declining after an unpublished failure explicitly discards only
        // this scratch trigger, so no unresolved durable state remains.
        set_mode(conn, Mode::ConfirmSaveString);
        trigedit_parse(&mut g, conn, "n");
        assert!(!crate::olc::test_unresolved_publication(
            EditorKind::Trigedit,
            vnum
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_ok());

        // A later failed attempt remains a blocker until a successful retry.
        do_trigedit(&mut g, ch, &vnum.to_string(), 0);
        save_with(&mut g, conn, |_path, _bytes| {
            Err(std::io::Error::other("injected replacement failure"))
        })
        .unwrap_err();
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());
        save_with(&mut g, conn, crate::olc::atomic_replace).unwrap();
        assert!(!crate::olc::test_unresolved_publication(
            EditorKind::Trigedit,
            vnum
        ));
        cleanup(&mut g, conn);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn post_rename_sync_failure_reconciles_live_trigger_and_retains_editor() {
        let _save_guard = crate::olc::test_save_list_guard();
        let conn = ConnId(4_040_099);
        crate::olc::abort_editor(conn);
        let (mut g, ch, zone_start) = editor_game(conn);
        let vnum = zone_start + 1;
        let lib = std::env::temp_dir().join(format!(
            "deltamud-trigedit-published-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&lib);
        g.config.lib_path = lib.to_string_lossy().into_owned();

        do_trigedit(&mut g, ch, &vnum.to_string(), 0);
        assert_eq!(crate::dg_db_scripts::real_trigger(vnum), -1);

        let error = save_with(&mut g, conn, |path, bytes| {
            crate::olc::atomic_replace_with_hooks(
                path,
                bytes,
                |_| Ok(()),
                |_| Err(std::io::Error::other("injected directory sync failure")),
            )
        })
        .unwrap_err();

        assert!(crate::olc::replacement_was_published(&error));
        assert!(crate::dg_db_scripts::real_trigger(vnum) >= 0);
        assert!(crate::olc::in_olc(conn));
        assert!(crate::olc::test_unresolved_publication(
            EditorKind::Trigedit,
            vnum
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());
        assert!(
            lib.join(format!("world/trg/{}.trg", g.zones[0].number))
                .exists()
        );

        // Discarding cannot clear a marker after rename: the scratch editor
        // closes, but durability still needs a same-entry retry.
        set_mode(conn, Mode::ConfirmSaveString);
        trigedit_parse(&mut g, conn, "n");
        assert!(!crate::olc::in_olc(conn));
        assert!(crate::olc::test_unresolved_publication(
            EditorKind::Trigedit,
            vnum
        ));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());

        do_trigedit(&mut g, ch, &vnum.to_string(), 0);
        save_with(&mut g, conn, crate::olc::atomic_replace).unwrap();
        assert!(!crate::olc::test_unresolved_publication(
            EditorKind::Trigedit,
            vnum
        ));
        cleanup(&mut g, conn);
        let _ = std::fs::remove_dir_all(lib);
    }
}
