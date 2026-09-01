// objsave.rs — player object (rent / crash) persistence, ported 1:1 from
// CircleMUD/DeltaMUD `src/objsave.c`. Covers the per-player inventory +
// equipment save/load to lib/plrobjs, the auto-equip layer, and the
// receptionist/cryogenicist rent special procs.
//
// ON-DISK FORMAT
// --------------
// The C code fwrite()s a `struct rent_info` header followed by a stream of
// `struct obj_file_elem` records. This module auto-detects that exact x86-64
// LP64 layout and the port's existing documented line-oriented format. Loaded
// files retain their detected format on atomic rewrite; MUD_CFORMAT_FILES only
// selects the format of a new file. The logical text representation is:
//
//   header line:  RENT <rentcode> <time> <net_cost_per_diem> <gold> <account>
//   object line:  OBJ <locate> <vnum> <type> <wearbits> <extrabits> <weight>
//                     <cost> <rent> <timer> <min_level> <bitvector>
//                     <curr_slots> <total_slots> <v0> <v1> <v2> <v3>
//                     <#affects> [loc mod ...] [obj_class]
//                     |<name>|<short>|<long>|<action>
//
// The `locate` field is the C obj_file_elem.locate encoding, preserved 1:1:
//   * locate  > 0 : equipped, value == wear_slot + 1
//   * locate == 0 : top-level inventory
//   * locate  < 0 : inside a container, value == -(container_row + 1)
// On load, auto_equip()/the container-row reconstruction interpret it exactly
// as Crash_load() does. bitvector / curr_slots / total_slots / min_level and
// the six C affect slots round-trip through both representations.
//
// WIRING (the integrator hooks these from game.rs; the C call sites are noted):
//   * enter_game()  -> crash_load(g, ch, &g.config.lib_path)  (C: Crash_load)
//   * disconnect()/ -> crash_save(g, ch, &g.config.lib_path)  (C: Crash_crashsave
//     do_quit          via Crash_save_all / really_quit's save path)
//   * receptionist / cryogenicist are SPECIAL() spec procs, dispatched by the
//     mob spec-proc layer exactly like shop::shop_keeper.

use crate::act::{ActArg, To, act};
use crate::handler::isname;
use crate::interpreter::one_argument;
use crate::object::{
    ExtraFlags, Object, ObjectAffect, ObjectGraphOrder, ObjectListOrder, ObjectType, WearFlags,
    walk_object_graph, walk_object_lists_postorder,
};
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// rent codes (structs.h) and config defaults (config.c)
// ---------------------------------------------------------------------------
const RENT_UNDEF: i32 = 0;
const RENT_CRASH: i32 = 1;
const RENT_RENTED: i32 = 2;
const RENT_CRYO: i32 = 3;
const RENT_FORCED: i32 = 4;
const RENT_TIMEDOUT: i32 = 5;

// gen_receptionist factors (objsave.c).
const RENT_FACTOR: i32 = 1;
const CRYO_FACTOR: i32 = 4;

// config.c defaults (no Config fields exist for these yet).
const FREE_RENT: bool = false; // free_rent = NO
const MAX_OBJ_SAVE: i64 = 50; // max_obj_save = 50
const MIN_RENT_COST: i32 = 250; // min_rent_cost = 250

// ITEM_* extra-flag bits (structs.h; mirror object.rs ExtraFlags values).
const ITEM_NORENT: u64 = 1 << 2;

// PLR_* (structs.h) — PC act_flags hold the PLR_ bitset.
// Public so the object movers (handler::obj_to_char / equip_char / unequip_char
// and state::obj_from_anywhere) can SET it whenever objects move on/off a PC —
// mirroring C handler.c:542/563 — so crash_save_all (which only saves PCs with
// PLR_CRASH set) is no longer a no-op (BUG 14).
pub const PLR_CRASH: i64 = 1 << 6;
const PLR_CRYO: i64 = 1 << 15;
// PRF2_LOCKOUT (structs.h) — cleared on rent (cmd_informative defines its mirror).
const PRF2_LOCKOUT: i64 = 1 << 1;

// MAX_BAG_ROW (objsave.c) — container-nesting reconstruction rows.
const MAX_BAG_ROW: usize = 5;

// mudlog visibility floor (immortals).
const NRM_INVIS: u8 = LVL_IMMORT;

// ---------------------------------------------------------------------------
// rent_info header (structs.h struct rent_info) — the fields we persist.
// ---------------------------------------------------------------------------
#[derive(Clone, Copy)]
struct RentInfo {
    time: i64,
    rentcode: i32,
    net_cost_per_diem: i32,
    gold: i32,
    account: i32,
}

impl Default for RentInfo {
    fn default() -> Self {
        RentInfo {
            time: 0,
            rentcode: RENT_UNDEF,
            net_cost_per_diem: 0,
            gold: 0,
            account: 0,
        }
    }
}

// ===========================================================================
// Filename: get_filename(name, ..., CRASH_FILE) -> plrobjs/<A-E…>/<name>.objs
// ===========================================================================

/// Port of get_filename() for CRASH_FILE: "plrobjs/<MIDDLE>/<name>.objs" under
/// the lib path. The MIDDLE bucket groups first-letters a-e/f-j/k-o/p-t/u-z.
/// Returns None for an empty name (C returns 0).
pub(crate) fn crash_filename(lib: &str, name: &str) -> Option<std::path::PathBuf> {
    // Player creation accepts 2..=20 ASCII alphabetic characters. Applying
    // exactly that grammar here keeps every filesystem caller from treating
    // separators, dot components, controls, or Unicode lookalikes as a path.
    if !(2..=20).contains(&name.len()) || !name.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return None;
    }
    let lname = name.to_ascii_lowercase();
    let first = lname.chars().next().unwrap_or('z');
    let middle = match first {
        'a'..='e' => "A-E",
        'f'..='j' => "F-J",
        'k'..='o' => "K-O",
        'p'..='t' => "P-T",
        'u'..='z' => "U-Z",
        _ => "ZZZ",
    };
    Some(
        std::path::Path::new(lib)
            .join("plrobjs")
            .join(middle)
            .join(format!("{}.objs", lname)),
    )
}

#[cfg(target_os = "linux")]
fn openat_owned(
    parent: std::os::fd::RawFd,
    name: &std::ffi::CStr,
    flags: libc::c_int,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;

    loop {
        let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if fd >= 0 {
            return Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn open_directory_without_symlinks(
    path: &std::path::Path,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::ffi::CString;
    use std::io::{Error, ErrorKind};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    let start = if path.is_absolute() { b"/\0" } else { b".\0" };
    let start = std::ffi::CStr::from_bytes_with_nul(start)
        .expect("static directory path is NUL terminated");
    let directory_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut current = openat_owned(libc::AT_FDCWD, start, directory_flags)?;

    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            // A configured lib path with parent traversal is not needed by the
            // deployed layout. Refuse it rather than weakening the beneath-root
            // guarantee or relying on lexical normalization.
            Component::ParentDir => {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "parent traversal is not allowed in the rent root",
                ));
            }
            Component::Prefix(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "unsupported rent root prefix",
                ));
            }
        };
        let name = CString::new(name.as_bytes())
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "NUL in rent root path"))?;
        current = openat_owned(
            std::os::fd::AsRawFd::as_raw_fd(&current),
            &name,
            directory_flags,
        )?;
    }

    Ok(current)
}

#[cfg(target_os = "linux")]
fn read_rent_file_beneath_root_after_parent_open<F>(
    lib: &str,
    name: &str,
    after_parent_open: F,
) -> std::io::Result<Vec<u8>>
where
    F: FnOnce(),
{
    use std::ffi::CString;
    use std::io::{Error, ErrorKind, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let path = crash_filename(lib, name)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid player name"))?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid rent bucket"))?;
    let filename = path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid rent filename"))?;

    // Every directory is opened relative to its already-open parent with
    // O_NOFOLLOW. Each descriptor pins that directory even if an attacker
    // renames a later pathname component before the final open.
    let parent = open_directory_without_symlinks(parent)?;
    after_parent_open();

    let filename = CString::new(filename.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "NUL in rent filename"))?;
    let fd = openat_owned(
        parent.as_raw_fd(),
        &filename,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    )?;
    let mut file = std::fs::File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "rent target is not a file",
        ));
    }

    // A legitimate player object file is far smaller; cap both the initial
    // size and the actual read. The second check handles a concurrently grown
    // file without allowing unbounded allocation/output.
    const MAX_RENT_FILE_BYTES: u64 = 16 * 1024 * 1024;
    if metadata.len() > MAX_RENT_FILE_BYTES {
        return Err(Error::new(ErrorKind::InvalidData, "rent file is too large"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_RENT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RENT_FILE_BYTES {
        return Err(Error::new(ErrorKind::InvalidData, "rent file is too large"));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn read_rent_file_beneath_root(lib: &str, name: &str) -> std::io::Result<Vec<u8>> {
    read_rent_file_beneath_root_after_parent_open(lib, name, || {})
}

#[cfg(not(target_os = "linux"))]
fn read_rent_file_beneath_root(_lib: &str, _name: &str) -> std::io::Result<Vec<u8>> {
    // The listing is administrative convenience, not a boot dependency. On a
    // platform without the descriptor-relative Linux implementation, fail
    // closed instead of falling back to a check-then-open pathname sequence.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure rent listing is only supported on Linux",
    ))
}

// ===========================================================================
// small accessors / predicates (utils.h macros)
// ===========================================================================

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(true)
}

fn get_name(g: &GameState, ch: CharId) -> String {
    g.get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default()
}

fn invis_lev(g: &GameState, ch: CharId) -> u8 {
    g.get_char(ch)
        .map(|c| c.invis_level.max(0) as u8)
        .unwrap_or(0)
}

/// GET_OBJ_RENT(): per-day rent cost of an object.
fn obj_rent(g: &GameState, oid: ObjId) -> i32 {
    g.get_obj(oid).map(|o| o.rent).unwrap_or(0)
}

/// IS_WARRIOR(ch) (utils.h): a non-NPC of the warrior class.
fn is_warrior(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| !c.is_npc && c.player.class == Class::Warrior)
        .unwrap_or(false)
}

// utils.h alignment predicates.
fn is_good(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.alignment >= 350).unwrap_or(false)
}
fn is_evil(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.alignment <= -350).unwrap_or(false)
}
fn is_neutral(g: &GameState, ch: CharId) -> bool {
    !is_good(g, ch) && !is_evil(g, ch)
}

/// mudlog(): broadcast to immortals at or above `min_level`, plus host log.
fn mudlog(g: &mut GameState, line: &str, min_level: u8) {
    // C mudlog honours the viewer's PRF_LOG{1,2,3} syslog level, so an
    // immortal without LOG1 does NOT see Normal lines (the battery's
    // Implementor has no log prefs and must not see this either).
    crate::syslog::mudlog(g, line, crate::syslog::NRM, min_level);
}

// ===========================================================================
// obj_file_elem packed (de)serialization — the OBJ <locate> ... line.
// ===========================================================================

