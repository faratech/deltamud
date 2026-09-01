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
// in lib/house/<vnum>.house. The port auto-detects those exact x86-64 LP64
// layouts as well as its existing line-oriented Rust formats, and preserves
// the detected format on every atomic rewrite. The control file additionally
// caches the owner/guest *names* in its Rust representation
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

use crate::act::{ActArg, To, act};
use crate::interpreter::{half_chop, is_abbrev, one_argument, search_block};
use crate::object::{
    ExtraFlags, Object, ObjectAffect, ObjectGraphOrder, ObjectListOrder, ObjectType, WearFlags,
    walk_object_graph, walk_object_lists_postorder,
};
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
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

const HCONTROL_FORMAT: &str = "Usage: hcontrol build <house vnum> <exit direction> <player name>\r\n\
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

fn hcontrol_format() -> &'static Mutex<crate::cformat::PersistenceFormat> {
    static FORMAT: OnceLock<Mutex<crate::cformat::PersistenceFormat>> = OnceLock::new();
    FORMAT.get_or_init(|| Mutex::new(crate::cformat::default_persistence_format()))
}

fn house_object_formats() -> &'static Mutex<HashMap<RoomVnum, crate::cformat::PersistenceFormat>> {
    static FORMATS: OnceLock<Mutex<HashMap<RoomVnum, crate::cformat::PersistenceFormat>>> =
        OnceLock::new();
    FORMATS.get_or_init(|| Mutex::new(HashMap::new()))
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
    let table = crate::lock_ok::lock(&houses());
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
    let table = crate::lock_ok::lock(&houses());
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
            g.principal_authority(id)
                .filter(|authority| authority.is_authenticated_player())
                .is_some_and(|authority| {
                    authority.authority >= i32::from(min_level)
                        && authority.authority >= i32::from(LVL_IMMORT)
                })
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
    let table = crate::lock_ok::lock(&houses());
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
    let table = crate::lock_ok::lock(&houses());
    let format = *crate::lock_ok::lock(&hcontrol_format());
    let bytes = match format {
        crate::cformat::PersistenceFormat::C => {
            let records: Vec<_> = table
                .iter()
                .map(|h| crate::cformat::CHouseControlRec {
                    vnum: h.vnum as i64,
                    atrium: h.atrium as i64,
                    exit_num: h.exit_num as i64,
                    built_on: h.built_on,
                    mode: h.mode,
                    owner: h.owner,
                    guests: h.guests.clone(),
                    last_payment: h.last_payment,
                })
                .collect();
            crate::cformat::encode_hcontrol(&records)
        }
        crate::cformat::PersistenceFormat::Rust => {
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
            out.into_bytes()
        }
    };
    drop(table);
    if crate::cformat::atomic_write(&path, &bytes).is_err() {
        eprintln!("SYSERR: Unable to open house control file");
    }
}

/// Parse the text control file into records (without world validation).
///
/// A recognizable Rust file may contain one damaged record among otherwise
/// valid houses. Reject and log that record, but retain the remaining records;
/// no present numeric token is allowed to alias to a default value.
fn parse_control_file(text: &str) -> Option<Vec<HouseControlRec>> {
    let mut recs = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    let mut saw_end = false;
    while let Some((line_number, line)) = lines.next() {
        let line = line.trim();
        if line == "$" || line.is_empty() {
            if line == "$" {
                saw_end = true;
                break;
            }
            continue;
        }
        let Some(header) = line.strip_prefix("H ") else {
            return None;
        };
        let parts: Vec<&str> = header.split_whitespace().collect();
        let mut rec = if parts.len() == 7 {
            (|| {
                let mut rec = HouseControlRec::blank();
                rec.vnum = parts[0].parse().ok()?;
                rec.atrium = parts[1].parse().ok()?;
                rec.exit_num = parts[2].parse().ok()?;
                rec.built_on = parts[3].parse().ok()?;
                rec.mode = parts[4].parse().ok()?;
                rec.owner = parts[5].parse().ok()?;
                rec.last_payment = parts[6].parse().ok()?;
                Some(rec)
            })()
        } else {
            None
        };
        let mut record_valid = rec.is_some();

        // Optional O / G lines.
        if let Some((_, next)) = lines.peek() {
            if next.trim_start().starts_with('O') {
                let (_, oline) = lines.next().expect("peeked owner line");
                let name = oline.trim().strip_prefix("O ").unwrap_or("").trim();
                if name.is_empty() {
                    record_valid = false;
                } else if name != "*" {
                    if let Some(rec) = rec.as_mut() {
                        rec.owner_name = name.to_lowercase();
                    }
                }
            }
        }
        if let Some((_, next)) = lines.peek() {
            if next.trim_start().starts_with('G') {
                let (_, gline) = lines.next().expect("peeked guest line");
                let guest_fields: Vec<&str> = gline
                    .trim()
                    .strip_prefix("G ")
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                // toks[0] = count, then id:name pairs.
                let declared = guest_fields
                    .first()
                    .and_then(|field| field.parse::<usize>().ok());
                if declared != Some(guest_fields.len().saturating_sub(1)) {
                    record_valid = false;
                }
                let mut guests = Vec::new();
                let mut guest_names = Vec::new();
                for tok in guest_fields.iter().skip(1) {
                    let (idpart, namepart) = match tok.split_once(':') {
                        Some((a, b)) => (a, b),
                        None => (*tok, "*"),
                    };
                    match idpart.parse::<i64>() {
                        Ok(gid) => {
                            guests.push(gid);
                            guest_names.push(if namepart == "*" {
                                String::new()
                            } else {
                                namepart.to_lowercase()
                            });
                        }
                        Err(_) => record_valid = false,
                    }
                }
                if let Some(rec) = rec.as_mut() {
                    if record_valid {
                        rec.guests = guests;
                        rec.guest_names = guest_names;
                    } else {
                        rec.guests.clear();
                        rec.guest_names.clear();
                    }
                }
            }
        }
        if record_valid {
            if let Some(rec) = rec {
                recs.push(rec);
            }
        } else {
            log::warn!(
                "SYSERR: rejected malformed Rust hcontrol record at line {}: field invalid or out of range",
                line_number + 1
            );
        }
    }

    // Bytes after the terminator make this structurally unlike the Rust text
    // format. Keeping this distinction prevents an ASCII-looking C record from
    // being misdetected.
    if !saw_end || lines.any(|(_, line)| !line.trim().is_empty()) {
        None
    } else {
        Some(recs)
    }
}

