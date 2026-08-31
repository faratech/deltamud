// objsave.rs — player object (rent / crash) persistence, ported 1:1 from
// CircleMUD/DeltaMUD `src/objsave.c`. Covers the per-player inventory +
// equipment save/load to lib/plrobjs, the auto-equip layer, and the
// receptionist/cryogenicist rent special procs.
//
// ON-DISK FORMAT
// --------------
// The C code fwrite()s a `struct rent_info` header followed by a stream of
// `struct obj_file_elem` records. Those raw binary layouts are non-portable
// (long width, struct padding) and — more importantly — the Rust port has no
// other consumer of the packed blob, so this module persists the SAME logical
// fields in a documented LINE-ORIENTED TEXT format (matching the precedent set
// by house.rs). The packed `obj_file_elem` semantics are reproduced exactly:
//
//   header line:  RENT <rentcode> <time> <net_cost_per_diem> <gold> <account>
//   object line:  OBJ <locate> <vnum> <type> <wearbits> <extrabits> <weight>
//                     <cost> <rent> <timer> <min_level> <bitvector>
//                     <curr_slots> <total_slots> <v0> <v1> <v2> <v3>
//                     <#affects> [loc mod ...] |<name>|<short>|<long>|<action>
//
// The `locate` field is the C obj_file_elem.locate encoding, preserved 1:1:
//   * locate  > 0 : equipped, value == wear_slot + 1
//   * locate == 0 : top-level inventory
//   * locate  < 0 : inside a container, value == -(container_row + 1)
// On load, auto_equip()/the container-row reconstruction interpret it exactly
// as Crash_load() does. (bitvector / curr_slots / total_slots have no field on
// the Rust Object yet and are written as 0; they round-trip for format
// fidelity and so a future Object can adopt them without a file-format break.)
//
// WIRING (the integrator hooks these from game.rs; the C call sites are noted):
//   * enter_game()  -> crash_load(g, ch, &g.config.lib_path)  (C: Crash_load)
//   * disconnect()/ -> crash_save(g, ch, &g.config.lib_path)  (C: Crash_crashsave
//     do_quit          via Crash_save_all / really_quit's save path)
//   * receptionist / cryogenicist are SPECIAL() spec procs, dispatched by the
//     mob spec-proc layer exactly like shop::shop_keeper.

use crate::act::{act, ActArg, To};
use crate::handler::isname;
use crate::interpreter::one_argument;
use crate::object::{ExtraFlags, Object, ObjectAffect, ObjectType, WearFlags};
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
fn crash_filename(lib: &str, name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() {
        return None;
    }
    let lname = name.to_lowercase();
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
    eprintln!("[ {} ]", line);
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
        o.weight,
        o.cost,
        o.rent,
        o.timer,
        o.level,            // min_level
        o.bitvector,        // affect bitvector
        o.curr_slots,       // durability counters
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
    out.push_str(&format!(
        "|{}|{}|{}|{}\n",
        sanitize(&o.name),
        sanitize(&o.short_description),
        sanitize(&o.description),
        sanitize(o.action_description.as_deref().unwrap_or(""))
    ));
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
    let locate: i32 = nums[0].parse().ok()?;
    let vnum: ObjVnum = nums[1].parse().unwrap_or(NOTHING);
    let ty: i32 = nums[2].parse().unwrap_or(9);
    let wear: u32 = nums[3].parse().unwrap_or(0);
    let extra: u64 = nums[4].parse().unwrap_or(0);
    let weight: i32 = nums[5].parse().unwrap_or(1);
    let cost: i32 = nums[6].parse().unwrap_or(0);
    let rent: i32 = nums[7].parse().unwrap_or(0);
    let timer: i32 = nums[8].parse().unwrap_or(-1);
    let min_level: i32 = nums[9].parse().unwrap_or(0);
    let bitvector: i64 = nums[10].parse().unwrap_or(0);
    let curr_slots: i32 = nums[11].parse().unwrap_or(0);
    let total_slots: i32 = nums[12].parse().unwrap_or(0);
    let v0: i32 = nums[13].parse().unwrap_or(0);
    let v1: i32 = nums[14].parse().unwrap_or(0);
    let v2: i32 = nums[15].parse().unwrap_or(0);
    let v3: i32 = nums[16].parse().unwrap_or(0);
    let naff: usize = nums[17].parse().unwrap_or(0);

    let mut affects = Vec::new();
    let mut idx = 18;
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
    obj.level = min_level.clamp(0, u8::MAX as i32) as u8;
    obj.values = [v0, v1, v2, v3];
    obj.affects = affects;
    obj.bitvector = bitvector;
    obj.curr_slots = curr_slots;
    obj.total_slots = total_slots;

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

/// Collect every object id in a containment tree (the object + recursively its
/// contents). Used so we can safely mutate while iterating.
fn collect_tree(g: &GameState, oid: ObjId, out: &mut Vec<ObjId>) {
    out.push(oid);
    if let Some(o) = g.get_obj(oid) {
        let contains = o.contains.clone();
        for c in contains {
            collect_tree(g, c, out);
        }
    }
}

/// Crash_extract_norents(): extract unrentable items anywhere in `oid`'s tree.
fn crash_extract_norents(g: &mut GameState, oid: ObjId) {
    let mut tree = Vec::new();
    collect_tree(g, oid, &mut tree);
    for o in tree {
        if g.get_obj(o).is_some() && crash_is_unrentable(g, o) {
            g.obj_from_anywhere(o);
            g.extract_obj(o);
        }
    }
}

/// Crash_extract_norents_from_equipped(): move outright-unrentable equipment to
/// inventory; otherwise scrub norents out of worn containers.
fn crash_extract_norents_from_equipped(g: &mut GameState, ch: CharId) {
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
            crash_extract_norents(g, oid);
        }
    }
}

