// house.rs — player housing system (CircleMUD/DeltaMUD house.c), ported 1:1 to
// the id-indexed GameState. Covers:
//
//   * do_hcontrol  — immortal house administration (build / crashsave / destroy
//                    / pay / update / show / guests)
//   * do_house     — house-owner guest control (add/remove/list guests)
//   * do_bed       — quit-the-game-in-your-house (rent-save + extract)
//   * House_boot   — load control records + objects, set ROOM_HOUSE/ATRIUM bits
//   * House_crashsave / House_save_all — persist a house's contents to disk
//   * House_can_enter — access gate consulted by movement
//
// PERSISTENCE / ON-DISK FORMAT
// ----------------------------
// The C stores `struct house_control_rec[num_of_houses]` as a raw fwrite() blob
// in lib/etc/hcontrol, and each house's objects as raw `obj_file_elem` records
// in lib/house/<vnum>.house. Neither binary layout is portable (time_t width,
// struct padding, and crucially the Rust port has no Obj_to_store/Obj_from_store
// object-store layer yet), so this module uses a documented LINE-ORIENTED TEXT
// format for both files. See the `serialize_*` / `parse_*` helpers below for the
// exact grammar. The control file additionally caches the owner/guest *names*
// alongside their idnums so `hcontrol show`/`house` can render names for offline
// players. The Rust port now carries the in-memory player_table index (C
// build_player_index) on GameState, so id<->name resolution works for offline
// players too; the control-file name cache remains as a final fallback.
//
// GRACEFUL DEGRADATION (matches C where the dep is missing):
//   * id<->name lookups (get_id_by_name / get_name_by_id) resolve against online
//     players (players_by_name + idnum), then the boot-loaded GameState
//     player_table index, then the on-disk name cache; an unknown id renders as
//     "<UNDEF>" exactly like C's NAME() macro.
//   * do_bed's write_aliases() (no alias system ported) is a no-op; the
//     rent-save is performed by serializing carried/worn objects is NOT done
//     here (do_bed is a quit path — the async loop owns player-file save exactly
//     as the do_quit port does), so do_bed mirrors really_quit: announce, clear
//     LOCKOUT, request the descriptor close (which drives save+extract).

use crate::act::{act, ActArg, To};
use crate::interpreter::{half_chop, is_abbrev, one_argument, search_block};
use crate::object::{ExtraFlags, Object, ObjectAffect, ObjectType, WearFlags};
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Constants (house.h / structs.h / db.h)
// ---------------------------------------------------------------------------

const MAX_HOUSES: usize = 100;
const MAX_GUESTS: usize = 100;

const HOUSE_PRIVATE: i32 = 0;
const HOUSE_OPEN: i32 = 1;

// Room flags whose bit values differ from the *named* Rust RoomFlags variants
// (room.rs names 1<<12 NO_RECALL and 1<<13 NO_SUMMON; C uses HOUSE_CRASH/ATRIUM).
// We manipulate the raw bitfield with from_bits_retain so the on-disk/world bit
// layout stays C-accurate.
const ROOM_HOUSE: u32 = 1 << 11;
const ROOM_PRIVATE: u32 = 1 << 9;
const ROOM_HOUSE_CRASH: u32 = 1 << 12;
const ROOM_ATRIUM: u32 = 1 << 13;

// PRF2_LOCKOUT (structs.h) — cleared by do_bed.
const PRF2_LOCKOUT: i64 = 1 << 1;

// ITEM_NORENT (structs.h ExtraFlags bit) — skipped on house load.
const ITEM_NORENT: u64 = 1 << 2;

const NRM_INVIS: u8 = LVL_IMMORT; // mudlog visibility floor.

const HCONTROL_FORMAT: &str =
    "Usage: hcontrol build <house vnum> <exit direction> <player name>\r\n\
\x20      hcontrol destroy <house vnum>\r\n\
\x20      hcontrol update <house vnum> <exit direction> [player name]\r\n\
\x20      hcontrol pay <house vnum>\r\n\
\x20      hcontrol show <guests>\r\n\
\x20      hcontrol crashsave <house vnum>\r\n\
\x20      hcontrol guests\n";

// ---------------------------------------------------------------------------
// In-memory house-control table (C: house_control[MAX_HOUSES], num_of_houses).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct HouseControlRec {
    vnum: RoomVnum,
    atrium: RoomVnum,
    exit_num: i32,
    built_on: i64,      // unix time
    mode: i32,          // HOUSE_PRIVATE / HOUSE_OPEN
    owner: i64,         // idnum of owner (-1 for HCRSH/open rooms)
    owner_name: String, // cached name for offline rendering
    guests: Vec<i64>,
    guest_names: Vec<String>,
    last_payment: i64, // unix time, 0 == None
}

impl HouseControlRec {
    fn blank() -> Self {
        HouseControlRec {
            vnum: NOWHERE,
            atrium: 0,
            exit_num: 0,
            built_on: 0,
            mode: HOUSE_OPEN,
            owner: -1,
            owner_name: String::new(),
            guests: Vec::new(),
            guest_names: Vec::new(),
            last_payment: 0,
        }
    }
}

