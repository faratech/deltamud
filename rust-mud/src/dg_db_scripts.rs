// dg_db_scripts.rs — load .trg prototypes and attach them to entities
// (port of dg_db_scripts.c: parse_trigger, read_trigger, real_trigger,
// dg_read_trigger, dg_obj_trigger, assign_triggers).
//
// The attach_trigger_to_{mob,obj,room} / parse_trigger_line entry points are
// the contract the world loader calls when it parses `T <vnum>` lines. The
// loader wires all three kinds: mob `T` (file_loader.rs, kind 0), room `T`
// (kind 2), and object `T` (kind 1); assign_triggers() then materialises the
// prototype triggers onto live instances.
#![allow(dead_code)]
//
// Trigger prototypes live in lib/world/trg/<n>.trg, listed in an `index`
// file. Each .trg file holds one or more `#vnum`-headed blocks:
//
//     #2001
//     Kobold Guard speech~          <- name (fread_string, '~'-terminated)
//     0 g 25                        <- attach_type  flags  narg
//     ~                             <- arglist (fread_string)
//     <command list lines...>~      <- cmdlist (fread_string, lines split)
//     #...                          <- next trigger, or `$~` ends the file
//
// We store the parsed prototypes in TRIG_INDEX (the trig_index array). The T
// lines inside .mob/.wld and the obj `T <vnum>` lines name which triggers
// attach to which prototype entity; because Character/Object/Room may not gain
// fields, the proto->trigger bindings are held here in PROTO_SCRIPTS, keyed by
// entity vnum + kind. assign_triggers() instantiates them onto a live entity.

use crate::dg_handler::{
    self, add_trigger, install_trig, ScriptKey, TrigData, MOB_TRIGGER, OBJ_TRIGGER, WLD_TRIGGER,
};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// A trigger prototype (trig_index[]->proto + index_data).
#[derive(Debug, Clone)]
pub struct TrigProto {
    pub vnum: i32,
    pub attach_type: i32,
    pub name: String,
    pub trigger_type: i64,
    pub narg: i32,
    pub arglist: String,
    pub cmdlist: Vec<String>,
}

// trig_index: rnum-ordered prototypes + a vnum->rnum map.
static TRIG_INDEX: OnceLock<Mutex<Vec<TrigProto>>> = OnceLock::new();
static TRIG_VNUM_MAP: OnceLock<Mutex<HashMap<i32, usize>>> = OnceLock::new();

// proto_script lists: which trigger vnums attach to which prototype entity.
// Keyed by (kind, vnum) where kind is MOB_TRIGGER/OBJ_TRIGGER/WLD_TRIGGER.
static PROTO_SCRIPTS: OnceLock<Mutex<HashMap<(i32, i32), Vec<i32>>>> = OnceLock::new();

fn index() -> &'static Mutex<Vec<TrigProto>> {
    TRIG_INDEX.get_or_init(|| Mutex::new(Vec::new()))
}
fn vnum_map() -> &'static Mutex<HashMap<i32, usize>> {
    TRIG_VNUM_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}
fn proto_scripts() -> &'static Mutex<HashMap<(i32, i32), Vec<i32>>> {
    PROTO_SCRIPTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// asciiflag_conv (db.c): letters a..z => bits 0..25, A..Z => bits 26..51;
/// if the whole token is digits, it's parsed as a decimal number instead.
pub fn asciiflag_conv(flag: &str) -> i64 {
    let mut flags: i64 = 0;
    let mut is_number = true;
    for c in flag.chars() {
        if c.is_ascii_lowercase() {
            flags |= 1 << (c as i64 - 'a' as i64);
        } else if c.is_ascii_uppercase() {
            flags |= 1 << (26 + (c as i64 - 'A' as i64));
        }
        if !c.is_ascii_digit() {
            is_number = false;
        }
    }
    if is_number {
        flags = flag.trim().parse().unwrap_or(0);
    }
    flags
}