// ===========================================================================
// rent cost calculation (objsave.c Crash_calculate_rent)
// ===========================================================================

fn crash_calculate_rent(g: &GameState, oid: ObjId) -> i32 {
    let mut tree = Vec::new();
    collect_tree(g, oid, &mut tree);
    let mut cost = 0;
    for o in tree {
        cost += obj_rent(g, o).max(0);
    }
    cost
}

// ===========================================================================
// Crash_save: recursive object serialization with the locate encoding.
// ===========================================================================

/// Crash_save(): serialize `oid`, then siblings, then its contents (contents at
/// MIN(0,locate)-1 — negative rows for container nesting), faithfully replaying
/// objsave.c's recursion order. The C weight-subtraction dance is unnecessary
/// here because we store each object's own (un-aggregated) weight directly.
fn crash_save_obj(g: &GameState, oid: ObjId, locate: i32, out: &mut String) {
    // C: Crash_save(next_content); Crash_save(contains, MIN(0,locate)-1); store.
    // We emit this object first, then recurse into contents at the next row,
    // which yields the identical (locate, object) set the C stream produces.
    obj_to_store(g, oid, locate, out);
    let contains = g
        .get_obj(oid)
        .map(|o| o.contains.clone())
        .unwrap_or_default();
    let inner_locate = locate.min(0) - 1;
    for c in contains {
        crash_save_obj(g, c, inner_locate, out);
    }
}

/// Serialize a character's full equipment + inventory into a text body, in the
/// C save order: each worn slot (locate = j+1) then carried (locate = 0).
fn serialize_objects(g: &GameState, ch: CharId) -> String {
    let mut out = String::new();
    let (equipment, carrying) = match g.get_char(ch) {
        Some(c) => (c.equipment, c.carrying.clone()),
        None => return out,
    };
    for j in 0..NUM_WEARS {
        if let Some(oid) = equipment[j] {
            crash_save_obj(g, oid, j as i32 + 1, &mut out);
        }
    }
    for oid in carrying {
        crash_save_obj(g, oid, 0, &mut out);
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
        rentcode: p[0].parse().unwrap_or(RENT_UNDEF),
        time: p[1].parse().unwrap_or(0),
        net_cost_per_diem: p[2].parse().unwrap_or(0),
        gold: p[3].parse().unwrap_or(0),
        account: p[4].parse().unwrap_or(0),
    })
}