/// Obj_to_store_from() (objsave.c:89-117): append one object record at
/// `locate`, carrying the affect bitvector and the durability counters
/// (C objsave.c writes curr_slots/total_slots/bitvector per record; #233).
fn obj_to_store(g: &GameState, oid: ObjId, locate: i32, out: &mut String) {
    let o = match g.get_obj(oid) {
        Some(o) => o,
        None => return,
    };
    let ty = o.obj_type as i32;
    out.push_str(&format!(
        "OBJ {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        locate,
        o.item_number,
        ty,
        o.wear_flags.bits(),
        o.extra_flags.bits(),
        stored_object_weight(g, oid),
        o.cost,
        o.rent,
        o.timer,
        o.min_level, // min_level: the EQUIP GATE (C stores obj->min_level
        // here; writing o.level silently erased the gate on
        // every rent/crash cycle -- issue #383)
        o.bitvector,  // affect bitvector
        o.curr_slots, // durability counters
        o.total_slots,
        o.values[0],
        o.values[1],
        o.values[2],
        o.values[3],
        o.affects.len()
    ));
    for a in &o.affects {
        out.push_str(&format!(" {} {}", a.location, a.modifier));
    }
    // The C record omits obj_class/action text because read_object() supplies
    // them from the prototype. Carry them in the Rust representation so its
    // self-contained loader retains the same prototype-derived fields.
    out.push_str(&format!(" {}", o.obj_class));
    out.push_str(&format!(
        "|{}|{}|{}|{}\n",
        sanitize(&o.name),
        sanitize(&o.short_description),
        sanitize(&o.description),
        sanitize(o.action_description.as_deref().unwrap_or(""))
    ));
}

/// Live container weights include all descendants. C's save routines subtract
/// each immediate child's aggregate weight before writing the parent record,
/// so the persisted value is the object's intrinsic weight.
fn stored_object_weight(g: &GameState, oid: ObjId) -> i32 {
    let Some(obj) = g.get_obj(oid) else {
        return 0;
    };
    obj.contains.iter().fold(obj.weight, |weight, child| {
        weight.saturating_sub(g.get_obj(*child).map(|o| o.weight).unwrap_or(0))
    })
}

fn sanitize(s: &str) -> String {
    s.replace('|', "/").replace(['\n', '\r'], " ")
}

/// Obj_from_store_to(): rebuild a live Object from one record line, returning
/// its id and the locate code. Reconstructs every saved field (the Rust port
/// has no read_object()-from-proto requirement here — all values are stored).
fn obj_from_store(g: &mut GameState, line: &str) -> Option<(ObjId, i32)> {
    // "OBJ <nums...> |name|short|long|action"
    let rest = line.strip_prefix("OBJ ")?;
    let (head, tail) = match rest.split_once('|') {
        Some((h, t)) => (h, t),
        None => (rest, ""),
    };
    let nums: Vec<&str> = head.split_whitespace().collect();
    if nums.len() < 18 {
        return None;
    }
    // Every numeric column above is present in this format. A malformed or
    // overflowing value therefore invalidates this record; it must not alias
    // to a plausible default object (most dangerously vnum/type/flags zero).
    let locate: i32 = nums[0].parse().ok()?;
    let vnum: ObjVnum = nums[1].parse().ok()?;
    let ty: i32 = nums[2].parse().ok()?;
    let wear: u32 = nums[3].parse().ok()?;
    let extra: u64 = nums[4].parse().ok()?;
    let weight: i32 = nums[5].parse().ok()?;
    let cost: i32 = nums[6].parse().ok()?;
    let rent: i32 = nums[7].parse().ok()?;
    let timer: i32 = nums[8].parse().ok()?;
    let min_level: i32 = nums[9].parse().ok()?;
    let bitvector: i64 = nums[10].parse().ok()?;
    let curr_slots: i32 = nums[11].parse().ok()?;
    let total_slots: i32 = nums[12].parse().ok()?;
    let v0: i32 = nums[13].parse().ok()?;
    let v1: i32 = nums[14].parse().ok()?;
    let v2: i32 = nums[15].parse().ok()?;
    let v3: i32 = nums[16].parse().ok()?;
    let naff: usize = nums[17].parse().ok()?;

    let affects_end = 18usize.checked_add(naff.checked_mul(2)?)?;
    // obj_class is the sole optional numeric field, for compatibility with
    // Rust rent files written before that column was added.
    if !(nums.len() == affects_end || nums.len() == affects_end.checked_add(1)?) {
        return None;
    }

    let mut affects = Vec::with_capacity(naff);
    let mut idx = 18;
    for _ in 0..naff {
        let loc: i32 = nums[idx].parse().ok()?;
        let modi: i32 = nums[idx + 1].parse().ok()?;
        affects.push(ObjectAffect {
            location: loc,
            modifier: modi,
        });
        idx += 2;
    }
    // Older Rust files end immediately after the affect pairs.
    let obj_class = match nums.get(idx) {
        Some(value) => value.parse().ok()?,
        None => -1,
    };

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
    obj.min_level = min_level;
    obj.level = min_level.clamp(0, u8::MAX as i32) as u8;
    obj.values = [v0, v1, v2, v3];
    obj.affects = affects;
    obj.bitvector = bitvector;
    obj.curr_slots = curr_slots;
    obj.total_slots = total_slots;
    obj.obj_class = obj_class;

    let oid = g.create_obj(obj);
    Some((oid, locate))
}

// ===========================================================================
// auto_equip() — place a loaded item at its wear slot or fall back to inventory
// ===========================================================================

/// Per-slot fit requirement for auto_equip's switch (objsave.c auto_equip).
enum SlotFit {
    /// Always fits (WEAR_LIGHT — no CAN_WEAR check).
    Always,
    /// Requires the object to carry this WearFlags bit.
    Requires(WearFlags),
    /// Unenumerated slot — C's `default: locate = 0` drops it to inventory.
    NeverFit,
}

/// CAN_WEAR(obj, bit) requirement for a given wear slot, mirroring the C
/// auto_equip() switch exactly: WEAR_LIGHT has no check; each armor/jewelry slot
/// requires its matching ITEM_WEAR_* bit; and any slot NOT in the switch hits
/// C's `default: locate = 0` (BUG 25 — slots 18-21 = shoulders/ankles/face must
/// fall to inventory, not be forced to require HOLD). WEAR_HOLD is handled
/// specially (warrior weapon allowance) by the caller and so is NeverFit here.
fn slot_wear_flag(slot: usize) -> SlotFit {
    match slot {
        WEAR_LIGHT => SlotFit::Always,
        WEAR_FINGER_R | WEAR_FINGER_L => SlotFit::Requires(WearFlags::FINGER),
        WEAR_NECK_1 | WEAR_NECK_2 => SlotFit::Requires(WearFlags::NECK),
        WEAR_BODY => SlotFit::Requires(WearFlags::BODY),
        WEAR_HEAD => SlotFit::Requires(WearFlags::HEAD),
        WEAR_LEGS => SlotFit::Requires(WearFlags::LEGS),
        WEAR_FEET => SlotFit::Requires(WearFlags::FEET),
        WEAR_HANDS => SlotFit::Requires(WearFlags::HANDS),
        WEAR_ARMS => SlotFit::Requires(WearFlags::ARMS),
        WEAR_SHIELD => SlotFit::Requires(WearFlags::SHIELD),
        WEAR_ABOUT => SlotFit::Requires(WearFlags::ABOUT),
        WEAR_WAIST => SlotFit::Requires(WearFlags::WAIST),
        WEAR_WRIST_R | WEAR_WRIST_L => SlotFit::Requires(WearFlags::WRIST),
        WEAR_WIELD => SlotFit::Requires(WearFlags::WIELD),
        // WEAR_HOLD handled specially by the caller; everything else (incl.
        // shoulders/ankles/face) is C's `default: locate = 0`.
        _ => SlotFit::NeverFit,
    }
}

/// auto_equip(): try to equip `obj` at the slot encoded by `locate` (==slot+1);
/// fall back to inventory on any mismatch. Mirrors objsave.c auto_equip().
fn auto_equip(g: &mut GameState, ch: CharId, oid: ObjId, mut locate: i32) {
    if locate > 0 {
        let slot = (locate - 1) as usize;

        // Determine whether the object fits the slot.
        let fits = if slot >= NUM_WEARS {
            false
        } else if slot == WEAR_HOLD {
            // HOLD: can_wear HOLD, or a warrior holding a wieldable weapon.
            let (can_hold, can_wield, is_weapon) = g
                .get_obj(oid)
                .map(|o| {
                    (
                        o.can_wear(WearFlags::HOLD),
                        o.can_wear(WearFlags::WIELD),
                        o.obj_type == ObjectType::Weapon,
                    )
                })
                .unwrap_or((false, false, false));
            can_hold || (is_warrior(g, ch) && can_wield && is_weapon)
        } else {
            match slot_wear_flag(slot) {
                SlotFit::Always => true, // WEAR_LIGHT
                SlotFit::Requires(bit) => g.get_obj(oid).map(|o| o.can_wear(bit)).unwrap_or(false),
                SlotFit::NeverFit => false, // C default: locate = 0 -> inventory
            }
        };

        if !fits {
            locate = 0;
        } else {
            let slot = (locate - 1) as usize;
            let occupied = g
                .get_char(ch)
                .map(|c| c.equipment[slot].is_some())
                .unwrap_or(true);
            if occupied {
                // double-equipped save: fall back to inventory.
                locate = 0;
            } else {
                // alignment-zap guard (prevent $M wipe through auto-equip).
                let (anti_evil, anti_good, anti_neutral) = g
                    .get_obj(oid)
                    .map(|o| {
                        (
                            o.extra_flags.contains(ExtraFlags::ANTI_EVIL),
                            o.extra_flags.contains(ExtraFlags::ANTI_GOOD),
                            o.extra_flags.contains(ExtraFlags::ANTI_NEUTRAL),
                        )
                    })
                    .unwrap_or((false, false, false));
                if (anti_evil && is_evil(g, ch))
                    || (anti_good && is_good(g, ch))
                    || (anti_neutral && is_neutral(g, ch))
                {
                    locate = 0;
                } else {
                    g.equip_char(ch, oid, slot);
                }
            }
        }
    }

    if locate <= 0 {
        g.obj_to_char(oid, ch);
    }
}

// ===========================================================================
// Crash_is_unrentable / norent extraction (objsave.c)
// ===========================================================================

/// Crash_is_unrentable(): NORENT / rent<0 / no-proto / KEY items can't be
/// stored; also items whose owner is >10 levels under the item's min_level.
fn crash_is_unrentable(g: &GameState, oid: ObjId) -> bool {
    let o = match g.get_obj(oid) {
        Some(o) => o,
        None => return false,
    };
    if o.extra_flags.contains(ExtraFlags::NO_RENT)
        || o.rent < 0
        || o.item_number <= NOTHING
        || o.obj_type == ObjectType::Key
    {
        return true;
    }
    // owner min-level gate (carried_by / worn_by).
    let owner = match o.loc {
        crate::object::ObjLoc::Carried(c) => Some(c),
        crate::object::ObjLoc::Worn(c, _) => Some(c),
        _ => None,
    };
    if let Some(owner) = owner {
        let lvl = g
            .get_char(owner)
            .map(|c| c.player.level as i32)
            .unwrap_or(0);
        if lvl + 10 < o.level as i32 {
            return true;
        }
    }
    false
}

