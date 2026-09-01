// dg_handler.rs — runtime trigger data model + attach/detach (port of
// dg_handler.c, plus the script-data scaffolding that C hangs off
// char/obj/room->script and ->memory).
//
// This module is the storage + constant-definition layer for the whole DG
// core; it exposes the full canonical API (every trigger-type bit, every
// accessor) that the VM (dg_scripts), the fire hooks (dg_triggers), the DG
// commands (dg_mobcmd/objcmd/wldcmd) and the world loader consume. Some of that
// surface is touched by only a subset of those consumers in this batch, so the
// module tolerates dead code rather than hiding part of the contract.
#![allow(dead_code)]
//
// In C every scriptable entity carries `struct script_data *script` and mobs
// also carry `struct script_memory *memory`. Rust's Character/Object/Room may
// not gain new fields (project rule), so all of that lives here in module-
// static tables keyed by the entity id (the shop.rs/quest.rs/arena.rs pattern):
//
//   SCRIPTS   : per-entity ScriptData (the attached trig_list + global vars +
//               script-type bitvector + current context). Keyed by ScriptKey.
//   TRIGS     : the live trig_data arena. Each running/attached trigger is a
//               TrigData behind a TrigId (the C `trig_data *`). Triggers are
//               kept here, not inlined into ScriptData, so a paused trigger
//               (wait event) and the var_subst code can reach it by id exactly
//               as C dereferences the pointer.
//   MEMORY    : per-mob script_memory list (greet_memory / entry_memory).
//
// This module owns the data + add_trigger/remove_trigger/extract_trigger and
// the ScriptKey enum the whole DG core keys on. The .trg *prototypes* live in
// dg_db_scripts.rs; the VM lives in dg_scripts.rs.

use crate::dg_event::{self, EventId};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

// Attach-type / data-type discriminants (dg_scripts.h).
pub const MOB_TRIGGER: i32 = 0;
pub const OBJ_TRIGGER: i32 = 1;
pub const WLD_TRIGGER: i32 = 2;

// Script add positions (add_trigger loc arg): -1 = end, 0 = front.

// ---------------------------------------------------------------------------
// Trigger-type bitvectors (dg_scripts.h). Mob/Obj/Wld each reuse the same bit
// positions for their own meanings; the .trg flag letter 'a'..'o' maps to bits
// (asciiflag_conv) — see dg_db_scripts. These are the canonical bit definitions
// (the conformance surface) referenced by the sibling dg_triggers fire hooks;
// the module-level allow keeps the full spec set even where one consumer
// touches only a subset.
// ---------------------------------------------------------------------------
pub const MTRIG_GLOBAL: i64 = 1 << 0;
pub const MTRIG_RANDOM: i64 = 1 << 1;
pub const MTRIG_COMMAND: i64 = 1 << 2;
pub const MTRIG_SPEECH: i64 = 1 << 3;
pub const MTRIG_ACT: i64 = 1 << 4;
pub const MTRIG_DEATH: i64 = 1 << 5;
pub const MTRIG_GREET: i64 = 1 << 6;
pub const MTRIG_GREET_ALL: i64 = 1 << 7;
pub const MTRIG_ENTRY: i64 = 1 << 8;
pub const MTRIG_RECEIVE: i64 = 1 << 9;
pub const MTRIG_FIGHT: i64 = 1 << 10;
pub const MTRIG_HITPRCNT: i64 = 1 << 11;
pub const MTRIG_BRIBE: i64 = 1 << 12;
pub const MTRIG_LOAD: i64 = 1 << 13;
pub const MTRIG_MEMORY: i64 = 1 << 14;

pub const OTRIG_RANDOM: i64 = 1 << 1;
pub const OTRIG_COMMAND: i64 = 1 << 2;
pub const OTRIG_FIGHT: i64 = 1 << 3;
pub const OTRIG_TIMER: i64 = 1 << 5;
pub const OTRIG_GET: i64 = 1 << 6;
pub const OTRIG_DROP: i64 = 1 << 7;
pub const OTRIG_GIVE: i64 = 1 << 8;
pub const OTRIG_WEAR: i64 = 1 << 9;
pub const OTRIG_REMOVE: i64 = 1 << 11;
pub const OTRIG_LOAD: i64 = 1 << 13;