fn houses() -> &'static Mutex<Vec<HouseControlRec>> {
    static HOUSES: OnceLock<Mutex<Vec<HouseControlRec>>> = OnceLock::new();
    HOUSES.get_or_init(|| Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// File-path helpers (db.h HCONTROL_FILE = "etc/hcontrol", House_get_filename).
// ---------------------------------------------------------------------------

fn hcontrol_path(lib: &str) -> std::path::PathBuf {
    std::path::Path::new(lib).join("etc").join("hcontrol")
}

/// House_get_filename(): "house/<vnum>.house" under the lib path. Returns None
/// for negative vnums (C returns 0).
fn house_filename(lib: &str, vnum: RoomVnum) -> Option<std::path::PathBuf> {
    if vnum < 0 {
        return None;
    }
    Some(
        std::path::Path::new(lib)
            .join("house")
            .join(format!("{}.house", vnum)),
    )
}

// ---------------------------------------------------------------------------
// id <-> name resolution (C: get_id_by_name / get_name_by_id, via player_table).
// We resolve against online players, then the boot-loaded GameState index
// (C player_table, offline-capable), then the name cache embedded in the
// control file.
// ---------------------------------------------------------------------------

/// get_id_by_name(): returns the persistent idnum for `name`, or -1 if unknown.
/// Checks online players, the shared player_table index, then cached names.
fn get_id_by_name(g: &GameState, name: &str) -> i64 {
    let lower = name.to_lowercase();
    if let Some(cid) = g.find_player_by_name(&lower) {
        if let Some(c) = g.get_char(cid) {
            if c.idnum >= 0 {
                return c.idnum;
            }
        }
    }
    // Shared GameState player_table index (resolves offline owners/guests).
    if let Some(id) = g.get_id_by_name(&lower) {
        return id;
    }
    // Fall back to the control-file name cache.
    let table = houses().lock().unwrap();
    for h in table.iter() {
        if h.owner >= 0 && h.owner_name.eq_ignore_ascii_case(&lower) {
            return h.owner;
        }
        for (idx, gname) in h.guest_names.iter().enumerate() {
            if gname.eq_ignore_ascii_case(&lower) {
                if let Some(&gid) = h.guests.get(idx) {
                    return gid;
                }
            }
        }
    }
    -1
}

/// get_name_by_id(): cached capitalized name for an idnum, or None.
fn get_name_by_id(g: &GameState, id: i64) -> Option<String> {
    if id < 0 {
        return None;
    }
    // Online player?
    for (_, &cid) in g.players_by_name.iter() {
        if let Some(c) = g.get_char(cid) {
            if c.idnum == id {
                return Some(c.player.name.clone());
            }
        }
    }
    // Shared GameState player_table index (offline-capable, canonical case).
    if let Some(n) = g.get_name_by_id(id) {
        return Some(n);
    }
    let table = houses().lock().unwrap();
    for h in table.iter() {
        if h.owner == id && !h.owner_name.is_empty() {
            return Some(h.owner_name.clone());
        }
        for (idx, &gid) in h.guests.iter().enumerate() {
            if gid == id {
                if let Some(n) = h.guest_names.get(idx) {
                    if !n.is_empty() {
                        return Some(n.clone());
                    }
                }
            }
        }
    }
    None
}

/// C's NAME(x) macro: get_name_by_id(x) or "<UNDEF>".
fn name_or_undef(g: &GameState, id: i64) -> String {
    get_name_by_id(g, id).unwrap_or_else(|| "<UNDEF>".to_string())
}

/// CAP(): uppercase the first character.
fn cap(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Room-flag helpers (raw-bit manipulation to stay C-accurate).
// ---------------------------------------------------------------------------

fn room_flag_set(g: &mut GameState, rnum: RoomRnum, bits: u32) {
    if let Some(r) = g.rooms.get_mut(rnum) {
        r.room_flags = RoomFlags::from_bits_retain(r.room_flags.bits() | bits);
    }
}
fn room_flag_remove(g: &mut GameState, rnum: RoomRnum, bits: u32) {
    if let Some(r) = g.rooms.get_mut(rnum) {
        r.room_flags = RoomFlags::from_bits_retain(r.room_flags.bits() & !bits);
    }
}
fn room_flag_isset(g: &GameState, rnum: RoomRnum, bits: u32) -> bool {
    g.room_opt(rnum)
        .map(|r| r.room_flags.bits() & bits != 0)
        .unwrap_or(false)
}

/// TOROOM(room, dir): destination vnum of an exit, or NOWHERE.
fn toroom(g: &GameState, rnum: RoomRnum, dir: usize) -> RoomVnum {
    if dir >= NUM_OF_DIRS {
        return NOWHERE;
    }
    match g.room_opt(rnum).and_then(|r| r.exits[dir].as_ref()) {
        Some(e) => e.to_room,
        None => NOWHERE,
    }
}

/// mudlog(): broadcast to immortals at or above `min_level`.
fn mudlog(g: &mut GameState, line: &str, min_level: u8) {
    let formatted = format!("[ {} ]\r\n", line);
    let imms: Vec<CharId> = g
        .players_by_name
        .values()
        .copied()
        .filter(|&id| {
            g.get_char(id)
                .map(|c| c.player.level >= min_level && c.player.level >= LVL_IMMORT)
                .unwrap_or(false)
        })
        .collect();
    for id in imms {
        g.send_to_char(id, &formatted);
    }
    // Also echo to the host log so a headless boot leaves a trace.
    eprintln!("[ {} ]", line);
}

// ===========================================================================
// find_house() (house.c)
// ===========================================================================

/// Index of the house whose vnum == `vnum`, or None.
fn find_house(vnum: RoomVnum) -> Option<usize> {
    let table = houses().lock().unwrap();
    table.iter().position(|h| h.vnum == vnum)
}

// ===========================================================================
// House control-file load/save (House_boot / House_save_control)
// ===========================================================================
//
// TEXT FORMAT (lib/etc/hcontrol), one record per "H" block:
//   H <vnum> <atrium> <exit_num> <built_on> <mode> <owner> <last_payment>
//   O <owner_name>
//   G <count> <id:name> <id:name> ...
// Records are separated implicitly; the file ends with "$".

/// House_save_control(): write the in-memory table to lib/etc/hcontrol.
fn house_save_control(g: &GameState) {
    let path = hcontrol_path(&g.config.lib_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let table = houses().lock().unwrap();
    let mut out = String::new();
    for h in table.iter() {
        out.push_str(&format!(
            "H {} {} {} {} {} {} {}\n",
            h.vnum, h.atrium, h.exit_num, h.built_on, h.mode, h.owner, h.last_payment
        ));
        out.push_str(&format!(
            "O {}\n",
            if h.owner_name.is_empty() {
                "*"
            } else {
                h.owner_name.as_str()
            }
        ));
        out.push_str(&format!("G {}", h.guests.len()));
        for (idx, &gid) in h.guests.iter().enumerate() {
            let gname = h.guest_names.get(idx).map(|s| s.as_str()).unwrap_or("*");
            let gname = if gname.is_empty() { "*" } else { gname };
            out.push_str(&format!(" {}:{}", gid, gname));
        }
        out.push('\n');
    }
    out.push_str("$\n");
    if std::fs::write(&path, out).is_err() {
        eprintln!("SYSERR: Unable to open house control file");
    }
}

/// Parse the text control file into a Vec of records (no world validation).
fn parse_control_file(text: &str) -> Vec<HouseControlRec> {
    let mut recs = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line == "$" || line.is_empty() {
            if line == "$" {
                break;
            }
            continue;
        }
        if !line.starts_with('H') {
            continue;
        }
        let parts: Vec<&str> = line[1..].split_whitespace().collect();
        if parts.len() < 7 {
            continue;
        }
        let mut rec = HouseControlRec::blank();
        rec.vnum = parts[0].parse().unwrap_or(NOWHERE);
        rec.atrium = parts[1].parse().unwrap_or(0);
        rec.exit_num = parts[2].parse().unwrap_or(0);
        rec.built_on = parts[3].parse().unwrap_or(0);
        rec.mode = parts[4].parse().unwrap_or(HOUSE_OPEN);
        rec.owner = parts[5].parse().unwrap_or(-1);
        rec.last_payment = parts[6].parse().unwrap_or(0);
        // Optional O / G lines.
        if let Some(&next) = lines.peek() {
            if next.trim_start().starts_with('O') {
                let oline = lines.next().unwrap().trim();
                let name = oline[1..].trim();
                if name != "*" {
                    rec.owner_name = name.to_lowercase();
                }
            }
        }
        if let Some(&next) = lines.peek() {
            if next.trim_start().starts_with('G') {
                let gline = lines.next().unwrap().trim();
                let toks: Vec<&str> = gline[1..].split_whitespace().collect();
                // toks[0] = count, then id:name pairs.
                for tok in toks.iter().skip(1) {
                    let (idpart, namepart) = match tok.split_once(':') {
                        Some((a, b)) => (a, b),
                        None => (*tok, "*"),
                    };
                    if let Ok(gid) = idpart.parse::<i64>() {
                        rec.guests.push(gid);
                        rec.guest_names.push(if namepart == "*" {
                            String::new()
                        } else {
                            namepart.to_lowercase()
                        });
                    }
                }
            }
        }
        recs.push(rec);
    }
    recs
}

/// House_boot(): load control records, validate vnums, set room bits, load
/// objects. Called by the integrator at boot (after the world is loaded).
pub fn boot_houses(lib_path: &str) {
    // This is the integrator hook; it just records the lib path so a later
    // House_boot(g) can run with the live GameState. Kept for symmetry with the
    // other boot_<system> entry points; the real work happens in house_boot().
    let _ = lib_path;
}

/// The real boot routine — needs the GameState to validate vnums and load
/// objects, mirroring C's House_boot() called from boot_db. The integrator
/// should call this once at startup after the world tables exist.
/// C get_name_by_id(owner) != NULL: resolve through the player index.
fn owner_exists(g: &GameState, idnum: i64) -> bool {
    g.get_name_by_id(idnum).is_some()
}

pub fn house_boot(g: &mut GameState) {
    let path = hcontrol_path(&g.config.lib_path);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            mudlog(g, "House control file does not exist.", NRM_INVIS);
            return;
        }
    };

    let parsed = parse_control_file(&text);
    let mut accepted: Vec<HouseControlRec> = Vec::new();
    let mut to_load: Vec<RoomVnum> = Vec::new();

    for temp in parsed.into_iter() {
        if accepted.len() >= MAX_HOUSES {
            break;
        }
        let real_house = match g.real_room(temp.vnum) {
            Some(r) => r,
            None => continue, // vnum doesn't exist -- skip
        };
        if accepted.iter().any(|h| h.vnum == temp.vnum) {
            continue; // already a house -- skip
        }

        let mut real_atrium: Option<RoomRnum> = None;
        // Owner sanity (C house.c:295-322): a record whose owner no longer
        // resolves through the player index is SKIPPED - the soft pass let
        // houses of deleted players survive boot (#178).
        if temp.owner >= 0 && !owner_exists(g, temp.owner) {
            mudlog(g, "SYSERR: House owner does not exist - skipping house.", NRM_INVIS);
            continue;
        }
        if temp.owner != -1 {
            // C validates atrium/exit ALWAYS for owner != -1, not only when
            // both fields are non-zero (#178).
            real_atrium = g.real_room(temp.atrium);
            if real_atrium.is_none() {
                mudlog(g, "DEBUG: House atrium does not exist?!", NRM_INVIS);
                continue;
            }
            if temp.exit_num < 0 || temp.exit_num as usize >= NUM_OF_DIRS {
                mudlog(g, "DEBUG: House has invalid exit num?!", NRM_INVIS);
                continue;
            }
            if toroom(g, real_house, temp.exit_num as usize) != temp.atrium {
                mudlog(g, "DEBUG: House exit num mismatch?!", NRM_INVIS);
                continue;
            }
        }

        room_flag_set(g, real_house, ROOM_HOUSE | ROOM_PRIVATE);
        if let Some(ra) = real_atrium {
            room_flag_set(g, ra, ROOM_ATRIUM);
        }
        to_load.push(temp.vnum);
        accepted.push(temp);
    }

    {
        let mut table = houses().lock().unwrap();
        *table = accepted;
    }

    for vnum in to_load {
        house_load(g, vnum);
    }

    house_save_control(g);
}