/// Crash_extract_norents(): extract unrentable items anywhere below `roots`.
/// Postorder keeps every child inspectable before a parent extraction removes
/// the rest of that parent's contents.
fn crash_extract_norents(g: &mut GameState, roots: &[ObjId]) {
    let walk = walk_object_graph(
        roots.iter().copied(),
        ObjectGraphOrder::Postorder,
        "Crash_extract_norents",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    );
    if walk.malformed() {
        return;
    }
    for visit in walk.visits {
        if g.get_obj(visit.id).is_some() && crash_is_unrentable(g, visit.id) {
            g.extract_obj(visit.id);
        }
    }
}

/// Crash_extract_norents_from_equipped(): move outright-unrentable equipment to
/// inventory; otherwise scrub norents out of worn containers.
fn crash_extract_norents_from_equipped(g: &mut GameState, ch: CharId) {
    let mut roots = Vec::new();
    for j in 0..NUM_WEARS {
        let eq = g.get_char(ch).and_then(|c| c.equipment[j]);
        let oid = match eq {
            Some(o) => o,
            None => continue,
        };
        // Top-level unrentability test (the eq slot itself).
        let kill_top = g
            .get_obj(oid)
            .map(|o| {
                o.extra_flags.contains(ExtraFlags::NO_RENT)
                    || o.rent < 0
                    || o.item_number <= NOTHING
                    || o.obj_type == ObjectType::Key
            })
            .unwrap_or(false);
        if kill_top {
            if let Some(o) = g.unequip_char(ch, j) {
                g.obj_to_char(o, ch);
            }
        } else {
            roots.push(oid);
        }
    }
    crash_extract_norents(g, &roots);
}

// ===========================================================================
// rent cost calculation (objsave.c Crash_calculate_rent)
// ===========================================================================

fn crash_calculate_rent(g: &GameState, roots: &[ObjId]) -> i32 {
    walk_object_graph(
        roots.iter().copied(),
        ObjectGraphOrder::Preorder,
        "Crash_calculate_rent",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    )
    .visits
    .into_iter()
    .map(|visit| obj_rent(g, visit.id).max(0))
    .sum()
}

// ===========================================================================
// Crash_save: recursive object serialization with the locate encoding.
// ===========================================================================

/// Serialize a character's full equipment + inventory into a text body, in the
/// C save order: each worn slot (locate = j+1) then carried (locate = 0).
fn serialize_objects(g: &GameState, ch: CharId) -> Option<String> {
    let mut out = String::new();
    let (equipment, carrying) = match g.get_char(ch) {
        Some(c) => (c.equipment, c.carrying.clone()),
        None => return Some(out),
    };
    let mut root_lists = Vec::new();
    let mut locates = Vec::new();
    for j in 0..NUM_WEARS {
        if let Some(oid) = equipment[j] {
            root_lists.push(vec![oid]);
            locates.push(j as i32 + 1);
        }
    }
    if !carrying.is_empty() {
        root_lists.push(carrying);
        locates.push(0);
    }
    let walk = walk_object_lists_postorder(
        root_lists,
        ObjectListOrder::NextThenContains,
        "Crash_save",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    );
    if walk.malformed() {
        log::warn!(
            "SYSERR: refusing partial Rust crash snapshot for {} because its object graph is malformed",
            get_name(g, ch)
        );
        return None;
    }
    for visit in walk.visits {
        let locate = if visit.depth == 0 {
            locates[visit.root_index]
        } else {
            -(visit.depth as i32)
        };
        obj_to_store(g, visit.id, locate, &mut out);
    }
    Some(out)
}

/// Convert C-binary rent records to the in-memory text pipeline lines
/// (issue #95): header line + one OBJ line per element with the container
/// locate code negated (locate < 0 == inside container row).
fn rent_to_text(
    g: &GameState,
    rent: &crate::cformat::CRentInfo,
    elems: &[crate::cformat::CObjFileElem],
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = write!(
        out,
        "RENT {} {} {} {} {}\n",
        rent.rentcode, rent.time, rent.net_cost_per_diem, rent.gold, rent.account
    );
    for e in elems {
        // C Crash_load does read_object(item_number, VIRTUAL) and copies the
        // proto, overriding the stored fields - the C record carries no type,
        // wear flags or names. Derive those from the obj proto here.
        let Ok(vnum) = i32::try_from(e.item_number) else {
            log::warn!(
                "SYSERR: rejected C rent object record with vnum {} outside the supported 32-bit range",
                e.item_number
            );
            continue;
        };
        let Ok(locate) = i32::try_from(e.locate) else {
            log::warn!(
                "SYSERR: rejected C rent object record {} with locate {} outside the supported 32-bit range",
                e.item_number,
                e.locate
            );
            continue;
        };
        let Some(proto) = g.obj_protos.get(&vnum) else {
            // C Obj_from_store_to returns NULL when real_object() fails.
            continue;
        };
        let ty = proto.obj_type as i32;
        let wear = proto.wear_flags.bits();
        let cost = proto.cost;
        let rentp = proto.rent;
        let name = proto.name.clone();
        let short = proto.short_desc.clone();
        let long = proto.description.clone();
        let action = proto.action_description.clone();
        let obj_class = proto.obj_class;
        let _ = write!(
            out,
            "OBJ {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            locate,
            e.item_number,
            ty,
            wear,
            e.extra_flags,
            e.weight,
            cost,
            rentp,
            e.timer,
            e.min_level,
            e.bitvector,
            e.curr_slots,
            e.total_slots,
            e.value[0],
            e.value[1],
            e.value[2],
            e.value[3],
            e.affected.iter().filter(|(l, _)| *l != 0).count()
        );
        for (l, m) in e.affected {
            if l != 0 {
                let _ = write!(out, " {} {}", l, m);
            }
        }
        let _ = write!(out, " {}", obj_class);
        let sanitize = |s: &str| s.replace('|', "/").replace(['\n', '\r'], " ");
        let _ = write!(
            out,
            "|{}|{}|{}|{}\n",
            sanitize(&name),
            sanitize(&short),
            sanitize(&long),
            sanitize(&action)
        );
    }
    out
}

fn write_rent_header(rent: &RentInfo) -> String {
    format!(
        "RENT {} {} {} {} {}\n",
        rent.rentcode, rent.time, rent.net_cost_per_diem, rent.gold, rent.account
    )
}

fn parse_rent_header(line: &str) -> Option<RentInfo> {
    let rest = line.strip_prefix("RENT ")?;
    let p: Vec<&str> = rest.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    Some(RentInfo {
        rentcode: p[0].parse().ok()?,
        time: p[1].parse().ok()?,
        net_cost_per_diem: p[2].parse().ok()?,
        gold: p[3].parse().ok()?,
        account: p[4].parse().ok()?,
    })
}

fn detect_rent_format(bytes: &[u8]) -> Option<crate::cformat::PersistenceFormat> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let header = text.lines().find(|line| !line.trim().is_empty())?;
        if parse_rent_header(header).is_some() {
            return Some(crate::cformat::PersistenceFormat::Rust);
        }
    }
    let (rent, _) = crate::cformat::decode_rent_file(bytes)?;
    (RENT_UNDEF..=RENT_TIMEDOUT)
        .contains(&rent.rentcode)
        .then_some(crate::cformat::PersistenceFormat::C)
}

/// Low-level file write: header + serialized object body to the player's
/// plrobjs file (creating the bucket directory as needed).
fn write_crash_file(g: &GameState, ch: CharId, rent: &RentInfo) -> bool {
    let name = get_name(g, ch);
    let path = match crash_filename(&g.config.lib_path, &name) {
        Some(p) => p,
        None => return false,
    };
    // Preserve the format already in use for this player. The environment
    // only selects the format for a brand-new (or corrupt) file.
    let format = std::fs::read(&path)
        .ok()
        .and_then(|bytes| detect_rent_format(&bytes))
        .unwrap_or_else(crate::cformat::default_persistence_format);
    let bytes = if format == crate::cformat::PersistenceFormat::C {
        let Some(elems) = cformat_elems(g, ch) else {
            return false;
        };
        let rent_c = crate::cformat::CRentInfo {
            time: rent.time as i32,
            rentcode: rent.rentcode,
            net_cost_per_diem: rent.net_cost_per_diem,
            gold: rent.gold,
            account: rent.account,
            nitems: i32::try_from(elems.len()).unwrap_or(i32::MAX),
        };
        crate::cformat::encode_rent_file(&rent_c, &elems)
    } else {
        let mut body = write_rent_header(rent);
        let Some(objects) = serialize_objects(g, ch) else {
            return false;
        };
        body.push_str(&objects);
        body.into_bytes()
    };
    // Atomic replacement (#386): never truncate the only durable copy before
    // all bytes have been written and synced.
    crate::cformat::atomic_write(&path, &bytes).is_ok()
}

/// Convert carried+worn objects to C obj_file_elem records (worn first per
/// Crash_crashsave's slot loop, then inventory), containers flattened
/// depth-first after their parent record (C Crash_save recursion).
fn cformat_elems(g: &GameState, ch: CharId) -> Option<Vec<crate::cformat::CObjFileElem>> {
    fn elem_for(g: &GameState, oid: ObjId, locate: i64) -> crate::cformat::CObjFileElem {
        let o = g.get_obj(oid);
        let o = match o {
            Some(o) => o,
            None => return crate::cformat::obj_to_c_elem(0, 0, 0, 0, [0; 4], 0, 0, -1, 0, 0, &[]),
        };
        crate::cformat::obj_to_c_elem(
            o.item_number as i64,
            locate,
            o.curr_slots,
            o.total_slots,
            o.values,
            o.extra_flags.bits() as i32,
            stored_object_weight(g, oid),
            o.timer,
            o.bitvector,
            o.min_level,
            &o.affects,
        )
    }
    let mut out = Vec::new();
    let mut root_lists = Vec::new();
    let mut locates = Vec::new();
    // Worn equipment: slot j -> locate j+1.
    if let Some(c) = g.get_char(ch) {
        for (j, slot) in c.equipment.iter().enumerate() {
            if let Some(oid) = slot {
                root_lists.push(vec![*oid]);
                locates.push((j + 1) as i64);
            }
        }
        // Inventory at locate 0.
        if !c.carrying.is_empty() {
            root_lists.push(c.carrying.clone());
            locates.push(0);
        }
    }
    let walk = walk_object_lists_postorder(
        root_lists,
        ObjectListOrder::NextThenContains,
        "C-format Crash_save",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    );
    if walk.malformed() {
        log::warn!(
            "SYSERR: refusing partial C crash snapshot for {} because its object graph is malformed",
            get_name(g, ch)
        );
        return None;
    }
    for visit in walk.visits {
        let locate = if visit.depth == 0 {
            locates[visit.root_index]
        } else {
            -(visit.depth as i64)
        };
        out.push(elem_for(g, visit.id, locate));
    }
    Some(out)
}

