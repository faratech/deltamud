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
    self, MOB_TRIGGER, OBJ_TRIGGER, ScriptKey, TrigData, WLD_TRIGGER, add_trigger, install_trig,
};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::path::Path;

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

// trig_index (rnum-ordered prototypes), the vnum->rnum map, and the
// proto_script lists (which trigger vnums attach to which prototype entity,
// keyed by (kind, vnum) where kind is MOB_TRIGGER/OBJ_TRIGGER/WLD_TRIGGER).
// As of the phase-1 statics migration these live on GameState as `dg`.

/// asciiflag_conv (db.c): letters a..z => bits 0..25, A..Z => bits 26..51;
/// if the whole token is digits, it's parsed as a decimal number instead.
pub fn asciiflag_conv(flag: &str) -> i64 {
    match asciiflag_conv_checked(flag) {
        Ok(flags) => flags,
        Err(error) => {
            log::warn!(
                "SYSERR: DG numeric flag {flag:?} is invalid: {error:?}; clamped to i64::MAX"
            );
            i64::MAX
        }
    }
}

fn asciiflag_conv_checked(flag: &str) -> Result<i64, crate::text::ParseIntError> {
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
    if is_number && !flag.trim().is_empty() {
        flags = crate::text::parse_i64_strict(flag)?;
    }
    Ok(flags)
}

/// real_trigger(g, vnum): vnum -> rnum, or -1.
pub fn real_trigger(g: &GameState, vnum: i32) -> i32 {
    g.dg.trig_rnum_map
        .get(&vnum)
        .map(|&r| r as i32)
        .unwrap_or(-1)
}

pub fn top_of_trigt(g: &GameState) -> usize {
    g.dg.proto_trigs.len()
}

pub fn trig_proto(g: &GameState, rnum: usize) -> Option<TrigProto> {
    g.dg.proto_trigs.get(rnum).cloned()
}

#[cfg(test)]
pub fn set_test_proto_trigger(g: &mut GameState, kind: i32, entity_vnum: i32, proto: TrigProto) {
    let rnum = g.dg.proto_trigs.len();
    g.dg.trig_rnum_map.insert(proto.vnum, rnum);
    g.dg.proto_scripts
        .entry((kind, entity_vnum))
        .or_default()
        .push(proto.vnum);
    g.dg.proto_trigs.push(proto);
}

/// C dg_olc.c:424-470 trigedit_save: install the edited prototype IN PLACE -
/// replace the existing table entry (or append a new one) without touching
/// any other prototype and without clearing proto_scripts, so live trigger
/// attachments and world files survive a trigedit save (#260).
pub fn upsert_proto_trigger(g: &mut GameState, proto: TrigProto) {
    if let Some(existing) = g.dg.proto_trigs.iter().position(|t| t.vnum == proto.vnum) {
        g.dg.proto_trigs[existing] = proto;
    } else {
        let rnum = g.dg.proto_trigs.len();
        g.dg.trig_rnum_map.insert(proto.vnum, rnum);
        g.dg.proto_trigs.push(proto);
    }
}