// ===========================================================================
// House object load/save (House_load / House_crashsave / House_save).
// ===========================================================================
//
// TEXT FORMAT (lib/house/<vnum>.house): one line per object, flattened
// depth-first (a parent precedes its contents). Each line:
//   <depth> <vnum> <type> <wearbits> <extrabits> <weight> <cost> <rent>
//           <timer> <v0> <v1> <v2> <v3> <#affects> [loc mod ...]
//           |<name>|<short>|<long>
// `depth` tracks container nesting (0 = top of room). On load we rebuild the
// tree from depth, skipping ITEM_KEY and ITEM_NORENT items exactly as C does.

fn serialize_obj_line(g: &GameState, oid: ObjId, depth: usize, out: &mut String) {
    let o = match g.get_obj(oid) {
        Some(o) => o,
        None => return,
    };
    let ty = o.obj_type as i32;
    out.push_str(&format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        depth,
        o.item_number,
        ty,
        o.wear_flags.bits(),
        o.extra_flags.bits(),
        o.weight,
        o.cost,
        o.rent,
        o.timer,
        o.values[0],
        o.values[1],
        o.values[2],
        o.values[3],
        o.level,       // min_level
        o.bitvector,   // affect bitvector
        o.curr_slots,  // durability counters (#233)
        o.total_slots,
        o.affects.len()
    ));
    for a in &o.affects {
        out.push_str(&format!(" {} {}", a.location, a.modifier));
    }
    out.push_str(&format!(
        "|{}|{}|{}\n",
        o.name.replace('|', "/").replace('\n', " "),
        o.short_description.replace('|', "/").replace('\n', " "),
        o.description.replace('|', "/").replace('\n', " ")
    ));
    // Recurse into contents at depth+1 (C: House_save walks obj->contains).
    let contains = o.contains.clone();
    for c in contains {
        serialize_obj_line(g, c, depth + 1, out);
    }
}