/// Crash_delete_crashfile (objsave.c:161): remove the player's plrobjs
/// crash file outright (ghost extraction; #115).
pub fn crash_delete_crashfile(g: &GameState, ch: CharId) {
    crash_delete_file(g, ch);
}

/// Name-keyed variant for the menu self-delete path, where no Character
/// entity exists yet (C interpreter.c:2412 Crash_delete_file) (#198).
pub fn crash_delete_file_by_name(lib_path: &str, name: &str) -> std::io::Result<()> {
    if let Some(path) = crash_filename(lib_path, name) {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        Ok(())
    }
}

fn crash_delete_file(g: &GameState, ch: CharId) {
    let name = get_name(g, ch);
    if let Some(path) = crash_filename(&g.config.lib_path, &name) {
        let _ = std::fs::remove_file(path);
    }
}

// ===========================================================================
// Crash_crashsave / Crash_rentsave / Crash_cryosave / Crash_idlesave
// ===========================================================================

/// Crash_crashsave(): write a RENT_CRASH file with the player's current eq +
/// inventory (no extraction). Clears PLR_CRASH only after a durable write.
/// Returns true for a successful save (and for the intentional NPC no-op).
pub fn crash_crashsave(g: &mut GameState, ch: CharId) -> bool {
    if is_npc(g, ch) {
        return true;
    }
    let rent = RentInfo {
        rentcode: RENT_CRASH,
        time: unix_now(),
        ..Default::default()
    };
    if !write_crash_file(g, ch, &rent) {
        return false;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.act_flags &= !PLR_CRASH;
    }
    true
}

/// Crash_rentsave(): RENT_RENTED save. Scrubs norents, records the per-diem
/// cost and the player's gold/bank, writes the file, then extracts the objects
/// from the world (the player walks away empty). Skips NPCs.
pub fn crash_rentsave(g: &mut GameState, ch: CharId, cost: i32) {
    if is_npc(g, ch) {
        return;
    }
    crash_extract_norents_from_equipped(g, ch);
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    crash_extract_norents(g, &carrying);

    let (gold, account) = g
        .get_char(ch)
        .map(|c| (c.points.gold, c.points.bank_gold))
        .unwrap_or((0, 0));
    let rent = RentInfo {
        rentcode: RENT_RENTED,
        time: unix_now(),
        net_cost_per_diem: cost,
        gold,
        account,
    };
    if !write_crash_file(g, ch, &rent) {
        return;
    }
    extract_all_player_objects(g, ch);
    set_load_room_to_current(g, ch);
    g.request_player_save(ch);
}

/// Crash_cryosave(): RENT_CRYO save (one-time freeze fee already charged by the
/// caller). Sets PLR_CRYO and extracts the player's objects. Skips NPCs.
pub fn crash_cryosave(g: &mut GameState, ch: CharId, cost: i32) {
    if is_npc(g, ch) {
        return;
    }
    crash_extract_norents_from_equipped(g, ch);
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    crash_extract_norents(g, &carrying);
    if let Some(c) = g.get_char_mut(ch) {
        crate::gold::debit_up_to(c, crate::gold::Account::Carried, i64::from(cost));
    }
    let (gold, account) = g
        .get_char(ch)
        .map(|c| (c.points.gold, c.points.bank_gold))
        .unwrap_or((0, 0));
    let rent = RentInfo {
        rentcode: RENT_CRYO,
        time: unix_now(),
        net_cost_per_diem: 0,
        gold,
        account,
    };
    if !write_crash_file(g, ch, &rent) {
        return;
    }
    extract_all_player_objects(g, ch);
    if let Some(c) = g.get_char_mut(ch) {
        c.act_flags |= PLR_CRYO;
    }
    set_load_room_to_current(g, ch);
    g.request_player_save(ch);
}

/// Crash_idlesave(): RENT_TIMEDOUT (force-rent) save at 2x cost, dropping the
/// most expensive items until the player can afford the bill. Skips NPCs.
pub fn crash_idlesave(g: &mut GameState, ch: CharId) {
    if is_npc(g, ch) {
        return;
    }
    crash_extract_norents_from_equipped(g, ch);
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    crash_extract_norents(g, &carrying);

    let (gold, account) = g
        .get_char(ch)
        .map(|c| (c.points.gold, c.points.bank_gold))
        .unwrap_or((0, 0));

    // inventory + equipment rent, each doubled (forcerent is 2x normal).
    let mut cost = inventory_rent(g, ch).saturating_mul(2);
    let mut cost_eq = equipment_rent(g, ch).saturating_mul(2);

    if i64::from(cost) + i64::from(cost_eq) > i64::from(gold) + i64::from(account) {
        // unequip everything (eq folds into inventory).
        for j in 0..NUM_WEARS {
            if let Some(o) = g.unequip_char(ch, j) {
                g.obj_to_char(o, ch);
            }
        }
        cost = cost.saturating_add(cost_eq);
        cost_eq = 0;
        let _ = cost_eq;

        // drop the single most expensive carried item until affordable.
        while i64::from(cost) > i64::from(gold) + i64::from(account)
            && !g
                .get_char(ch)
                .map(|c| c.carrying.is_empty())
                .unwrap_or(true)
        {
            crash_extract_expensive(g, ch);
            cost = inventory_rent(g, ch).saturating_mul(2);
        }
    }

    // If nothing left at all, delete the file and bail.
    let has_inv = !g
        .get_char(ch)
        .map(|c| c.carrying.is_empty())
        .unwrap_or(true);
    let has_eq = (0..NUM_WEARS).any(|j| g.get_char(ch).and_then(|c| c.equipment[j]).is_some());
    if !has_inv && !has_eq {
        crash_delete_file(g, ch);
        return;
    }

    let rent = RentInfo {
        rentcode: RENT_TIMEDOUT,
        time: unix_now(),
        net_cost_per_diem: cost,
        gold,
        account,
    };
    if !write_crash_file(g, ch, &rent) {
        return;
    }
    extract_all_player_objects(g, ch);
}

/// Crash_extract_expensive(): extract the single highest-rent item in the
/// player's top-level inventory (objsave.c walks ch->carrying only).
fn crash_extract_expensive(g: &mut GameState, ch: CharId) {
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let mut max: Option<ObjId> = None;
    let mut max_rent = i32::MIN;
    for oid in carrying {
        let r = obj_rent(g, oid);
        if r > max_rent {
            max_rent = r;
            max = Some(oid);
        }
    }
    if let Some(oid) = max {
        g.extract_obj(oid);
    }
}

fn inventory_rent(g: &GameState, ch: CharId) -> i32 {
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    crash_calculate_rent(g, &carrying)
}

fn equipment_rent(g: &GameState, ch: CharId) -> i32 {
    let equipment: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    crash_calculate_rent(g, &equipment)
}

/// Extract every equipped + carried object of `ch` (post-save).
fn extract_all_player_objects(g: &mut GameState, ch: CharId) {
    let mut roots: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    roots.extend(
        g.get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default(),
    );
    g.extract_objs(roots);
}

fn set_load_room_to_current(g: &mut GameState, ch: CharId) {
    let load_room = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .map(|r| g.room(r).number);
    if let Some(vnum) = load_room {
        if let Some(c) = g.get_char_mut(ch) {
            c.load_room = vnum;
        }
    }
}

// ===========================================================================
// Crash_load — the auto-equip + container-row reconstruction reader.
// ===========================================================================

/// Result of crash_load, mirroring objsave.c Crash_load return codes:
/// 0 = clean load (keep in rent room), 1 = no/crash file (go to temple),
/// 2 = rented eq lost (couldn't pay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashLoadResult {
    Clean = 0,
    CrashOrNone = 1,
    RentLost = 2,
}