pub const WTRIG_GLOBAL: i64 = 1 << 0;
pub const WTRIG_RANDOM: i64 = 1 << 1;
pub const WTRIG_COMMAND: i64 = 1 << 2;
pub const WTRIG_SPEECH: i64 = 1 << 3;
pub const WTRIG_RESET: i64 = 1 << 5;
pub const WTRIG_ENTER: i64 = 1 << 6;
pub const WTRIG_DROP: i64 = 1 << 7;

// obj command-trigger location bits (OCMD_*).
pub const OCMD_EQUIP: i32 = 1 << 0;
pub const OCMD_INVEN: i32 = 1 << 1;
pub const OCMD_ROOM: i32 = 1 << 2;

pub const TRIG_NEW: i32 = 0;
pub const TRIG_RESTART: i32 = 1;

pub const MAX_SCRIPT_DEPTH: i32 = 10;

// player/room id bases for UID arithmetic (dg_scripts.h).
pub const ROOM_ID_BASE: i64 = 50000;
pub const MOBOBJ_ID_BASE: i64 = 200000;

// ---------------------------------------------------------------------------
// ScriptKey — identifies a scriptable entity. C dispatches on `type` + a
// void*; we key the static tables on this enum. RoomRnum is the dense index
// (matching world[rnum]); UID arithmetic uses the room vnum/rnum mapping in
// dg_scripts::id_of.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptKey {
    Mob(CharId),
    Obj(ObjId),
    Room(RoomRnum),
}

impl ScriptKey {
    pub fn trig_type(&self) -> i32 {
        match self {
            ScriptKey::Mob(_) => MOB_TRIGGER,
            ScriptKey::Obj(_) => OBJ_TRIGGER,
            ScriptKey::Room(_) => WLD_TRIGGER,
        }
    }
}

/// A trigger variable (trig_var_data): name/value/context.
#[derive(Debug, Clone)]
pub struct TrigVar {
    pub name: String,
    pub value: String,
    pub context: i64,
}

/// Live trigger instance (trig_data). Created by reading a prototype.
#[derive(Debug, Clone)]
pub struct TrigData {
    pub nr: usize,        // trig_index rnum
    pub vnum: i32,        // trigger vnum (GET_TRIG_VNUM)
    pub attach_type: i32, // MOB/OBJ/WLD intent
    pub name: String,
    pub trigger_type: i64, // bitvector
    pub narg: i32,
    pub arglist: String,
    pub cmdlist: Vec<String>, // one entry per script line (cmdlist_element)
    pub curr_line: usize,     // index into cmdlist (curr_state)
    pub depth: i32,           // if/while nesting depth (0 = not running)
    pub loops: i32,
    pub wait_event: Option<EventId>,
    pub var_list: Vec<TrigVar>,
    pub purged: bool,
    // while-loop bookkeeping: maps a `done` line index -> its `while` line.
    pub loop_origin: HashMap<usize, usize>,
}

impl TrigData {
    pub fn get_var(&self, name: &str) -> Option<&TrigVar> {
        self.var_list
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(name))
    }
}

/// script_data: the per-entity script container.
#[derive(Debug, Default, Clone)]
pub struct ScriptData {
    pub types: i64,             // SCRIPT_TYPES bitvector (union of trig types)
    pub trig_list: Vec<TrigId>, // attached triggers, in order
    pub global_vars: Vec<TrigVar>,
    pub context: i64,
}

/// script_memory entry (remembered actor + optional command).
#[derive(Debug, Clone)]
pub struct ScriptMemory {
    pub id: i64, // GET_ID of who to remember
    pub cmd: Option<String>,
}

// ---------------------------------------------------------------------------
// Module-static arenas.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrigId(pub u64);

static SCRIPTS: OnceLock<Mutex<HashMap<ScriptKey, ScriptData>>> = OnceLock::new();
static TRIGS: OnceLock<Mutex<HashMap<TrigId, TrigData>>> = OnceLock::new();
static MEMORY: OnceLock<Mutex<HashMap<CharId, Vec<ScriptMemory>>>> = OnceLock::new();
static NEXT_TRIG_ID: AtomicU64 = AtomicU64::new(1);