/// Low-level file write: header + serialized object body to the player's
/// plrobjs file (creating the bucket directory as needed).
fn write_crash_file(g: &GameState, ch: CharId, rent: &RentInfo) -> bool {
    let name = get_name(g, ch);
    let path = match crash_filename(&g.config.lib_path, &name) {
        Some(p) => p,
        None => return false,
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = write_rent_header(rent);
    body.push_str(&serialize_objects(g, ch));
    std::fs::write(&path, body).is_ok()
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
/// inventory (no extraction). Clears PLR_CRASH. Skips NPCs.
pub fn crash_crashsave(g: &mut GameState, ch: CharId) {
    if is_npc(g, ch) {
        return;
    }
    let rent = RentInfo {
        rentcode: RENT_CRASH,
        time: unix_now(),
        ..Default::default()
    };
    if !write_crash_file(g, ch, &rent) {
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.act_flags &= !PLR_CRASH;
    }
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
    for oid in carrying {
        crash_extract_norents(g, oid);
    }

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
    for oid in carrying {
        crash_extract_norents(g, oid);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.points.gold = (c.points.gold - cost).max(0);
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
    for oid in carrying {
        crash_extract_norents(g, oid);
    }

    let (gold, account) = g
        .get_char(ch)
        .map(|c| (c.points.gold, c.points.bank_gold))
        .unwrap_or((0, 0));

    // inventory + equipment rent, each doubled (forcerent is 2x normal).
    let mut cost = inventory_rent(g, ch) << 1;
    let mut cost_eq = equipment_rent(g, ch) << 1;

    if cost + cost_eq > gold + account {
        // unequip everything (eq folds into inventory).
        for j in 0..NUM_WEARS {
            if let Some(o) = g.unequip_char(ch, j) {
                g.obj_to_char(o, ch);
            }
        }
        cost += cost_eq;
        cost_eq = 0;
        let _ = cost_eq;

        // drop the single most expensive carried item until affordable.
        while cost > gold + account
            && !g
                .get_char(ch)
                .map(|c| c.carrying.is_empty())
                .unwrap_or(true)
        {
            crash_extract_expensive(g, ch);
            cost = inventory_rent(g, ch) << 1;
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
        g.obj_from_anywhere(oid);
        g.extract_obj(oid);
    }
}

fn inventory_rent(g: &GameState, ch: CharId) -> i32 {
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    carrying.iter().map(|&o| crash_calculate_rent(g, o)).sum()
}

fn equipment_rent(g: &GameState, ch: CharId) -> i32 {
    let mut cost = 0;
    for j in 0..NUM_WEARS {
        if let Some(o) = g.get_char(ch).and_then(|c| c.equipment[j]) {
            cost += crash_calculate_rent(g, o);
        }
    }
    cost
}

/// Detach and extract every equipped + carried object of `ch` (post-save).
fn extract_all_player_objects(g: &mut GameState, ch: CharId) {
    let mut roots: Vec<ObjId> = Vec::new();
    for j in 0..NUM_WEARS {
        if let Some(o) = g.unequip_char(ch, j) {
            roots.push(o);
        }
    }
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    for o in carrying {
        g.obj_from_anywhere(o);
        roots.push(o);
    }
    for o in roots {
        g.extract_obj(o);
    }
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
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
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

    let mut lines = text.lines();
    // First non-empty line should be the RENT header.
    let header_line = lines.by_ref().find(|l| !l.trim().is_empty()).unwrap_or("");
    let rent = parse_rent_header(header_line).unwrap_or_default();
    let orig_rent_code = rent.rentcode;

    // Rented / timed-out: charge accrued rent or lose the equipment.
    if rent.rentcode == RENT_RENTED || rent.rentcode == RENT_TIMEDOUT {
        let secs_per_real_day = 86400.0f32;
        let num_of_days = (unix_now() - rent.time) as f32 / secs_per_real_day;
        let cost = (rent.net_cost_per_diem as f32 * num_of_days) as i32;
        let (gold, bank) = g
            .get_char(ch)
            .map(|c| (c.points.gold, c.points.bank_gold))
            .unwrap_or((0, 0));
        if cost > gold + bank {
            let lev = invis_lev(g, ch).max(LVL_IMMORT);
            mudlog(
                g,
                &format!("{} entering game, rented equipment lost (no $).", name),
                lev,
            );
            crash_crashsave(g, ch);
            return CrashLoadResult::RentLost;
        } else if let Some(c) = g.get_char_mut(ch) {
            let pay_from_bank = (cost - c.points.gold).max(0);
            c.points.bank_gold -= pay_from_bank;
            c.points.gold = (c.points.gold - cost).max(0);
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

    for line in lines {
        let line = line.trim_end();
        if line.is_empty() || !line.starts_with("OBJ ") {
            continue;
        }
        let (obj, locate) = match obj_from_store(g, line) {
            Some(t) => t,
            None => continue,
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
            let neg = (-locate) as usize; // 0 for inventory, 1.. for nesting.
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
            if locate < 0 && (-locate) <= MAX_BAG_ROW as i32 {
                g.obj_from_anywhere(obj);
                cont_row[(-locate - 1) as usize].push(obj);
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
/// inventory + equipment as a RENT_CRASH file. No-op for NPCs / link-dead.
pub fn crash_save(g: &mut GameState, ch: CharId, lib_path: &str) {
    let _ = lib_path; // path is derived from g.config.lib_path (kept for parity)
    crash_crashsave(g, ch);
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
    let path = match crash_filename(&g.config.lib_path, name) {
        Some(p) => p,
        None => return,
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            g.send_to_char(ch, &format!("{} has no rent file.\r\n", name));
            return;
        }
    };
    let mut buf = format!("{}\r\n", path.display());
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
        let vnum: i32 = nums[1].parse().unwrap_or(-1);
        let rent_each: i32 = nums[7].parse().unwrap_or(0);
        let locate: i32 = nums[0].parse().unwrap_or(0);
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
/// unrentable object in `oid`'s tree. Returns the count found.
fn crash_report_unrentables(g: &mut GameState, ch: CharId, recep: CharId, oid: ObjId) -> i32 {
    let mut tree = Vec::new();
    collect_tree(g, oid, &mut tree);
    let mut count = 0;
    for o in tree {
        if crash_is_unrentable(g, o) {
            count += 1;
            let line = format!(
                "$n tells you, 'You cannot store {}.'",
                g.get_obj(o)
                    .map(|x| x.short_description.clone())
                    .unwrap_or_default()
            );
            act(g, &line, false, recep, Some(o), ActArg::Char(ch), To::Vict);
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
    oid: ObjId,
    cost: &mut i64,
    nitems: &mut i64,
    display: bool,
    factor: i32,
) {
    let mut tree = Vec::new();
    collect_tree(g, oid, &mut tree);
    for o in tree {
        if !crash_is_unrentable(g, o) {
            *nitems += 1;
            let each = (obj_rent(g, o) * factor).max(0);
            *cost += each as i64;
            if display {
                let line = format!(
                    "$n tells you, '{:5} coins for {}..'",
                    obj_rent(g, o) * factor,
                    g.get_obj(o)
                        .map(|x| x.short_description.clone())
                        .unwrap_or_default()
                );
                act(g, &line, false, recep, Some(o), ActArg::Char(ch), To::Vict);
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
    let mut norent = 0;
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    for oid in &carrying {
        norent += crash_report_unrentables(g, ch, recep, *oid);
    }
    for i in 0..NUM_WEARS {
        if let Some(oid) = g.get_char(ch).and_then(|c| c.equipment[i]) {
            norent += crash_report_unrentables(g, ch, recep, oid);
        }
    }
    if norent != 0 {
        return 0;
    }

    let mut totalcost: i64 = (MIN_RENT_COST * factor) as i64;
    let mut numitems: i64 = 0;

    for oid in &carrying {
        crash_report_rent(
            g,
            ch,
            recep,
            *oid,
            &mut totalcost,
            &mut numitems,
            display,
            factor,
        );
    }
    for i in 0..NUM_WEARS {
        if let Some(oid) = g.get_char(ch).and_then(|c| c.equipment[i]) {
            crash_report_rent(
                g,
                ch,
                recep,
                oid,
                &mut totalcost,
                &mut numitems,
                display,
                factor,
            );
        }
    }

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
                .map(|c| c.points.gold + c.points.bank_gold)
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
            crash_crashsave(g, cid);
            g.request_player_save(cid);
            if let Some(c) = g.get_char_mut(cid) {
                c.act_flags &= !PLR_CRASH;
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
}

#[cfg(test)]
mod durability_roundtrip_tests {
    use super::*;

    #[test]
    fn obj_to_store_round_trips_bitvector_and_durability() {
        // Issue #233: the record must carry bitvector + curr/total_slots;
        // a rent cycle used to zero them (objsave.c:89-117 writes all three).
        let mut g = GameState::new(crate::config::Config::default());
        let mut o = crate::object::Object::new(99, "scythe death".into(), "the Scythe of Death".into());
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