/// Crash_load(): load `ch`'s objects from the plrobjs file, auto-equipping
/// worn items and rebuilding container nesting via the MAX_BAG_ROW algorithm.
/// On a rented/timed-out file it charges the accrued per-diem (or aborts to a
/// crash-save if the player can't pay), and finally re-writes the file as a
/// RENT_CRASH control block.
///
/// `pay_callback` lets the caller persist the post-charge gold/bank to the DB
/// (the C path calls save_char()); pass a closure that does the DB save, or a
/// no-op. Returns the CircleMUD load code.
pub fn crash_load_full(g: &mut GameState, ch: CharId) -> CrashLoadResult {
    let name = get_name(g, ch);
    let path = match crash_filename(&g.config.lib_path, &name) {
        Some(p) => p,
        None => return CrashLoadResult::CrashOrNone,
    };
    // Detect from raw bytes. A C file is decoded only in memory so the final
    // RENT_CRASH rewrite below can preserve its binary runtime format.
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                g.send_to_char(
                    ch,
                    "\r\n********************* NOTICE *********************\r\n\
                     There was a problem loading your objects from disk.\r\n\
                     Contact a God for assistance.\r\n",
                );
            }
            let lev = invis_lev(g, ch).max(LVL_IMMORT);
            mudlog(
                g,
                &format!("{} entering game with no equipment.", name),
                lev,
            );
            return CrashLoadResult::CrashOrNone;
        }
    };
    let text = match detect_rent_format(&bytes) {
        Some(crate::cformat::PersistenceFormat::Rust) => {
            String::from_utf8(bytes).expect("validated UTF-8 rent file")
        }
        Some(crate::cformat::PersistenceFormat::C) => {
            let (rent, elems) =
                crate::cformat::decode_rent_file(&bytes).expect("validated C rent file");
            rent_to_text(g, &rent, &elems)
        }
        None => {
            g.send_to_char(
                ch,
                "\r\n********************* NOTICE *********************\r\n\
                 There was a problem loading your objects from disk.\r\n\
                 Contact a God for assistance.\r\n",
            );
            mudlog(
                g,
                &format!("{} entering game with a corrupt equipment file.", name),
                invis_lev(g, ch).max(LVL_IMMORT),
            );
            return CrashLoadResult::CrashOrNone;
        }
    };

    let mut lines = text.lines();
    // First non-empty line should be the RENT header.
    let header_line = lines.by_ref().find(|l| !l.trim().is_empty()).unwrap_or("");
    let rent = parse_rent_header(header_line).unwrap_or_default();
    let orig_rent_code = rent.rentcode;

    // C objsave.c:190-264 Crash_clean_file + config.c:177/180
    // crash_file_timeout (10 real days) / rent_file_timeout (30 real days):
    // stale files are DELETED (crash/forced/timed-out) or their items lost
    // (rented). Without this the item-lease economy never lapses (#193).
    {
        const CRASH_FILE_TIMEOUT: f64 = 10.0 * 86400.0; // config.c:177
        const RENT_FILE_TIMEOUT: f64 = 30.0 * 86400.0; // config.c:180
        let age = unix_now().saturating_sub(rent.time) as f64;
        let limit = match rent.rentcode {
            RENT_CRASH | RENT_FORCED | RENT_TIMEDOUT => CRASH_FILE_TIMEOUT,
            RENT_RENTED => RENT_FILE_TIMEOUT,
            _ => f64::INFINITY,
        };
        if age > limit {
            let name = g
                .get_char(ch)
                .map(|c| c.get_name().to_string())
                .unwrap_or_else(|| "unknown".into());
            mudlog(
                g,
                &format!(
                    "{}'s rent file is {} days old - deleting it.",
                    name,
                    (age / 86400.0) as i64
                ),
                LVL_IMMORT,
            );
            crash_delete_file(g, ch);
            return CrashLoadResult::CrashOrNone;
        }
    }

    // Rented / timed-out: charge accrued rent or lose the equipment.
    if rent.rentcode == RENT_RENTED || rent.rentcode == RENT_TIMEDOUT {
        let secs_per_real_day = 86400.0f32;
        let num_of_days = unix_now().saturating_sub(rent.time) as f32 / secs_per_real_day;
        let cost = (rent.net_cost_per_diem as f32 * num_of_days) as i32;
        let (gold, bank) = g
            .get_char(ch)
            .map(|c| (c.points.gold, c.points.bank_gold))
            .unwrap_or((0, 0));
        if i64::from(cost) > i64::from(gold) + i64::from(bank) {
            let lev = invis_lev(g, ch).max(LVL_IMMORT);
            mudlog(
                g,
                &format!("{} entering game, rented equipment lost (no $).", name),
                lev,
            );
            crash_crashsave(g, ch);
            return CrashLoadResult::RentLost;
        } else if let Some(c) = g.get_char_mut(ch) {
            crate::gold::debit_carried_then_bank(c, i64::from(cost));
            // NOTE: C calls save_char(ch, NOWHERE) here; the async Game loop
            // owns DB persistence — the deducted gold is saved on disconnect.
        }
    }

    // Log the entry line for each rent code.
    let lev = invis_lev(g, ch).max(LVL_IMMORT);
    let entry = match orig_rent_code {
        RENT_RENTED => format!("{} un-renting and entering game.", name),
        RENT_CRASH => format!("{} retrieving crash-saved items and entering game.", name),
        RENT_CRYO => format!("{} un-cryo'ing and entering game.", name),
        RENT_FORCED | RENT_TIMEDOUT => {
            format!("{} retrieving force-saved items and entering game.", name)
        }
        _ => format!("WARNING: {} entering game with undefined rent code.", name),
    };
    mudlog(g, &entry, lev);

    // Container reconstruction rows (objsave.c cont_row[MAX_BAG_ROW]). Each row
    // is a list of object ids in original order.
    let mut cont_row: [Vec<ObjId>; MAX_BAG_ROW] = Default::default();

    for (line_offset, line) in lines.enumerate() {
        let line = line.trim_end();
        if line.is_empty() || !line.starts_with("OBJ ") {
            continue;
        }
        let (obj, locate) = match obj_from_store(g, line) {
            Some(t) => t,
            None => {
                log::warn!(
                    "SYSERR: rejected malformed Rust rent object record for {} at {}:{}",
                    name,
                    path.display(),
                    line_offset + 2
                );
                continue;
            }
        };

        // Newly-created object starts detached (Nowhere). auto_equip places it
        // into eq or inventory.
        auto_equip(g, ch, obj, locate);

        if locate > 0 {
            // ---- item equipped --------------------------------------------
            // Any pending content rows >0 have lost their container -> dump to
            // inventory.
            for j in (1..MAX_BAG_ROW).rev() {
                if !cont_row[j].is_empty() {
                    for c in cont_row[j].drain(..).collect::<Vec<_>>() {
                        g.obj_from_anywhere(c);
                        g.obj_to_char(c, ch);
                    }
                }
            }
            if !cont_row[0].is_empty() {
                let is_container = g
                    .get_obj(obj)
                    .map(|o| o.obj_type == ObjectType::Container)
                    .unwrap_or(false);
                if is_container {
                    // remove from eq, fill, re-equip.
                    let slot = (locate - 1) as usize;
                    let removed = g.unequip_char(ch, slot);
                    if let Some(removed) = removed {
                        // empty it (should already be empty) then fill in order.
                        let existing = g
                            .get_obj(removed)
                            .map(|o| o.contains.clone())
                            .unwrap_or_default();
                        for c in existing {
                            g.obj_from_anywhere(c);
                            g.obj_to_char(c, ch);
                        }
                        for c in cont_row[0].drain(..).collect::<Vec<_>>() {
                            g.obj_from_anywhere(c);
                            g.obj_to_obj(c, removed);
                        }
                        g.equip_char(ch, removed, slot);
                    }
                } else {
                    // not a container -> dump the content list to inventory.
                    for c in cont_row[0].drain(..).collect::<Vec<_>>() {
                        g.obj_from_anywhere(c);
                        g.obj_to_char(c, ch);
                    }
                }
            }
        } else {
            // ---- locate <= 0 ----------------------------------------------
            // Widen before negation so a corrupt but in-range i32::MIN locate
            // cannot overflow while the record is being rejected/flattened.
            let neg = usize::try_from(-i64::from(locate)).unwrap_or(usize::MAX);
            // 0 for inventory, 1.. for nesting.
            // Higher rows than this item's own row have lost their container.
            let mut j = MAX_BAG_ROW - 1;
            while j > neg {
                if !cont_row[j].is_empty() {
                    for c in cont_row[j].drain(..).collect::<Vec<_>>() {
                        g.obj_from_anywhere(c);
                        g.obj_to_char(c, ch);
                    }
                }
                j -= 1;
            }

            // If a content list exists at exactly this row, this object is its
            // container.
            if j == neg && !cont_row[j].is_empty() {
                let is_container = g
                    .get_obj(obj)
                    .map(|o| o.obj_type == ObjectType::Container)
                    .unwrap_or(false);
                if is_container {
                    g.obj_from_anywhere(obj); // take from char
                    let existing = g
                        .get_obj(obj)
                        .map(|o| o.contains.clone())
                        .unwrap_or_default();
                    for c in existing {
                        g.obj_from_anywhere(c);
                        g.obj_to_char(c, ch);
                    }
                    for c in cont_row[j].drain(..).collect::<Vec<_>>() {
                        g.obj_from_anywhere(c);
                        g.obj_to_obj(c, obj);
                    }
                    g.obj_to_char(obj, ch); // add to inv first
                } else {
                    for c in cont_row[j].drain(..).collect::<Vec<_>>() {
                        g.obj_from_anywhere(c);
                        g.obj_to_char(c, ch);
                    }
                }
            }

            // For a negative locate, make this object part of the content list
            // at row (-locate-1), appended to preserve original order.
            if locate < 0 && -i64::from(locate) <= MAX_BAG_ROW as i64 {
                g.obj_from_anywhere(obj);
                cont_row[(-i64::from(locate) - 1) as usize].push(obj);
            }
        }
    }

    // Re-write the file as a crash control block (RENT_CRASH).
    let crash_rent = RentInfo {
        rentcode: RENT_CRASH,
        time: unix_now(),
        ..Default::default()
    };
    // The object body on disk is unchanged from what was just loaded; only the
    // header needs rewriting. We re-serialize the player's now-loaded objects so
    // the on-disk file matches the live state exactly (idempotent with C, which
    // only rewinds and rewrites the control block).
    let _ = write_crash_file(g, ch, &crash_rent);

    if orig_rent_code == RENT_RENTED || orig_rent_code == RENT_CRYO {
        CrashLoadResult::Clean
    } else {
        CrashLoadResult::CrashOrNone
    }
}

// ===========================================================================
// Public API requested by the assignment.
// ===========================================================================

/// Public save entry point (wired from disconnect / quit). Equivalent to the C
/// Crash_crashsave path used by Crash_save_all: persist the player's current
/// inventory + equipment as a RENT_CRASH file. Returns false on write failure;
/// NPCs are an intentional successful no-op.
pub fn crash_save(g: &mut GameState, ch: CharId, lib_path: &str) -> bool {
    let _ = lib_path; // path is derived from g.config.lib_path (kept for parity)
    crash_crashsave(g, ch)
}

/// Public load entry point (wired from enter_game). Returns true on a clean
/// rent/cryo retrieval (player stays in the rent room), false when the player
/// should be sent to the temple (no file, crash file, or rent lost) — matching
/// the C convention where Crash_load()'s nonzero return triggers a temple jump.
pub fn crash_load(g: &mut GameState, ch: CharId, lib_path: &str) -> bool {
    let _ = lib_path;
    crash_load_full(g, ch) == CrashLoadResult::Clean
}

// ===========================================================================
// Crash_listrent — list a player's stored rent file (immortal "rentlist").
// ===========================================================================

/// Crash_listrent(): show the rent file header + each stored object to `ch`.
pub fn crash_listrent(g: &mut GameState, ch: CharId, name: &str) {
    let bytes = match read_rent_file_beneath_root(&g.config.lib_path, name) {
        Ok(bytes) => bytes,
        Err(_) => {
            g.send_to_char(ch, &format!("No readable rent file for {}.\r\n", name));
            return;
        }
    };
    let text = match detect_rent_format(&bytes) {
        Some(crate::cformat::PersistenceFormat::Rust) => {
            String::from_utf8(bytes).expect("validated UTF-8 rent file")
        }
        Some(crate::cformat::PersistenceFormat::C) => {
            let (rent, elems) =
                crate::cformat::decode_rent_file(&bytes).expect("validated C rent file");
            rent_to_text(g, &rent, &elems)
        }
        None => {
            g.send_to_char(ch, &format!("{} has a corrupt rent file.\r\n", name));
            return;
        }
    };
    let mut buf = format!("Rent file for {}\r\n", name);
    let mut lines = text.lines();
    let header = lines.by_ref().find(|l| !l.trim().is_empty()).unwrap_or("");
    let rent = parse_rent_header(header).unwrap_or_default();
    buf.push_str(match rent.rentcode {
        RENT_RENTED => "Rent\r\n",
        RENT_CRASH => "Crash\r\n",
        RENT_CRYO => "Cryo\r\n",
        RENT_TIMEDOUT | RENT_FORCED => "TimedOut\r\n",
        _ => "Undef\r\n",
    });
    for line in lines {
        if !line.starts_with("OBJ ") {
            continue;
        }
        let (head, tail) = match line.strip_prefix("OBJ ").and_then(|r| r.split_once('|')) {
            Some((h, t)) => (h, t),
            None => continue,
        };
        let nums: Vec<&str> = head.split_whitespace().collect();
        if nums.len() < 18 {
            continue;
        }
        let (Ok(vnum), Ok(rent_each), Ok(locate)) = (
            nums[1].parse::<i32>(),
            nums[7].parse::<i32>(),
            nums[0].parse::<i32>(),
        ) else {
            log::warn!(
                "SYSERR: rejected malformed Rust rent listing record for {}",
                name
            );
            continue;
        };
        let short = tail.splitn(4, '|').nth(1).unwrap_or("");
        buf.push_str(&format!(
            " [{:5}] ({:5}au) <{:2}> {:<20}\r\n",
            vnum, rent_each, locate, short
        ));
    }
    g.send_to_char(ch, &buf);
}