/// House_crashsave(): serialize every object in the house room to its file and
/// clear ROOM_HOUSE_CRASH. No-op if the room/file path is invalid.
pub fn house_crashsave(g: &mut GameState, vnum: RoomVnum) {
    let rnum = match g.real_room(vnum) {
        Some(r) => r,
        None => return,
    };
    let path = match house_filename(&g.config.lib_path, vnum) {
        Some(p) => p,
        None => return,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let contents = g.room(rnum).contents.clone();
    let mut out = String::new();
    for oid in contents {
        serialize_obj_line(g, oid, 0, &mut out);
    }
    if std::fs::write(&path, out).is_err() {
        eprintln!("SYSERR: Error saving house file");
        return;
    }
    room_flag_remove(g, rnum, ROOM_HOUSE_CRASH);
}

/// House_load(): rebuild objects from the house file into the room. Skips
/// ITEM_KEY and ITEM_NORENT objects (as C does). Returns true on success.
fn house_load(g: &mut GameState, vnum: RoomVnum) -> bool {
    let rnum = match g.real_room(vnum) {
        Some(r) => r,
        None => return false,
    };
    let msg = format!("Loading house {} (real_room {})", vnum, rnum);
    mudlog(g, &msg, NRM_INVIS);

    let path = match house_filename(&g.config.lib_path, vnum) {
        Some(p) => p,
        None => return false,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return false, // no file found
    };

    // Stack of (depth, ObjId) for rebuilding container nesting.
    let mut stack: Vec<(usize, ObjId)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (oid, depth, is_key_or_norent) = match parse_obj_line(g, line) {
            Some(t) => t,
            None => continue,
        };

        // Pop stack entries whose depth >= this object's depth.
        while let Some(&(d, _)) = stack.last() {
            if d >= depth {
                stack.pop();
            } else {
                break;
            }
        }

        if depth == 0 {
            // Top-level object: C only places it if it's not a key and not
            // NORENT; otherwise it is discarded (extracted).
            if is_key_or_norent {
                g.extract_obj(oid);
            } else {
                g.obj_to_room(oid, rnum);
                stack.push((depth, oid));
            }
        } else {
            // Nested object: parent is the current stack top.
            match stack.last().copied() {
                Some((_, parent)) => {
                    g.obj_to_obj(oid, parent);
                    stack.push((depth, oid));
                }
                None => {
                    // Orphaned nesting (corrupt file) — drop to room as top.
                    if is_key_or_norent {
                        g.extract_obj(oid);
                    } else {
                        g.obj_to_room(oid, rnum);
                        stack.push((0, oid));
                    }
                }
            }
        }
    }
    true
}

/// Parse one serialized object line into a live Object. Returns
/// (id, depth, is_key_or_norent) or None on a malformed line.
fn parse_obj_line(g: &mut GameState, line: &str) -> Option<(ObjId, usize, bool)> {
    // Split off the |name|short|long tail.
    let (head, tail) = match line.split_once('|') {
        Some((h, t)) => (h, t),
        None => (line, ""),
    };
    let nums: Vec<&str> = head.split_whitespace().collect();
    if nums.len() < 14 {
        return None;
    }
    let depth: usize = nums[0].parse().ok()?;
    let vnum: ObjVnum = nums[1].parse().unwrap_or(NOTHING);
    let ty: i32 = nums[2].parse().unwrap_or(9);
    let wear: u32 = nums[3].parse().unwrap_or(0);
    let extra: u64 = nums[4].parse().unwrap_or(0);
    let weight: i32 = nums[5].parse().unwrap_or(1);
    let cost: i32 = nums[6].parse().unwrap_or(0);
    let rent: i32 = nums[7].parse().unwrap_or(0);
    let timer: i32 = nums[8].parse().unwrap_or(-1);
    let v0: i32 = nums[9].parse().unwrap_or(0);
    let v1: i32 = nums[10].parse().unwrap_or(0);
    let v2: i32 = nums[11].parse().unwrap_or(0);
    let v3: i32 = nums[12].parse().unwrap_or(0);
    // Records written before the #233 fix lack level/bitvector/curr_slots/
    // total_slots (14 head numbers); new records carry 18. Old files load
    // with the pre-fix defaults.
    let (level, bitvector, curr_slots, total_slots, naff, mut idx) = if nums.len() >= 18 {
        (
            nums[13].parse().unwrap_or(0),
            nums[14].parse().unwrap_or(0),
            nums[15].parse().unwrap_or(0),
            nums[16].parse().unwrap_or(0),
            nums[17].parse::<usize>().unwrap_or(0),
            18,
        )
    } else {
        (0, 0, 0, 0, nums[13].parse::<usize>().unwrap_or(0), 14)
    };

    let mut affects = Vec::new();
    for _ in 0..naff {
        if idx + 1 < nums.len() {
            let loc: i32 = nums[idx].parse().unwrap_or(0);
            let modi: i32 = nums[idx + 1].parse().unwrap_or(0);
            affects.push(ObjectAffect {
                location: loc,
                modifier: modi,
            });
            idx += 2;
        }
    }

    // name|short|long from the tail.
    let mut tparts = tail.splitn(3, '|');
    let name = tparts.next().unwrap_or("").to_string();
    let short = tparts.next().unwrap_or("").to_string();
    let long = tparts.next().unwrap_or("").to_string();

    let mut obj = Object::new(vnum, name, short);
    obj.description = long;
    obj.obj_type = ObjectType::from_i32(ty);
    obj.wear_flags = WearFlags::from_bits_retain(wear);
    obj.extra_flags = ExtraFlags::from_bits_retain(extra);
    obj.weight = weight;
    obj.cost = cost;
    obj.rent = rent;
    obj.timer = timer;
    obj.values = [v0, v1, v2, v3];
    obj.affects = affects;
    obj.level = level.clamp(0, u8::MAX as i32) as u8;
    obj.bitvector = bitvector;
    obj.curr_slots = curr_slots;
    obj.total_slots = total_slots;

    let is_key_or_norent = obj.obj_type == ObjectType::Key || (extra & ITEM_NORENT) != 0;
    let oid = g.create_obj(obj);
    Some((oid, depth, is_key_or_norent))
}