fn control_from_c(r: crate::cformat::CHouseControlRec) -> Option<HouseControlRec> {
    Some(HouseControlRec {
        vnum: RoomVnum::try_from(r.vnum).ok()?,
        atrium: RoomVnum::try_from(r.atrium).ok()?,
        exit_num: i32::try_from(r.exit_num).ok()?,
        built_on: r.built_on,
        mode: r.mode,
        owner: r.owner,
        owner_name: String::new(),
        guests: r.guests,
        guest_names: Vec::new(),
        last_payment: r.last_payment,
    })
}

fn decode_control_bytes(
    bytes: &[u8],
) -> Option<(Vec<HouseControlRec>, crate::cformat::PersistenceFormat)> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        if let Some(parsed) = parse_control_file(text) {
            return Some((parsed, crate::cformat::PersistenceFormat::Rust));
        }
    }
    let records = crate::cformat::decode_hcontrol(bytes)?;
    let mut parsed = Vec::with_capacity(records.len());
    for (record_number, record) in records.into_iter().enumerate() {
        if let Some(record) = control_from_c(record) {
            parsed.push(record);
        } else {
            log::warn!(
                "SYSERR: rejected C hcontrol record {} with a room or exit number outside the supported 32-bit range",
                record_number + 1
            );
        }
    }
    Some((parsed, crate::cformat::PersistenceFormat::C))
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
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => {
            mudlog(g, "House control file does not exist.", NRM_INVIS);
            return;
        }
    };

    // Detect from raw bytes before attempting UTF-8. C records commonly
    // contain invalid UTF-8 in padding/fixed fields and must never hit
    // read_to_string first (#95).
    let (parsed, detected) = if let Some(decoded) = decode_control_bytes(&bytes) {
        decoded
    } else {
        mudlog(g, "SYSERR: House control file is corrupt.", NRM_INVIS);
        return;
    };
    *crate::lock_ok::lock(&hcontrol_format()) = detected;
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
            mudlog(
                g,
                "SYSERR: House owner does not exist - skipping house.",
                NRM_INVIS,
            );
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
        let mut table = crate::lock_ok::lock(&houses());
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
//           <timer> <v0> <v1> <v2> <v3> <min_level> <bitvector>
//           <curr_slots> <total_slots> <#affects> [loc mod ...] [obj_class]
//           |<name>|<short>|<long>|<action>
// `depth` tracks container nesting (0 = top of room). On load we rebuild the
// tree from depth, skipping ITEM_KEY and ITEM_NORENT items exactly as C does.

fn stored_object_weight(g: &GameState, oid: ObjId) -> i32 {
    let Some(obj) = g.get_obj(oid) else {
        return 0;
    };
    obj.contains.iter().fold(obj.weight, |weight, child| {
        weight.saturating_sub(g.get_obj(*child).map(|o| o.weight).unwrap_or(0))
    })
}

fn rust_house_object_file(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut saw_record = false;
    for line in text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
    {
        let Some((head, _)) = line.split_once('|') else {
            return false;
        };
        let fields: Vec<_> = head.split_whitespace().collect();
        // This is format recognition, not record validation. Keep malformed
        // or overflowing numeric fields in the Rust path so parse_obj_line can
        // reject/log only that record while later valid records still load.
        if fields.len() < 14 {
            return false;
        }
        saw_record = true;
    }
    saw_record
}

fn detect_house_object_format(bytes: &[u8]) -> Option<crate::cformat::PersistenceFormat> {
    if bytes.is_empty() {
        return None;
    }
    if rust_house_object_file(bytes) {
        return Some(crate::cformat::PersistenceFormat::Rust);
    }
    let elems = crate::cformat::decode_obj_file(bytes)?;
    elems
        .iter()
        .all(|elem| i32::try_from(elem.item_number).is_ok())
        .then_some(crate::cformat::PersistenceFormat::C)
}