// ===========================================================================
// Receptionist / cryogenicist spec procs (objsave.c gen_receptionist)
// ===========================================================================

/// Crash_report_unrentables(): tell `ch` (via the receptionist) about each
/// unrentable object below `roots`. Returns the count found.
fn crash_report_unrentables(g: &mut GameState, ch: CharId, recep: CharId, roots: &[ObjId]) -> i32 {
    let walk = walk_object_graph(
        roots.iter().copied(),
        ObjectGraphOrder::Preorder,
        "Crash_report_unrentables",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    );
    let mut count = 0;
    for visit in walk.visits {
        if crash_is_unrentable(g, visit.id) {
            count += 1;
            let line = format!(
                "$n tells you, 'You cannot store {}.'",
                g.get_obj(visit.id)
                    .map(|x| x.short_description.clone())
                    .unwrap_or_default()
            );
            act(
                g,
                &line,
                false,
                recep,
                Some(visit.id),
                ActArg::Char(ch),
                To::Vict,
            );
        }
    }
    count
}

/// Crash_report_rent(): accumulate rentable items + cost; optionally narrate
/// each item's per-item cost. Walks the whole containment tree.
fn crash_report_rent(
    g: &mut GameState,
    ch: CharId,
    recep: CharId,
    roots: &[ObjId],
    cost: &mut i64,
    nitems: &mut i64,
    display: bool,
    factor: i32,
) {
    let walk = walk_object_graph(
        roots.iter().copied(),
        ObjectGraphOrder::Preorder,
        "Crash_report_rent",
        |oid| g.get_obj(oid).map(|o| o.contains.clone()),
    );
    for visit in walk.visits {
        if !crash_is_unrentable(g, visit.id) {
            *nitems += 1;
            let each = (obj_rent(g, visit.id) * factor).max(0);
            *cost += each as i64;
            if display {
                let line = format!(
                    "$n tells you, '{:5} coins for {}..'",
                    obj_rent(g, visit.id) * factor,
                    g.get_obj(visit.id)
                        .map(|x| x.short_description.clone())
                        .unwrap_or_default()
                );
                act(
                    g,
                    &line,
                    false,
                    recep,
                    Some(visit.id),
                    ActArg::Char(ch),
                    To::Vict,
                );
            }
        }
    }
}

/// Crash_rent_deadline(): tell the player how many days they can afford.
fn crash_rent_deadline(g: &mut GameState, ch: CharId, recep: CharId, cost: i64) {
    if cost == 0 {
        return;
    }
    let (gold, bank) = g
        .get_char(ch)
        .map(|c| (c.points.gold as i64, c.points.bank_gold as i64))
        .unwrap_or((0, 0));
    let deadline = (gold + bank) / cost;
    let line = format!(
        "$n tells you, 'You can rent for {} day{} with the gold you have\r\n\
         on hand and in the bank.'",
        deadline,
        if deadline > 1 { "s" } else { "" }
    );
    act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);
}

/// Crash_offer_rent(): the price quote. Returns the total cost (0 == decline /
/// can't store). When `display`, narrates the itemized breakdown.
fn crash_offer_rent(
    g: &mut GameState,
    ch: CharId,
    recep: CharId,
    display: bool,
    factor: i32,
) -> i64 {
    // Report unrentables first; any present cancels the offer.
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let mut roots = carrying;
    for i in 0..NUM_WEARS {
        if let Some(oid) = g.get_char(ch).and_then(|c| c.equipment[i]) {
            roots.push(oid);
        }
    }
    let norent = crash_report_unrentables(g, ch, recep, &roots);
    if norent != 0 {
        return 0;
    }

    let mut totalcost: i64 = (MIN_RENT_COST * factor) as i64;
    let mut numitems: i64 = 0;

    crash_report_rent(
        g,
        ch,
        recep,
        &roots,
        &mut totalcost,
        &mut numitems,
        display,
        factor,
    );

    if numitems == 0 {
        act(
            g,
            "$n tells you, 'But you are not carrying anything!  Just quit!'",
            false,
            recep,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        return 0;
    }
    if numitems > MAX_OBJ_SAVE {
        let line = format!(
            "$n tells you, 'Sorry, but I cannot store more than {} items.'",
            MAX_OBJ_SAVE
        );
        act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);
        return 0;
    }

    if display {
        let line = format!(
            "$n tells you, 'Plus, my {} coin fee..'",
            MIN_RENT_COST * factor
        );
        act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);

        // C objsave.c Crash_offer_rent: town-citizens (GET_CITIZEN >= 1) get a
        // percentage rent reduction equal to their citizen rank. The displayed
        // reduction is (rank * totalcost / 100) computed BEFORE the subtraction;
        // the floor is MAX(1, ...). READ_CITIZEN selects the title by sex.
        let citizen = g.get_char(ch).map(|c| c.citizen).unwrap_or(0) as i64;
        if citizen >= 1 {
            let sex = g
                .get_char(ch)
                .map(|c| c.player.sex)
                .unwrap_or(Gender::Neutral);
            let title = {
                let idx = match sex {
                    Gender::Male => 0,
                    Gender::Female => 1,
                    Gender::Neutral => 2,
                };
                crate::constants::CITIZEN_TITLES[citizen as usize][idx]
            };
            let line = format!(
                "$n tells you, 'Your fame has marked you as a {}, and I honor that with a {} coin reduction in your rent.'",
                title,
                (citizen * totalcost) / 100
            );
            act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);
            totalcost -= (totalcost * citizen) / 100;
            totalcost = totalcost.max(1);
        }

        if totalcost < 0 {
            totalcost = 0;
        }
        let line = format!(
            "$n tells you, 'For a total of {} coins{}.'",
            totalcost,
            if factor == RENT_FACTOR {
                " per day"
            } else {
                ""
            }
        );
        act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);

        let gold = g.get_char(ch).map(|c| c.points.gold as i64).unwrap_or(0);
        if totalcost > gold {
            act(
                g,
                "$n tells you, '...which I see you can't afford.'",
                false,
                recep,
                None,
                ActArg::Char(ch),
                To::Vict,
            );
            return 0;
        } else if factor == RENT_FACTOR {
            crash_rent_deadline(g, ch, recep, totalcost);
        }
    }
    totalcost
}

/// gen_receptionist(): the shared rent/offer logic for both the receptionist
/// (RENT_FACTOR) and cryogenicist (CRYO_FACTOR). Returns true if it handled the
/// command (CircleMUD SPECIAL convention).
fn gen_receptionist(g: &mut GameState, ch: CharId, recep: CharId, cmd: &str, mode: i32) -> bool {
    // !ch->desc || IS_NPC(ch): receptionist only serves real players.
    let serves = g
        .get_char(ch)
        .map(|c| c.desc.is_some() && !c.is_npc)
        .unwrap_or(false);
    if !serves {
        return false;
    }

    let action_table = [
        "smile", "dance", "sigh", "blush", "burp", "cough", "fart", "twiddle", "yawn",
    ];

    // Idle social: !cmd && !number(0,5).
    if cmd.is_empty() {
        if g.rng.number(0, 5) == 0 {
            let pick = action_table[g.rng.number(0, 8) as usize];
            crate::cmd_social::do_action_named(g, recep, pick, "");
        }
        return false;
    }

    let is_offer = cmd.eq_ignore_ascii_case("offer");
    let is_rent = cmd.eq_ignore_ascii_case("rent");
    if !is_offer && !is_rent {
        return false;
    }

    // Receptionist must be awake.
    let recep_awake = g
        .get_char(recep)
        .map(|c| c.position > Position::Sleeping)
        .unwrap_or(false);
    if !recep_awake {
        g.send_to_char(ch, "She is unable to talk to you...\r\n");
        return true;
    }

    // CAN_SEE gate (immortals bypass).
    let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
    if !g.can_see(recep, ch) && ch_level < LVL_IMMORT {
        act(
            g,
            "$n says, 'I don't deal with people I can't see!'",
            false,
            recep,
            None,
            ActArg::None,
            To::Room,
        );
        return true;
    }

    // Free rent / immortals.
    if FREE_RENT || ch_level >= LVL_IMMORT {
        act(
            g,
            "$n tells you, 'Rent is free here.  Just quit, and your objects will be saved!'",
            false,
            recep,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        return true;
    }

    if is_rent {
        let cost = crash_offer_rent(g, ch, recep, false, mode);
        if cost == 0 {
            return true;
        }
        let cost = cost.max(0);

        if mode == RENT_FACTOR {
            crash_offer_rent(g, ch, recep, true, mode);
            let line = format!(
                "$n tells you, 'Rent will cost you {} gold coins per day.'",
                cost
            );
            act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);
        } else {
            let line = format!(
                "$n tells you, 'It will cost you {} gold coins to be frozen.'",
                cost
            );
            act(g, &line, false, recep, None, ActArg::Char(ch), To::Vict);
        }

        let gold = g.get_char(ch).map(|c| c.points.gold as i64).unwrap_or(0);
        if cost > gold {
            act(
                g,
                "$n tells you, '...which I see you can't afford.'",
                false,
                recep,
                None,
                ActArg::Char(ch),
                To::Vict,
            );
            return true;
        }
        if cost != 0 && mode == RENT_FACTOR {
            crash_rent_deadline(g, ch, recep, cost);
        }

        let name = get_name(g, ch);
        if mode == RENT_FACTOR {
            act(
                g,
                "$n stores your belongings and helps you into your private chamber.",
                false,
                recep,
                None,
                ActArg::Char(ch),
                To::Vict,
            );
            if let Some(c) = g.get_char_mut(ch) {
                c.prf2_flags &= !PRF2_LOCKOUT;
            }
            crash_rentsave(g, ch, cost as i32);
            let total = g
                .get_char(ch)
                .map(|c| i64::from(c.points.gold) + i64::from(c.points.bank_gold))
                .unwrap_or(0);
            let log = format!("{} has rented ({}/day, {} tot.)", name, cost, total);
            let lev = invis_lev(g, ch).max(LVL_IMMORT);
            mudlog(g, &log, lev);
        } else {
            // cryo
            act(
                g,
                "$n stores your belongings and helps you into your private chamber.\r\n\
                 A white mist appears in the room, chilling you to the bone...\r\n\
                 You begin to lose consciousness...",
                false,
                recep,
                None,
                ActArg::Char(ch),
                To::Vict,
            );
            crash_cryosave(g, ch, cost as i32);
            if let Some(c) = g.get_char_mut(ch) {
                c.act_flags |= PLR_CRYO;
            }
            let log = format!("{} has wiz-rented.", name);
            let lev = invis_lev(g, ch).max(LVL_IMMORT);
            mudlog(g, &log, lev);
        }

        act(
            g,
            "$n helps $N into $S private chamber.",
            false,
            recep,
            None,
            ActArg::Char(ch),
            To::NotVict,
        );
        // extract_char + save_char: the async Game loop owns the player-file
        // save; we request the descriptor close so the standard quit path runs
        // (mirrors do_quit/really_quit, which the loop turns into save+extract).
        if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
            if let Some(d) = g.descriptors.get_mut(&conn) {
                d.state = crate::connection::ConState::Close;
            }
        } else {
            g.extract_char(ch);
        }
    } else {
        // offer
        crash_offer_rent(g, ch, recep, true, mode);
        act(
            g,
            "$N gives $n an offer.",
            false,
            ch,
            None,
            ActArg::Char(recep),
            To::Room,
        );
    }
    true
}