/// House_delete_file(): remove a house's object file.
fn house_delete_file(g: &GameState, vnum: RoomVnum) {
    if let Some(path) = house_filename(&g.config.lib_path, vnum) {
        let _ = std::fs::remove_file(path);
    }
}

/// House_listrent(): list the objects stored in a house file to `ch`.
fn house_listrent(g: &mut GameState, ch: CharId, vnum: RoomVnum) {
    let path = match house_filename(&g.config.lib_path, vnum) {
        Some(p) => p,
        None => return,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            g.send_to_char(ch, &format!("No objects on file for house #{}.\r\n", vnum));
            return;
        }
    };
    let mut buf = String::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (head, tail) = match line.split_once('|') {
            Some((h, t)) => (h, t),
            None => (line, ""),
        };
        let nums: Vec<&str> = head.split_whitespace().collect();
        if nums.len() < 14 {
            continue;
        }
        let vnum_o: i32 = nums[1].parse().unwrap_or(-1);
        let rent: i32 = nums[7].parse().unwrap_or(0);
        let mut tparts = tail.splitn(3, '|');
        let _name = tparts.next().unwrap_or("");
        let short = tparts.next().unwrap_or("");
        buf.push_str(&format!(" [{:5}] ({:5}au) {}\r\n", vnum_o, rent, short));
    }
    g.send_to_char(ch, &buf);
}

// ===========================================================================
// hcontrol command and its subroutines (house.c)
// ===========================================================================

fn hcontrol_list_houses(g: &mut GameState, ch: CharId, showguests: bool) {
    let table: Vec<HouseControlRec> = houses().lock().unwrap().clone();
    if table.is_empty() {
        g.send_to_char(ch, "No houses have been defined.\r\n");
        return;
    }
    let mut buf = String::new();
    buf.push_str("Address  Atrium  Build Date  Guests  Owner        Last Paymt\r\n");
    buf.push_str("-------  ------  ----------  ------  ------------ ----------\r\n");

    for h in &table {
        let built_on = if h.built_on != 0 {
            fmt_date(h.built_on)
        } else {
            "Unknown".to_string()
        };
        let last_pay = if h.last_payment != 0 {
            fmt_date(h.last_payment)
        } else {
            "None".to_string()
        };
        let own_name = if h.owner != -1 {
            cap(&name_or_undef(g, h.owner))
        } else {
            "HCRSH".to_string()
        };

        buf.push_str(&format!(
            "{:7} {:7}  {:<10}    {:2}    {:<12} {}\r\n",
            h.vnum,
            h.atrium,
            built_on,
            h.guests.len(),
            own_name,
            last_pay
        ));

        if !h.guests.is_empty() && showguests {
            buf.push_str("     Guests: ");
            for &gid in &h.guests {
                buf.push_str(&cap(&name_or_undef(g, gid)));
                buf.push(' ');
            }
            buf.push_str("\r\n");
        }
    }
    g.send_to_char(ch, &buf);
}

fn hcontrol_list_houses_guests(g: &mut GameState, ch: CharId) {
    let table: Vec<HouseControlRec> = houses().lock().unwrap().clone();
    if table.is_empty() {
        g.send_to_char(ch, "No houses have been defined.\r\n");
        return;
    }
    let mut buf = String::new();
    buf.push_str("Address  Owner        # Guests     \r\n");
    buf.push_str("-------  ------       - ---------- \r\n");

    for h in &table {
        let own_name = if h.owner != -1 {
            cap(&name_or_undef(g, h.owner))
        } else {
            "HCRSH".to_string()
        };
        buf.push_str(&format!(
            "{:7} {:<12} {:2} ",
            h.vnum,
            own_name,
            h.guests.len()
        ));

        if !h.guests.is_empty() {
            let mut count = 0;
            let n = h.guests.len();
            for (j, &gid) in h.guests.iter().enumerate() {
                count += 1;
                if count > 5 && j != n + 1 {
                    count = 0;
                    buf.push_str("\r\n                        ");
                }
                buf.push_str(&cap(&name_or_undef(g, gid)));
                buf.push(' ');
            }
        }
        buf.push_str("\r\n");
    }
    g.send_to_char(ch, &buf);
}