/// read_trigger(g, rnum): instantiate a live TrigData from a prototype, returning
/// its TrigId (trig_data_copy + install). Returns None if rnum is invalid.
pub fn read_trigger(g: &GameState, rnum: usize) -> Option<dg_handler::TrigId> {
    let proto = g.dg.proto_trigs.get(rnum).cloned()?;
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
pub fn boot_triggers(g: &mut GameState, lib_path: &str) {
    g.dg.proto_trigs.clear();
    g.dg.trig_rnum_map.clear();
    g.dg.proto_scripts.clear();

    let pending_new_zones = match crate::olc::pending_new_zone_publications(lib_path) {
        Ok(pending) => pending,
        Err(error) => {
            log::error!(
                "Refusing to load triggers because new-zone transaction state is unreadable: {error}"
            );
            return;
        }
    };

    let dir = Path::new(lib_path).join("world").join("trg");
    let files = read_index(&dir);

    for fname in files {
        if crate::olc::new_zone_index_entry_is_pending(&pending_new_zones, &fname) {
            log::warn!("Skipping incomplete new-zone trigger file {fname:?} during boot");
            continue;
        }
        let path = dir.join(&fname);
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        parse_trg_file(g, &data);
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
fn parse_trg_file(g: &mut GameState, data: &str) {
    let lines: Vec<&str> = data.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with('$') {
            break;
        }
        if let Some(rest) = t.strip_prefix('#') {
            i += 1;
            match crate::text::parse_i32_strict(rest) {
                Ok(vnum) if vnum >= 0 => parse_trigger(g, vnum, &lines, &mut i),
                Ok(_) | Err(_) => {
                    log::warn!("SYSERR: rejected invalid DG trigger header {t:?}");
                }
            }
        } else {
            i += 1;
        }
    }
}

/// parse_trigger: read one trigger block starting at lines[*i] (the line after
/// the `#vnum` header). Installs it into trig_index.
fn parse_trigger(g: &mut GameState, vnum: i32, lines: &[&str], i: &mut usize) {
    let name = read_tilde_string(lines, i);

    // flag line: "attach_type flags narg"
    let flag_line = next_nonblank(lines, i).unwrap_or_default();
    let parts: Vec<&str> = flag_line.split_whitespace().collect();
    let attach_type = match parts.first() {
        Some(raw) => match crate::text::parse_i32_strict(raw) {
            Ok(value) => Some(value),
            Err(error) => {
                log::warn!(
                    "SYSERR: trigger #{vnum} attach type {raw:?} is invalid: {error:?}; record rejected"
                );
                None
            }
        },
        None => Some(0),
    };
    let flags = parts.get(1).copied().unwrap_or("0");
    let trigger_type = match asciiflag_conv_checked(flags) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warn!(
                "SYSERR: trigger #{vnum} flags {flags:?} are invalid: {error:?}; record rejected"
            );
            None
        }
    };
    // narg is present only when the line has 3 fields (C: k == 3).
    let narg = if parts.len() >= 3 {
        match crate::text::parse_i32_strict(parts[2]) {
            Ok(value) => Some(value),
            Err(error) => {
                log::warn!(
                    "SYSERR: trigger #{vnum} numeric argument {:?} is invalid: {error:?}; record rejected",
                    parts[2]
                );
                None
            }
        }
    } else {
        Some(0)
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

    let (Some(attach_type), Some(trigger_type), Some(narg)) = (attach_type, trigger_type, narg)
    else {
        return;
    };

    let proto = TrigProto {
        vnum,
        attach_type,
        name,
        trigger_type,
        narg,
        arglist,
        cmdlist,
    };

    let rnum = g.dg.proto_trigs.len();
    g.dg.proto_trigs.push(proto);
    g.dg.trig_rnum_map.insert(vnum, rnum);
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
pub fn attach_trigger_to_mob(g: &mut GameState, mob_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(g, MOB_TRIGGER, mob_vnum, trig_vnum)
}
/// Record a trigger binding for an object prototype (dg_obj_trigger).
pub fn attach_trigger_to_obj(g: &mut GameState, obj_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(g, OBJ_TRIGGER, obj_vnum, trig_vnum)
}
/// Record a trigger binding for a room prototype (dg_read_trigger WLD_TRIGGER).
pub fn attach_trigger_to_room(g: &mut GameState, room_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(g, WLD_TRIGGER, room_vnum, trig_vnum)
}

fn record_proto(g: &mut GameState, kind: i32, entity_vnum: i32, trig_vnum: i32) -> bool {
    if real_trigger(g, trig_vnum) < 0 {
        crate::dg_scripts::script_log(&format!(
            "Trigger vnum #{} asked for but non-existant!",
            trig_vnum
        ));
        return false;
    }
    // C dg_db_scripts.c:231-242/295-305 appends unconditionally: listing a
    // trigger twice attaches and fires it twice. The dedupe silently masked
    // duplicates instead of reproducing C (#159).
    g.dg.proto_scripts
        .entry((kind, entity_vnum))
        .or_default()
        .push(trig_vnum);
    true
}

/// Persistently add a prototype trigger binding. Unlike `add_trigger`, this
/// updates the proto-script table that OLC save paths write as `T <vnum>`.
pub fn add_proto_trigger(g: &mut GameState, kind: i32, entity_vnum: i32, trig_vnum: i32) -> bool {
    record_proto(g, kind, entity_vnum, trig_vnum)
}

/// Insert a prototype trigger binding at a 1-based script-editor position.
/// Out-of-range positions append, matching the C editor's "999" append path.
pub fn insert_proto_trigger(
    g: &mut GameState,
    kind: i32,
    entity_vnum: i32,
    trig_vnum: i32,
    pos: usize,
) -> bool {
    if real_trigger(g, trig_vnum) < 0 {
        crate::dg_scripts::script_log(&format!(
            "trigger #{} non-existant, for entity #{}",
            trig_vnum, entity_vnum
        ));
        return false;
    }
    let list = g.dg.proto_scripts.entry((kind, entity_vnum)).or_default();
    if list.contains(&trig_vnum) {
        return true;
    }
    let idx = pos.saturating_sub(1).min(list.len());
    list.insert(idx, trig_vnum);
    true
}

/// Remove one named/numbered prototype trigger binding.
pub fn remove_proto_trigger(g: &mut GameState, kind: i32, entity_vnum: i32, name: &str) -> bool {
    // Resolve the removal set immutably first (the name path reads the proto
    // table), then mutate the binding list — the borrows never overlap.
    let remove: Vec<i32> = if let Ok(vnum) = name.parse::<i32>() {
        vec![vnum]
    } else {
        let bound =
            g.dg.proto_scripts
                .get(&(kind, entity_vnum))
                .cloned()
                .unwrap_or_default();
        bound
            .into_iter()
            .filter(|&v| {
                let rn = real_trigger(g, v);
                if rn < 0 {
                    return false;
                }
                trig_proto(g, rn as usize)
                    .map(|p| p.name.eq_ignore_ascii_case(name))
                    .unwrap_or(false)
            })
            .collect()
    };
    if remove.is_empty() {
        return false;
    }
    let Some(list) = g.dg.proto_scripts.get_mut(&(kind, entity_vnum)) else {
        return false;
    };
    let before = list.len();
    list.retain(|v| !remove.contains(v));
    let changed = list.len() != before;
    if list.is_empty() {
        g.dg.proto_scripts.remove(&(kind, entity_vnum));
    }
    changed
}

/// Remove a prototype trigger binding by the C script editor's 1-based list
/// position.
pub fn remove_proto_trigger_at(g: &mut GameState, kind: i32, entity_vnum: i32, pos: usize) -> bool {
    if pos == 0 {
        return false;
    }
    let Some(list) = g.dg.proto_scripts.get_mut(&(kind, entity_vnum)) else {
        return false;
    };
    let idx = pos - 1;
    if idx >= list.len() {
        return false;
    }
    list.remove(idx);
    if list.is_empty() {
        g.dg.proto_scripts.remove(&(kind, entity_vnum));
    }
    true
}

/// Remove all prototype trigger bindings for an entity.
pub fn clear_proto_triggers(g: &mut GameState, kind: i32, entity_vnum: i32) -> bool {
    g.dg.proto_scripts.remove(&(kind, entity_vnum)).is_some()
}

/// Convenience for the file_loader: parse a raw `T <vnum>` line (mob/wld) or an
/// obj `T <vnum>` line and record the binding. Returns true on success.
pub fn parse_trigger_line(g: &mut GameState, kind: i32, entity_vnum: i32, line: &str) -> bool {
    // line is like "T 2001" — take the last whitespace token as the vnum.
    let vnum: Option<i32> = line.split_whitespace().last().and_then(|s| s.parse().ok());
    match vnum {
        Some(v) => record_proto(g, kind, entity_vnum, v),
        None => {
            crate::dg_scripts::script_log("Error assigning trigger!");
            false
        }
    }
}

/// Read back the trigger vnums bound to a mob/obj/room prototype, in load order.
/// Used by the OLC save paths (medit/oedit) to re-emit the `T <vnum>` lines —
/// the analogue of C's `script_save_to_disk(fp, ent, *_TRIGGER)` walking
/// ent->proto_script. `kind` is MOB_TRIGGER(0) / OBJ_TRIGGER(1) / WLD_TRIGGER(2).
pub fn proto_trigger_vnums(g: &GameState, kind: i32, entity_vnum: i32) -> Vec<i32> {
    g.dg.proto_scripts
        .get(&(kind, entity_vnum))
        .cloned()
        .unwrap_or_default()
}

/// assign_triggers(entity, type): instantiate every recorded prototype trigger
/// onto a freshly-loaded live entity. Called from load_mobile/load_object and
/// at room boot. `entity_vnum` is the mob/obj/room vnum; `key` is the live id.
pub fn assign_triggers(g: &mut GameState, key: ScriptKey, entity_vnum: i32) {
    let kind = key.trig_type();
    let trig_vnums =
        g.dg.proto_scripts
            .get(&(kind, entity_vnum))
            .cloned()
            .unwrap_or_default();

    for tv in trig_vnums {
        let rnum = real_trigger(g, tv);
        if rnum < 0 {
            crate::dg_scripts::script_log(&format!(
                "trigger #{} non-existant, for entity #{}",
                tv, entity_vnum
            ));
            continue;
        }
        if let Some(tid) = read_trigger(g, rnum as usize) {
            add_trigger(key, tid, -1);
        }
    }
}

/// Attach all room prototype triggers to every loaded room (called once at
/// boot since rooms are not created per-instance). Iterates the live rooms and
/// assigns whatever proto bindings exist for their vnum.
pub fn assign_room_triggers(g: &mut GameState) {
    let rooms: Vec<(RoomRnum, RoomVnum)> = g
        .rooms
        .iter()
        .enumerate()
        .map(|(rn, r)| (rn, r.number))
        .collect();
    for (rnum, vnum) in rooms {
        // Only assign if this room has proto bindings, to avoid creating empty
        // script containers (matches C only CREATE()ing on first trigger).
        let has = g.dg.proto_scripts.contains_key(&(WLD_TRIGGER, vnum));
        if has {
            assign_triggers(g, ScriptKey::Room(rnum), vnum);
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
        let _lock = crate::dg_handler::DG_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut g = crate::state::GameState::new(crate::config::Config::default());
        boot_triggers(&mut g, &trg_dir());
        // 38 distinct trigger prototypes ship in lib/world/trg.
        assert_eq!(top_of_trigt(&g), 38, "expected 38 trigger prototypes");

        // Spot-check known prototypes (flags via asciiflag_conv).
        // #1400 "2 g 100": wld (2), flag g => bit6 (WTRIG_ENTER), narg 100.
        let rn = real_trigger(&g, 1400);
        assert!(rn >= 0);
        let p = trig_proto(&g, rn as usize).unwrap();
        assert_eq!(p.attach_type, WLD_TRIGGER);
        assert_eq!(p.trigger_type, 1 << 6);
        assert_eq!(p.narg, 100);
        assert!(p.cmdlist.iter().any(|l| l.contains("wteleport")));

        // #2001 "0 g 25": mob (0), flag g => bit6 (MTRIG_GREET), narg 25.
        let rn = real_trigger(&g, 2001);
        let p = trig_proto(&g, rn as usize).unwrap();
        assert_eq!(p.attach_type, MOB_TRIGGER);
        assert_eq!(p.trigger_type, 1 << 6);
        assert_eq!(p.narg, 25);

        // #2010 "0 bg 50": mob, flags b+g => bits 1|6, narg 50.
        let rn = real_trigger(&g, 2010);
        let p = trig_proto(&g, rn as usize).unwrap();
        assert_eq!(p.trigger_type, (1 << 1) | (1 << 6));

        // #2048 "1 c 3": obj (1), flag c => bit2 (OTRIG_COMMAND), narg 3,
        // arglist "read".
        let rn = real_trigger(&g, 2048);
        let p = trig_proto(&g, rn as usize).unwrap();
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

    #[test]
    fn trigger_loader_rejects_overflowing_records_without_shifting_fields() {
        let _lock = crate::dg_handler::DG_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut g = crate::state::GameState::new(crate::config::Config::default());
        parse_trg_file(
            &mut g,
            "#1\nBad attach~\n2147483648 a 1\narg~\ncmd~\n\
#2\nBad flags~\n0 9223372036854775808 1\narg~\ncmd~\n\
#3\nBad narg~\n0 a 2147483648\narg~\ncmd~\n\
#4\nGood~\n0 a 7\narg~\ncmd~\n$~\n",
        );

        assert_eq!(top_of_trigt(&g), 1);
        let good = trig_proto(&g, 0).unwrap();
        assert_eq!(good.vnum, 4);
        assert_eq!(good.attach_type, MOB_TRIGGER);
        assert_eq!(good.trigger_type, 1);
        assert_eq!(good.narg, 7);
    }
}
