// spec_assign.rs — the vnum->special-procedure registry plus the central
// `special()` dispatcher (CircleMUD spec_assign.c + interpreter.c::special).
//
// In C the spec is stored directly on the prototype index entry
// (`mob_index[rnum].func`, `obj_index[rnum].func`, `world[rnum].func`) and
// `special()` walks the actor's room (room spec, equipment, inventory, mobs
// present, objs present) calling each non-NULL func in turn, stopping at the
// first that returns 1.
//
// The Rust port keeps the world data immutable-by-tier, so instead of a `func`
// pointer on every prototype we keep three side tables (vnum -> SpecFn) built
// once at boot by `assign_specs()`. `special()` then performs the same five-
// phase walk, looking each present entity's prototype vnum up in those tables.
//
// Wired procs:
//   * postmaster          (mail::postmaster)        — mob vnums 199, 1201
//   * receptionist        (objsave::receptionist)   — mob vnums 1200, 101
//   * puff                (spec_procs::puff)         — mob vnum 1 (Limbo)
//   * janitor             (spec_procs::janitor)      — mob vnum 1202
//   * librarian           (spec_procs::librarian)    — mob vnum 102 (Itrius)
//   * arenaentrancemaster (arena::arenaentrancemaster) — mob vnum 4800
//   * temple_healer       (spec_procs::temple_healer) — mob vnum 4801
//   * temple_mana_regenerator (spec_procs::…)        — mob vnum 4802
//   * gen_board           (boards::board)           — obj vnums 199/198/197 + god boards
//   * portal              (spec_procs::portal)       — obj vnum 20
//   * tent                (spec_procs::tent)         — obj vnum 500
//   * shop_keeper         (shop::shop_keeper)        — every shop keeper mob; resolved
//                         dynamically because shop_keeper() self-validates by vnum and
//                         returns false for any mob that does not own a shop (mirroring
//                         C's assign_the_shopkeepers() in shop.c, which is separate
//                         from spec_assign.c's assign_mobiles()).
//   * dump / pet_shops    (spec_procs::…)            — registered via register_room_specs
//   * king's-castle procs (castle::assign_kings_castle)
//
// The remaining spec_assign.c SPECIAL() declarations (cryogenicist, mayor, snake,
// thief, magic_user, cityguard, guild, guild_guard, trainer, pray_for_items) are
// either bound through OLC/medit data rather than a code ASSIGNMOB, or (bank,
// pray_for_items) are declared-but-never-assigned no-ops in the C source itself.
// They are absent from the static tables here, which is byte-for-byte equivalent
// to C leaving the corresponding `func` NULL: the spec walk skips them and the
// command falls through to normal dispatch.

use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::OnceLock;

/// A *mobile* special procedure: `(game, actor, me, cmd, arg) -> consumed`.
/// `me` is the mob running the proc (CircleMUD's `me`, a `char_data*`).
/// `cmd` is the command word the actor typed (lowercased, possibly abbreviated;
/// procs prefix-match it against canonical names exactly like the interpreter),
/// or "" for a periodic pulse call. Returns true iff the proc consumed the
/// command and normal dispatch must be skipped.
///
/// This is the canonical SpecFn name referenced elsewhere (mobs are the common
/// case). C's `SPECIAL()` macro type-erases `me` to `void*`; Rust cannot, so
/// object specs use the parallel `ObjSpecFn` below with `me: ObjId`.
pub type SpecFn = fn(&mut GameState, CharId, CharId, &str, &str) -> bool;

/// An *object* special procedure: identical to SpecFn but its `me` is the
/// object running the proc (CircleMUD's `me`, an `obj_data*`).
pub type ObjSpecFn = fn(&mut GameState, CharId, ObjId, &str, &str) -> bool;

/// A room special procedure. C passes `(ch, world + room, cmd, arg)`; the room
/// has no entity id in this arena, so the proc receives the room's rnum as
/// `me_room`. No room procs are ported yet, but the type + table exist so the
/// `special()` walk is structurally complete and a future dump/pet_shops port
/// only needs to register here.
pub type RoomSpecFn = fn(&mut GameState, CharId, RoomRnum, &str, &str) -> bool;

/// The three side tables, mirroring C's `*_index[rnum].func` / `world[rnum].func`.
struct SpecTables {
    mobs: HashMap<MobVnum, SpecFn>,
    objs: HashMap<ObjVnum, ObjSpecFn>,
    rooms: HashMap<RoomVnum, RoomSpecFn>,
}

static SPECS: OnceLock<SpecTables> = OnceLock::new();