fn hcontrol_build_house(g: &mut GameState, ch: CharId, arg: &str) {
    if houses().lock().unwrap().len() >= MAX_HOUSES {
        g.send_to_char(ch, "Max houses already defined.\r\n");
        return;
    }

    // first arg: house's vnum
    let (a1, rest) = one_argument(arg);
    if a1.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let virt_house: RoomVnum = a1.parse().unwrap_or(0);
    let real_house = match g.real_room(virt_house) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "No such room exists.\r\n");
            return;
        }
    };
    if find_house(virt_house).is_some() {
        g.send_to_char(ch, "House already exists.\r\n");
        return;
    }

    // second arg: direction of house's exit
    let (a2, rest) = one_argument(rest);
    if a2.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let exit_num = match search_block(&a2, &DIR_NAMES) {
        Some(d) => d,
        None => {
            g.send_to_char(ch, &format!("'{}' is not a valid direction.\r\n", a2));
            return;
        }
    };
    if toroom(g, real_house, exit_num) == NOWHERE {
        g.send_to_char(
            ch,
            &format!(
                "There is no exit {} from room {}.\r\n",
                DIR_NAMES[exit_num], virt_house
            ),
        );
        return;
    }

    let virt_atrium = toroom(g, real_house, exit_num);
    let real_atrium = match g.real_room(virt_atrium) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "A house's exit must be a two-way door.\r\n");
            return;
        }
    };
    if toroom(g, real_atrium, REV_DIR[exit_num]) != virt_house {
        g.send_to_char(ch, "A house's exit must be a two-way door.\r\n");
        return;
    }

    // third arg: player's name
    let (a3, _) = one_argument(rest);
    if a3.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let owner = get_id_by_name(g, &a3);
    if owner < 0 {
        g.send_to_char(ch, &format!("Unknown player '{}'.\r\n", a3));
        return;
    }

    let mut temp = HouseControlRec::blank();
    temp.mode = HOUSE_PRIVATE;
    temp.vnum = virt_house;
    temp.atrium = virt_atrium;
    temp.exit_num = exit_num as i32;
    temp.built_on = now();
    temp.last_payment = 0;
    temp.owner = owner;
    temp.owner_name = a3.to_lowercase();
    temp.guests = Vec::new();
    temp.guest_names = Vec::new();

    houses().lock().unwrap().push(temp);

    room_flag_set(g, real_house, ROOM_HOUSE | ROOM_PRIVATE);
    room_flag_set(g, real_atrium, ROOM_ATRIUM);
    house_crashsave(g, virt_house);

    g.send_to_char(ch, "House built.  Mazel tov!\r\n");
    house_save_control(g);
}

fn hcontrol_crashsave_house(g: &mut GameState, ch: CharId, arg: &str) {
    if houses().lock().unwrap().len() >= MAX_HOUSES {
        g.send_to_char(ch, "Max crashsaveables/houses already defined.\r\n");
        return;
    }
    let (a1, _) = one_argument(arg);
    if a1.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let virt_house: RoomVnum = a1.parse().unwrap_or(0);
    let real_house = match g.real_room(virt_house) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "No such room exists.\r\n");
            return;
        }
    };
    if find_house(virt_house).is_some() {
        g.send_to_char(ch, "House already exists.\r\n");
        return;
    }

    let mut temp = HouseControlRec::blank();
    temp.mode = HOUSE_OPEN;
    temp.vnum = virt_house;
    temp.atrium = 0;
    temp.exit_num = 0;
    temp.built_on = now();
    temp.last_payment = 0;
    temp.owner = -1;
    temp.guests = Vec::new();
    temp.guest_names = Vec::new();

    houses().lock().unwrap().push(temp);

    room_flag_set(g, real_house, ROOM_HOUSE | ROOM_PRIVATE);
    house_crashsave(g, virt_house);

    g.send_to_char(ch, "Crashsaveable room built.  Mazel tov!\r\n");
    house_save_control(g);
}

fn hcontrol_destroy_house(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let target_vnum: RoomVnum = arg.trim().parse().unwrap_or(0);
    let i = match find_house(target_vnum) {
        Some(i) => i,
        None => {
            g.send_to_char(ch, "Unknown house.\r\n");
            return;
        }
    };

    let (atrium_vnum, house_vnum) = {
        let table = houses().lock().unwrap();
        (table[i].atrium, table[i].vnum)
    };

    match g.real_room(atrium_vnum) {
        Some(ra) => room_flag_remove(g, ra, ROOM_ATRIUM),
        None => mudlog(g, "SYSERR: House had invalid atrium!", NRM_INVIS),
    }
    match g.real_room(house_vnum) {
        Some(rh) => room_flag_remove(g, rh, ROOM_HOUSE | ROOM_PRIVATE | ROOM_HOUSE_CRASH),
        None => mudlog(g, "SYSERR: House had invalid vnum!", NRM_INVIS),
    }

    house_delete_file(g, house_vnum);

    houses().lock().unwrap().remove(i);

    g.send_to_char(ch, "House deleted.\r\n");
    house_save_control(g);

    // Re-set ROOM_ATRIUM on every surviving house's atrium (in case the
    // destroyed house shared an atrium with another). --JE 9/19/94
    let atriums: Vec<RoomVnum> = houses().lock().unwrap().iter().map(|h| h.atrium).collect();
    for av in atriums {
        if let Some(ra) = g.real_room(av) {
            room_flag_set(g, ra, ROOM_ATRIUM);
        }
    }
}

fn hcontrol_pay_house(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let target_vnum: RoomVnum = arg.trim().parse().unwrap_or(0);
    let i = match find_house(target_vnum) {
        Some(i) => i,
        None => {
            g.send_to_char(ch, "Unknown house.\r\n");
            return;
        }
    };

    let name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let invis = g.get_char(ch).map(|c| c.invis_level as u8).unwrap_or(0);
    let msg = format!("Payment for house {} collected by {}.", arg.trim(), name);
    mudlog(g, &msg, LVL_IMMORT.max(invis));

    {
        let mut table = houses().lock().unwrap();
        table[i].last_payment = now();
    }
    house_save_control(g);
    g.send_to_char(ch, "Payment recorded.\r\n");
}