fn scripts() -> &'static Mutex<HashMap<ScriptKey, ScriptData>> {
    SCRIPTS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn trigs() -> &'static Mutex<HashMap<TrigId, TrigData>> {
    TRIGS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn memory() -> &'static Mutex<HashMap<CharId, Vec<ScriptMemory>>> {
    MEMORY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Wipe all runtime script state (boot / copyover). Prototypes (dg_db_scripts)
/// are reloaded separately.
pub fn boot_handler() {
    crate::lock_ok::lock(&scripts()).clear();
    crate::lock_ok::lock(&trigs()).clear();
    crate::lock_ok::lock(&memory()).clear();
    NEXT_TRIG_ID.store(1, Ordering::Relaxed);
}

// ---- ScriptData access ----------------------------------------------------

/// SCRIPT_CHECK(go, type): entity has a script whose type-bitvector includes
/// `type`. Cheap fast-path the fire hooks call before doing any work.
pub fn script_check(key: ScriptKey, ty: i64) -> bool {
    scripts()
        .lock()
        .unwrap()
        .get(&key)
        .map(|sc| sc.types & ty != 0)
        .unwrap_or(false)
}

pub fn has_script(key: ScriptKey) -> bool {
    crate::lock_ok::lock(&scripts()).contains_key(&key)
}

pub fn script_types(key: ScriptKey) -> i64 {
    scripts()
        .lock()
        .unwrap()
        .get(&key)
        .map(|s| s.types)
        .unwrap_or(0)
}

/// Snapshot the attached trigger ids for an entity (TRIGGERS(sc) walk).
pub fn trigger_ids(key: ScriptKey) -> Vec<TrigId> {
    scripts()
        .lock()
        .unwrap()
        .get(&key)
        .map(|s| s.trig_list.clone())
        .unwrap_or_default()
}

pub fn get_context(key: ScriptKey) -> i64 {
    scripts()
        .lock()
        .unwrap()
        .get(&key)
        .map(|s| s.context)
        .unwrap_or(0)
}
pub fn set_context(key: ScriptKey, ctx: i64) {
    if let Some(sc) = crate::lock_ok::lock(&scripts()).get_mut(&key) {
        sc.context = ctx;
    }
}

/// Read a global variable honouring context (find_replacement global-var path):
/// matches name and (context==0 || context==sc.context).
pub fn get_global_var(key: ScriptKey, name: &str) -> Option<String> {
    let map = crate::lock_ok::lock(&scripts());
    let sc = map.get(&key)?;
    sc.global_vars
        .iter()
        .find(|v| v.name.eq_ignore_ascii_case(name) && (v.context == 0 || v.context == sc.context))
        .map(|v| v.value.clone())
}

/// Snapshot a script's global variable list as (name, value, context) tuples,
/// in storage order (script_stat enumerates `sc->global_vars` in order).
pub fn global_vars(key: ScriptKey) -> Vec<(String, String, i64)> {
    scripts()
        .lock()
        .unwrap()
        .get(&key)
        .map(|sc| {
            sc.global_vars
                .iter()
                .map(|v| (v.name.clone(), v.value.clone(), v.context))
                .collect()
        })
        .unwrap_or_default()
}

/// add_var into a script's global list (used by remote/global, and load).
pub fn add_global_var(key: ScriptKey, name: &str, value: &str, context: i64) {
    let mut map = crate::lock_ok::lock(&scripts());
    let sc = map.entry(key).or_default();
    add_var_in(&mut sc.global_vars, name, value, context);
}

/// remove_var from a script's globals; returns true if found.
pub fn remove_global_var(key: ScriptKey, name: &str) -> bool {
    if let Some(sc) = crate::lock_ok::lock(&scripts()).get_mut(&key) {
        let before = sc.global_vars.len();
        sc.global_vars
            .retain(|v| !v.name.eq_ignore_ascii_case(name));
        return sc.global_vars.len() != before;
    }
    false
}

/// Shared add_var: overwrite existing same-name var, else prepend (C add_var
/// keeps newest first and does NOT compare context on overwrite).
pub fn add_var_in(list: &mut Vec<TrigVar>, name: &str, value: &str, context: i64) {
    if let Some(v) = list.iter_mut().find(|v| v.name.eq_ignore_ascii_case(name)) {
        v.value = value.to_string();
        return;
    }
    list.insert(
        0,
        TrigVar {
            name: name.to_string(),
            value: value.to_string(),
            context,
        },
    );
}