/// ROOM_DEATH room vnums, captured from the loaded world before `assign_specs()`
/// builds the room table. C's assign_rooms() reads `world[i]` directly inside
/// the dts_are_dumps loop; our table-build runs without a GameState borrow, so
/// `set_death_trap_rooms()` (called by main.rs after load_world) stashes the
/// vnums here for `assign_rooms()` to consume. Empty if never set (dts_are_dumps
/// off, or no world) -> no death-trap dumps, exactly as the C loop is a no-op
/// when dts_are_dumps is NO.
static DEATH_TRAP_ROOMS: OnceLock<Vec<RoomVnum>> = OnceLock::new();

/// Record the ROOM_DEATH room vnums for the dts_are_dumps dump registration.
/// Call once, after the world loads and before `assign_specs()`. Idempotent
/// (later calls are ignored) since the spec tables are built exactly once.
pub fn set_death_trap_rooms(vnums: Vec<RoomVnum>) {
    let _ = DEATH_TRAP_ROOMS.set(vnums);
}

fn death_trap_rooms() -> Vec<RoomVnum> {
    DEATH_TRAP_ROOMS.get().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Spec-proc adapters.
//
// The ported procs do not all share the bare SpecFn signature, so we wrap them
// to the unified `(g, ch, me, cmd, arg)` shape:
//   * shop::shop_keeper already matches SpecFn exactly.
//   * mail::postmaster already matches SpecFn exactly (its `me` is the mailman).
//   * boards::board takes (g, ch, cmd, arg) and self-locates its board object,
//     so we drop `me` here.
// ---------------------------------------------------------------------------

/// Adapter so boards::board (which finds its own board object in the room) fits
/// the ObjSpecFn signature. `_me` is the board object that triggered the walk;
/// board() re-locates the board itself, so we drop it.
fn gen_board(g: &mut GameState, ch: CharId, _me: ObjId, cmd: &str, arg: &str) -> bool {
    crate::boards::board(g, ch, cmd, arg)
}

// ---------------------------------------------------------------------------
// assign_mobiles / assign_objects / assign_rooms (spec_assign.c).
//
// Each builds entries in the in-progress tables. Only ported procs are added;
// the C ASSIGNMOB/ASSIGNOBJ lines for unported procs are reproduced as comments
// so the mapping stays auditable against the C source.
// ---------------------------------------------------------------------------

fn assign_mobiles(mobs: &mut HashMap<MobVnum, SpecFn>) {
    // DeltaMUD spec_assign.c ASSIGNMOB lines. Procs not yet ported are noted;
    // their vnums stay unassigned (== C leaving mob_index[].func NULL), so the
    // spec walk skips the mob and the command falls through to normal dispatch.
    //
    // Limbo.
    ASSIGNMOB(mobs, 1, crate::spec_procs::puff as SpecFn); // ASSIGNMOB(1, puff)
                                                           // Immortal Zone.
    ASSIGNMOB(mobs, 1200, crate::objsave::receptionist as SpecFn); // ASSIGNMOB(1200, receptionist)
    ASSIGNMOB(mobs, 1201, crate::mail::postmaster as SpecFn);
    ASSIGNMOB(mobs, 1202, crate::spec_procs::janitor as SpecFn); // immortal-zone janitor
                                                                 // Battle Arena (Thargor 7/25-28/98). arenaentrancemaster lives in arena.rs
                                                                 // (the full arena subsystem); temple_healer / temple_mana_regenerator in
                                                                 // spec_procs.rs.
    ASSIGNMOB(mobs, 4800, crate::arena::arenaentrancemaster as SpecFn);
    ASSIGNMOB(mobs, 4801, crate::spec_procs::temple_healer as SpecFn);
    ASSIGNMOB(
        mobs,
        4802,
        crate::spec_procs::temple_mana_regenerator as SpecFn,
    );
    // Itrius.
    ASSIGNMOB(mobs, 199, crate::mail::postmaster as SpecFn);
    ASSIGNMOB(mobs, 101, crate::objsave::receptionist as SpecFn); // ASSIGNMOB(101, receptionist)
    ASSIGNMOB(mobs, 102, crate::spec_procs::librarian as SpecFn); // ASSIGNMOB(102, librarian)

    // spec_procs.rs (Batch 11+): guild / guild_guard / cityguard / mayor /
    // janitor / fido / thief + the generic combat specs. The module owns the
    // authoritative vnum→proc map and registers through this closure (mirroring
    // the castle integration just below). When the relevant zones aren't loaded
    // the vnums simply never match a present mob and the entries are inert.
    crate::spec_procs::register_mob_specs(|vnum, func| ASSIGNMOB(mobs, vnum, func));

    // King's Castle (Greenhill) zone procs (C: castle.c::assign_kings_castle,
    // wired in from spec_assign.c). assign_kings_castle() hands back the
    // (absolute mob vnum, spec) pairs for every special mob in the castle —
    // Castle Guards, the King, Peter, the Training Master, James, the cleaning
    // woman, Tim/Tom, Dick/David, and Jerry. Each CastleSpec shares the SpecFn
    // shape exactly, so they register directly. When zone 150 is not loaded the
    // vnums simply never match a present mob and the entries are inert.
    for (vnum, func) in crate::castle::assign_kings_castle() {
        ASSIGNMOB(mobs, vnum, func as SpecFn);
    }

    // Shop keepers (C: shop.c::assign_the_shopkeepers, not spec_assign.c). Each
    // keeper mob gets shop_keeper as its func. shop_keeper() self-validates by
    // matching `me`'s vnum against the loaded shop table and returns false for
    // any non-keeper, so we do not need shop.c's private keeper-vnum list here:
    // special() falls back to shop_keeper for any present mob without a static
    // entry (see special()), which is behaviourally identical to C assigning the
    // func directly. (Nothing to register statically.)
}

fn assign_objects(objs: &mut HashMap<ObjVnum, ObjSpecFn>) {
    ASSIGNOBJ(objs, 20, crate::spec_procs::portal as ObjSpecFn); // ASSIGNOBJ(20, portal) — generic portal
    ASSIGNOBJ(objs, 500, crate::spec_procs::tent as ObjSpecFn); // ASSIGNOBJ(500, tent)

    // Mortal boards.
    ASSIGNOBJ(objs, 199, gen_board as ObjSpecFn); // general board
    ASSIGNOBJ(objs, 198, gen_board as ObjSpecFn); // social board
    ASSIGNOBJ(objs, 197, gen_board as ObjSpecFn); // auction board

    // Immortal boards.
    ASSIGNOBJ(objs, 1200, gen_board as ObjSpecFn); // implementor board
    ASSIGNOBJ(objs, 1201, gen_board as ObjSpecFn); // general board
    ASSIGNOBJ(objs, 1202, gen_board as ObjSpecFn); // ideas board
    ASSIGNOBJ(objs, 1203, gen_board as ObjSpecFn); // quest board
    ASSIGNOBJ(objs, 1204, gen_board as ObjSpecFn); // relations board
    ASSIGNOBJ(objs, 1205, gen_board as ObjSpecFn); // frozen board
    ASSIGNOBJ(objs, 1206, gen_board as ObjSpecFn); // typos board
    ASSIGNOBJ(objs, 1207, gen_board as ObjSpecFn); // bugs board
    ASSIGNOBJ(objs, 1208, gen_board as ObjSpecFn); // reimbursement board
    ASSIGNOBJ(objs, 1209, gen_board as ObjSpecFn); // builders board
                                                   // SPECIAL(bank) is declared in DeltaMUD's spec_assign.c (a forward prototype
                                                   // so OLC can reference it) but it is never DEFINED in any .c file and never
                                                   // ASSIGNOBJ'd to a vnum — it is a dangling no-op in the C source. There is
                                                   // nothing to port and nothing to assign; banking is handled by the BANKER
                                                   // mob flag path (act.other.c), not an object spec. Left intentionally absent.
}

fn assign_rooms(rooms: &mut HashMap<RoomVnum, RoomSpecFn>) {
    // SPECIAL(dump); SPECIAL(pet_shops); SPECIAL(pray_for_items);
    //
    // C assign_rooms() does two things: (1) the dts_are_dumps loop sets
    // world[i].func = dump on every ROOM_DEATH room, and (2) the stock
    // pet-shop entrance (vnum 3031) gets pet_shops. (pray_for_items is declared
    // but never assigned in DeltaMUD's spec_assign.c.)
    //
    // spec_procs.rs owns the dump / pet_shops procs and registers them through
    // this closure. The ROOM_DEATH set is world-dependent, so it is supplied by
    // assign_specs's caller (main.rs) via DEATH_TRAP_ROOMS, populated right
    // after the world loads; pet_shops is static and always registered.
    let death_rooms = death_trap_rooms();
    crate::spec_procs::register_room_specs(
        |vnum, func| {
            rooms.insert(vnum, func);
        },
        &death_rooms,
    );
}

// ---------------------------------------------------------------------------
// ASSIGN* helpers (spec_assign.c ASSIGNMOB / ASSIGNOBJ / ASSIGNROOM).
//
// The C versions look up real_mobile/real_object/real_room and log a SYSERR if
// the vnum does not exist (unless mini_mud). We register by *vnum* (resolved at
// dispatch time against live entities), so a non-existent vnum is harmless — it
// simply never matches a present entity, exactly as a missing prototype would.
// We therefore skip the existence check (it required the full index at boot)
// and just insert; the C log line was advisory only.
// ---------------------------------------------------------------------------

#[allow(non_snake_case)]
fn ASSIGNMOB(mobs: &mut HashMap<MobVnum, SpecFn>, vnum: MobVnum, func: SpecFn) {
    mobs.insert(vnum, func);
}

#[allow(non_snake_case)]
fn ASSIGNOBJ(objs: &mut HashMap<ObjVnum, ObjSpecFn>, vnum: ObjVnum, func: ObjSpecFn) {
    objs.insert(vnum, func);
}

// ---------------------------------------------------------------------------
// Boot entry point.
// ---------------------------------------------------------------------------

/// Build the special-procedure tables. Call once at boot (after the world,
/// shops, mail and boards have loaded). Idempotent: the OnceLock guarantees the
/// tables are built exactly once even if called twice.
pub fn assign_specs() {
    SPECS.get_or_init(|| {
        let mut mobs = HashMap::new();
        let mut objs = HashMap::new();
        let mut rooms = HashMap::new();
        assign_mobiles(&mut mobs);
        assign_objects(&mut objs);
        assign_rooms(&mut rooms);
        SpecTables { mobs, objs, rooms }
    });
}

/// Lazy accessor: build-on-first-use so callers (and `special()`) work even if
/// `assign_specs()` was never explicitly called at boot.
fn tables() -> &'static SpecTables {
    SPECS.get_or_init(|| {
        let mut mobs = HashMap::new();
        let mut objs = HashMap::new();
        let mut rooms = HashMap::new();
        assign_mobiles(&mut mobs);
        assign_objects(&mut objs);
        assign_rooms(&mut rooms);
        SpecTables { mobs, objs, rooms }
    })
}