fn hcontrol_update_house(g: &mut GameState, ch: CharId, arg: &str) {
    // first arg: house's vnum
    let (a1, rest) = one_argument(arg);
    if a1.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let virt_house: RoomVnum = a1.parse().unwrap_or(0);
    let house = match find_house(virt_house) {
        Some(h) => h,
        None => {
            g.send_to_char(ch, "Unknown house.\r\n");
            return;
        }
    };
    let real_house = match g.real_room(virt_house) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "Unknown house.\r\n");
            return;
        }
    };

    // second arg: direction of house's exit
    let (a2, rest) = one_argument(rest);
    if a2.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let exit_num = match search_block(&a2, &DIR_NAMES) {
        Some(d) => d,
        None => {
            g.send_to_char(ch, &format!("'{}' is not a valid direction.\r\n", a2));
            return;
        }
    };
    if toroom(g, real_house, exit_num) == NOWHERE {
        g.send_to_char(
            ch,
            &format!(
                "There is no exit {} from room {}.\r\n",
                DIR_NAMES[exit_num], virt_house
            ),
        );
        return;
    }

    let virt_atrium = toroom(g, real_house, exit_num);
    let real_atrium = match g.real_room(virt_atrium) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "A house's exit must be a two-way door.\r\n");
            return;
        }
    };
    if toroom(g, real_atrium, REV_DIR[exit_num]) != virt_house {
        g.send_to_char(ch, "A house's exit must be a two-way door.\r\n");
        return;
    }

    // third arg: player's name (optional)
    let (a3, _) = one_argument(rest);
    let owner: i64;
    let owner_name: Option<String>;
    if a3.is_empty() {
        owner = -1;
        owner_name = None;
    } else {
        owner = get_id_by_name(g, &a3);
        if owner < 0 {
            g.send_to_char(ch, &format!("Unknown player '{}'.\r\n", a3));
            return;
        }
        owner_name = Some(a3.to_lowercase());
    }

    {
        let mut table = houses().lock().unwrap();
        table[house].atrium = virt_atrium;
        table[house].exit_num = exit_num as i32;
        if owner != -1 {
            table[house].owner = owner;
            if let Some(n) = owner_name {
                table[house].owner_name = n;
            }
        }
    }

    room_flag_set(g, real_house, ROOM_HOUSE | ROOM_PRIVATE);
    room_flag_set(g, real_atrium, ROOM_ATRIUM);
    house_crashsave(g, virt_house);

    g.send_to_char(ch, "House Updated!\r\n");
    house_save_control(g);
}

/// do_hcontrol — the immortal house-admin command.
pub fn do_hcontrol(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg1, arg2) = half_chop(argument);

    if is_abbrev(&arg1, "build") {
        hcontrol_build_house(g, ch, &arg2);
    } else if is_abbrev(&arg1, "crashsave") {
        hcontrol_crashsave_house(g, ch, &arg2);
    } else if is_abbrev(&arg1, "destroy") {
        hcontrol_destroy_house(g, ch, &arg2);
    } else if is_abbrev(&arg1, "pay") {
        hcontrol_pay_house(g, ch, &arg2);
    } else if is_abbrev(&arg1, "update") {
        hcontrol_update_house(g, ch, &arg2);
    } else if is_abbrev(&arg1, "show") {
        if is_abbrev(&arg2, "guests") {
            hcontrol_list_houses(g, ch, true);
        } else {
            hcontrol_list_houses(g, ch, false);
        }
    } else if is_abbrev(&arg1, "guests") {
        hcontrol_list_houses_guests(g, ch);
    } else {
        g.send_to_char(ch, HCONTROL_FORMAT);
    }
}

// ===========================================================================
// do_house — house-owner guest control.
// ===========================================================================

pub fn do_house(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);

    let in_room = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    if !room_flag_isset(g, in_room, ROOM_HOUSE) {
        g.send_to_char(ch, "You must be in your house to set guests.\r\n");
        return;
    }
    let room_vnum = g.room(in_room).number;
    let i = match find_house(room_vnum) {
        Some(i) => i,
        None => {
            g.send_to_char(ch, "Um.. this house seems to be screwed up.\r\n");
            return;
        }
    };

    let my_idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
    let owner = houses().lock().unwrap()[i].owner;
    if my_idnum != owner {
        g.send_to_char(ch, "Only the primary owner can set guests.\r\n");
        return;
    }

    if arg.is_empty() {
        // List guests.
        let guests = houses().lock().unwrap()[i].guests.clone();
        g.send_to_char(ch, "Guests of your house:\r\n");
        if guests.is_empty() {
            g.send_to_char(ch, "  None.\r\n");
        } else {
            for gid in guests {
                let line = format!("{}\r\n", cap(&name_or_undef(g, gid)));
                g.send_to_char(ch, &line);
            }
        }
        return;
    }

    let id = get_id_by_name(g, &arg);
    if id < 0 {
        g.send_to_char(ch, "No such player.\r\n");
        return;
    }

    // Toggle: remove if already a guest, else add.
    let mut deleted = false;
    {
        let mut table = houses().lock().unwrap();
        if let Some(pos) = table[i].guests.iter().position(|&gx| gx == id) {
            table[i].guests.remove(pos);
            if pos < table[i].guest_names.len() {
                table[i].guest_names.remove(pos);
            }
            deleted = true;
        } else {
            table[i].guests.push(id);
            table[i].guest_names.push(arg.to_lowercase());
        }
    }
    house_save_control(g);
    if deleted {
        g.send_to_char(ch, "Guest deleted.\r\n");
    } else {
        // Enforce MAX_GUESTS like C's fixed array would.
        let mut table = houses().lock().unwrap();
        if table[i].guests.len() > MAX_GUESTS {
            table[i].guests.truncate(MAX_GUESTS);
            table[i].guest_names.truncate(MAX_GUESTS);
        }
        drop(table);
        g.send_to_char(ch, "Guest added.\r\n");
    }
}

// ===========================================================================
// do_bed — quit the game while in your house (rent-save + extract).
// ===========================================================================

/// Crash_is_unrentable() lite: an object is unrentable if it is flagged NORENT.
/// (Full Crash_is_unrentable also rejects ITEM_KEY and rent<0; mirrored here.)
fn is_unrentable(g: &GameState, oid: ObjId) -> bool {
    match g.get_obj(oid) {
        Some(o) => {
            o.obj_type == ObjectType::Key || (o.extra_flags.bits() & ITEM_NORENT) != 0 || o.rent < 0
        }
        None => false,
    }
}

/// Crash_report_unbedables(): recursively report unrentable items; returns the
/// count found.
fn report_unbedables(g: &mut GameState, ch: CharId, oid: ObjId) -> i32 {
    let mut count = 0;
    if is_unrentable(g, oid) {
        count += 1;
        act(
            g,
            "You cannot go to bed with $p.",
            false,
            ch,
            Some(oid),
            ActArg::None,
            To::Char,
        );
    }
    let contains = g
        .get_obj(oid)
        .map(|o| o.contains.clone())
        .unwrap_or_default();
    for c in contains {
        count += report_unbedables(g, ch, c);
    }
    count
}