/// SPECIAL(receptionist): rent factor. `me` is the receptionist mob, `cmd` the
/// resolved command word, `arg` the remaining argument. Returns true if handled.
pub fn receptionist(g: &mut GameState, ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    gen_receptionist(g, ch, me, cmd, RENT_FACTOR)
}

/// SPECIAL(cryogenicist): cryo factor.
pub fn cryogenicist(g: &mut GameState, ch: CharId, me: CharId, cmd: &str, _arg: &str) -> bool {
    gen_receptionist(g, ch, me, cmd, CRYO_FACTOR)
}

// ===========================================================================
// do_offer / do_rent receptionist helpers (player-typed commands).
// ===========================================================================
//
// CircleMUD has no standalone do_offer/do_rent ACMDs — "offer"/"rent" are
// intercepted by the receptionist SPECIAL when the player is in the rent room.
// The assignment asks for do_offer/do_rent receptionist helpers, so these scan
// the player's room for a mob running the receptionist/cryogenicist proc and
// dispatch to it; with no receptionist present they print the C "huh?"-style
// fallback. They are wired into the command table alongside the other ACMDs.

/// Find a receptionist/cryogenicist mob in `ch`'s room (by the mob vnum the
/// spec-proc registry assigns). Returns (mob_id, factor) or None.
///
/// The spec-proc registry isn't exposed to this module, so we approximate C's
/// behaviour: dispatch to every awake NPC in the room whose name suggests a
/// receptionist; the gen_receptionist gate (desc/awake/can_see) rejects bad
/// targets. The integrator can replace this scan with the real spec-proc lookup
/// once the registry is available (see gaps).
fn dispatch_room_receptionist(g: &mut GameState, ch: CharId, cmd: &str) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let people = g.room(rnum).people.clone();
    for npc in people {
        if npc == ch {
            continue;
        }
        let is_recep = g
            .get_char(npc)
            .map(|c| {
                c.is_npc
                    && (isname("receptionist", &c.player.name)
                        || isname("cryogenicist", &c.player.name)
                        || c.short_desc
                            .as_deref()
                            .map(|s| s.to_lowercase().contains("receptionist"))
                            .unwrap_or(false))
            })
            .unwrap_or(false);
        if !is_recep {
            continue;
        }
        let factor = g
            .get_char(npc)
            .map(|c| {
                if isname("cryogenicist", &c.player.name) {
                    CRYO_FACTOR
                } else {
                    RENT_FACTOR
                }
            })
            .unwrap_or(RENT_FACTOR);
        if gen_receptionist(g, ch, npc, cmd, factor) {
            return true;
        }
    }
    false
}

/// do_offer: ask the room's receptionist for a rent quote.
pub fn do_offer(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let _ = one_argument(argument);
    if !dispatch_room_receptionist(g, ch, "offer") {
        g.send_to_char(ch, "There's no one here to make you an offer.\r\n");
    }
}

/// do_rent: rent (or cryo-rent) with the room's receptionist.
pub fn do_rent(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let _ = one_argument(argument);
    if !dispatch_room_receptionist(g, ch, "rent") {
        g.send_to_char(ch, "There's no one here to rent your equipment to.\r\n");
    }
}

// ===========================================================================
// Crash_save_all — crash-save every connected player flagged PLR_CRASH.
// ===========================================================================