/// Look up the special procedure for a mobile prototype vnum (C
/// GET_MOB_SPEC). Returns None if the mob has no statically-assigned spec.
/// Note: shop keepers are NOT in this table (they resolve dynamically via
/// shop_keeper in `special()`), so a None here does not mean "no behaviour".
pub fn get_mob_spec(vnum: MobVnum) -> Option<SpecFn> {
    tables().mobs.get(&vnum).copied()
}

/// Look up the special procedure for an object prototype vnum (C GET_OBJ_SPEC).
pub fn get_obj_spec(vnum: ObjVnum) -> Option<ObjSpecFn> {
    tables().objs.get(&vnum).copied()
}

/// Look up the special procedure for a room vnum (C GET_ROOM_SPEC).
pub fn get_room_spec(vnum: RoomVnum) -> Option<RoomSpecFn> {
    tables().rooms.get(&vnum).copied()
}

// ---------------------------------------------------------------------------
// special() — the central dispatcher (interpreter.c::special).
// ---------------------------------------------------------------------------

/// Run every special procedure reachable from `ch`'s current room for the typed
/// command, in CircleMUD's fixed order, stopping at the first that consumes it.
///
/// Walk order (must match C exactly — order is behaviourally load-bearing):
///   1. the room's spec proc
///   2. each worn equipment object's spec proc (slots 0..NUM_WEARS)
///   3. each carried inventory object's spec proc
///   4. each mob present in the room (newest-first, char_list order)
///   5. each object lying in the room
///
/// `cmd` is the command word the actor typed (lowercased), `arg` the remainder.
/// Returns true iff some proc consumed the command, in which case
/// command_interpreter MUST skip normal dispatch.
///
/// Borrow discipline: every phase snapshots the relevant id list (room people /
/// contents, inventory, equipment) into an owned Vec before iterating, because
/// each spec call takes `&mut GameState` and may move/extract entities. Each id
/// is re-validated (its prototype vnum re-read) right before its spec runs, so a
/// proc that extracts a later entity in the list cannot cause a stale-id panic;
/// a vanished entity is simply skipped (its `get_*` returns None), mirroring C's
/// reliance on the linked-list `next` pointers staying valid for that pulse.
pub fn special(g: &mut GameState, ch: CharId, cmd: &str, arg: &str) -> bool {
    if g.config.no_specials {
        return false;
    }

    // The actor's room. If the actor is nowhere (just extracted / between
    // rooms) there is nothing to dispatch against — C dereferences
    // ch->in_room unconditionally, but our arena guards it.
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };

    // ---- 1. room spec ---------------------------------------------------
    if let Some(room_vnum) = g.room_opt(rnum).map(|r| r.number) {
        if let Some(func) = get_room_spec(room_vnum) {
            if func(g, ch, rnum, cmd, arg) {
                return true;
            }
            // A room spec could in principle move the actor; re-validate the
            // room before continuing the walk (C re-reads ch->in_room each
            // phase implicitly via the macros).
            let _ = rnum;
        }
    }

    // ---- 2. equipment ---------------------------------------------------
    let equipment: Vec<ObjId> = match g.get_char(ch) {
        Some(c) => c.equipment.iter().flatten().copied().collect(),
        None => return false,
    };
    for oid in equipment {
        let vnum = match g.get_obj(oid) {
            Some(o) => o.item_number,
            None => continue,
        };
        if vnum == NOTHING {
            continue;
        }
        if let Some(func) = get_obj_spec(vnum) {
            if func(g, ch, oid, cmd, arg) {
                return true;
            }
        }
    }

    // ---- 3. inventory ---------------------------------------------------
    let carrying: Vec<ObjId> = match g.get_char(ch) {
        Some(c) => c.carrying.clone(),
        None => return false,
    };
    for oid in carrying {
        let vnum = match g.get_obj(oid) {
            Some(o) => o.item_number,
            None => continue,
        };
        if vnum == NOTHING {
            continue;
        }
        if let Some(func) = get_obj_spec(vnum) {
            if func(g, ch, oid, cmd, arg) {
                return true;
            }
        }
    }

    // ---- 4. mobs present in the room ------------------------------------
    // Re-read the room each iteration is unnecessary; snapshot the people list.
    let people: Vec<CharId> = match g.room_opt(rnum) {
        Some(r) => r.people.clone(),
        None => return false,
    };
    for k in people {
        // Re-validate: the mob may have been extracted by an earlier proc.
        let (is_npc, vnum) = match g.get_char(k) {
            Some(c) => (c.is_npc, c.nr),
            None => continue,
        };
        if !is_npc {
            // Players never carry a mob spec; skip the lookup (C's
            // mob_index access is only valid for NPCs).
            continue;
        }
        // Statically-assigned mob spec first (postmaster, etc.).
        if let Some(func) = get_mob_spec(vnum) {
            if func(g, ch, k, cmd, arg) {
                return true;
            }
            continue;
        }
        // Otherwise try the shop keeper proc. It self-validates by matching
        // `k`'s vnum against the loaded shop table and returns false instantly
        // for any non-keeper mob, so this faithfully reproduces C's
        // assign_the_shopkeepers() (which sets keeper mobs' func to
        // shop_keeper) without needing shop.c's private keeper-vnum list.
        if crate::shop::shop_keeper(g, ch, k, cmd, arg) {
            return true;
        }
    }

    // ---- 5. objects lying in the room -----------------------------------
    let contents: Vec<ObjId> = match g.room_opt(rnum) {
        Some(r) => r.contents.clone(),
        None => return false,
    };
    for oid in contents {
        let vnum = match g.get_obj(oid) {
            Some(o) => o.item_number,
            None => continue,
        };
        if vnum == NOTHING {
            continue;
        }
        if let Some(func) = get_obj_spec(vnum) {
            if func(g, ch, oid, cmd, arg) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::room::Room;

    fn pet_shop_world(no_specials: bool) -> (GameState, CharId) {
        let mut cfg = Config::default();
        cfg.no_specials = no_specials;
        let mut g = GameState::new(cfg);
        let shop = g.add_room(Room::new(
            3031,
            0,
            "Pet Shop".to_string(),
            "A pet shop.".to_string(),
        ));
        g.add_room(Room::new(
            3032,
            0,
            "Pet Room".to_string(),
            "Pets wait here.".to_string(),
        ));
        let ch = g.create_char(Character::new_player(
            "Buyer".to_string(),
            Class::Warrior,
            Race::Human,
        ));
        g.char_to_room(ch, shop);
        (g, ch)
    }

    #[test]
    fn special_honors_no_specials_even_with_lazy_tables() {
        let (mut enabled, enabled_ch) = pet_shop_world(false);
        assert!(special(&mut enabled, enabled_ch, "list", ""));

        let (mut disabled, disabled_ch) = pet_shop_world(true);
        assert!(!special(&mut disabled, disabled_ch, "list", ""));
    }
}