pub fn do_bed(g: &mut GameState, ch: CharId, _argument: &str, _subcmd: i32) {
    let in_room = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    if !room_flag_isset(g, in_room, ROOM_HOUSE) {
        g.send_to_char(ch, "You must be in your house to go to bed.\r\n");
        return;
    }
    let room_vnum = g.room(in_room).number;
    let i = match find_house(room_vnum) {
        Some(i) => i,
        None => {
            g.send_to_char(ch, "Um.. this house seems to be screwed up.\r\n");
            return;
        }
    };
    let my_idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
    let owner = houses().lock().unwrap()[i].owner;
    if my_idnum != owner {
        g.send_to_char(ch, "Only the primary owner can go to bed in the house.\r\n");
        return;
    }

    // Report (and refuse on) any unbedable items in inventory or equipment.
    let mut nobed = 0;
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    for o in carrying {
        nobed += report_unbedables(g, ch, o);
    }
    let worn: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    for o in worn {
        nobed += report_unbedables(g, ch, o);
    }
    if nobed != 0 {
        return;
    }

    // write_aliases(ch): no alias system ported — no-op.
    // Clear the AFK lockout pref (C: REMOVE_BIT PRF2_LOCKOUT).
    if let Some(c) = g.get_char_mut(ch) {
        c.prf2_flags &= !PRF2_LOCKOUT;
    }

    // C act.other.c:506 do_bed: Crash_rentsave then House_crashsave for the
    // house room before quitting (#164).
    {
        let lib = g.config.lib_path.clone();
        crate::objsave::crash_rentsave(g, ch, 0);
        house_crashsave(g, room_vnum);
    }

    // Announce departure first.
    act(
        g,
        "$n has quit the game. (bed)",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    // Close every other socket bound to this same player (anti-dupe), then this
    // one — the loop performs save_char + extract_char on close.
    let idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
    let my_conn = g.get_char(ch).and_then(|c| c.desc);
    if idnum >= 0 {
        let conns: Vec<ConnId> = g.descriptors.keys().copied().collect();
        for conn in conns {
            if Some(conn) == my_conn {
                continue;
            }
            let other = g.descriptors.get(&conn).and_then(|d| d.character);
            if let Some(oc) = other {
                if g.get_char(oc).map(|c| c.idnum).unwrap_or(-2) == idnum {
                    if let Some(d) = g.descriptors.get_mut(&conn) {
                        d.state = crate::connection::ConState::Close;
                    }
                }
            }
        }
    }
    if let Some(conn) = my_conn {
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.state = crate::connection::ConState::Close;
        }
    }
}

// ===========================================================================
// House_save_all + House_can_enter (public boot/heartbeat/movement hooks).
// ===========================================================================

/// House_save_all(): crash-save every house whose room carries ROOM_HOUSE_CRASH.
/// Call from the periodic save heartbeat (C: House_save_all in the autosave).
pub fn house_save_all(g: &mut GameState) {
    let vnums: Vec<RoomVnum> = houses().lock().unwrap().iter().map(|h| h.vnum).collect();
    for vnum in vnums {
        if let Some(rnum) = g.real_room(vnum) {
            if room_flag_isset(g, rnum, ROOM_HOUSE_CRASH) {
                house_crashsave(g, vnum);
            }
        }
    }
}

/// house_for_owner(idnum): the vnum of the house owned by `idnum`, or None.
/// Mirrors spell_home's scan (spells.c): walk the control table and keep the
/// *last* house whose owner matches, exactly as the C loop overwrites `homenum`
/// on every match. Used by spell_home to teleport an owner to their house.
pub fn house_for_owner(idnum: i64) -> Option<RoomVnum> {
    if idnum < 0 {
        return None;
    }
    let table = houses().lock().unwrap();
    let mut found: Option<RoomVnum> = None;
    for h in table.iter() {
        if h.owner == idnum {
            found = Some(h.vnum);
        }
    }
    found
}

/// True if the control table records `vnum` as a house owned by `owner_idnum`.
pub fn house_owned_by(vnum: RoomVnum, owner_idnum: i64) -> bool {
    if owner_idnum < 0 {
        return false;
    }
    let table = houses().lock().unwrap();
    table
        .iter()
        .any(|h| h.vnum == vnum && h.owner == owner_idnum)
}

/// House_can_enter(ch, house): true if `ch` may enter the house at vnum `house`.
/// GRGOD+ and non-houses always pass. Consulted by the movement gate.
pub fn house_can_enter(g: &GameState, ch: CharId, house: RoomVnum) -> bool {
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    if level >= LVL_GRGOD {
        return true;
    }
    let i = match find_house(house) {
        Some(i) => i,
        None => return true,
    };
    let table = houses().lock().unwrap();
    let h = &table[i];
    let my_idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
    match h.mode {
        x if x == HOUSE_PRIVATE => {
            if my_idnum == h.owner {
                return true;
            }
            if h.guests.iter().any(|&gx| gx == my_idnum) {
                return true;
            }
            false
        }
        x if x == HOUSE_OPEN => true,
        _ => false,
    }
}

#[cfg(test)]
pub fn set_test_houses(house_records: Vec<(RoomVnum, i64)>) {
    let mut table = houses().lock().unwrap();
    table.clear();
    for (vnum, owner) in house_records {
        let mut rec = HouseControlRec::blank();
        rec.vnum = vnum;
        rec.owner = owner;
        rec.mode = HOUSE_PRIVATE;
        table.push(rec);
    }
}

// ===========================================================================
// Small utilities.
// ===========================================================================

/// now(): current unix time in seconds.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// fmt_date(): "Mon Day Year"-style 10-char prefix of asctime(localtime(t)),
/// matching C's `*(timestr+10)=0` truncation (e.g. "Sun Jun 15"). We render in
/// UTC via chrono to avoid a libc localtime dependency.
fn fmt_date(unix: i64) -> String {
    use chrono::{TimeZone, Utc};
    match Utc.timestamp_opt(unix, 0).single() {
        Some(dt) => dt.format("%a %b %e").to_string(),
        None => "Unknown".to_string(),
    }
}