/// real_trigger(vnum): vnum -> rnum, or -1.
pub fn real_trigger(vnum: i32) -> i32 {
    vnum_map().lock().unwrap().get(&vnum).map(|&r| r as i32).unwrap_or(-1)
}

pub fn top_of_trigt() -> usize {
    index().lock().unwrap().len()
}

pub fn trig_proto(rnum: usize) -> Option<TrigProto> {
    index().lock().unwrap().get(rnum).cloned()
}

/// read_trigger(rnum): instantiate a live TrigData from a prototype, returning
/// its TrigId (trig_data_copy + install). Returns None if rnum is invalid.
pub fn read_trigger(rnum: usize) -> Option<dg_handler::TrigId> {
    let proto = index().lock().unwrap().get(rnum).cloned()?;
    let t = TrigData {
        nr: rnum,
        vnum: proto.vnum,
        attach_type: proto.attach_type,
        name: proto.name,
        trigger_type: proto.trigger_type,
        narg: proto.narg,
        arglist: proto.arglist,
        cmdlist: proto.cmdlist,
        curr_line: 0,
        depth: 0,
        loops: 0,
        wait_event: None,
        var_list: Vec::new(),
        purged: false,
        loop_origin: HashMap::new(),
    };
    Some(install_trig(t))
}

/// boot_triggers(lib_path): load every prototype from lib/world/trg. Reads the
/// `index` file for the list of .trg files (falling back to a directory scan),
/// then parses each `#vnum` block. Clears any previous index first.
pub fn boot_triggers(lib_path: &str) {
    index().lock().unwrap().clear();
    vnum_map().lock().unwrap().clear();
    proto_scripts().lock().unwrap().clear();

    let dir = Path::new(lib_path).join("world").join("trg");
    let files = read_index(&dir);

    for fname in files {
        let path = dir.join(&fname);
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        parse_trg_file(&data);
    }
}

/// Read the trg `index` file; each line names a .trg file, terminated by `$`.
/// If absent, scan the directory for *.trg.
fn read_index(dir: &Path) -> Vec<String> {
    let index_path = dir.join("index");
    if let Ok(contents) = std::fs::read_to_string(&index_path) {
        let mut out = Vec::new();
        for line in contents.lines() {
            let t = line.trim();
            if t == "$" || t.is_empty() {
                break;
            }
            out.push(t.to_string());
        }
        if !out.is_empty() {
            return out;
        }
    }
    // Fallback: directory scan.
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".trg") {
                out.push(name);
            }
        }
    }
    out
}

/// Parse a whole .trg file (which may contain many `#vnum` blocks). Mirrors the
/// C index_boot loop: read `#vnum`, call parse_trigger, repeat until `$`.
fn parse_trg_file(data: &str) {
    let lines: Vec<&str> = data.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with('$') {
            break;
        }
        if let Some(rest) = t.strip_prefix('#') {
            let vnum: i32 = rest.trim().parse().unwrap_or(-1);
            i += 1;
            if vnum >= 0 {
                parse_trigger(vnum, &lines, &mut i);
            } else {
                // malformed header; skip line
            }
        } else {
            i += 1;
        }
    }
}

/// parse_trigger: read one trigger block starting at lines[*i] (the line after
/// the `#vnum` header). Installs it into trig_index.
fn parse_trigger(vnum: i32, lines: &[&str], i: &mut usize) {
    let name = read_tilde_string(lines, i);

    // flag line: "attach_type flags narg"
    let flag_line = next_nonblank(lines, i).unwrap_or_default();
    let parts: Vec<&str> = flag_line.split_whitespace().collect();
    let attach_type: i32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let flags = parts.get(1).copied().unwrap_or("0");
    let trigger_type = asciiflag_conv(flags);
    // narg is present only when the line has 3 fields (C: k == 3).
    let narg: i32 = if parts.len() >= 3 {
        parts[2].parse().unwrap_or(0)
    } else {
        0
    };

    let arglist = read_tilde_string(lines, i);
    let cmd_block = read_tilde_string(lines, i);

    // Split the command block into lines (cmdlist_element chain). C strtok on
    // "\n\r" drops empty lines; we keep the same behaviour.
    let cmdlist: Vec<String> = cmd_block
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    let proto = TrigProto {
        vnum,
        attach_type,
        name,
        trigger_type,
        narg,
        arglist,
        cmdlist,
    };

    let mut idx = index().lock().unwrap();
    let rnum = idx.len();
    idx.push(proto);
    drop(idx);
    vnum_map().lock().unwrap().insert(vnum, rnum);
}