pub fn remove_var_in(list: &mut Vec<TrigVar>, name: &str) -> bool {
    let before = list.len();
    list.retain(|v| !v.name.eq_ignore_ascii_case(name));
    list.len() != before
}

// ---- Trigger arena access -------------------------------------------------

/// Insert a freshly-read TrigData into the arena, returning its id.
pub fn install_trig(mut t: TrigData) -> TrigId {
    let id = TrigId(NEXT_TRIG_ID.fetch_add(1, Ordering::Relaxed));
    t.purged = false;
    crate::lock_ok::lock(&trigs()).insert(id, t);
    id
}

pub fn with_trig<R>(id: TrigId, f: impl FnOnce(&TrigData) -> R) -> Option<R> {
    crate::lock_ok::lock(&trigs()).get(&id).map(f)
}

pub fn with_trig_mut<R>(id: TrigId, f: impl FnOnce(&mut TrigData) -> R) -> Option<R> {
    crate::lock_ok::lock(&trigs()).get_mut(&id).map(f)
}

pub fn trig_clone(id: TrigId) -> Option<TrigData> {
    crate::lock_ok::lock(&trigs()).get(&id).cloned()
}

// ---- add_trigger / remove_trigger / extract (dg_scripts.c / dg_handler.c) --

/// add_trigger(sc, t, loc): attach trigger id `t` to entity `key`. loc < 0 =>
/// append; loc == 0 => prepend; loc == n => after the (n)th. Folds the
/// trigger's type bits into SCRIPT_TYPES.
pub fn add_trigger(key: ScriptKey, t: TrigId, loc: i32) {
    let ttype = with_trig(t, |tr| tr.trigger_type).unwrap_or(0);
    let mut map = crate::lock_ok::lock(&scripts());
    let sc = map.entry(key).or_default();

    if loc == 0 {
        sc.trig_list.insert(0, t);
    } else if loc < 0 || (loc as usize) >= sc.trig_list.len() {
        sc.trig_list.push(t);
    } else {
        sc.trig_list.insert(loc as usize, t);
    }
    sc.types |= ttype;
}

/// extract_trigger(trig): cancel any wait event, drop from the arena. Caller
/// has already unlinked it from the owning ScriptData (remove_trigger does).
pub fn extract_trigger(g: &mut GameState, id: TrigId) {
    let ev = with_trig(id, |t| t.wait_event).flatten();
    dg_event::cancel_for_trigger(g, ev);
    crate::lock_ok::lock(&trigs()).remove(&id);
}

/// extract_script(sc): remove every trigger on `key`, then drop the script
/// container. (C frees the script struct; we just remove the table entry.)
pub fn extract_script(g: &mut GameState, key: ScriptKey) {
    let ids = trigger_ids(key);
    for id in ids {
        extract_trigger(g, id);
    }
    crate::lock_ok::lock(&scripts()).remove(&key);
}

/// remove_trigger(sc, name): name may be "N.keyword", a bare number, or a
/// keyword; mirrors C exactly. Returns true if a trigger was removed. Also
/// recomputes SCRIPT_TYPES and drops the script container if empty.
pub fn remove_trigger(g: &mut GameState, key: ScriptKey, name: &str) -> bool {
    // Parse the C "num . name" form.
    let (mut num, search_name, by_string): (i32, String, bool) = if let Some(dot) = name.find('.') {
        let (n, rest) = name.split_at(dot);
        let rest = &rest[1..];
        let num = match crate::text::parse_i32_atoi(n) {
            Ok(num) => num,
            Err(error) => {
                log::warn!("DG remove_trigger rejected invalid ordinal {n:?}: {error:?}");
                return false;
            }
        };
        (num, rest.to_string(), true)
    } else if name
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        let num = match crate::text::parse_i32_atoi(name) {
            Ok(num) => num,
            Err(error) => {
                log::warn!("DG remove_trigger rejected invalid ordinal {name:?}: {error:?}");
                return false;
            }
        };
        (num, String::new(), false)
    } else {
        (0, name.to_string(), true)
    };

    let ids = trigger_ids(key);
    let mut found: Option<usize> = None;
    let mut n = 0;
    for (idx, &tid) in ids.iter().enumerate() {
        let tname = with_trig(tid, |t| t.name.clone()).unwrap_or_default();
        if by_string {
            if crate::handler::isname(&search_name, &tname) {
                n += 1;
                if n >= num.max(1) {
                    found = Some(idx);
                    break;
                }
            }
        } else {
            n += 1;
            if n >= num.max(1) {
                found = Some(idx);
                break;
            }
        }
        let _ = &mut num;
    }

    let Some(idx) = found else { return false };
    let tid = ids[idx];

    {
        let mut map = crate::lock_ok::lock(&scripts());
        if let Some(sc) = map.get_mut(&key) {
            sc.trig_list.retain(|&t| t != tid);
        }
    }
    extract_trigger(g, tid);

    // Recompute SCRIPT_TYPES; drop empty script.
    let remaining = trigger_ids(key);
    if remaining.is_empty() {
        crate::lock_ok::lock(&scripts()).remove(&key);
    } else {
        let mut types = 0i64;
        for t in &remaining {
            types |= with_trig(*t, |x| x.trigger_type).unwrap_or(0);
        }
        if let Some(sc) = crate::lock_ok::lock(&scripts()).get_mut(&key) {
            sc.types = types;
        }
    }
    true
}