/// Crash_save_all(): for each playing PC with PLR_CRASH set, crash-save and
/// clear the flag. Called on a periodic tick by the heartbeat (integrator-wired).
pub fn crash_save_all(g: &mut GameState) {
    let ids: Vec<CharId> = g
        .descriptors
        .values()
        .filter_map(|d| {
            if d.state == crate::connection::ConState::Playing {
                d.character
            } else {
                None
            }
        })
        .collect();
    for cid in ids {
        let needs = g
            .get_char(cid)
            .map(|c| !c.is_npc && (c.act_flags & PLR_CRASH) != 0)
            .unwrap_or(false);
        if needs {
            if crash_crashsave(g, cid) {
                g.request_player_save(cid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::{ConState, Descriptor};
    use crate::room::Room;
    use crate::world::ObjectProto;

    fn temp_lib_path(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "deltamud-objsave-{}-{}-{}",
            std::process::id(),
            name,
            nanos
        ));
        std::fs::create_dir_all(&path).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn save_game(name: &str) -> (GameState, CharId, RoomRnum) {
        let mut config = Config::default();
        config.lib_path = temp_lib_path(name);
        let mut g = GameState::new(config);
        let room = g.add_room(Room::new(
            3050,
            0,
            "Rent Room".to_string(),
            "A quiet inn room.".to_string(),
        ));
        let mut ch = Character::new_player("Saver".to_string(), Class::Warrior, Race::Human);
        ch.idnum = 42;
        let ch = g.create_char(ch);
        g.char_to_room(ch, room);
        (g, ch, room)
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

    #[test]
    fn crash_save_all_queues_player_row_save_for_flagged_pc() {
        let (mut g, ch, _room) = save_game("crash-save-all");
        let conn = ConnId(1);
        let mut d = Descriptor::new(conn, "test".to_string());
        d.state = ConState::Playing;
        d.character = Some(ch);
        g.descriptors.insert(conn, d);
        g.get_char_mut(ch).unwrap().act_flags |= PLR_CRASH;

        crash_save_all(&mut g);

        assert_eq!(g.get_char(ch).unwrap().act_flags & PLR_CRASH, 0);
        assert_eq!(g.player_save_requests, vec![ch]);
    }

    #[test]
    fn rent_and_cryo_save_set_load_room_and_queue_player_save() {
        let (mut g, ch, room) = save_game("rent-save");

        crash_rentsave(&mut g, ch, 0);
        assert_eq!(g.get_char(ch).unwrap().load_room, g.room(room).number);
        assert_eq!(g.player_save_requests, vec![ch]);

        g.player_save_requests.clear();
        if let Some(c) = g.get_char_mut(ch) {
            c.load_room = NOWHERE;
        }

        crash_cryosave(&mut g, ch, 0);
        assert_eq!(g.get_char(ch).unwrap().load_room, g.room(room).number);
        assert_eq!(g.player_save_requests, vec![ch]);
    }

    #[test]
    fn serializers_reject_a_shared_identity_instead_of_publishing_partial_state() {
        let (mut g, ch, _room) = save_game("graph-serialization");
        let root = g.create_obj(Object::new(
            100,
            "root".to_string(),
            "a root container".to_string(),
        ));
        let child = g.create_obj(Object::new(
            101,
            "child".to_string(),
            "a child container".to_string(),
        ));
        let shared = g.create_obj(Object::new(
            102,
            "shared".to_string(),
            "a shared object".to_string(),
        ));
        g.obj_to_char(root, ch);
        g.obj_to_obj(child, root);
        g.obj_to_obj(shared, child);
        // Malformed second parent: neither persistence format may silently
        // omit it and report a successful durable snapshot.
        g.get_obj_mut(root).unwrap().contains.push(shared);

        assert!(serialize_objects(&g, ch).is_none());
        assert!(cformat_elems(&g, ch).is_none());
    }

    #[test]
    fn c_rent_load_rebuilds_locates_and_preserves_c_format_on_rewrite() {
        let (mut g, ch, _room) = save_game("c-rent-load");
        let mut container_proto = proto(200, ObjectType::Container);
        container_proto.obj_class = 3;
        container_proto.action_description = "prototype action".into();
        g.obj_protos.insert(200, container_proto);
        g.obj_protos.insert(201, proto(201, ObjectType::Armor));
        g.obj_protos.insert(202, proto(202, ObjectType::Armor));
        let now = unix_now() as i32;
        let rent = crate::cformat::CRentInfo {
            time: now,
            rentcode: RENT_CRASH,
            net_cost_per_diem: 0,
            gold: 0,
            account: 0,
            nitems: -123,
        };
        // Exact Crash_save order for carrying [parent, sibling]: next sibling,
        // then the parent's contents, then the parent itself.
        let elems = [
            crate::cformat::obj_to_c_elem(202, 0, 0, 0, [0; 4], 0, 4, -1, 0, 0, &[]),
            crate::cformat::obj_to_c_elem(201, -1, 0, 0, [0; 4], 0, 2, -1, 0, 0, &[]),
            crate::cformat::obj_to_c_elem(200, 0, 7, 9, [1, 2, 3, 4], 0, 10, 11, 12, 13, &[]),
        ];
        let path = crash_filename(&g.config.lib_path, "Saver").unwrap();
        crate::cformat::atomic_write(&path, &crate::cformat::encode_rent_file(&rent, &elems))
            .unwrap();

        assert_eq!(crash_load_full(&mut g, ch), CrashLoadResult::CrashOrNone);
        let parent = g
            .get_char(ch)
            .unwrap()
            .carrying
            .iter()
            .find_map(|oid| {
                g.get_obj(*oid)
                    .filter(|obj| obj.item_number == 200)
                    .map(|_| *oid)
            })
            .unwrap();
        assert_eq!(g.get_obj(parent).unwrap().contains.len(), 1);
        assert_eq!(g.get_obj(parent).unwrap().obj_class, 3);
        assert_eq!(
            g.get_obj(parent).unwrap().action_description.as_deref(),
            Some("prototype action")
        );
        assert_eq!(
            g.get_obj(g.get_obj(parent).unwrap().contains[0])
                .unwrap()
                .item_number,
            201
        );

        let rewritten = std::fs::read(&path).unwrap();
        assert!(!rewritten.starts_with(b"RENT "));
        let (_, rewritten) = crate::cformat::decode_rent_file(&rewritten).unwrap();
        assert_eq!(
            rewritten
                .iter()
                .map(|elem| (elem.locate, elem.item_number))
                .collect::<Vec<_>>(),
            vec![(0, 202), (-1, 201), (0, 200)]
        );
        assert_eq!(rewritten[2].weight, 10);
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    #[test]
    fn c_rent_conversion_rejects_locates_outside_i32_without_wrapping() {
        let mut g = GameState::new(Config::default());
        g.obj_protos.insert(450, proto(450, ObjectType::Armor));
        let rent = crate::cformat::CRentInfo {
            time: 0,
            rentcode: RENT_CRASH,
            net_cost_per_diem: 0,
            gold: 0,
            account: 0,
            nitems: 2,
        };
        let elems = [
            crate::cformat::obj_to_c_elem(
                450,
                i64::from(i32::MAX) + 1,
                0,
                0,
                [0; 4],
                0,
                1,
                -1,
                0,
                0,
                &[],
            ),
            crate::cformat::obj_to_c_elem(
                450,
                i64::from(i32::MIN),
                0,
                0,
                [0; 4],
                0,
                1,
                -1,
                0,
                0,
                &[],
            ),
        ];

        let converted = rent_to_text(&g, &rent, &elems);
        let objects: Vec<_> = converted
            .lines()
            .filter(|line| line.starts_with("OBJ "))
            .collect();
        assert_eq!(objects.len(), 1);
        assert!(objects[0].starts_with("OBJ -2147483648 450 "));
    }

    #[test]
    fn existing_rust_rent_file_remains_text_on_rewrite() {
        let (mut g, ch, _room) = save_game("rust-rent-save");
        let path = crash_filename(&g.config.lib_path, "Saver").unwrap();
        crate::cformat::atomic_write(
            &path,
            format!("RENT {} {} 0 0 0\n", RENT_CRASH, unix_now()).as_bytes(),
        )
        .unwrap();
        let object = g.create_obj(Object::new(400, "keepsake".into(), "a keepsake".into()));
        g.obj_to_char(object, ch);

        let lib = g.config.lib_path.clone();
        assert!(crash_save(&mut g, ch, &lib));
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.starts_with(b"RENT "));
        assert!(std::str::from_utf8(&saved).unwrap().contains("OBJ 0 400 "));
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    #[test]
    fn pre_objclass_rust_rent_record_remains_readable() {
        let mut g = GameState::new(Config::default());
        let line = "OBJ 0 401 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 0|old|an old object|Old object.|";
        let (oid, locate) = obj_from_store(&mut g, line).unwrap();
        let object = g.get_obj(oid).unwrap();
        assert_eq!(locate, 0);
        assert_eq!(object.item_number, 401);
        assert_eq!(object.obj_class, -1);
        assert_eq!(object.min_level, 10);
        assert_eq!(object.total_slots, 13);
    }

    #[test]
    fn rust_rent_header_accepts_boundaries_and_rejects_i32_overflow() {
        let header = format!(
            "RENT {} {} {} {} {}",
            i32::MIN,
            i64::MIN,
            i32::MAX,
            i32::MIN,
            i32::MAX
        );
        let parsed = parse_rent_header(&header).unwrap();
        assert_eq!(parsed.rentcode, i32::MIN);
        assert_eq!(parsed.time, i64::MIN);
        assert_eq!(parsed.net_cost_per_diem, i32::MAX);
        assert_eq!(parsed.gold, i32::MIN);
        assert_eq!(parsed.account, i32::MAX);

        for header in [
            "RENT 2147483648 0 0 0 0",
            "RENT 0 -9223372036854775809 0 0 0",
            "RENT 0 0 -2147483649 0 0",
            "RENT 0 0 0 not-a-number 0",
        ] {
            assert!(parse_rent_header(header).is_none(), "accepted {header}");
        }
    }

    #[test]
    fn rust_rent_record_rejects_each_present_malformed_numeric_column() {
        let base =
            "OBJ 0 401 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 1 5 -6 3|old|an old object|Old object.|";
        let (head, tail) = base.split_once('|').unwrap();
        let fields: Vec<_> = head.split_whitespace().collect();
        // Skip the literal OBJ token. This covers every required fixed field,
        // the affect pair, and the present optional obj_class column.
        for field in 1..fields.len() {
            let mut malformed = fields.clone();
            malformed[field] = "not-a-number";
            let line = format!("{}|{}", malformed.join(" "), tail);
            let mut g = GameState::new(Config::default());
            assert!(
                obj_from_store(&mut g, &line).is_none(),
                "numeric field {} was silently defaulted",
                field - 1
            );
            assert!(g.objs.is_empty());
        }

        let mut g = GameState::new(Config::default());
        assert!(
            obj_from_store(
                &mut g,
                "OBJ 0 401 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 1 5|bad|bad|bad|"
            )
            .is_none()
        );
        assert!(g.objs.is_empty(), "an incomplete affect pair was loaded");
    }

    #[test]
    fn real_rent_load_keeps_i32_boundaries_and_rejects_overflow_records() {
        let (mut g, ch, _room) = save_game("numeric-boundaries");
        let path = crash_filename(&g.config.lib_path, "Saver").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            "RENT {} {} 0 0 0\n\
             OBJ 0 2147483648 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 0|overflow|overflow|overflow|\n\
             OBJ -2147483649 402 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 0|underflow|underflow|underflow|\n\
             OBJ 0 2147483647 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 0|max|max|max|\n\
             OBJ -2147483648 403 9 1 0 5 6 7 -1 10 11 12 13 1 2 3 4 0|min|min|min|\n",
            RENT_CRASH,
            unix_now()
        );
        std::fs::write(&path, body).unwrap();

        assert_eq!(crash_load_full(&mut g, ch), CrashLoadResult::CrashOrNone);
        let loaded: Vec<_> = g
            .get_char(ch)
            .unwrap()
            .carrying
            .iter()
            .filter_map(|oid| g.get_obj(*oid).map(|obj| obj.item_number))
            .collect();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(&i32::MAX));
        assert!(loaded.contains(&403));
        assert!(!loaded.contains(&NOTHING));
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    #[test]
    fn crash_save_reports_write_failure_and_keeps_dirty_flag() {
        let path = temp_lib_path("write-failure-root");
        std::fs::remove_dir_all(&path).unwrap();
        std::fs::write(&path, b"not a directory").unwrap();
        let mut config = Config::default();
        config.lib_path = path.clone();
        let mut g = GameState::new(config);
        let mut player = Character::new_player("Saver".into(), Class::Warrior, Race::Human);
        player.act_flags |= PLR_CRASH;
        let player = g.create_char(player);

        assert!(!crash_save(&mut g, player, &path));
        assert_ne!(g.get_char(player).unwrap().act_flags & PLR_CRASH, 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn malformed_object_graph_preserves_the_last_durable_crash_file() {
        let (mut g, player, _room) = save_game("malformed-graph-save");
        let root = g.create_obj(Object::new(501, "root".into(), "root".into()));
        let child = g.create_obj(Object::new(502, "child".into(), "child".into()));
        g.obj_to_char(root, player);
        g.obj_to_obj(child, root);
        g.get_obj_mut(root).unwrap().contains.push(child);
        g.get_char_mut(player).unwrap().act_flags |= PLR_CRASH;

        let path = crash_filename(&g.config.lib_path, "Saver").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"last known good snapshot").unwrap();

        assert!(!crash_crashsave(&mut g, player));
        assert_eq!(std::fs::read(&path).unwrap(), b"last known good snapshot");
        assert_ne!(g.get_char(player).unwrap().act_flags & PLR_CRASH, 0);
        let _ = std::fs::remove_dir_all(&g.config.lib_path);
    }

    #[test]
    fn rent_paths_accept_mixed_case_in_every_bucket_and_reject_hostile_names() {
        let cases = [
            ("Alice", "A-E/alice.objs"),
            ("Farah", "F-J/farah.objs"),
            ("Kora", "K-O/kora.objs"),
            ("Pia", "P-T/pia.objs"),
            ("Uma", "U-Z/uma.objs"),
        ];
        for (name, suffix) in cases {
            let path = crash_filename("/mud/lib", name).expect("valid player name");
            assert!(path.ends_with(suffix));
        }
        for invalid in [
            "../Alice",
            "/tmp/Alice",
            "Alice/Bob",
            ".Alice",
            "Ali\0ce",
            "Al／ice",
            "A",
            "ANameThatIsFarTooLongForPlayers",
        ] {
            assert!(crash_filename("/mud/lib", invalid).is_none(), "{invalid:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rent_listing_reader_refuses_final_and_bucket_symlinks() {
        use std::os::unix::fs::symlink;

        let lib = temp_lib_path("rent-symlink");
        let root = std::path::Path::new(&lib).join("plrobjs");
        let bucket = root.join("A-E");
        std::fs::create_dir_all(&bucket).unwrap();
        let outside = std::path::Path::new(&lib).join("outside.objs");
        std::fs::write(&outside, b"secret").unwrap();
        symlink(&outside, bucket.join("alice.objs")).unwrap();
        assert!(read_rent_file_beneath_root(&lib, "Alice").is_err());

        std::fs::remove_dir_all(&bucket).unwrap();
        let external_bucket = std::path::Path::new(&lib).join("external-bucket");
        std::fs::create_dir_all(&external_bucket).unwrap();
        std::fs::write(external_bucket.join("alice.objs"), b"secret").unwrap();
        symlink(&external_bucket, &bucket).unwrap();
        assert!(read_rent_file_beneath_root(&lib, "Alice").is_err());

        // `plrobjs` itself is also an untrusted intermediate component. A
        // canonicalize-then-open implementation could be redirected here.
        std::fs::remove_file(&bucket).unwrap();
        std::fs::remove_dir(&root).unwrap();
        symlink(&external_bucket, &root).unwrap();
        assert!(read_rent_file_beneath_root(&lib, "Alice").is_err());

        let _ = std::fs::remove_dir_all(lib);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rent_listing_reader_pins_bucket_across_adversarial_path_swap() {
        use std::os::unix::fs::symlink;

        let lib = temp_lib_path("rent-bucket-swap");
        let root = std::path::Path::new(&lib).join("plrobjs");
        let bucket = root.join("A-E");
        let pinned_bucket = root.join("A-E-pinned");
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(bucket.join("alice.objs"), b"safe rent data").unwrap();

        let external_bucket = std::path::Path::new(&lib).join("external-bucket");
        std::fs::create_dir_all(&external_bucket).unwrap();
        std::fs::write(external_bucket.join("alice.objs"), b"secret outside data").unwrap();

        let bytes = read_rent_file_beneath_root_after_parent_open(&lib, "Alice", || {
            // This is the deterministic form of the old check/open race: after
            // validation, replace the pathname with a symlink to another tree.
            std::fs::rename(&bucket, &pinned_bucket).unwrap();
            symlink(&external_bucket, &bucket).unwrap();
        })
        .unwrap();
        assert_eq!(bytes, b"safe rent data");

        let _ = std::fs::remove_dir_all(lib);
    }
}

#[cfg(test)]
mod durability_roundtrip_tests {
    use super::*;

    #[test]
    fn obj_to_store_round_trips_bitvector_and_durability() {
        // Issue #233: the record must carry bitvector + curr/total_slots;
        // a rent cycle used to zero them (objsave.c:89-117 writes all three).
        let mut g = GameState::new(crate::config::Config::default());
        let mut o =
            crate::object::Object::new(99, "scythe death".into(), "the Scythe of Death".into());
        o.bitvector = 0x4000_0000;
        o.curr_slots = 7;
        o.total_slots = 42;
        let oid = g.create_obj(o);

        let mut out = String::new();
        obj_to_store(&g, oid, 3, &mut out);
        let (back, locate) = obj_from_store(&mut g, out.trim_end()).expect("line parses");
        assert_eq!(locate, 3);
        let o2 = g.get_obj(back).unwrap();
        assert_eq!(o2.bitvector, 0x4000_0000, "bitvector survives a rent cycle");
        assert_eq!(o2.curr_slots, 7);
        assert_eq!(o2.total_slots, 42);
    }
}