/// Read a `~`-terminated string block (fread_string). Accepts an inline `~`
/// (same line) or a lone `~` later; joins interior lines with `\n`.
fn read_tilde_string(lines: &[&str], i: &mut usize) -> String {
    let mut out = String::new();
    let mut first = true;
    while *i < lines.len() {
        let raw = lines[*i];
        *i += 1;
        if let Some(pos) = raw.find('~') {
            if !first {
                out.push('\n');
            }
            out.push_str(&raw[..pos]);
            return out;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(raw);
        first = false;
    }
    out
}

/// Next non-blank line, advancing the cursor.
fn next_nonblank<'a>(lines: &'a [&'a str], i: &mut usize) -> Option<&'a str> {
    while *i < lines.len() {
        let l = lines[*i];
        *i += 1;
        if !l.trim().is_empty() {
            return Some(l);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// proto_script bindings: which trigger vnums attach to which prototype entity.
// The file_loader handles the `T` lines; it can call these to record bindings,
// then assign_triggers materialises them when an instance is created.
// ---------------------------------------------------------------------------

/// Record "trigger `vnum` attaches to mob proto `mob_vnum`" (dg_read_trigger,
/// MOB_TRIGGER). Logs (returns false) if the trigger vnum doesn't exist.
pub fn attach_trigger_to_mob(mob_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(MOB_TRIGGER, mob_vnum, trig_vnum)
}
/// Record a trigger binding for an object prototype (dg_obj_trigger).
pub fn attach_trigger_to_obj(obj_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(OBJ_TRIGGER, obj_vnum, trig_vnum)
}
/// Record a trigger binding for a room prototype (dg_read_trigger WLD_TRIGGER).
pub fn attach_trigger_to_room(room_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(WLD_TRIGGER, room_vnum, trig_vnum)
}

fn record_proto(kind: i32, entity_vnum: i32, trig_vnum: i32) -> bool {
    if real_trigger(trig_vnum) < 0 {
        crate::dg_scripts::script_log(&format!(
            "Trigger vnum #{} asked for but non-existant!",
            trig_vnum
        ));
        return false;
    }
    proto_scripts()
        .lock()
        .unwrap()
        .entry((kind, entity_vnum))
        .or_default()
        .push(trig_vnum);
    true
}

/// Convenience for the file_loader: parse a raw `T <vnum>` line (mob/wld) or an
/// obj `T <vnum>` line and record the binding. Returns true on success.
pub fn parse_trigger_line(kind: i32, entity_vnum: i32, line: &str) -> bool {
    // line is like "T 2001" — take the last whitespace token as the vnum.
    let vnum: Option<i32> = line.split_whitespace().last().and_then(|s| s.parse().ok());
    match vnum {
        Some(v) => record_proto(kind, entity_vnum, v),
        None => {
            crate::dg_scripts::script_log("Error assigning trigger!");
            false
        }
    }
}

/// assign_triggers(entity, type): instantiate every recorded prototype trigger
/// onto a freshly-loaded live entity. Called from load_mobile/load_object and
/// at room boot. `entity_vnum` is the mob/obj/room vnum; `key` is the live id.
pub fn assign_triggers(key: ScriptKey, entity_vnum: i32) {
    let kind = key.trig_type();
    let trig_vnums = proto_scripts()
        .lock()
        .unwrap()
        .get(&(kind, entity_vnum))
        .cloned()
        .unwrap_or_default();

    for tv in trig_vnums {
        let rnum = real_trigger(tv);
        if rnum < 0 {
            crate::dg_scripts::script_log(&format!(
                "trigger #{} non-existant, for entity #{}",
                tv, entity_vnum
            ));
            continue;
        }
        if let Some(tid) = read_trigger(rnum as usize) {
            add_trigger(key, tid, -1);
        }
    }
}

/// Attach all room prototype triggers to every loaded room (called once at
/// boot since rooms are not created per-instance). Iterates the live rooms and
/// assigns whatever proto bindings exist for their vnum.
pub fn assign_room_triggers(g: &mut GameState) {
    let rooms: Vec<(RoomRnum, RoomVnum)> =
        g.rooms.iter().enumerate().map(|(rn, r)| (rn, r.number)).collect();
    for (rnum, vnum) in rooms {
        // Only assign if this room has proto bindings, to avoid creating empty
        // script containers (matches C only CREATE()ing on first trigger).
        let has = proto_scripts()
            .lock()
            .unwrap()
            .contains_key(&(WLD_TRIGGER, vnum));
        if has {
            assign_triggers(ScriptKey::Room(rnum), vnum);
        }
    }
}

// ---------------------------------------------------------------------------
// Conformance self-check against the shipped lib/world/trg/*.trg set.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod conformance {
    use super::*;

    fn trg_dir() -> String {
        // Resolve lib relative to the crate (rust-mud/.. = deltamud).
        let candidates = [
            "/web/deltamud/lib",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../lib"),
        ];
        for c in candidates {
            if std::path::Path::new(c).join("world/trg/index").exists() {
                return c.to_string();
            }
        }
        candidates[0].to_string()
    }

    #[test]
    fn boots_all_trg_prototypes() {
        let _lock = crate::dg_handler::DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        boot_triggers(&trg_dir());
        // 38 distinct trigger prototypes ship in lib/world/trg.
        assert_eq!(top_of_trigt(), 38, "expected 38 trigger prototypes");

        // Spot-check known prototypes (flags via asciiflag_conv).
        // #1400 "2 g 100": wld (2), flag g => bit6 (WTRIG_ENTER), narg 100.
        let rn = real_trigger(1400);
        assert!(rn >= 0);
        let p = trig_proto(rn as usize).unwrap();
        assert_eq!(p.attach_type, WLD_TRIGGER);
        assert_eq!(p.trigger_type, 1 << 6);
        assert_eq!(p.narg, 100);
        assert!(p.cmdlist.iter().any(|l| l.contains("wteleport")));

        // #2001 "0 g 25": mob (0), flag g => bit6 (MTRIG_GREET), narg 25.
        let rn = real_trigger(2001);
        let p = trig_proto(rn as usize).unwrap();
        assert_eq!(p.attach_type, MOB_TRIGGER);
        assert_eq!(p.trigger_type, 1 << 6);
        assert_eq!(p.narg, 25);

        // #2010 "0 bg 50": mob, flags b+g => bits 1|6, narg 50.
        let rn = real_trigger(2010);
        let p = trig_proto(rn as usize).unwrap();
        assert_eq!(p.trigger_type, (1 << 1) | (1 << 6));

        // #2048 "1 c 3": obj (1), flag c => bit2 (OTRIG_COMMAND), narg 3,
        // arglist "read".
        let rn = real_trigger(2048);
        let p = trig_proto(rn as usize).unwrap();
        assert_eq!(p.attach_type, OBJ_TRIGGER);
        assert_eq!(p.trigger_type, 1 << 2);
        assert_eq!(p.arglist, "read");
    }

    #[test]
    fn asciiflag_conv_letters_and_numbers() {
        assert_eq!(asciiflag_conv("a"), 1 << 0);
        assert_eq!(asciiflag_conv("g"), 1 << 6);
        assert_eq!(asciiflag_conv("bg"), (1 << 1) | (1 << 6));
        assert_eq!(asciiflag_conv("A"), 1 << 26);
        assert_eq!(asciiflag_conv("100"), 100); // all-digit => decimal
    }
}