// ---- script memory (mob greet/entry memory triggers) ----------------------

pub fn remember(ch: CharId, id: i64, cmd: Option<String>) {
    let mut map = crate::lock_ok::lock(&memory());
    let list = map.entry(ch).or_default();
    // C remember() prepends a new node unconditionally.
    list.insert(0, ScriptMemory { id, cmd });
}

pub fn forget(ch: CharId, id: i64) {
    if let Some(list) = crate::lock_ok::lock(&memory()).get_mut(&ch) {
        list.retain(|m| m.id != id);
    }
}

pub fn memory_for(ch: CharId) -> Vec<ScriptMemory> {
    memory()
        .lock()
        .unwrap()
        .get(&ch)
        .cloned()
        .unwrap_or_default()
}

pub fn extract_script_mem(ch: CharId) {
    crate::lock_ok::lock(&memory()).remove(&ch);
}

/// Called when a char/obj is extracted from the world so its attached script
/// state does not leak (mirrors free_char/free_obj clearing ->script).
pub fn on_char_extracted(g: &mut GameState, ch: CharId) {
    extract_script(g, ScriptKey::Mob(ch));
    extract_script_mem(ch);
    // The mob memory used by the MEMORY trigger lives in dg_mobcmd's table
    // (mremember/mforget write there); clear it too so a recycled CharId can't
    // inherit stale remembered victims.
    crate::dg_mobcmd::script_mem_clear(g, ch);
}
pub fn on_obj_extracted(g: &mut GameState, obj: ObjId) {
    extract_script(g, ScriptKey::Obj(obj));
}

// Test-only: a process-wide lock serialising access to the DG module-static
// tables across parallel test threads (production is single-threaded). Every
// DG test acquires this guard for its duration so one test's boot_handler()
// can't clear another test's triggers mid-run.
#[cfg(test)]
pub static DG_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(name: &str) -> TrigData {
        TrigData {
            nr: 0,
            vnum: 9000,
            attach_type: WLD_TRIGGER,
            name: name.to_string(),
            trigger_type: WTRIG_COMMAND,
            narg: 0,
            arglist: String::new(),
            cmdlist: Vec::new(),
            curr_line: 0,
            depth: 0,
            loops: 0,
            wait_event: None,
            var_list: Vec::new(),
            purged: false,
            loop_origin: HashMap::new(),
        }
    }

    #[test]
    fn overflowing_detach_ordinal_cannot_remove_the_first_trigger() {
        let mut g = crate::state::GameState::new(crate::config::Config::default());
        boot_handler();
        let key = ScriptKey::Room(987_654);
        let first = install_trig(trigger("first"));
        let second = install_trig(trigger("second"));
        add_trigger(key, first, -1);
        add_trigger(key, second, -1);

        assert!(!remove_trigger(&mut g, key, "2147483648"));
        assert!(!remove_trigger(&mut g, key, "2147483648.first"));
        assert_eq!(trigger_ids(key), vec![first, second]);

        extract_script(&mut g, key);
    }
}