fn c_house_elem(g: &GameState, oid: ObjId) -> Option<crate::cformat::CObjFileElem> {
    let obj = g.get_obj(oid)?;
    Some(crate::cformat::obj_to_c_elem(
        i64::from(obj.item_number),
        0,
        obj.curr_slots,
        obj.total_slots,
        obj.values,
        obj.extra_flags.bits() as i32,
        stored_object_weight(g, oid),
        obj.timer,
        obj.bitvector,
        obj.min_level,
        &obj.affects,
    ))
}

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
        stored_object_weight(g, oid),
        o.cost,
        o.rent,
        o.timer,
        o.values[0],
        o.values[1],
        o.values[2],
        o.values[3],
        o.min_level,  // min_level: the equip gate (see objsave.rs #383)
        o.bitvector,  // affect bitvector
        o.curr_slots, // durability counters (#233)
        o.total_slots,
        o.affects.len()
    ));
    for a in &o.affects {
        out.push_str(&format!(" {} {}", a.location, a.modifier));
    }
    out.push_str(&format!(" {}", o.obj_class));
    out.push_str(&format!(
        "|{}|{}|{}|{}\n",
        o.name.replace('|', "/").replace('\n', " "),
        o.short_description.replace('|', "/").replace('\n', " "),
        o.description.replace('|', "/").replace('\n', " "),
        o.action_description
            .as_deref()
            .unwrap_or("")
            .replace('|', "/")
            .replace('\n', " ")
    ));
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
    let contents = g.room(rnum).contents.clone();
    let format = crate::lock_ok::lock(&house_object_formats())
        .get(&vnum)
        .copied()
        .or_else(|| {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| detect_house_object_format(&bytes))
        })
        .unwrap_or_else(crate::cformat::default_persistence_format);
    let bytes = match format {
        crate::cformat::PersistenceFormat::Rust => {
            let mut out = String::new();
            let walk = walk_object_graph(
                contents,
                ObjectGraphOrder::Preorder,
                "House_crashsave (Rust)",
                |oid| g.get_obj(oid).map(|o| o.contains.clone()),
            );
            if walk.malformed() {
                log::warn!(
                    "SYSERR: refusing partial Rust house snapshot for room {} because its object graph is malformed",
                    vnum
                );
                return;
            }
            for visit in walk.visits {
                serialize_obj_line(g, visit.id, visit.depth, &mut out);
            }
            out.into_bytes()
        }
        crate::cformat::PersistenceFormat::C => {
            let walk = walk_object_lists_postorder(
                vec![contents],
                ObjectListOrder::ContainsThenNext,
                "House_save (C)",
                |oid| g.get_obj(oid).map(|o| o.contains.clone()),
            );
            if walk.malformed() {
                log::warn!(
                    "SYSERR: refusing partial C house snapshot for room {} because its object graph is malformed",
                    vnum
                );
                return;
            }
            let Some(elems): Option<Vec<_>> = walk
                .visits
                .into_iter()
                .map(|visit| c_house_elem(g, visit.id))
                .collect()
            else {
                log::warn!(
                    "SYSERR: refusing partial C house snapshot for room {} because an object could not be encoded",
                    vnum
                );
                return;
            };
            crate::cformat::encode_obj_file(&elems)
        }
    };
    if crate::cformat::atomic_write(&path, &bytes).is_err() {
        eprintln!("SYSERR: Error saving house file");
        return;
    }
    crate::lock_ok::lock(&house_object_formats()).insert(vnum, format);
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
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return false, // no file found
    };

    let format = if bytes.is_empty() {
        crate::lock_ok::lock(&house_object_formats())
            .get(&vnum)
            .copied()
            .unwrap_or_else(crate::cformat::default_persistence_format)
    } else {
        let Some(format) = detect_house_object_format(&bytes) else {
            mudlog(g, "SYSERR: Corrupt house object file.", NRM_INVIS);
            return false;
        };
        format
    };
    crate::lock_ok::lock(&house_object_formats()).insert(vnum, format);

    if format == crate::cformat::PersistenceFormat::C {
        let Some(elems) = crate::cformat::decode_obj_file(&bytes) else {
            mudlog(g, "SYSERR: Corrupt C-format house object file.", NRM_INVIS);
            return false;
        };
        for elem in elems {
            let Ok(vnum) = ObjVnum::try_from(elem.item_number) else {
                continue;
            };
            let Some(oid) = g.load_object(vnum) else {
                continue;
            };
            if let Some(obj) = g.get_obj_mut(oid) {
                obj.values = elem.value;
                obj.extra_flags = ExtraFlags::from_bits_retain(u64::from(elem.extra_flags as u32));
                obj.weight = elem.weight;
                obj.timer = elem.timer;
                obj.bitvector = elem.bitvector;
                obj.curr_slots = elem.curr_slots;
                obj.total_slots = elem.total_slots;
                obj.min_level = elem.min_level;
                obj.affects = elem
                    .affected
                    .iter()
                    .filter(|(location, _)| *location != 0)
                    .map(|(location, modifier)| ObjectAffect {
                        location: i32::from(*location),
                        modifier: i32::from(*modifier),
                    })
                    .collect();
            }
            let forbidden = g
                .get_obj(oid)
                .map(|obj| {
                    obj.obj_type == ObjectType::Key || obj.extra_flags.contains(ExtraFlags::NO_RENT)
                })
                .unwrap_or(true);
            if forbidden {
                g.extract_obj(oid);
            } else {
                // C House_load deliberately ignores obj_file_elem.locate and
                // flattens every record into the room.
                g.obj_to_room(oid, rnum);
            }
        }
        return true;
    }

    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };

    // Stack of (depth, ObjId) for rebuilding container nesting.
    let mut stack: Vec<(usize, ObjId)> = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (oid, depth, is_key_or_norent) = match parse_obj_line(g, line) {
            Some(t) => t,
            None => {
                log::warn!(
                    "SYSERR: rejected malformed Rust house object record for house {} at line {}",
                    vnum,
                    line_number + 1
                );
                continue;
            }
        };

        // Pop stack entries whose depth >= this object's depth.
        while let Some(&(d, _)) = stack.last() {
            if d >= depth {
                stack.pop();
            } else {
                break;
            }
        }

        if is_key_or_norent {
            g.extract_obj(oid);
            continue;
        }

        if depth == 0 {
            g.obj_to_room(oid, rnum);
            stack.push((depth, oid));
        } else {
            // Nested object: parent is the current stack top.
            match stack.last().copied() {
                Some((_, parent)) => {
                    g.obj_to_obj(oid, parent);
                    stack.push((depth, oid));
                }
                None => {
                    // Orphaned nesting (corrupt file) — drop to room as top.
                    g.obj_to_room(oid, rnum);
                    stack.push((0, oid));
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
    // All fixed columns are present in both versions. Reject the whole record
    // when any one is malformed or overflowing rather than manufacturing an
    // object with default vnum/type/flags/cost values.
    let depth: usize = nums[0].parse().ok()?;
    let vnum: ObjVnum = nums[1].parse().ok()?;
    let ty: i32 = nums[2].parse().ok()?;
    let wear: u32 = nums[3].parse().ok()?;
    let extra: u64 = nums[4].parse().ok()?;
    let weight: i32 = nums[5].parse().ok()?;
    let cost: i32 = nums[6].parse().ok()?;
    let rent: i32 = nums[7].parse().ok()?;
    let timer: i32 = nums[8].parse().ok()?;
    let v0: i32 = nums[9].parse().ok()?;
    let v1: i32 = nums[10].parse().ok()?;
    let v2: i32 = nums[11].parse().ok()?;
    let v3: i32 = nums[12].parse().ok()?;
    // Records written before the #233 fix lack level/bitvector/curr_slots/
    // total_slots (14 fixed head numbers); new records carry 18. Try the new
    // layout first to preserve current files, then the exact legacy layout.
    // Exact token-count checks keep an invalid field from shifting later data.
    let parse_tail = |extended: bool| -> Option<(i32, i64, i32, i32, Vec<ObjectAffect>, i32)> {
        let (level, bitvector, curr_slots, total_slots, naff, mut idx): (
            i32,
            i64,
            i32,
            i32,
            usize,
            usize,
        ) = if extended {
            if nums.len() < 18 {
                return None;
            }
            (
                nums[13].parse().ok()?,
                nums[14].parse().ok()?,
                nums[15].parse().ok()?,
                nums[16].parse().ok()?,
                nums[17].parse().ok()?,
                18,
            )
        } else {
            (0, 0, 0, 0, nums[13].parse().ok()?, 14)
        };
        let affects_end = idx.checked_add(naff.checked_mul(2)?)?;
        if !(nums.len() == affects_end || nums.len() == affects_end.checked_add(1)?) {
            return None;
        }
        let mut affects = Vec::with_capacity(naff);
        for _ in 0..naff {
            affects.push(ObjectAffect {
                location: nums[idx].parse().ok()?,
                modifier: nums[idx + 1].parse().ok()?,
            });
            idx += 2;
        }
        let obj_class = match nums.get(idx) {
            Some(value) => value.parse().ok()?,
            None => -1,
        };
        Some((
            level,
            bitvector,
            curr_slots,
            total_slots,
            affects,
            obj_class,
        ))
    };
    let (level, bitvector, curr_slots, total_slots, affects, obj_class) =
        parse_tail(true).or_else(|| parse_tail(false))?;

    // name|short|long|action from the tail. Older files have three fields.
    let mut tparts = tail.splitn(4, '|');
    let name = tparts.next().unwrap_or("").to_string();
    let short = tparts.next().unwrap_or("").to_string();
    let long = tparts.next().unwrap_or("").to_string();
    let action = tparts.next().unwrap_or("").to_string();

    let mut obj = Object::new(vnum, name, short);
    obj.description = long;
    if !action.is_empty() {
        obj.action_description = Some(action);
    }
    obj.obj_type = ObjectType::from_i32(ty);
    obj.wear_flags = WearFlags::from_bits_retain(wear);
    obj.extra_flags = ExtraFlags::from_bits_retain(extra);
    obj.weight = weight;
    obj.cost = cost;
    obj.rent = rent;
    obj.timer = timer;
    obj.values = [v0, v1, v2, v3];
    obj.affects = affects;
    obj.min_level = level;
    obj.level = level.clamp(0, u8::MAX as i32) as u8;
    obj.bitvector = bitvector;
    obj.curr_slots = curr_slots;
    obj.total_slots = total_slots;
    obj.obj_class = obj_class;

    let is_key_or_norent = obj.obj_type == ObjectType::Key || (extra & ITEM_NORENT) != 0;
    let oid = g.create_obj(obj);
    Some((oid, depth, is_key_or_norent))
}

/// House_delete_file(): remove a house's object file.
fn house_delete_file(g: &GameState, vnum: RoomVnum) {
    if let Some(path) = house_filename(&g.config.lib_path, vnum) {
        let _ = std::fs::remove_file(path);
    }
    crate::lock_ok::lock(&house_object_formats()).remove(&vnum);
}

/// House_listrent(): list the objects stored in a house file to `ch`.
fn house_listrent(g: &mut GameState, ch: CharId, vnum: RoomVnum) {
    let path = match house_filename(&g.config.lib_path, vnum) {
        Some(p) => p,
        None => return,
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => {
            g.send_to_char(ch, &format!("No objects on file for house #{}.\r\n", vnum));
            return;
        }
    };
    let mut buf = String::new();
    if detect_house_object_format(&bytes) == Some(crate::cformat::PersistenceFormat::C) {
        if let Some(elems) = crate::cformat::decode_obj_file(&bytes) {
            for elem in elems {
                let Some(proto) = i32::try_from(elem.item_number)
                    .ok()
                    .and_then(|item| g.obj_protos.get(&item))
                else {
                    continue;
                };
                buf.push_str(&format!(
                    " [{:5}] ({:5}au) {}\r\n",
                    elem.item_number, proto.rent, proto.short_desc
                ));
            }
        }
        g.send_to_char(ch, &buf);
        return;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        g.send_to_char(
            ch,
            &format!("House #{} has a corrupt object file.\r\n", vnum),
        );
        return;
    };
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
        let (Ok(vnum_o), Ok(rent)) = (nums[1].parse::<i32>(), nums[7].parse::<i32>()) else {
            log::warn!(
                "SYSERR: rejected malformed Rust house listing record for house {}",
                vnum
            );
            continue;
        };
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
    let table: Vec<HouseControlRec> = crate::lock_ok::lock(&houses()).clone();
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
    let table: Vec<HouseControlRec> = crate::lock_ok::lock(&houses()).clone();
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

fn hcontrol_vnum(g: &mut GameState, ch: CharId, value: &str) -> Option<RoomVnum> {
    match crate::text::parse_i32_atoi(value) {
        Ok(value) => Some(value),
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(
                ch,
                "That room number is outside the supported 32-bit range.\r\n",
            );
            None
        }
        Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
    }
}

fn hcontrol_build_house(g: &mut GameState, ch: CharId, arg: &str) {
    if crate::lock_ok::lock(&houses()).len() >= MAX_HOUSES {
        g.send_to_char(ch, "Max houses already defined.\r\n");
        return;
    }

    // first arg: house's vnum
    let (a1, rest) = one_argument(arg);
    if a1.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let Some(virt_house) = hcontrol_vnum(g, ch, &a1) else {
        return;
    };
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
    // Building immediately crash-saves the room. Refuse existing contents so
    // zone-reset fixtures cannot be baked into the new house file and then
    // duplicated after reset plus reboot.
    if !g.room(real_house).contents.is_empty() {
        g.send_to_char(
            ch,
            "The house room must be empty before it can be built.\r\n",
        );
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

    crate::lock_ok::lock(&houses()).push(temp);

    room_flag_set(g, real_house, ROOM_HOUSE | ROOM_PRIVATE);
    room_flag_set(g, real_atrium, ROOM_ATRIUM);
    house_crashsave(g, virt_house);

    g.send_to_char(ch, "House built.  Mazel tov!\r\n");
    house_save_control(g);
}

fn hcontrol_crashsave_house(g: &mut GameState, ch: CharId, arg: &str) {
    if crate::lock_ok::lock(&houses()).len() >= MAX_HOUSES {
        g.send_to_char(ch, "Max crashsaveables/houses already defined.\r\n");
        return;
    }
    let (a1, _) = one_argument(arg);
    if a1.is_empty() {
        g.send_to_char(ch, HCONTROL_FORMAT);
        return;
    }
    let Some(virt_house) = hcontrol_vnum(g, ch, &a1) else {
        return;
    };
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

    crate::lock_ok::lock(&houses()).push(temp);

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
    let Some(target_vnum) = hcontrol_vnum(g, ch, arg.trim()) else {
        return;
    };
    let i = match find_house(target_vnum) {
        Some(i) => i,
        None => {
            g.send_to_char(ch, "Unknown house.\r\n");
            return;
        }
    };

    let (atrium_vnum, house_vnum) = {
        let table = crate::lock_ok::lock(&houses());
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

    crate::lock_ok::lock(&houses()).remove(i);

    g.send_to_char(ch, "House deleted.\r\n");
    house_save_control(g);

    // Re-set ROOM_ATRIUM on every surviving house's atrium (in case the
    // destroyed house shared an atrium with another). --JE 9/19/94
    let atriums: Vec<RoomVnum> = crate::lock_ok::lock(&houses())
        .iter()
        .map(|h| h.atrium)
        .collect();
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
    let Some(target_vnum) = hcontrol_vnum(g, ch, arg.trim()) else {
        return;
    };
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
        let mut table = crate::lock_ok::lock(&houses());
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
    let Some(virt_house) = hcontrol_vnum(g, ch, &a1) else {
        return;
    };
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
        let mut table = crate::lock_ok::lock(&houses());
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
    let owner = crate::lock_ok::lock(&houses())[i].owner;
    if my_idnum != owner {
        g.send_to_char(ch, "Only the primary owner can set guests.\r\n");
        return;
    }

    if arg.is_empty() {
        // List guests.
        let guests = crate::lock_ok::lock(&houses())[i].guests.clone();
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
    let mut full = false;
    {
        let mut table = crate::lock_ok::lock(&houses());
        if let Some(pos) = table[i].guests.iter().position(|&gx| gx == id) {
            table[i].guests.remove(pos);
            if pos < table[i].guest_names.len() {
                table[i].guest_names.remove(pos);
            }
            deleted = true;
        } else if table[i].guests.len() >= MAX_GUESTS {
            // The C array has a hard 100-entry capacity. Reject guest #101
            // before mutating or saving so the command never reports an add
            // which was immediately truncated away (#395).
            full = true;
        } else {
            table[i].guests.push(id);
            table[i].guest_names.push(arg.to_lowercase());
        }
    }
    if full {
        g.send_to_char(ch, "Your house guest list is full.\r\n");
        return;
    }
    house_save_control(g);
    if deleted {
        g.send_to_char(ch, "Guest deleted.\r\n");
    } else {
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

/// Crash_report_unbedables(): report unrentable items below `roots`; returns
/// the count found.
fn report_unbedables(g: &mut GameState, ch: CharId, roots: &[ObjId]) -> i32 {
    let walk = walk_object_graph(
        roots.iter().copied(),
        ObjectGraphOrder::Preorder,
        "Crash_report_unbedables",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    );
    let mut count = 0;
    for visit in walk.visits {
        if is_unrentable(g, visit.id) {
            count += 1;
            act(
                g,
                "You cannot go to bed with $p.",
                false,
                ch,
                Some(visit.id),
                ActArg::None,
                To::Char,
            );
        }
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
    let owner = crate::lock_ok::lock(&houses())[i].owner;
    if my_idnum != owner {
        g.send_to_char(ch, "Only the primary owner can go to bed in the house.\r\n");
        return;
    }

    // Report (and refuse on) any unbedable items in inventory or equipment.
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let mut roots = carrying;
    let worn: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    roots.extend(worn);
    let nobed = report_unbedables(g, ch, &roots);
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
    let vnums: Vec<RoomVnum> = crate::lock_ok::lock(&houses())
        .iter()
        .map(|h| h.vnum)
        .collect();
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
    let table = crate::lock_ok::lock(&houses());
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
    let table = crate::lock_ok::lock(&houses());
    table
        .iter()
        .any(|h| h.vnum == vnum && h.owner == owner_idnum)
}

/// House_can_enter(ch, house): true if `ch` may enter the house at vnum `house`.
/// GRGOD+ and non-houses always pass. Consulted by the movement gate.
pub fn house_can_enter(g: &GameState, ch: CharId, house: RoomVnum) -> bool {
    // The privacy override is administrative authority, so connected PCs use
    // persisted trust (including through switch) rather than display level.
    // Descriptorless NPC levels remain available to ordinary movement logic,
    // but never confer this private-property override.
    if has_house_privacy_override(g, ch) {
        return true;
    }
    let i = match find_house(house) {
        Some(i) => i,
        None => return true,
    };
    let table = crate::lock_ok::lock(&houses());
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

fn has_house_privacy_override(g: &GameState, ch: CharId) -> bool {
    let Some(authority) = g
        .principal_authority(ch)
        .filter(|authority| authority.is_authenticated_player())
    else {
        return false;
    };
    let Some(principal) = g.get_char(authority.principal) else {
        return false;
    };
    !g.authority_quarantine.contains(&principal.idnum)
        && authority.authority >= i32::from(LVL_GRGOD)
}

#[cfg(test)]
pub fn set_test_houses(house_records: Vec<(RoomVnum, i64)>) {
    let mut table = crate::lock_ok::lock(&houses());
    table.clear();
    for (vnum, owner) in house_records {
        let mut rec = HouseControlRec::blank();
        rec.vnum = vnum;
        rec.owner = owner;
        rec.mode = HOUSE_PRIVATE;
        table.push(rec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::room::{Exit, Room};
    use crate::types::ConnId;
    use crate::world::ObjectProto;

    fn temp_lib(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deltamud-house-{}-{}-{}",
            std::process::id(),
            name,
            stamp
        ))
    }

    fn proto(vnum: ObjVnum, kind: ObjectType) -> ObjectProto {
        ObjectProto {
            vnum,
            name: format!("object {}", vnum),
            short_desc: format!("object {}", vnum),
            description: format!("Object {} is here.", vnum),
            obj_type: kind,
            wear_flags: WearFlags::TAKE,
            extra_flags: ExtraFlags::empty(),
            weight: 1,
            cost: 25,
            rent: 5,
            values: [0; 4],
            curr_slots: 0,
            total_slots: 0,
            obj_class: -1,
            min_level: 0,
            bitvector: 0,
            action_description: String::new(),
            affects: Vec::new(),
            ex_descriptions: Vec::new(),
        }
    }

    fn connected_player(
        g: &mut GameState,
        conn: ConnId,
        name: &str,
        level: Level,
        trust: i32,
    ) -> CharId {
        let mut character = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        character.player.level = level;
        character.trust = trust;
        character.idnum = conn.0 as i64;
        character.desc = Some(conn);
        let character = g.create_char(character);
        let mut descriptor = Descriptor::new(conn, "house-authority.test".to_string());
        descriptor.character = Some(character);
        g.descriptors.insert(conn, descriptor);
        character
    }

    #[test]
    fn house_privacy_override_uses_trust_and_switched_aliases_fail_closed() {
        let mut g = GameState::new(Config::default());
        let display = connected_player(&mut g, ConnId(901), "Display", LVL_IMPL, 1);
        let trusted = connected_player(&mut g, ConnId(902), "Trusted", 1, i32::from(LVL_GRGOD));
        assert!(!has_house_privacy_override(&g, display));
        assert!(has_house_privacy_override(&g, trusted));

        let mut high_level_npc = Character::new_npc(7_199);
        high_level_npc.player.level = LVL_IMPL;
        let high_level_npc = g.create_char(high_level_npc);
        assert!(!has_house_privacy_override(&g, high_level_npc));

        g.authority_quarantine.insert(ConnId(902).0 as i64);
        assert!(!has_house_privacy_override(&g, trusted));
        g.authority_quarantine.remove(&(ConnId(902).0 as i64));

        let mut body = Character::new_npc(7_200);
        body.desc = Some(ConnId(902));
        let body = g.create_char(body);
        g.get_char_mut(trusted).unwrap().desc = None;
        {
            let descriptor = g.descriptors.get_mut(&ConnId(902)).unwrap();
            descriptor.character = Some(body);
            descriptor.original = Some(trusted);
        }
        assert!(has_house_privacy_override(&g, body));

        let mut alias_body = Character::new_npc(7_201);
        alias_body.desc = Some(ConnId(903));
        let alias_body = g.create_char(alias_body);
        let mut duplicate = Descriptor::new(ConnId(903), "house-authority.test".to_string());
        duplicate.character = Some(alias_body);
        duplicate.original = Some(trusted);
        g.descriptors.insert(ConnId(903), duplicate);

        assert!(!has_house_privacy_override(&g, body));
    }

    #[test]
    fn hcontrol_build_refuses_room_with_reset_contents() {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!(
            "deltamud-house-build-{}-{}",
            std::process::id(),
            unique
        ));
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let house = g.add_room(Room::new(1_900_500, 19_005, "House".into(), String::new()));
        let atrium = g.add_room(Room::new(1_900_501, 19_005, "Atrium".into(), String::new()));
        g.room_mut(house).exits[1] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: -1,
            to_room: 1_900_501,
        });
        g.room_mut(atrium).exits[3] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: -1,
            to_room: 1_900_500,
        });
        g.room_mut(house).contents.push(ObjId(999));

        let mut owner = Character::new_player("Owner".into(), Class::Warrior, Race::Human);
        owner.idnum = 42;
        let owner = g.create_char(owner);
        g.update_player_index(42, "Owner", 1, 0, "");
        hcontrol_build_house(&mut g, owner, "1900500 east Owner");

        assert!(find_house(1_900_500).is_none());
        assert!(!dir.join("house/1900500.house").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hcontrol_rejects_overflowing_room_numbers_before_lookup() {
        set_test_houses(Vec::new());
        let mut g = GameState::new(Config::default());
        let conn = ConnId(910);
        let mut admin = Character::new_player("Admin".into(), Class::Warrior, Race::Human);
        admin.desc = Some(conn);
        let admin = g.create_char(admin);
        let mut descriptor = Descriptor::new(conn, "test".into());
        descriptor.character = Some(admin);
        g.descriptors.insert(conn, descriptor);

        for command in [
            "build 2147483648 east Admin",
            "crashsave 2147483648",
            "destroy 2147483648",
            "pay 2147483648",
            "update 2147483648 east Admin",
        ] {
            do_hcontrol(&mut g, admin, command, 0);
        }

        assert!(crate::lock_ok::lock(&houses()).is_empty());
        assert_eq!(
            g.descriptors[&conn]
                .outbuf
                .matches("outside the supported 32-bit range")
                .count(),
            5
        );
    }

    #[test]
    fn full_guest_list_rejects_guest_101_without_mutation_or_false_success() {
        let dir = temp_lib("full-guest-list");
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(790_100, 0, "House".into(), String::new()));
        room_flag_set(&mut g, room, ROOM_HOUSE);

        let conn = ConnId(911);
        let mut owner = Character::new_player("Owner".into(), Class::Warrior, Race::Human);
        owner.idnum = 42;
        owner.desc = Some(conn);
        let owner = g.create_char(owner);
        g.char_to_room(owner, room);
        let mut descriptor = Descriptor::new(conn, "test".into());
        descriptor.character = Some(owner);
        g.descriptors.insert(conn, descriptor);
        g.update_player_index(999, "Overflowguest", 1, 0, "");

        set_test_houses(vec![(790_100, 42)]);
        {
            let mut table = crate::lock_ok::lock(&houses());
            table[0].guests = (1..=MAX_GUESTS as i64).collect();
            table[0].guest_names = (1..=MAX_GUESTS)
                .map(|number| format!("guest{number}"))
                .collect();
        }

        do_house(&mut g, owner, "Overflowguest", 0);

        let table = crate::lock_ok::lock(&houses());
        assert_eq!(table[0].guests.len(), MAX_GUESTS);
        assert!(!table[0].guests.contains(&999));
        drop(table);
        let output = &g.descriptors[&conn].outbuf;
        assert!(output.contains("guest list is full"));
        assert!(!output.contains("Guest added"));

        set_test_houses(Vec::new());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn raw_c_hcontrol_starting_with_ascii_h_is_not_misdetected_as_text() {
        let record = crate::cformat::CHouseControlRec {
            vnum: i64::from(b'H'),
            atrium: 1,
            exit_num: 2,
            built_on: 3,
            mode: HOUSE_OPEN,
            owner: 4,
            guests: vec![5],
            last_payment: 6,
        };
        let bytes = crate::cformat::encode_hcontrol(&[record]);
        assert!(std::str::from_utf8(&bytes).is_ok());

        let (decoded, format) = decode_control_bytes(&bytes).unwrap();
        assert_eq!(format, crate::cformat::PersistenceFormat::C);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].vnum, i32::from(b'H'));
        assert_eq!(decoded[0].guests, vec![5]);
    }

    #[test]
    fn existing_rust_hcontrol_is_still_detected() {
        let bytes = b"H 500 501 1 10 0 42 11\nO owner\nG 1 43:guest\n$\n";
        let (decoded, format) = decode_control_bytes(bytes).unwrap();
        assert_eq!(format, crate::cformat::PersistenceFormat::Rust);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].vnum, 500);
        assert_eq!(decoded[0].owner_name, "owner");
        assert_eq!(decoded[0].guests, vec![43]);
    }

    #[test]
    fn rust_hcontrol_rejects_only_the_record_with_an_invalid_numeric_field() {
        let invalid_headers = [
            "H 2147483648 501 1 10 0 42 11",
            "H 500 -2147483649 1 10 0 42 11",
            "H 500 501 2147483648 10 0 42 11",
            "H 500 501 1 9223372036854775808 0 42 11",
            "H 500 501 1 10 -2147483649 42 11",
            "H 500 501 1 10 0 9223372036854775808 11",
            "H 500 501 1 10 0 42 -9223372036854775809",
        ];
        for invalid in invalid_headers {
            let text = format!(
                "{invalid}\nO rejected\nG 0\n\
                 H 600 601 1 10 0 52 11\nO accepted\nG 1 53:guest\n$\n"
            );
            let (decoded, format) = decode_control_bytes(text.as_bytes()).unwrap();
            assert_eq!(format, crate::cformat::PersistenceFormat::Rust);
            assert_eq!(decoded.len(), 1, "invalid header was defaulted: {invalid}");
            assert_eq!(decoded[0].vnum, 600);
            assert_eq!(decoded[0].owner_name, "accepted");
            assert_eq!(decoded[0].guests, vec![53]);
        }

        for invalid_guests in ["G 18446744073709551616", "G 1 9223372036854775808:guest"] {
            let text = format!(
                "H 500 501 1 10 0 42 11\nO rejected\n{invalid_guests}\n\
                 H 600 601 1 10 0 52 11\nO accepted\nG 0\n$\n"
            );
            let (decoded, format) = decode_control_bytes(text.as_bytes()).unwrap();
            assert_eq!(format, crate::cformat::PersistenceFormat::Rust);
            assert_eq!(decoded.len(), 1);
            assert_eq!(decoded[0].vnum, 600);
        }
    }

    #[test]
    fn hcontrol_numeric_boundaries_round_trip_without_aliasing() {
        let text = format!(
            "H {} {} {} {} {} {} {}\nO boundary\nG 1 {}:guest\n$\n",
            i32::MAX,
            i32::MIN,
            i32::MAX,
            i64::MIN,
            i32::MIN,
            i64::MIN,
            i64::MAX,
            i64::MAX
        );
        let (decoded, format) = decode_control_bytes(text.as_bytes()).unwrap();
        assert_eq!(format, crate::cformat::PersistenceFormat::Rust);
        assert_eq!(decoded.len(), 1);
        let record = &decoded[0];
        assert_eq!(record.vnum, i32::MAX);
        assert_eq!(record.atrium, i32::MIN);
        assert_eq!(record.exit_num, i32::MAX);
        assert_eq!(record.built_on, i64::MIN);
        assert_eq!(record.mode, i32::MIN);
        assert_eq!(record.owner, i64::MIN);
        assert_eq!(record.last_payment, i64::MAX);
        assert_eq!(record.guests, vec![i64::MAX]);

        let raw = crate::cformat::encode_hcontrol(&[crate::cformat::CHouseControlRec {
            vnum: i64::from(i32::MAX) + 1,
            atrium: 1,
            exit_num: 2,
            built_on: 3,
            mode: HOUSE_OPEN,
            owner: 4,
            guests: Vec::new(),
            last_payment: 5,
        }]);
        let (decoded, format) = decode_control_bytes(&raw).unwrap();
        assert_eq!(format, crate::cformat::PersistenceFormat::C);
        assert!(decoded.is_empty(), "C vnum overflow wrapped into an i32");
    }

    #[test]
    fn pre_objclass_rust_house_record_remains_readable() {
        let mut g = GameState::new(Config::default());
        let line = "0 321 9 1 0 5 6 7 -1 1 2 3 4 10 11 12 13 0|old|an old object|Old object.";
        let (oid, depth, forbidden) = parse_obj_line(&mut g, line).unwrap();
        let object = g.get_obj(oid).unwrap();
        assert_eq!(depth, 0);
        assert!(!forbidden);
        assert_eq!(object.item_number, 321);
        assert_eq!(object.obj_class, -1);
        assert_eq!(object.min_level, 10);
        assert_eq!(object.total_slots, 13);
    }

    #[test]
    fn legacy_house_record_with_absent_extended_fields_remains_readable() {
        let mut g = GameState::new(Config::default());
        let line = "0 322 9 1 0 5 6 7 -1 1 2 3 4 0|old|an old object|Old object.";
        let (oid, depth, forbidden) = parse_obj_line(&mut g, line).unwrap();
        let object = g.get_obj(oid).unwrap();
        assert_eq!(depth, 0);
        assert!(!forbidden);
        assert_eq!(object.item_number, 322);
        assert_eq!(object.min_level, 0);
        assert_eq!(object.bitvector, 0);
        assert_eq!(object.curr_slots, 0);
        assert_eq!(object.total_slots, 0);
        assert_eq!(object.obj_class, -1);

        // Two legacy affect pairs make the old record as long as the new
        // fixed header. It still must be recognized by its exact old count.
        let line = "0 323 9 1 0 5 6 7 -1 1 2 3 4 2 5 -6 7 -8|old|an old object|Old object.";
        let (oid, _, _) = parse_obj_line(&mut g, line).unwrap();
        let object = g.get_obj(oid).unwrap();
        assert_eq!(
            object
                .affects
                .iter()
                .map(|affect| (affect.location, affect.modifier))
                .collect::<Vec<_>>(),
            vec![(5, -6), (7, -8)]
        );
        assert_eq!(object.min_level, 0);
    }

    #[test]
    fn rust_house_record_rejects_each_present_malformed_numeric_column() {
        let base =
            "0 321 9 1 0 5 6 7 -1 1 2 3 4 10 11 12 13 1 5 -6 3|old|an old object|Old object.";
        let (head, tail) = base.split_once('|').unwrap();
        let fields: Vec<_> = head.split_whitespace().collect();
        for field in 0..fields.len() {
            let mut malformed = fields.clone();
            malformed[field] = "not-a-number";
            let line = format!("{}|{}", malformed.join(" "), tail);
            let mut g = GameState::new(Config::default());
            assert!(
                parse_obj_line(&mut g, &line).is_none(),
                "numeric field {field} was silently defaulted"
            );
            assert!(g.objs.is_empty());
        }

        let mut g = GameState::new(Config::default());
        assert!(
            parse_obj_line(
                &mut g,
                "0 321 9 1 0 5 6 7 -1 1 2 3 4 10 11 12 13 1 5|bad|bad|bad"
            )
            .is_none()
        );
        assert!(g.objs.is_empty(), "an incomplete affect pair was loaded");
    }

    #[test]
    fn real_house_load_keeps_i32_boundaries_and_rejects_overflow_records() {
        let dir = temp_lib("numeric-boundaries");
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(790_404, 0, "House".into(), String::new()));
        let path = house_filename(&g.config.lib_path, 790_404).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = "0 2147483648 9 1 0 5 6 7 -1 1 2 3 4 0|overflow|overflow|overflow\n\
                    0 -2147483649 9 1 0 5 6 7 -1 1 2 3 4 0|underflow|underflow|underflow\n\
                    18446744073709551616 404 9 1 0 5 6 7 -1 1 2 3 4 0|depth|depth|depth\n\
                    bogus 405 9 1 0 5 6 7 -1 1 2 3 4 0|syntax|syntax|syntax\n\
                    0 2147483647 9 1 0 5 6 7 -1 1 2 3 4 0|max|max|max\n\
                    0 -2147483648 9 1 0 5 6 7 -1 1 2 3 4 0|min|min|min\n";
        std::fs::write(&path, body).unwrap();

        assert!(house_load(&mut g, 790_404));
        let mut loaded: Vec<_> = g
            .room(room)
            .contents
            .iter()
            .filter_map(|oid| g.get_obj(*oid).map(|obj| obj.item_number))
            .collect();
        loaded.sort_unstable();
        assert_eq!(loaded, vec![i32::MIN, i32::MAX]);
        assert!(!loaded.contains(&NOTHING));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn c_house_save_matches_contains_then_next_order_and_intrinsic_weights() {
        let dir = temp_lib("c-save-order");
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(790001, 0, "House".into(), String::new()));

        let mut root_obj = Object::new(100, "root".into(), "root".into());
        root_obj.weight = 15;
        let root = g.create_obj(root_obj);
        let mut first_obj = Object::new(101, "first".into(), "first".into());
        first_obj.weight = 2;
        let first = g.create_obj(first_obj);
        let mut second_obj = Object::new(102, "second".into(), "second".into());
        second_obj.weight = 3;
        let second = g.create_obj(second_obj);
        let mut sibling_obj = Object::new(103, "sibling".into(), "sibling".into());
        sibling_obj.weight = 4;
        let sibling = g.create_obj(sibling_obj);
        g.get_obj_mut(root).unwrap().contains = vec![first, second];
        g.room_mut(room).contents = vec![root, sibling];
        crate::lock_ok::lock(&house_object_formats())
            .insert(790001, crate::cformat::PersistenceFormat::C);

        house_crashsave(&mut g, 790001);

        let path = house_filename(&g.config.lib_path, 790001).unwrap();
        let elems = crate::cformat::decode_obj_file(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            elems
                .iter()
                .map(|elem| elem.item_number)
                .collect::<Vec<_>>(),
            vec![102, 101, 103, 100]
        );
        assert_eq!(
            elems.iter().map(|elem| elem.weight).collect::<Vec<_>>(),
            vec![3, 2, 4, 10]
        );
        assert!(elems.iter().all(|elem| elem.locate == 0));
        crate::lock_ok::lock(&house_object_formats()).remove(&790001);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_house_graph_preserves_last_snapshot_and_dirty_flag() {
        let dir = temp_lib("malformed-house-graph");
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(790004, 0, "House".into(), String::new()));
        let root = g.create_obj(Object::new(110, "root".into(), "root".into()));
        let child = g.create_obj(Object::new(111, "child".into(), "child".into()));
        g.obj_to_room(root, room);
        g.obj_to_obj(child, root);
        g.get_obj_mut(root).unwrap().contains.push(child);
        room_flag_set(&mut g, room, ROOM_HOUSE_CRASH);
        crate::lock_ok::lock(&house_object_formats())
            .insert(790004, crate::cformat::PersistenceFormat::Rust);
        let path = house_filename(&g.config.lib_path, 790004).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"last known good house snapshot").unwrap();

        house_crashsave(&mut g, 790004);

        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"last known good house snapshot"
        );
        assert!(room_flag_isset(&g, room, ROOM_HOUSE_CRASH));
        crate::lock_ok::lock(&house_object_formats()).remove(&790004);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn c_house_load_ignores_locate_flattens_objects_and_preserves_format() {
        let dir = temp_lib("c-load");
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(790002, 0, "House".into(), String::new()));
        g.obj_protos.insert(200, proto(200, ObjectType::Container));
        g.obj_protos.insert(201, proto(201, ObjectType::Armor));
        let elems = [
            crate::cformat::obj_to_c_elem(
                200,
                -2,
                7,
                9,
                [1, 2, 3, 4],
                0,
                11,
                12,
                13,
                14,
                &[ObjectAffect {
                    location: 3,
                    modifier: -2,
                }],
            ),
            crate::cformat::obj_to_c_elem(201, 9, 0, 0, [0; 4], 0, 5, -1, 0, 0, &[]),
        ];
        let path = house_filename(&g.config.lib_path, 790002).unwrap();
        crate::cformat::atomic_write(&path, &crate::cformat::encode_obj_file(&elems)).unwrap();

        assert!(house_load(&mut g, 790002));
        assert_eq!(g.room(room).contents.len(), 2);
        assert!(g.room(room).contents.iter().all(|oid| {
            matches!(g.get_obj(*oid).map(|obj| obj.loc), Some(crate::object::ObjLoc::Room(r)) if r == room)
        }));
        let loaded = g
            .room(room)
            .contents
            .iter()
            .find_map(|oid| g.get_obj(*oid).filter(|obj| obj.item_number == 200));
        assert_eq!(loaded.unwrap().values, [1, 2, 3, 4]);
        assert_eq!(loaded.unwrap().affects[0].modifier, -2);

        house_crashsave(&mut g, 790002);
        let rewritten = std::fs::read(&path).unwrap();
        assert!(!rust_house_object_file(&rewritten));
        let rewritten = crate::cformat::decode_obj_file(&rewritten).unwrap();
        assert_eq!(
            rewritten
                .iter()
                .map(|elem| elem.item_number)
                .collect::<Vec<_>>(),
            vec![200, 201]
        );
        assert!(rewritten.iter().all(|elem| elem.locate == 0));
        crate::lock_ok::lock(&house_object_formats()).remove(&790002);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_rust_house_file_keeps_nesting_and_format() {
        let dir = temp_lib("rust-load-save");
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(790003, 0, "House".into(), String::new()));
        let mut parent_obj = Object::new(300, "container".into(), "container".into());
        parent_obj.obj_type = ObjectType::Container;
        parent_obj.weight = 10;
        parent_obj.obj_class = 2;
        parent_obj.action_description = Some("house action".into());
        let parent = g.create_obj(parent_obj);
        let mut child_obj = Object::new(301, "child".into(), "child".into());
        child_obj.weight = 2;
        let child = g.create_obj(child_obj);
        g.obj_to_room(parent, room);
        g.obj_to_obj(child, parent);
        crate::lock_ok::lock(&house_object_formats())
            .insert(790003, crate::cformat::PersistenceFormat::Rust);

        house_crashsave(&mut g, 790003);
        let path = house_filename(&g.config.lib_path, 790003).unwrap();
        let first = std::fs::read(&path).unwrap();
        assert!(rust_house_object_file(&first));
        assert_eq!(first.iter().filter(|byte| **byte == b'\n').count(), 2);

        g.extract_obj(parent);
        assert!(g.room(room).contents.is_empty());
        assert!(house_load(&mut g, 790003));
        assert_eq!(g.room(room).contents.len(), 1);
        let loaded_parent = g.room(room).contents[0];
        assert_eq!(g.get_obj(loaded_parent).unwrap().item_number, 300);
        assert_eq!(g.get_obj(loaded_parent).unwrap().obj_class, 2);
        assert_eq!(
            g.get_obj(loaded_parent)
                .unwrap()
                .action_description
                .as_deref(),
            Some("house action")
        );
        assert_eq!(g.get_obj(loaded_parent).unwrap().contains.len(), 1);
        assert_eq!(
            g.get_obj(g.get_obj(loaded_parent).unwrap().contains[0])
                .unwrap()
                .item_number,
            301
        );
        house_crashsave(&mut g, 790003);
        assert!(rust_house_object_file(&std::fs::read(path).unwrap()));
        crate::lock_ok::lock(&house_object_formats()).remove(&790003);
        let _ = std::fs::remove_dir_all(dir);
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
