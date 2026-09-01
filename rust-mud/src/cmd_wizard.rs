// cmd_wizard.rs — full port of C `src/act.wizard.c` (the immortal command set).
// Every ACMD in that file is ported here against the single-owner GameState
// contract (state.rs / handler.rs / act.rs).
//
// House style (see cmd_informative.rs / cmd_item.rs): read needed scalars into
// locals first, then mutate / send; re-look-up entities by id; never hold a
// &Character/&Object across a mutation; use act() for broadcasts. Color is
// emitted as literal `&`-codes; the output path strips them per-player.
//
// Contract-gap policy (documented in the manifest, never stubbed): a few
// global side-effects in the C source touch systems reachable only outside this
// sync command path — chiefly the async player DB (database.rs) used by the
// OFFLINE variants of `set` / `set file` / `stat player` / `stat file` /
// `show player`, and the on-disk player-file password buffer.
//
// OFFLINE immortal commands (set / stat / show on a logged-off player's full
// record) are handled by the ASYNC BRIDGE: C loads the record synchronously
// (retrieve_player_entry), edits, and saves; the Rust DB is async, so when the
// target is offline-but-indexed we don't degrade to "no such player". Instead
// try_defer_offline() emits "[ Loading <name> from the player file... ]" and
// queues the verbatim immortal command via GameState::queue_offline_op. The
// async Game loop (game.rs::drain_offline_ops) then loads the player into the
// world, REPLAYS the command through command_interpreter (so this exact handler
// logic re-runs against the now-in-world char and the immortal sees the normal
// output / change), persists the edited record, and extracts it — matching C's
// load-edit-save. `set file` / `stat file` replay as the `player` form so the
// replayed pass takes the online lookup branch instead of re-deferring. `last`
// already renders an offline player straight from the boot-loaded player_table
// index, so it stays synchronous (no deferral needed).

use crate::act::{ActArg, To, act};
use crate::connection::ConState;
use crate::constants;
use crate::dg_handler::{self, OBJ_TRIGGER, ScriptKey, WLD_TRIGGER};
use crate::flags::*;
use crate::gcmd::*;
use crate::interpreter::{command_interpreter, half_chop, is_abbrev, one_argument, search_block};
use crate::limits::exp_to_level;
use crate::object::{ObjLoc, ObjectType};
use crate::state::{
    GameState, OfflineOpAuthority, PLAYER_INSPECTION_DENIED, ProcessDisposition, ShutdownMode,
    ShutdownRequest,
};
use crate::syslog::{BRF, CMP, NRM, PFT};
use crate::types::*;
use crate::world::zone_vnum_bounds;

// ---------------------------------------------------------------------------
// Level constants (structs.h) — the contract gives LVL_IMMORT/GOD/GRGOD/IMPL;
// DeltaMUD also has LVL_HERO/LVL_DEMIGOD and LVL_BUILDER aliases.
// ---------------------------------------------------------------------------
const LVL_HERO: u8 = 100;
const LVL_DEMIGOD: u8 = 102;
const LVL_BUILDER: u8 = LVL_IMMORT;
const LVL_FREEZE: u8 = LVL_GRGOD;
const MAX_STAT: i8 = 25;
const MAX_PLAYER_STAT: i8 = 18;

// ---------------------------------------------------------------------------
// SCMD_* subcommands used by this file (interpreter.h).
// ---------------------------------------------------------------------------
#[allow(dead_code)]
const SCMD_ECHO: i32 = 0;
const SCMD_EMOTE: i32 = 1;
const SCMD_POOFIN: i32 = 0;
const SCMD_POOFOUT: i32 = 1;
const SCMD_SHUTDOWN: i32 = 1;
const SCMD_DATE: i32 = 0;

// do_wizutil subcmds.
const SCMD_REROLL: i32 = 0;
const SCMD_PARDON: i32 = 1;
const SCMD_NOTITLE: i32 = 2;
const SCMD_SQUELCH: i32 = 3;
const SCMD_FREEZE: i32 = 4;
const SCMD_THAW: i32 = 5;
const SCMD_UNAFFECT: i32 = 6;

// ---------------------------------------------------------------------------
// Flag bits referenced here but not in flags.rs (structs.h values).
// ---------------------------------------------------------------------------
const PRF_NOREPEAT: i64 = 1 << 11;
const PRF_NOWIZ: i64 = 1 << 15;
const PRF_ROOMFLAGS: i64 = 1 << 21;
const PRF_SUMMONABLE: i64 = 1 << 10;
const PRF_AFK: i64 = 1 << 22;
const PRF_COLOR_1: i64 = 1 << 13;
const PRF_COLOR_2: i64 = 1 << 14;
const PRF_LOG1: i64 = 1 << 16;
const PRF_LOG2: i64 = 1 << 17;
const PRF_LOG3: i64 = 1 << 29;

const PRF2_QCHAN: i64 = 1 << 0;
const PRF2_LOCKOUT: i64 = 1 << 1;
const PRF2_INTANGIBLE: i64 = 1 << 9;

const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;
const PLR_FROZEN: i64 = 1 << 2;
const PLR_WRITING: i64 = 1 << 4;
const PLR_MAILING: i64 = 1 << 5;
const PLR_SITEOK: i64 = 1 << 7;
const PLR_NOSHOUT: i64 = 1 << 8;
const PLR_NOTITLE: i64 = 1 << 9;
const PLR_DELETED: i64 = 1 << 10;
const PLR_NOWIZLIST: i64 = 1 << 12;
const PLR_NODELETE: i64 = 1 << 13;
const PLR_INVSTART: i64 = 1 << 14;
const PLR_QUESTOR: i64 = 1 << 16;
const PLR_MULTIOK: i64 = 1 << 17;
const PLR_MBUILDER: i64 = 1 << 18;

// AFF flags referenced here but not in flags.rs.
const AFF_PLAGUED: i64 = 1 << 23;

// Room flags referenced here but not in room.rs's bitflags struct.
const ROOM_GODROOM_BIT: u32 = 1 << 10;
const ROOM_HOUSE_BIT: u32 = 1 << 11;
const ROOM_PRIVATE_BIT: u32 = 1 << 9;
const ROOM_DEATH_BIT: u32 = 1 << 1;
const ROOM_IMPROOM_BIT: u32 = 1 << 16;

// Object extra flag bits.
const ITEM_NORENT: u64 = 1 << 2;

// Common short strings.
const OK: &str = "&YOkay.&n\r\n";
const NOPERSON: &str = "&CNo-one by that name here.&n\r\n";

// config.c: impboard=1200 — the immortal board object vnum protected from
// `load` by non-GRGOD immortals (do_load).
const IMPBOARD: i32 = 1200;

// ---------------------------------------------------------------------------
// Small pure / world helpers (no GameState mutation).
// ---------------------------------------------------------------------------

/// CAP(): capitalise the first character in place.
fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// ONOFF(): C macro -> "ON"/"OFF".
fn onoff(b: bool) -> &'static str {
    if b { "ON" } else { "OFF" }
}

/// YESNO().
fn yesno(b: bool) -> &'static str {
    if b { "YES" } else { "NO" }
}

/// sprinttype(): index a "\n"-terminated name table by ordinal, with the
/// CircleMUD "UNDEFINED" fallback.
fn sprinttype(idx: i32, table: &[&str]) -> String {
    let mut i = 0;
    while i < table.len() && table[i] != "\n" {
        if i as i32 == idx {
            return table[i].to_string();
        }
        i += 1;
    }
    "UNDEFINED".to_string()
}

/// sprintbit() (utils.c:402-423): render a bit-flag long against a name table.
/// C walks the vector right until it is exhausted (no 32-bit cap) while the
/// name index `nr` freezes on the "\n" terminator, so every set bit above the
/// table prints "UNDEFINED ". A negative vector is "<INVALID BITVECTOR>" and
/// nothing set leaves "NOBITS ".
fn sprintbit(bits: i64, table: &[&str]) -> String {
    if bits < 0 {
        return "<INVALID BITVECTOR>".to_string();
    }
    let mut out = String::new();
    let mut nr = 0usize;
    let mut v = bits;
    while v != 0 {
        // C tests *names[nr] != '\n' — the first character, not the whole entry.
        let known = nr < table.len() && !table[nr].starts_with('\n');
        if (v & 1) != 0 {
            if known {
                out.push_str(table[nr]);
                out.push(' ');
            } else {
                out.push_str("UNDEFINED ");
            }
        }
        if known {
            nr += 1;
        }
        v >>= 1;
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

/// The C `CON_*` ordinal of a descriptor state, for
/// `sprinttype(d->connected, connected_types, …)` (act.wizard.c:912-914). The
/// ordinals are the contract — `constants::CONNECTED_TYPES` is laid out in
/// structs.h order (CON_PLAYING 0 … CON_QDEITY 31).
fn conn_state_index(state: ConState) -> i32 {
    match state {
        ConState::Playing => 0,         // CON_PLAYING
        ConState::Close => 1,           // CON_CLOSE
        ConState::GetName => 2,         // CON_GET_NAME
        ConState::ConfirmName => 3,     // CON_NAME_CNFRM
        ConState::GetOldPassword => 4,  // CON_PASSWORD
        ConState::GetNewPassword => 5,  // CON_NEWPASSWD
        ConState::ConfirmPassword => 6, // CON_CNFPASSWD
        ConState::GetSex => 7,          // CON_QSEX
        ConState::GetClass => 8,        // CON_QCLASS
        ConState::ReadMotd => 9,        // CON_RMOTD
        ConState::Menu => 10,           // CON_MENU
        ConState::GetRace => 23,        // CON_QRACE
        ConState::RollStats => 24,      // CON_QROLLSTATS
        ConState::GetHometown => 25,    // CON_QHOMETOWN
        ConState::GetNewbie => 27,      // CON_NEWBIE
        ConState::GetDeity => 31,       // CON_QDEITY
        ConState::ExDesc => 11,         // CON_EXDESC
        ConState::ChPwdGetOld => 12,    // CON_CHPWD_GETOLD
        ConState::ChPwdGetNew => 13,    // CON_CHPWD_GETNEW
        ConState::ChPwdVerify => 14,    // CON_CHPWD_VRFY
        ConState::DelCnf1 => 15,        // CON_DELCNF1
        ConState::DelCnf2 => 16,        // CON_DELCNF2
        ConState::QAnsi => 22,          // CON_QANSI
    }
}

/// IS_GOD() (utils.h:560): `!IS_NPC(ch) && (godcmds1 || godcmds2 || godcmds3
/// || godcmds4)` — a granted-command test, NOT a level test. A level-100
/// mortal handed bits via `set cmdgeneral on` is a god; a level-103 immortal
/// with no bits is not. (shop.rs carries the same corrected helper for its own
/// gates.)
pub fn is_god(g: &GameState, id: CharId) -> bool {
    g.get_char(id)
        .map(|c| {
            !c.is_npc && (c.godcmds1 != 0 || c.godcmds2 != 0 || c.godcmds3 != 0 || c.godcmds4 != 0)
        })
        .unwrap_or(false)
}

/// Numeric `is_number` (CircleMUD): non-empty and all-digit (optionally signed).
fn is_number(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    let bytes = t.as_bytes();
    let start = if bytes[0] == b'-' { 1 } else { 0 };
    start < bytes.len() && bytes[start..].iter().all(|b| b.is_ascii_digit())
}

/// C `atoi` syntax with explicit overflow rejection at command entry points.
fn command_atoi(g: &mut GameState, ch: CharId, s: &str) -> Option<i32> {
    match crate::text::parse_i32_atoi(s) {
        Ok(value) => Some(value),
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(ch, "That number is outside the supported range.\r\n");
            None
        }
        Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
    }
}

/// two_arguments(): first two whitespace tokens (orig case) + remainder.
fn two_arguments(argument: &str) -> (String, String, String) {
    let (a, rest) = one_argument(argument);
    let (b, rest2) = one_argument(rest);
    (a, b, rest2.to_string())
}

/// GET_LEVEL of an id, 0 if gone.
fn level_of(g: &GameState, id: CharId) -> u8 {
    g.get_char(id).map(|c| c.player.level).unwrap_or(0)
}

/// GET_INVIS_LEV.
fn invis_lev(g: &GameState, id: CharId) -> i32 {
    g.get_char(id).map(|c| c.invis_level).unwrap_or(0)
}

fn is_npc(g: &GameState, id: CharId) -> bool {
    g.get_char(id).map(|c| c.is_npc).unwrap_or(false)
}

fn name_of(g: &GameState, id: CharId) -> String {
    g.get_char(id)
        .map(|c| c.player.name.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Trigger-type name tables (dg_triggers.c: trig_types/otrig_types/wtrig_types).
// Index == bit position; trailing "\n" terminates (sprintbit sentinel). Used by
// script_stat to label GET_TRIG_TYPE.
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

/// real_zone(number): the zone rnum whose `number*100..=top` range covers a
/// vnum (db.c real_zone). Returns -1 when no zone covers it (matches C, where
/// can_edit_zone then bounds-checks the negative).
fn real_zone(g: &GameState, number: i32) -> i32 {
    for (idx, z) in g.zones.iter().enumerate() {
        if z.contains_vnum(number) {
            return idx as i32;
        }
    }
    -1
}

/// can_edit_zone(ch, zone_rnum) — olc.c. Persisted Implementor trust passes
/// unconditionally; a negative/out-of-range zone fails; otherwise the
/// authenticated principal's name must appear in the zone's builder list.
fn can_edit_zone(g: &GameState, ch: CharId, number: i32) -> bool {
    let Some(principal) = authenticated_player_authority(g, ch) else {
        return false;
    };
    if principal.authority >= i32::from(LVL_IMPL) {
        return true;
    }
    if number < 0 || number as usize >= g.zones.len() {
        return false;
    }
    let builders = &g.zones[number as usize].builders;
    crate::handler::isname(&name_of(g, principal.principal), builders)
}

/// script_stat(ch, sc) — dg_scripts.c. Lists an entity's global script context
/// and each attached trigger (name/vnum/rnum, intended assignment, type bits,
/// numeric arg, arglist, and — for a parked trigger — its current line and
/// locals). Called by the do_sstat_* helpers. `key` selects the entity whose
/// ScriptData (dg_handler) is walked. Each `global_vars` entry is enumerated
/// under the "Global Variables:" header (name[:context] = value, UID-resolved).
fn script_stat(g: &mut GameState, ch: CharId, key: ScriptKey) {
    // find_uid_name(uid): resolve a UID_CHAR-prefixed value to a char/obj name
    // (dg UID space: char UID = CharId.0, obj UID = ObjId.0).
    fn find_uid_name(g: &GameState, value: &str) -> String {
        let mut chars = value.chars();
        if chars.next() == Some(crate::dg_scripts::UID_CHAR) {
            let rest = chars.as_str().trim();
            if let Ok(id) = rest.parse::<u64>() {
                if let Some(c) = g.get_char(CharId(id)) {
                    return c.player.name.clone();
                }
                if let Some(o) = g.get_obj(ObjId(id)) {
                    return o.name.clone();
                }
            }
            return format!("uid = {}, (not found)", rest);
        }
        value.to_string()
    }

    let context = dg_handler::get_context(key);
    let globals = dg_handler::global_vars(key);
    g.send_to_char(
        ch,
        &format!(
            "Global Variables: {}\r\n",
            if globals.is_empty() { "None" } else { "" }
        ),
    );
    g.send_to_char(ch, &format!("Global context: {}\r\n", context));
    for (gname, gvalue, gctx) in &globals {
        let label = if *gctx != 0 {
            format!("{}:{}", gname, gctx)
        } else {
            gname.clone()
        };
        let shown = if gvalue.starts_with(crate::dg_scripts::UID_CHAR) {
            find_uid_name(g, gvalue)
        } else {
            gvalue.clone()
        };
        g.send_to_char(ch, &format!("    {:>15}:  {}\r\n", label, shown));
    }

    for tid in dg_handler::trigger_ids(key) {
        let t = match dg_handler::trig_clone(tid) {
            Some(t) => t,
            None => continue,
        };
        // GET_TRIG_RNUM == the trig_index rnum (TrigData.nr).
        g.send_to_char(
            ch,
            &format!(
                "\r\n  Trigger: &y{}&n, VNum: [&g{:5}&n], RNum: [{:5}]\r\n",
                t.name, t.vnum, t.nr as i32
            ),
        );
        let (assign, table): (&str, &[&str]) = match t.attach_type {
            x if x == OBJ_TRIGGER => ("Objects", OTRIG_TYPES),
            x if x == WLD_TRIGGER => ("Rooms", WTRIG_TYPES),
            _ => ("Mobiles", TRIG_TYPES),
        };
        g.send_to_char(
            ch,
            &format!("  Trigger Intended Assignment: {}\r\n", assign),
        );
        let typebits = sprintbit(t.trigger_type, table);
        let arg = if t.arglist.is_empty() {
            "None"
        } else {
            &t.arglist
        };
        g.send_to_char(
            ch,
            &format!(
                "  Trigger Type: {}, Numeric Arg: {}, Arg list: {}\r\n",
                typebits, t.narg, arg
            ),
        );
        // GET_TRIG_WAIT(t): when a running trigger is parked on a `wait`, C also
        // prints the remaining pulse count, the paused command line and the
        // trigger's local variables. The remaining-pulse scalar is owned by the
        // dg_event queue and not exposed by EventId (no getter to read it without
        // editing dg_event.rs); the paused line and locals are shown.
        if t.wait_event.is_some() {
            let curr = t.cmdlist.get(t.curr_line).cloned().unwrap_or_default();
            g.send_to_char(ch, &format!("    Current line: {}\r\n", curr));
            g.send_to_char(
                ch,
                &format!(
                    "  Variables: {}\r\n",
                    if t.var_list.is_empty() { "None" } else { "" }
                ),
            );
            for tv in &t.var_list {
                let shown = if tv.value.starts_with(crate::dg_scripts::UID_CHAR) {
                    find_uid_name(g, &tv.value)
                } else {
                    tv.value.clone()
                };
                g.send_to_char(ch, &format!("    {:>15}:  {}\r\n", tv.name, shown));
            }
        }
    }
}

/// do_sstat_*(): "Script information:" header, then script_stat (or "None.").
fn do_sstat(g: &mut GameState, ch: CharId, key: ScriptKey) {
    g.send_to_char(ch, "Script information:\r\n");
    if !dg_handler::has_script(key) {
        g.send_to_char(ch, "  None.\r\n");
        return;
    }
    script_stat(g, ch, key);
}

/// CAN_SEE_OBJ (immortal stat path): ITEM_INVISIBLE needs AFF_DETECT_INVIS.
fn can_see_obj(g: &GameState, ch: CharId, oid: ObjId) -> bool {
    let obj = match g.get_obj(oid) {
        Some(o) => o,
        None => return false,
    };
    let invis = obj.extra_flags.bits() & (1u64 << 5) != 0; // ITEM_INVISIBLE
    if !invis {
        return true;
    }
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_DETECT_INVIS != 0)
        .unwrap_or(false)
}

/// skill_name(num): spell/skill name (spell_parser.c). Backed by the ported
/// spells[] name table in spell_parser.rs, so object spell displays and affect
/// readouts render real names ("armor", "fire breath", ...) rather than
/// "UNDEFINED".
use crate::spell_parser::skill_name;

/// mudlog(str, type, level) — the shared facility (utils.c). Writes a
/// timestamped line to the on-disk syslog file and echoes it to every online
/// immortal whose syslog preference is at/above `log_type` and level at/above
/// `min_level`. Delegates to the ported `syslog::mudlog` (file write + colour +
/// per-immortal PRF_LOG filtering), so callers pass the same `type` C uses.
fn mudlog(g: &mut GameState, line: &str, log_type: u8, min_level: u8) {
    crate::syslog::mudlog(g, line, log_type, min_level as Level);
}

/// send_to_all (comm.c): every playing descriptor.
fn send_to_all(g: &mut GameState, msg: &str) {
    let ids: Vec<CharId> = g.players_by_name.values().copied().collect();
    for id in ids {
        g.send_to_char(id, msg);
    }
}

// ---------------------------------------------------------------------------
// World-wide visible finders (handler.c get_char_vis / get_obj_vis), missing
// from the shared contract, so implemented here. They first scan the actor's
// room (room-vis ordinals), then the whole character/object lists.
// ---------------------------------------------------------------------------

/// get_char_vis(ch, name): visible character in room first, then world.
fn get_char_vis(g: &GameState, ch: CharId, arg: &str) -> Option<CharId> {
    if let Some(id) = g.get_char_room_vis(ch, arg) {
        return Some(id);
    }
    let (mut count, name) = crate::handler::get_number(arg);
    if count == 0 {
        return None;
    }
    for cid in g.char_ids() {
        if crate::handler::isname(&name, &name_of(g, cid)) && g.can_see(ch, cid) {
            count -= 1;
            if count == 0 {
                return Some(cid);
            }
        }
    }
    None
}

/// Async-bridge gate (the offline branch of do_set / do_stat-file / show
/// player). C's retrieve_player_entry loads the logged-off player's full record
/// synchronously; the Rust DB is async, so when the named target is OFFLINE but
/// present in the player_table we defer the whole immortal command instead of
/// degrading to "no such player". game.rs drains the queue next heartbeat: it
/// loads the player into the world, REPLAYS `command` verbatim (so the existing
/// online do_set/do_stat logic applies to the now-in-world char), then persists
/// + extracts. Returns true (and emits the "[ Loading … ]" line + queues) when
/// the op was deferred; false when `name` isn't an offline-indexed player (the
/// caller then sends its normal not-found line). Never fires for a name that is
/// already online (find_player_by_name resolves it) — including the requester —
/// so the online path is untouched and the bridge can't double-load.
fn try_defer_offline(
    g: &mut GameState,
    ch: CharId,
    name: &str,
    command: &str,
    authority: OfflineOpAuthority,
) -> bool {
    let first = name.split_whitespace().next().unwrap_or("");
    if first.is_empty() {
        return false;
    }
    // Already in the world (online, or already loaded by a prior op) -> the
    // normal online path handles it; do not defer.
    if g.find_player_by_name(first).is_some() {
        return false;
    }
    // Offline but in the persistent index -> bridge it.
    if g.get_id_by_name(first).is_some() {
        g.send_to_char(
            ch,
            &format!("[ Loading {} from the player file... ]\r\n", first),
        );
        g.queue_offline_op(ch, first, command, authority);
        return true;
    }
    false
}

/// Apply the one player-inspection authority rule and its one denial string.
/// Callers resolve persisted target trust from the live principal or player
/// index; the async bridge repeats this with the freshly loaded entity before
/// replay.
fn authorize_player_inspection(g: &mut GameState, requester: CharId, target_trust: i32) -> bool {
    if g.can_inspect_player_authority(requester, target_trust) {
        true
    } else {
        g.send_to_char(requester, PLAYER_INSPECTION_DENIED);
        false
    }
}

/// get_player_vis(ch, name): a visible PC (in room first, then world).
fn get_player_vis(g: &GameState, ch: CharId, arg: &str) -> Option<CharId> {
    let (mut count, name) = crate::handler::get_number(arg);
    if count == 0 {
        return None;
    }
    // Room first (CircleMUD scans room then world; PCs only).
    if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
        for &cid in &g.rooms[rnum].people {
            if is_npc(g, cid) {
                continue;
            }
            if crate::handler::isname(&name, &name_of(g, cid)) && g.can_see(ch, cid) {
                count -= 1;
                if count == 0 {
                    return Some(cid);
                }
            }
        }
    }
    let (mut count, name) = crate::handler::get_number(arg);
    for cid in g.char_ids() {
        if is_npc(g, cid) {
            continue;
        }
        if crate::handler::isname(&name, &name_of(g, cid)) && g.can_see(ch, cid) {
            count -= 1;
            if count == 0 {
                return Some(cid);
            }
        }
    }
    None
}

/// get_obj_vis(ch, name): equipment, then inventory, then room, then world.
fn get_obj_vis(g: &GameState, ch: CharId, arg: &str) -> Option<ObjId> {
    // equipment + inventory
    if let Some(c) = g.get_char(ch) {
        let mut list: Vec<ObjId> = c.equipment.iter().flatten().copied().collect();
        list.extend(c.carrying.iter().copied());
        if let Some(o) = g.get_obj_in_list_vis(ch, arg, &list) {
            return Some(o);
        }
        if let Some(rnum) = c.in_room {
            let room_list = g.rooms[rnum].contents.clone();
            if let Some(o) = g.get_obj_in_list_vis(ch, arg, &room_list) {
                return Some(o);
            }
        }
    }
    // whole world
    let world: Vec<ObjId> = g.obj_ids();
    g.get_obj_in_list_vis(ch, arg, &world)
}

// ---------------------------------------------------------------------------
// find_target_room (act.wizard.c): resolve a goto/at/teleport room arg.
// Returns Some(rnum) on success; on failure it has already messaged `ch` and
// returns None.
// ---------------------------------------------------------------------------
fn find_target_room(g: &mut GameState, ch: CharId, rawroomstr: &str) -> Option<RoomRnum> {
    let (roomstr, _rest) = one_argument(rawroomstr);
    if roomstr.is_empty() {
        g.send_to_char(ch, "You must supply a room number or name.\r\n");
        return None;
    }

    // C act.wizard.c:206: cdsr() (maputils.c:1030) is tried FIRST — it resolves
    // the surface-map "<x>x<y>" coordinate form, and (its trailing else) any
    // bare numeric vnum. It yields NOWHERE on a malformed string or an
    // out-of-range coordinate, letting the digit/mob/obj arms below run.
    let location: RoomRnum;
    if let Some(r) = crate::dg_wldcmd::cdsr(g, &roomstr) {
        location = r;
    } else if roomstr
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
        && !roomstr.contains('.')
    {
        let tmp = match crate::text::parse_i32_strict(&roomstr) {
            Ok(vnum) => vnum,
            Err(_) => {
                g.send_to_char(ch, "Invalid or out-of-range room number.\r\n");
                return None;
            }
        };
        match g.real_room(tmp) {
            Some(r) => location = r,
            None => {
                g.send_to_char(ch, "No room exists with that number.\r\n");
                return None;
            }
        }
    } else if let Some(mob) = get_char_vis(g, ch, &roomstr) {
        match g.get_char(mob).and_then(|c| c.in_room) {
            Some(r) => location = r,
            None => {
                g.send_to_char(ch, "No such creature or object around.\r\n");
                return None;
            }
        }
    } else if let Some(obj) = get_obj_vis(g, ch, &roomstr) {
        match g.get_obj(obj).map(|o| o.loc) {
            Some(ObjLoc::Room(r)) => location = r,
            _ => {
                g.send_to_char(ch, "That object is not available.\r\n");
                return None;
            }
        }
    } else {
        g.send_to_char(ch, "No such creature or object around.\r\n");
        return None;
    }

    // < GRGOD restriction checks use the authenticated player's persisted
    // trust. A high-level switched body or stale display level cannot bypass
    // GODROOM, private-room, or house ownership restrictions.
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You are not godly enough to use that room!\r\n");
        return None;
    };
    let flags = g.room(location).room_flags.bits();
    if authority.authority < i32::from(LVL_GRGOD) && (flags & ROOM_IMPROOM_BIT) != 0 {
        g.send_to_char(ch, "You are not godly enough to use that room!\r\n");
        return None;
    }
    if authority.authority < i32::from(LVL_GRGOD) {
        if (flags & ROOM_GODROOM_BIT) != 0 {
            g.send_to_char(ch, "You are not godly enough to use that room!\r\n");
            return None;
        }
        if (flags & ROOM_PRIVATE_BIT) != 0 && g.room(location).people.len() > 1 {
            g.send_to_char(
                ch,
                "There's a private conversation going on in that room.\r\n",
            );
            return None;
        }
        if (flags & ROOM_HOUSE_BIT) != 0 {
            // House_can_enter(ch, vnum): owner/guest (or LVL_GRGOD+) may enter.
            let house_vnum = g.room(location).number;
            if !crate::house::house_can_enter(g, authority.principal, house_vnum) {
                g.send_to_char(ch, "That's private property -- no trespassing!\r\n");
                return None;
            }
        }
    }
    Some(location)
}

// ===========================================================================
// do_echo / do_emote
// ===========================================================================
pub fn do_echo(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let argument = arg.trim_start();
    if argument.is_empty() {
        g.send_to_char(ch, "Yes.. but what?\r\n");
        return;
    }
    let body = format!("{}\r\n", argument);
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let name = name_of(g, ch);
    let people = g.room(rnum).people.clone();
    let ch_norepeat = g
        .get_char(ch)
        .map(|c| c.prf_flags & PRF_NOREPEAT != 0)
        .unwrap_or(false);
    // Snapshot the sender's PRF2 bits for the intangible filter below.
    let ch_prf2 = g.get_char(ch).map(|c| c.prf2_flags).unwrap_or(0);
    for vict in people {
        if vict == ch {
            if ch_norepeat {
                g.send_to_char(ch, OK);
            } else {
                if subcmd == SCMD_EMOTE {
                    g.send_to_char(ch, &name);
                    g.send_to_char(ch, " ");
                }
                g.send_to_char(ch, &body);
            }
            continue;
        }
        // isignore / PLR_WRITING gating: ignore lists aren't modelled, and
        // PLR_WRITING is checked against the writer's act_flags.
        let writing = g
            .get_char(vict)
            .map(|c| c.act_flags & PLR_WRITING != 0)
            .unwrap_or(false);
        if writing {
            continue;
        }
        if subcmd == SCMD_EMOTE {
            // C act.wizard.c:149: an intangible sender who is not building
            // hides the emote from mortal recipients who are not themselves
            // intangible.
            let vict_prf2 = g.get_char(vict).map(|c| c.prf2_flags).unwrap_or(0);
            let vict_authority = target_principal_authority(g, vict)
                .map(|principal| principal.authority)
                .unwrap_or(-1);
            if (ch_prf2 & PRF2_INTANGIBLE) != 0
                && (ch_prf2 & PRF2_MBUILDING) == 0
                && (vict_prf2 & PRF2_INTANGIBLE) == 0
                && vict_authority < i32::from(LVL_IMMORT)
            {
                continue;
            }
            // PERS(ch, vict): visible name else "someone".
            let pers = if g.can_see(vict, ch) {
                name.clone()
            } else {
                "someone".to_string()
            };
            g.send_to_char(vict, &pers);
            g.send_to_char(vict, " ");
        }
        g.send_to_char(vict, &body);
    }
}

// ===========================================================================
// do_send
// ===========================================================================
pub fn do_send(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (target, message) = half_chop(arg);
    if target.is_empty() {
        g.send_to_char(ch, "Send what to who?\r\n");
        return;
    }
    let vict = match get_char_vis(g, ch, &target) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, NOPERSON);
            return;
        }
    };
    g.send_to_char(vict, &message);
    g.send_to_char(vict, "\r\n");
    if g.get_char(ch)
        .map(|c| c.prf_flags & PRF_NOREPEAT != 0)
        .unwrap_or(false)
    {
        g.send_to_char(ch, "Sent.\r\n");
    } else {
        let vname = name_of(g, vict);
        g.send_to_char(ch, &format!("You send '{}' to {}.\r\n", message, vname));
    }
}

// ===========================================================================
// do_at
// ===========================================================================
pub fn do_at(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (loc_str, command) = half_chop(arg);
    if loc_str.is_empty() {
        g.send_to_char(ch, "You must supply a room number or a name.\r\n");
        return;
    }
    if command.is_empty() {
        g.send_to_char(ch, "What do you want to do there?\r\n");
        return;
    }
    let location = match find_target_room(g, ch, &loc_str) {
        Some(l) => l,
        None => return,
    };
    let original_loc = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    g.char_from_room(ch);
    g.char_to_room(ch, location);
    crate::interpreter::command_interpreter_authenticated(g, ch, &command);

    // If the char is still there, send them back.
    if g.get_char(ch).and_then(|c| c.in_room) == Some(location) {
        g.char_from_room(ch);
        g.char_to_room(ch, original_loc);
    }
}

// ===========================================================================
// do_goto
// ===========================================================================
pub fn do_goto(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let location = match find_target_room(g, ch, arg) {
        Some(l) => l,
        None => return,
    };
    if g.get_char(ch).and_then(|c| c.in_room) == Some(location) {
        g.send_to_char(ch, "You're already there... weirdo!\r\n");
        return;
    }

    let poofout = g.get_char(ch).and_then(|c| c.poofout.clone());
    let out_msg = match poofout {
        Some(p) => format!("$n {}", p),
        None => "You blink and suddenly realize that $n is gone.".to_string(),
    };
    act(g, &out_msg, true, ch, None, ActArg::None, To::Room);

    g.char_from_room(ch);
    g.char_to_room(ch, location);

    let poofin = g.get_char(ch).and_then(|c| c.poofin.clone());
    let in_msg = match poofin {
        Some(p) => format!("$n {}", p),
        None => "$n appears in a light from the heavens.".to_string(),
    };
    act(g, &in_msg, true, ch, None, ActArg::None, To::Room);

    look_at_room(g, ch);
}

/// look_at_room shim: the full renderer lives in cmd_informative::do_look; we
/// invoke the look command so the room is shown exactly as in C (look_at_room).
fn look_at_room(g: &mut GameState, ch: CharId) {
    crate::cmd_informative::do_look(g, ch, "", 0);
}

// ===========================================================================
// do_trans (transfer)
// ===========================================================================
pub fn do_trans(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "Whom do you wish to transfer?\r\n");
        return;
    }
    if !name.eq_ignore_ascii_case("all") {
        let victim = match get_char_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, NOPERSON);
                return;
            }
        };
        if victim == ch {
            g.send_to_char(ch, "That doesn't make much sense, does it?\r\n");
            return;
        }
        let (Some(authority), Some(victim_authority)) = (
            target_principal_authority(g, ch),
            target_principal_authority(g, victim),
        ) else {
            g.send_to_char(ch, "Go transfer someone your own size.\r\n");
            return;
        };
        let ordinary_npc = is_npc(g, victim) && !victim_authority.descriptor_controls_target;
        if authority.authority < victim_authority.authority && !ordinary_npc {
            g.send_to_char(ch, "Go transfer someone your own size.\r\n");
            return;
        }
        transfer_one(g, ch, victim);
        let dest = g.get_char(victim).and_then(|c| c.in_room);
        if let Some(rnum) = dest {
            let vname = name_of(g, victim);
            let cname = name_of(g, ch);
            let rname = g.room(rnum).name.clone();
            let vnum = g.room(rnum).number;
            let line = format!(
                "[WATCHDOG] {} has transferred {} to {} (vnum {})",
                cname, vname, rname, vnum
            );
            mudlog(g, &line, CMP, LVL_IMPL);
        }
    } else {
        // Trans All
        let Some(ch_authority) = authenticated_player_authority(g, ch) else {
            g.send_to_char(ch, "I think not.\r\n");
            return;
        };
        if ch_authority.authority < i32::from(LVL_GRGOD) {
            g.send_to_char(ch, "I think not.\r\n");
            return;
        }
        let targets: Vec<CharId> = g
            .descriptors
            .values()
            .filter(|descriptor| descriptor.state == ConState::Playing)
            .filter_map(|descriptor| descriptor.character)
            .collect();
        for victim in targets {
            if victim == ch {
                continue;
            }
            let Some(victim_authority) = target_principal_authority(g, victim) else {
                continue;
            };
            if victim_authority.authority >= ch_authority.authority {
                continue;
            }
            transfer_one(g, ch, victim);
        }
        g.send_to_char(ch, OK);
    }
}

fn transfer_one(g: &mut GameState, ch: CharId, victim: CharId) {
    act(
        g,
        "$n disappears in a mushroom cloud.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(victim);
    let dest = g.get_char(ch).and_then(|c| c.in_room);
    if let Some(rnum) = dest {
        g.char_to_room(victim, rnum);
    }
    act(
        g,
        "$n arrives from a puff of smoke.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "$n has transferred you!",
        false,
        ch,
        None,
        ActArg::Char(victim),
        To::Vict,
    );
    look_at_room(g, victim);
}

// ===========================================================================
// do_teleport
// ===========================================================================
pub fn do_teleport(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (who, where_arg, _rest) = two_arguments(arg);
    if who.is_empty() {
        g.send_to_char(ch, "Whom do you wish to teleport?\r\n");
        return;
    }
    let victim = match get_char_vis(g, ch, &who) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, NOPERSON);
            return;
        }
    };
    if victim == ch {
        g.send_to_char(ch, "Use 'goto' to teleport yourself.\r\n");
        return;
    }
    let authority = target_principal_authority(g, ch).map(|target| target.authority);
    let victim_authority = target_principal_authority(g, victim).map(|target| target.authority);
    if authority.is_none() || victim_authority.is_none() || victim_authority >= authority {
        g.send_to_char(ch, "Maybe you shouldn't do that.\r\n");
        return;
    }
    if where_arg.is_empty() {
        g.send_to_char(ch, "Where do you wish to send this person?\r\n");
        return;
    }
    let target = match find_target_room(g, ch, &where_arg) {
        Some(t) => t,
        None => return,
    };
    g.send_to_char(ch, OK);
    act(
        g,
        "$n disappears in a puff of smoke.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(victim);
    g.char_to_room(victim, target);
    act(
        g,
        "$n arrives from a puff of smoke.",
        false,
        victim,
        None,
        ActArg::None,
        To::Room,
    );
    act(
        g,
        "$n has teleported you!",
        false,
        ch,
        None,
        ActArg::Char(victim),
        To::Vict,
    );
    look_at_room(g, victim);
    let vname = name_of(g, victim);
    let cname = name_of(g, ch);
    let rname = g.room(target).name.clone();
    let vnum = g.room(target).number;
    let line = format!(
        "[WATCHDOG] {} has teleported {} to {} (vnum {})",
        cname, vname, rname, vnum
    );
    mudlog(g, &line, CMP, LVL_IMPL);
}

// ===========================================================================
// do_vnum
// ===========================================================================
pub fn do_vnum(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (kind, name, _rest) = two_arguments(arg);
    if kind.is_empty() || name.is_empty() || (!is_abbrev(&kind, "mob") && !is_abbrev(&kind, "obj"))
    {
        g.send_to_char(ch, "Usage: vnum { obj | mob } <name>\r\n");
        return;
    }
    if is_abbrev(&kind, "mob") && !vnum_mobile(g, ch, &name) {
        g.send_to_char(ch, "No mobiles by that name.\r\n");
    }
    if is_abbrev(&kind, "obj") && !vnum_object(g, ch, &name) {
        g.send_to_char(ch, "No objects by that name.\r\n");
    }
}

/// vnum_mobile (db.c): list prototypes whose namelist matches `name`.
fn vnum_mobile(g: &mut GameState, ch: CharId, name: &str) -> bool {
    let mut found = 0;
    let mut protos: Vec<(MobVnum, String, String)> = g
        .mob_protos
        .values()
        .map(|m| (m.vnum, m.name.clone(), m.short_desc.clone()))
        .collect();
    protos.sort_by_key(|p| p.0);
    let mut lines = String::new();
    for (vnum, namelist, short) in protos {
        if crate::handler::isname(name, &namelist) {
            found += 1;
            lines.push_str(&format!("{:3}. [{:5}] {}\r\n", found, vnum, short));
        }
    }
    g.send_to_char(ch, &lines);
    found != 0
}

/// vnum_object (db.c): list object prototypes whose namelist matches.
fn vnum_object(g: &mut GameState, ch: CharId, name: &str) -> bool {
    let mut found = 0;
    let mut protos: Vec<(ObjVnum, String, String)> = g
        .obj_protos
        .values()
        .map(|o| (o.vnum, o.name.clone(), o.short_desc.clone()))
        .collect();
    protos.sort_by_key(|p| p.0);
    let mut lines = String::new();
    for (vnum, namelist, short) in protos {
        if crate::handler::isname(name, &namelist) {
            found += 1;
            lines.push_str(&format!("{:3}. [{:5}] {}\r\n", found, vnum, short));
        }
    }
    g.send_to_char(ch, &lines);
    found != 0
}

// ===========================================================================
// do_stat_room / do_stat_object / do_stat_character + do_stat dispatcher
// ===========================================================================

fn do_stat_room(g: &mut GameState, ch: CharId) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    };
    let room_vnum = g.room(rnum).number;
    if authority.authority < i32::from(LVL_IMMORT) && !can_edit_zone(g, ch, real_zone(g, room_vnum))
    {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }

    let (name, sector, vnum, zone_num, light, flags, has_desc, desc, exits) = {
        let rm = g.room(rnum);
        let zone_num = g
            .zones
            .get(rm.zone as usize)
            .map(|z| z.number)
            .unwrap_or(rm.zone);
        (
            rm.name.clone(),
            rm.sector_type as i32,
            rm.number,
            zone_num,
            rm.light,
            rm.room_flags.bits() as i64,
            !rm.description.is_empty(),
            rm.description.clone(),
            rm.exits.clone(),
        )
    };

    g.send_to_char(ch, &format!("Room name: &c{}&n\r\n", name));
    let sectstr = sprinttype(sector, constants::SECTOR_TYPES);
    g.send_to_char(
        ch,
        &format!(
            "Zone: [{:3}], VNum: [&g{:5}&n], RNum: [{:5}], Type: {} Light: [{:2}]\r\n",
            zone_num, vnum, rnum, sectstr, light
        ),
    );
    let flagstr = sprintbit(flags, constants::ROOM_BITS);
    // C act.wizard.c:473-474: (rm->func == NULL) ? "None" : "Exists".
    let room_spec = crate::spec_assign::get_room_spec(g, room_vnum).is_some();
    g.send_to_char(
        ch,
        &format!(
            "SpecProc: {}, Flags: {}\r\n",
            if room_spec { "Exists" } else { "None" },
            flagstr
        ),
    );

    g.send_to_char(ch, "Description:\r\n");
    if has_desc {
        g.send_to_char(ch, &desc);
    } else {
        g.send_to_char(ch, "  None.\r\n");
    }

    // Extra descs.
    let extra: Vec<String> = g
        .room(rnum)
        .extra_descriptions
        .iter()
        .map(|(k, _)| k.clone())
        .collect();
    if !extra.is_empty() {
        let mut line = String::from("Extra descs:&c");
        for k in &extra {
            line.push(' ');
            line.push_str(k);
        }
        line.push_str("&n\r\n");
        g.send_to_char(ch, &line);
    }

    // Chars present.
    let people = g.room(rnum).people.clone();
    let mut buf = String::from("Chars present:&y");
    let mut found = 0;
    let mut idx = 0usize;
    let total = people.len();
    for k in people {
        if !g.can_see(ch, k) {
            idx += 1;
            continue;
        }
        let kind = if !is_npc(g, k) {
            "PC"
        } else if g.get_char(k).map(|c| c.nr == NOBODY).unwrap_or(true) {
            "NPC"
        } else {
            "MOB"
        };
        let nm = name_of(g, k);
        buf.push_str(&format!(
            "{} {}({})",
            if found > 0 { "," } else { "" },
            nm,
            kind
        ));
        found += 1;
        if buf.len() >= 62 {
            if idx + 1 < total {
                buf.push_str(",\r\n");
            } else {
                buf.push_str("\r\n");
            }
            g.send_to_char(ch, &buf);
            buf.clear();
            found = 0;
        }
        idx += 1;
    }
    if !buf.is_empty() {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }
    g.send_to_char(ch, "&n");

    // Contents.
    let contents = g.room(rnum).contents.clone();
    if !contents.is_empty() {
        let mut buf = String::from("Contents:&g");
        let mut found = 0;
        let mut idx = 0usize;
        let total = contents.len();
        for j in contents {
            if !can_see_obj(g, ch, j) {
                idx += 1;
                continue;
            }
            let short = g
                .get_obj(j)
                .map(|o| o.short_description.clone())
                .unwrap_or_default();
            buf.push_str(&format!("{} {}", if found > 0 { "," } else { "" }, short));
            found += 1;
            if buf.len() >= 62 {
                if idx + 1 < total {
                    buf.push_str(",\r\n");
                } else {
                    buf.push_str("\r\n");
                }
                g.send_to_char(ch, &buf);
                buf.clear();
                found = 0;
            }
            idx += 1;
        }
        if !buf.is_empty() {
            buf.push_str("\r\n");
            g.send_to_char(ch, &buf);
        }
        g.send_to_char(ch, "&n");
    }

    // Exits.
    for (i, exo) in exits.iter().enumerate() {
        if let Some(ex) = exo {
            let to_str = if ex.to_room == NOWHERE {
                " &cNONE&n".to_string()
            } else {
                format!("&c{:5}&n", ex.to_room)
            };
            let exinfo = sprintbit(ex.exit_info as i64, constants::EXIT_BITS);
            let kw = ex.keyword.clone().unwrap_or_else(|| "None".to_string());
            g.send_to_char(
                ch,
                &format!(
                    "Exit &c{:<5}&n:  To: [{}], Key: [{:5}], Keywrd: {}, Type: {}\r\n ",
                    DIR_NAMES[i], to_str, ex.key, kw, exinfo
                ),
            );
            match &ex.description {
                Some(d) if !d.is_empty() => g.send_to_char(ch, d),
                _ => g.send_to_char(ch, "  No exit description.\r\n"),
            }
        }
    }
    // do_sstat_room: DG-script room trigger listing.
    do_sstat(g, ch, ScriptKey::Room(rnum));
}

fn do_stat_object(g: &mut GameState, ch: CharId, j: ObjId) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    };
    let obj_vnum = g.get_obj(j).map(|o| o.item_number).unwrap_or(NOTHING);
    if authority.authority < i32::from(LVL_IMMORT) && !can_edit_zone(g, ch, real_zone(g, obj_vnum))
    {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }
    let (
        vnum,
        short,
        namelist,
        ldesc,
        otype,
        wear,
        bitvector,
        extra,
        weight,
        cost,
        rent,
        timer,
        minlvl,
        loc,
        contained_in,
        carried_by,
        worn_by,
        values,
        contains,
        affects,
        curr_slots,
        total_slots,
    ) = {
        let o = match g.get_obj(j) {
            Some(o) => o,
            None => return,
        };
        (
            o.item_number,
            if o.short_description.is_empty() {
                "<None>".to_string()
            } else {
                o.short_description.clone()
            },
            o.name.clone(),
            if o.description.is_empty() {
                "None".to_string()
            } else {
                o.description.clone()
            },
            o.obj_type as i32,
            o.wear_flags.bits() as i64,
            o.bitvector,
            o.extra_flags.bits() as i64,
            o.weight,
            o.cost,
            o.rent,
            o.timer,
            o.level,
            o.loc,
            match o.loc {
                ObjLoc::Contained(c) => Some(c),
                _ => None,
            },
            match o.loc {
                ObjLoc::Carried(c) => Some(c),
                _ => None,
            },
            match o.loc {
                ObjLoc::Worn(c, _) => Some(c),
                _ => None,
            },
            o.values,
            o.contains.clone(),
            o.affects.clone(),
            o.curr_slots,  // GET_OBJ_CSLOTS
            o.total_slots, // GET_OBJ_TSLOTS
        )
    };

    g.send_to_char(
        ch,
        &format!("Name: '&y{}&n', Aliases: {}\r\n", short, namelist),
    );
    let typestr = sprinttype(otype, constants::ITEM_TYPES);
    // C act.wizard.c:605-610: obj_index[GET_OBJ_RNUM(j)].func ? "Exists" : "None".
    let obj_spec = crate::spec_assign::get_obj_spec(g, vnum).is_some();
    let rnum = if obj_spec { "Exists" } else { "None" };
    g.send_to_char(
        ch,
        &format!(
            "VNum: [&g{:5}&n], RNum: [{:5}], Type: {}, SpecProc: {}\r\n",
            vnum, -1, typestr, rnum
        ),
    );
    g.send_to_char(ch, &format!("L-Des: {}\r\n", ldesc));

    g.send_to_char(ch, "Can be worn on: ");
    g.send_to_char(
        ch,
        &format!("{}\r\n", sprintbit(wear, constants::WEAR_BITS)),
    );
    g.send_to_char(ch, "Set char bits : ");
    g.send_to_char(
        ch,
        &format!("{}\r\n", sprintbit(bitvector, constants::AFFECTED_BITS)),
    );
    g.send_to_char(ch, "Extra flags   : ");
    g.send_to_char(
        ch,
        &format!("{}\r\n", sprintbit(extra, constants::EXTRA_BITS)),
    );

    g.send_to_char(
        ch,
        &format!(
            "Weight: {}, Value: {}, Cost/day: {}, Timer: {} Level: {}\r\n",
            weight, cost, rent, timer, minlvl
        ),
    );

    let mut line = String::from("In room: ");
    match loc {
        ObjLoc::Room(r) => line.push_str(&g.room(r).number.to_string()),
        _ => line.push_str("Nowhere"),
    }
    line.push_str(", In object: ");
    line.push_str(&match contained_in {
        Some(c) => g
            .get_obj(c)
            .map(|o| o.short_description.clone())
            .unwrap_or_else(|| "None".to_string()),
        None => "None".to_string(),
    });
    line.push_str(", Carried by: ");
    line.push_str(&match carried_by {
        Some(c) => name_of(g, c),
        None => "Nobody".to_string(),
    });
    line.push_str(", Worn by: ");
    line.push_str(&match worn_by {
        Some(c) => name_of(g, c),
        None => "Nobody".to_string(),
    });
    line.push_str("\r\n");
    g.send_to_char(ch, &line);

    // Type-specific values block.
    let detail = match ObjectType::from_i32(otype) {
        ObjectType::Light => {
            if values[2] == -1 {
                "Hours left: Infinite".to_string()
            } else {
                format!("Hours left: [{}]", values[2])
            }
        }
        ObjectType::Scroll | ObjectType::Potion => format!(
            "Spells: (Level {}) {}, {}, {}",
            values[0],
            skill_name(values[1]),
            skill_name(values[2]),
            skill_name(values[3])
        ),
        ObjectType::Wand | ObjectType::Staff => format!(
            "Spell: {} at level {}, {} (of {}) charges remaining",
            skill_name(values[3]),
            values[0],
            values[2],
            values[1]
        ),
        ObjectType::Weapon => format!(
            "Todam: {}d{} (avg-dmg {:.1}), Message type: {}",
            values[1],
            values[2],
            ((values[2] + 1) as f64 / 2.0) * values[1] as f64,
            values[3]
        ),
        ObjectType::Armor => format!("Defense-app: [{}]", values[0]),
        ObjectType::Container => format!(
            "Weight capacity: {}, Lock Type: {}, Key Num: {}, Corpse: {}",
            values[0],
            sprintbit(values[1] as i64, constants::CONTAINER_BITS),
            values[2],
            yesno(values[3] != 0)
        ),
        ObjectType::LiqContainer | ObjectType::Fountain => format!(
            "Capacity: {}, Contains: {}, Poisoned: {}, Liquid: {}",
            values[0],
            values[1],
            yesno(values[3] != 0),
            sprinttype(values[2], constants::DRINKS)
        ),
        ObjectType::Note => format!("Tongue: {}", values[0]),
        ObjectType::Key => String::new(),
        ObjectType::Food => format!(
            "Makes full: {}, Poisoned: {}",
            values[0],
            yesno(values[3] != 0)
        ),
        ObjectType::Money => format!("Coins: {}", values[0]),
        _ => format!(
            "Values 0-3: [{}] [{}] [{}] [{}]",
            values[0], values[1], values[2], values[3]
        ),
    };
    // act.wizard.c do_stat_object: "Quality: [%d] [%d]" with
    // GET_OBJ_CSLOTS / GET_OBJ_TSLOTS (obj_flags.curr_slots / total_slots).
    g.send_to_char(
        ch,
        &format!(
            "{}\r\nQuality: [{}] [{}]\r\n",
            detail, curr_slots, total_slots
        ),
    );

    // Contents.
    if !contains.is_empty() {
        let mut buf = String::from("\r\nContents:&g");
        let mut found = 0;
        let mut idx = 0usize;
        let total = contains.len();
        for j2 in contains {
            let short = g
                .get_obj(j2)
                .map(|o| o.short_description.clone())
                .unwrap_or_default();
            buf.push_str(&format!("{} {}", if found > 0 { "," } else { "" }, short));
            found += 1;
            if buf.len() >= 62 {
                if idx + 1 < total {
                    buf.push_str(",\r\n");
                } else {
                    buf.push_str("\r\n");
                }
                g.send_to_char(ch, &buf);
                buf.clear();
                found = 0;
            }
            idx += 1;
        }
        if !buf.is_empty() {
            buf.push_str("\r\n");
            g.send_to_char(ch, &buf);
        }
        g.send_to_char(ch, "&n");
    }

    // Affections.
    g.send_to_char(ch, "Affections:");
    let mut found = 0;
    for a in &affects {
        if a.modifier != 0 {
            let loc = sprinttype(a.location, constants::APPLY_TYPES);
            g.send_to_char(
                ch,
                &format!(
                    "{} {:+} to {}",
                    if found > 0 { "," } else { "" },
                    a.modifier,
                    loc
                ),
            );
            found += 1;
        }
    }
    if found == 0 {
        g.send_to_char(ch, " None");
    }
    g.send_to_char(ch, "\r\n");
    // do_sstat_object: DG-script object trigger listing.
    do_sstat(g, ch, ScriptKey::Obj(j));
}

fn do_stat_character(g: &mut GameState, ch: CharId, k: CharId) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You find yourself unable to.\r\n");
        return;
    };
    if !is_npc(g, k) {
        let Some(target) = exact_player_authority(g, k) else {
            g.send_to_char(ch, PLAYER_INSPECTION_DENIED);
            return;
        };
        if !authorize_player_inspection(g, ch, target.authority) {
            return;
        }
        if authority.authority < i32::from(LVL_IMMORT) {
            g.send_to_char(ch, "You find yourself unable to.\r\n");
            return;
        }
    } else if authority.authority < i32::from(LVL_IMMORT) {
        // Mortal builders may stat a mob whose zone they own (can_edit_zone of
        // real_zone(GET_MOB_VNUM)).
        let mob_vnum = g.get_char(k).map(|c| c.nr).unwrap_or(NOBODY);
        if !can_edit_zone(g, ch, real_zone(g, mob_vnum)) {
            g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
            return;
        }
    }

    // Snapshot everything we need before any send.
    let kc = match g.get_char(k) {
        Some(c) => c,
        None => return,
    };
    let sex = kc.player.sex;
    let npc = kc.is_npc;
    let is_mob = npc && kc.nr != NOBODY;
    let kname = kc.player.name.clone();
    let idnum = kc.idnum;
    let in_room_vnum = kc.in_room.map(|r| g.rooms[r].number).unwrap_or(NOWHERE);
    let loadroom = kc.load_room;
    let mob_vnum = kc.nr;
    let title = kc.player.title.clone();
    let trust = kc.trust;
    // C act.wizard.c:828 prints IS_GOD(k) — the granted-command bits.
    let gcmd = is_god(g, k);
    let long_descr = kc.long_desc.clone();
    let class = kc.player.class as i32;
    let klevel = kc.player.level;
    let exp = kc.points.exp;
    let align = kc.alignment;
    let citizen = kc.citizen as i32; // GET_CITIZEN (Cstat is GET_CITIZEN+1).
    let abils = kc.aff_abils;
    let points = kc.points.clone();
    let position = kc.position as i32;
    let fighting = kc.fighting;
    let default_pos = kc.position as i32; // mob_specials.default_pos not modelled separately
    let timer = kc.timer;
    let act_flags = kc.act_flags;
    let prf_flags = kc.prf_flags;
    let prf2_flags = kc.prf2_flags;
    let affect_flags = kc.affect_flags;
    let carry_weight = kc.carry_weight;
    let carry_items = kc.carry_items;
    let n_inv = kc.carrying.len();
    let n_eq = kc.equipment.iter().flatten().count();
    let conditions = kc.conditions;
    let master = kc.master;
    let followers = kc.followers.clone();
    let affected = kc.affected.clone();
    let connected = kc.desc.is_some();
    // C sprinttype(k->desc->connected, connected_types) — the real state.
    let conn_state = kc
        .desc
        .and_then(|conn| g.descriptors.get(&conn))
        .map(|d| d.state);
    let hometown = kc.player.hometown;
    let talks = kc.talks;
    let clan = kc.clan;
    let time_birth = kc.player.time_birth;
    let time_played = kc.player.time_played.max(0);
    let last_logon = kc.last_logon.timestamp();
    let practices = kc.spells_to_learn;
    let next_quest = kc.next_quest;
    let countdown = kc.quest_countdown;
    let quest_mob = kc.quest_mob;
    let quest_obj = kc.quest_obj;
    let wins = kc.wins;
    let losses = kc.losses;
    let damnodice = g
        .mob_protos
        .get(&mob_vnum)
        .map(|m| m.damnodice)
        .unwrap_or(0);
    let damsizedice = g
        .mob_protos
        .get(&mob_vnum)
        .map(|m| m.damsizedice)
        .unwrap_or(0);
    // C act.wizard.c:1000-1002: mob_index[GET_MOB_RNUM(k)].func ? "Exists" : "None".
    let mob_spec = crate::spec_assign::get_mob_spec(g, mob_vnum).is_some();
    // C act.wizard.c:908: attack_hit_text[k->mob_specials.attack_type].singular.
    let attack_type = g
        .mob_protos
        .get(&mob_vnum)
        .map(|m| m.attack_type)
        .unwrap_or(0);
    let attack_word = constants::ATTACK_HIT_TEXT
        .get(attack_type.clamp(0, constants::ATTACK_HIT_TEXT.len() as i32 - 1) as usize)
        .map(|(s, _)| (*s).to_string())
        .unwrap_or_else(|| "hit".to_string());
    // C act.wizard.c:847-850 / 870-873 / 887-890: MaxWeapon, practices-per,
    // and the hit/mana/move regen rates.
    let maxweapon = constants::LVL_MAXDMG_WEAPON
        .get(klevel as usize)
        .copied()
        .unwrap_or(0);
    let learn_per = constants::INT_APP
        .get((abils.intel as i32).clamp(0, constants::INT_APP.len() as i32 - 1) as usize)
        .map(|a| a.learn)
        .unwrap_or(0);
    let nstl = constants::WIS_APP
        .get((abils.wis as i32).clamp(0, constants::WIS_APP.len() as i32 - 1) as usize)
        .map(|a| a.bonus)
        .unwrap_or(0);
    let (hit_regen, mana_regen, move_regen) = (
        crate::limits::hit_gain(g, k),
        crate::limits::mana_gain(g, k),
        crate::limits::move_gain(g, k),
    );

    let sexstr = match sex {
        Gender::Neutral => "NEUTRAL-SEX",
        Gender::Male => "MALE",
        Gender::Female => "FEMALE",
    };
    let kind = if !npc {
        "PC"
    } else if mob_vnum == NOBODY {
        "NPC"
    } else {
        "MOB"
    };
    let mut hdr = format!(
        "{} {} '{}'  IDNum: [{:5}], In room [{:5}]",
        sexstr, kind, kname, idnum, in_room_vnum
    );
    if !npc {
        hdr.push_str(&format!(", LoadRoom: [{:5}]", loadroom));
    }
    hdr.push_str("\r\n");
    g.send_to_char(ch, &hdr);

    if is_mob {
        g.send_to_char(
            ch,
            &format!(
                "Alias: {}, VNum: [{:5}], RNum: [{:5}]\r\n",
                kname, mob_vnum, -1
            ),
        );
    }

    g.send_to_char(
        ch,
        &format!(
            "Title: {}     Trust: {}     God-Commands: {}",
            title.clone().unwrap_or_else(|| "<None>".to_string()),
            trust,
            if gcmd { "&YYes&n\r\n" } else { "No\r\n" }
        ),
    );

    g.send_to_char(
        ch,
        &format!(
            "L-Des: {}",
            long_descr.unwrap_or_else(|| "<None>\r\n".to_string())
        ),
    );

    let classstr = if npc {
        sprinttype(class, constants::NPC_CLASS_TYPES)
    } else {
        // pc_class_types live in class.c (not surfaced); use abbreviation set.
        match Class::from_u8(class as u8) {
            Class::MagicUser => "Magic User".to_string(),
            Class::Cleric => "Cleric".to_string(),
            Class::Thief => "Thief".to_string(),
            Class::Warrior => "Warrior".to_string(),
            Class::Artisan => "Artisan".to_string(),
        }
    };
    let class_label = if npc { "Monster Class: " } else { "Class: " };
    let lvl_line = if klevel < LVL_IMMORT {
        format!(
            "{}{}, Lev: [&y{:2}&n], XP: [&y{:7}&n], Align: [{:4}], MaxWeapon: [{}], Cstat: [{}]\r\n",
            class_label,
            classstr,
            klevel,
            exp,
            align,
            maxweapon,
            citizen + 1
        )
    } else {
        format!(
            "{}{}, Lev: [&y{:2}&n], XP: [&y{:7}&n], Align: [{:4}], Cstat: [{}]\r\n",
            class_label,
            classstr,
            klevel,
            exp,
            align,
            citizen + 1
        )
    };
    g.send_to_char(ch, &lvl_line);

    if !npc {
        let created = ctime(time_birth);
        let last = ctime(last_logon);
        let played_hours = time_played / 3600;
        let played_minutes = (time_played % 3600) / 60;
        let (age_years, _, _, _) = mud_age_parts(time_birth);
        g.send_to_char(
            ch,
            &format!(
                "Created: [{}], Last Logon: [{}], Played [{}h {}m], Age [{}]\r\n",
                created.chars().take(10).collect::<String>(),
                last.chars().take(10).collect::<String>(),
                played_hours,
                played_minutes,
                age_years
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "Hometown: [{}], Speaks: [{}/{}/{}], (STL[{}]/per[{}]/NSTL[{}]) Clan: [{}]\r\n",
                hometown,
                talks[0] as i32,
                talks[1] as i32,
                talks[2] as i32,
                practices,
                learn_per,
                nstl,
                clan
            ),
        );
    }

    g.send_to_char(
        ch,
        &format!(
            "Str: [&c{}/{}&n]  Int: [&c{}&n]  Wis: [&c{}&n]  Dex: [&c{}&n]  Con: [&c{}&n]  Cha: [&c{}&n]\r\n",
            abils.str, abils.str_add, abils.intel, abils.wis, abils.dex, abils.con, abils.cha
        ),
    );

    g.send_to_char(
        ch,
        &format!(
            "Hit p.:[&g{}/{}+{}&n]  Mana p.:[&g{}/{}+{}&n]  Move p.:[&g{}/{}+{}&n]\r\n",
            points.hit,
            points.max_hit,
            hit_regen,
            points.mana,
            points.max_mana,
            mana_regen,
            points.move_points,
            points.max_move,
            move_regen
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "Coins: [{:9}], Bank: [{:9}] (Total: {})\r\n",
            points.gold,
            points.bank_gold,
            i64::from(points.gold) + i64::from(points.bank_gold)
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "Defense: [{}], Magic Defense: [{:2}], Power: [{:2}], Magic Power: [{}] Technique: [{:2}]\r\n",
            points.defense, points.mdefense, points.power, points.mpower, points.technique
        ),
    );

    let posstr = sprinttype(position, constants::POSITION_TYPES);
    let mut buf = format!(
        "Pos: {}, Fighting: {}",
        posstr,
        match fighting {
            Some(f) => name_of(g, f),
            None => "Nobody".to_string(),
        }
    );
    if is_mob {
        buf.push_str(&format!(", Attack type: {}", attack_word));
    }
    if let Some(st) = conn_state {
        buf.push_str(&format!(
            ", Connected: {}",
            sprinttype(conn_state_index(st), constants::CONNECTED_TYPES)
        ));
    }
    if !npc {
        // Arena status block (GET_ARENASTAT), matching act.wizard.c do_stat_character.
        let stat = crate::arena::arena_stat(&g, k);
        buf.push_str("\r\nArena: ");
        match stat {
            crate::arena::ARENA_NOT => buf.push_str("[NO]"),
            crate::arena::ARENA_COMBATANT1 => buf.push_str("[COMBAT1]"),
            crate::arena::ARENA_COMBATANT1W => buf.push_str("[COMBAT1W]"),
            crate::arena::ARENA_COMBATANT2 => buf.push_str("[COMBAT2]"),
            crate::arena::ARENA_COMBATANT3 => buf.push_str("[COMBAT3]"),
            crate::arena::ARENA_COMBATANTZ => buf.push_str("[COMBATZ]"),
            crate::arena::ARENA_OBSERVER => {
                buf.push_str("[OBSERV]");
                match crate::arena::arena_observing(g, k) {
                    Some(t) => buf.push_str(&format!(", Observing: [{}]", name_of(g, t))),
                    None => buf.push_str(", Observing: [NOBODY]"),
                }
            }
            _ => buf.push_str("[UNKNOWN]"),
        }
        buf.push_str(&format!(", Wins: [{}]", wins));
        buf.push_str(&format!(", Losses: [{}]", losses));
        if connected {
            let ft = crate::arena::arena_flee_timer(g, k);
            if ft > 0 {
                let lf = match crate::arena::arena_last_fighting(g, k) {
                    Some(o) => name_of(g, o),
                    None => String::new(),
                };
                buf.push_str(&format!(", Fled-a-match: {} [timer {}]", lf, ft));
            }
        }
    }
    buf.push_str("\r\n");
    g.send_to_char(ch, &buf);

    let dposstr = sprinttype(default_pos, constants::POSITION_TYPES);
    g.send_to_char(
        ch,
        &format!(
            "Default position: {}, Idle Timer (in tics) [{}]\r\n",
            dposstr, timer
        ),
    );

    if npc {
        let flagstr = sprintbit(act_flags, constants::ACTION_BITS);
        g.send_to_char(ch, &format!("NPC flags: &c{}&n\r\n", flagstr));
    } else {
        let mut qline = format!(
            "Quest Next: [{}], Quest Timeleft: [{}]",
            next_quest, countdown
        );
        if quest_mob > 0 {
            qline.push_str(&format!(", On Quest for Mob: [{}]", quest_mob));
        }
        if quest_mob < 0 {
            qline.push_str(&format!(
                ", Killed target mob of level: [{}]",
                quest_mob.abs()
            ));
        }
        if quest_obj > 0 {
            qline.push_str(&format!(", On Quest for Obj: [{}]", quest_obj));
        }
        qline.push_str("\r\n");
        g.send_to_char(ch, &qline);

        g.send_to_char(
            ch,
            &format!(
                "PLR : &c{}&n\r\n",
                sprintbit(act_flags, constants::PLAYER_BITS)
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "PRF : &g{}&n\r\n",
                sprintbit(prf_flags, constants::PREFERENCE_BITS)
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "PRF2: &g{}&n\r\n",
                sprintbit(prf2_flags, constants::PREFERENCE2_BITS)
            ),
        );
    }

    if is_mob {
        g.send_to_char(
            ch,
            &format!(
                "Mob Spec-Proc: {}, NPC Bare Hand Dam: {}d{}\r\n",
                if mob_spec { "Exists" } else { "None" },
                damnodice,
                damsizedice
            ),
        );
    }

    g.send_to_char(
        ch,
        &format!(
            "Carried: weight: {}, items: {}; Items in: inventory: {}, eq: {}\r\n",
            carry_weight, carry_items, n_inv, n_eq
        ),
    );

    g.send_to_char(
        ch,
        &format!(
            "Hunger: {}, Thirst: {}, Drunk: {}\r\n",
            conditions[FULL], conditions[THIRST], conditions[DRUNK]
        ),
    );

    // Master / followers.
    let mut buf = format!(
        "Master is: {}, Followers are:",
        match master {
            Some(m) => name_of(g, m),
            None => "<none>".to_string(),
        }
    );
    let mut found = 0;
    let total = followers.len();
    for (i, fol) in followers.iter().enumerate() {
        let pers = if g.can_see(ch, *fol) {
            name_of(g, *fol)
        } else {
            "someone".to_string()
        };
        buf.push_str(&format!("{} {}", if found > 0 { "," } else { "" }, pers));
        found += 1;
        if buf.len() >= 62 {
            if i + 1 < total {
                buf.push_str(",\r\n");
            } else {
                buf.push_str("\r\n");
            }
            g.send_to_char(ch, &buf);
            buf.clear();
            found = 0;
        }
    }
    if !buf.is_empty() {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }

    // AFF bitvector.
    g.send_to_char(
        ch,
        &format!(
            "AFF: &y{}&n\r\n",
            sprintbit(affect_flags, constants::AFFECTED_BITS)
        ),
    );

    // Active spell affects.
    for aff in &affected {
        if aff.spell_type == -1 && aff.duration == -1 {
            let bits = sprintbit(aff.bitvector, constants::AFFECTED_BITS);
            g.send_to_char(ch, &format!("SPL: (&YO&nPERM) &c{:<21}&n \r\n", bits));
            continue;
        }
        let spell = skill_name(aff.spell_type);
        let mut line = format!("SPL: ({:3}hr) &c{:<21}&n ", aff.duration + 1, spell);
        let mut had_mod = false;
        if aff.modifier != 0 {
            line.push_str(&format!(
                "{:+} to {}",
                aff.modifier,
                sprinttype(aff.location, constants::APPLY_TYPES)
            ));
            had_mod = true;
        }
        if aff.bitvector != 0 {
            line.push_str(if had_mod { ", sets " } else { "sets " });
            line.push_str(&sprintbit(aff.bitvector, constants::AFFECTED_BITS));
        }
        line.push_str("\r\n");
        g.send_to_char(ch, &line);
    }
    // do_sstat_character: DG-script trigger listing (mobs carry triggers).
    do_sstat(g, ch, ScriptKey::Mob(k));
}

// ===========================================================================
// do_stat dispatcher
// ===========================================================================
pub fn do_stat(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (kind, rest) = half_chop(arg);
    if kind.is_empty() {
        g.send_to_char(ch, "Stats on who or what?\r\n");
        return;
    }
    if is_abbrev(&kind, "room") {
        do_stat_room(g, ch);
    } else if is_abbrev(&kind, "mob") {
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which mobile?\r\n");
        } else if let Some(v) = get_char_vis(g, ch, &rest) {
            do_stat_character(g, ch, v);
        } else {
            g.send_to_char(ch, "No such mobile around.\r\n");
        }
    } else if is_abbrev(&kind, "player") {
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which player?\r\n");
        } else {
            let online = get_player_vis(g, ch, &rest);
            if let Some(v) = online {
                let Some(target) = exact_player_authority(g, v) else {
                    g.send_to_char(ch, PLAYER_INSPECTION_DENIED);
                    return;
                };
                if !authorize_player_inspection(g, ch, target.authority) {
                    return;
                }
                do_stat_character(g, ch, v);
            } else {
                if let Some(target) = g.player_index(&rest)
                    && !authorize_player_inspection(g, ch, target.trust)
                {
                    return;
                }
                if try_defer_offline(
                    g,
                    ch,
                    &rest,
                    &format!("stat player {}", rest),
                    OfflineOpAuthority::InspectPlayer,
                ) {
                    // The replay repeats this trust check after the DB load.
                } else {
                    g.send_to_char(ch, "No such player around.\r\n");
                }
            }
        }
    } else if is_abbrev(&kind, "file") {
        // stat file <name>: retrieve_player_entry() loads an OFFLINE player's
        // full record. The real read is an async DB query (database::load_player)
        // unreachable from this sync path, so when the named player exists in the
        // index we defer through the async bridge (game.rs loads + replays +
        // extracts). We replay as `stat player <name>` so the replayed pass takes
        // the online get_player_vis branch (the char is now in the world) instead
        // of re-entering this `file` branch and deferring forever.
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which player?\r\n");
        } else {
            // C act.wizard.c:1140-1143: retrieve_player_entry(), then refuse a
            // target whose authority exceeds the requester's — "Sorry, you
            // can't do that." — before any record is rendered. Target trust
            // comes from the live character when online, else the persistent
            // player index (C's player_table, which retrieve_player_entry
            // walks).
            let online = get_player_vis(g, ch, &rest);
            let target_trust = match online {
                Some(v) => exact_player_authority(g, v).map(|target| target.authority),
                None => g.player_index(&rest).map(|p| p.trust),
            };
            match target_trust {
                None => g.send_to_char(ch, "There is no such player.\r\n"),
                Some(trust) => {
                    if !authorize_player_inspection(g, ch, trust) {
                        return;
                    }
                    match online {
                        Some(v) => do_stat_character(g, ch, v),
                        // Offline: the async bridge loads the record, replays
                        // `stat player <name>` so the online path renders it, then
                        // saves + extracts (C retrieve_player_entry/insert_player_entry).
                        None => {
                            try_defer_offline(
                                g,
                                ch,
                                &rest,
                                &format!("stat player {}", rest),
                                OfflineOpAuthority::InspectPlayer,
                            );
                        }
                    }
                }
            }
        }
    } else if is_abbrev(&kind, "object") {
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which object?\r\n");
        } else if let Some(o) = get_obj_vis(g, ch, &rest) {
            do_stat_object(g, ch, o);
        } else {
            g.send_to_char(ch, "No such object around.\r\n");
        }
    } else {
        // Bareword: equipment, inventory, room chars, room objs, world char/obj.
        let eq: Vec<ObjId> = g
            .get_char(ch)
            .map(|c| c.equipment.iter().flatten().copied().collect())
            .unwrap_or_default();
        if let Some(o) = g.get_obj_in_list_vis(ch, &kind, &eq) {
            do_stat_object(g, ch, o);
            return;
        }
        let inv: Vec<ObjId> = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        if let Some(o) = g.get_obj_in_list_vis(ch, &kind, &inv) {
            do_stat_object(g, ch, o);
            return;
        }
        if let Some(v) = g.get_char_room_vis(ch, &kind) {
            do_stat_character(g, ch, v);
            return;
        }
        let room_objs: Vec<ObjId> = g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.rooms[r].contents.clone())
            .unwrap_or_default();
        if let Some(o) = g.get_obj_in_list_vis(ch, &kind, &room_objs) {
            do_stat_object(g, ch, o);
            return;
        }
        if let Some(v) = get_char_vis(g, ch, &kind) {
            do_stat_character(g, ch, v);
            return;
        }
        if let Some(o) = get_obj_vis(g, ch, &kind) {
            do_stat_object(g, ch, o);
            return;
        }
        g.send_to_char(ch, "Nothing around by that name.\r\n");
    }
}

// ===========================================================================
// do_shutdown
// ===========================================================================
/// utils.c:333 touch(): create an empty control file for the autorun
/// supervisor (#211/#199).
fn write_control_file(g: &GameState, name: &str) {
    // C chdir(DFLT_DIR="lib") at boot then touch("../.fastboot") (db.h):
    // the marker lands in the DELTAMUD ROOT, where the autorun wrapper
    // greps it. The old hardcoded "lib/etc/<name>" landed two levels below
    // the wrapper's search path, so shutdown reboot/die/pause silently
    // rebooted as plain crashes.
    let root = std::path::Path::new(&g.config.lib_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    if let Ok(mut f) = std::fs::File::create(root.join(name)) {
        use std::io::Write;
        let _ = f.write_all(b"");
    }
}

fn requested_shutdown_mode(option: &str) -> Option<ShutdownMode> {
    if option.is_empty() {
        Some(ShutdownMode::Shutdown)
    } else if option.eq_ignore_ascii_case("reboot") {
        Some(ShutdownMode::Reboot)
    } else if option.eq_ignore_ascii_case("now") {
        Some(ShutdownMode::Now)
    } else if option.eq_ignore_ascii_case("die") {
        Some(ShutdownMode::Die)
    } else if option.eq_ignore_ascii_case("pause") {
        Some(ShutdownMode::Pause)
    } else {
        None
    }
}

fn requested_shutdown_disposition(option: &str) -> Option<ProcessDisposition> {
    requested_shutdown_mode(option).map(ShutdownMode::disposition)
}

pub fn do_shutdown(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    if subcmd != SCMD_SHUTDOWN {
        g.send_to_char(ch, "If you want to shut something down, say so!\r\n");
        return;
    }
    let (option, _rest) = one_argument(arg);
    let Some(mode) = requested_shutdown_mode(&option) else {
        g.send_to_char(ch, "Unknown shutdown option.\r\n");
        return;
    };
    let Some(authorization) = crate::interpreter::authenticated_command_request(g, ch) else {
        g.send_to_char(ch, "Shutdown requires direct authenticated input.\r\n");
        return;
    };
    if g.shutdown_requested.is_some() {
        g.send_to_char(ch, "A shutdown request is already pending.\r\n");
        return;
    }
    g.shutdown_requested = Some(ShutdownRequest::Command {
        authorization,
        mode,
    });
    g.send_to_char(
        ch,
        "Shutdown request queued for authority revalidation.\r\n",
    );
}

/// Publish the visible/control-file effects only after the async game shell
/// has revalidated the queued session and shutdown grant.
pub(crate) fn publish_authorized_shutdown(g: &mut GameState, ch: CharId, mode: ShutdownMode) {
    let cname = name_of(g, ch);
    if mode == ShutdownMode::Shutdown {
        log_line(g, &format!("(GC) Shutdown by {}.", cname));
        send_to_all(g, "&m[&YINFO&m]&n Shutting down.\r\n");
    } else if mode == ShutdownMode::Reboot {
        log_line(g, &format!("(GC) Reboot by {}.", cname));
        send_to_all(
            g,
            "&m[&YINFO&m]&n Rebooting.. come back in a minute or two.\r\n",
        );
        // C act.wizard.c:1212: touch(FASTBOOT_FILE) - the autorun wrapper
        // distinguishes reboot/stop/pause by these control files (#211).
        write_control_file(g, ".fastboot");
    } else if mode == ShutdownMode::Now {
        log_line(g, &format!("(GC) Shutdown NOW by {}.", cname));
        send_to_all(
            g,
            "&m[&YINFO&m]&n Rebooting.. come back in a minute or two.\r\n",
        );
        write_control_file(g, ".fastboot");
    } else if mode == ShutdownMode::Die {
        log_line(g, &format!("(GC) Shutdown by {}.", cname));
        send_to_all(g, "&m[&YINFO&m]&n Shutting down for maintenance.\r\n");
        // C act.wizard.c:1230: touch(KILLSCRIPT_FILE).
        write_control_file(g, ".killscript");
    } else if mode == ShutdownMode::Pause {
        log_line(g, &format!("(GC) Shutdown by {}.", cname));
        send_to_all(g, "&m[&YINFO&m]&n Shutting down for maintenance.\r\n");
        // C act.wizard.c:1238: touch(PAUSE_FILE).
        write_control_file(g, "pause");
    }
}

/// log() — C basic_mud_log writes a timestamped line to the syslog file only
/// (no immortal echo). The shared facility we have always writes the file; we
/// route through it at NRM/LVL_IMMORT so the disk line is written (the only
/// difference from C `log()` is that the line also reaches online immortals
/// whose syslog level admits it — a superset of C's file-only behaviour).
fn log_line(g: &mut GameState, line: &str) {
    mudlog(g, line, NRM, LVL_IMMORT);
}

// ===========================================================================
// do_snoop / do_switch / do_return
// ===========================================================================
// Snoop links live on Character (snooping / snoop_by). When the snooped char
// receives output, state::send_to_char tees it to the snooper.

/// stop_snooping (act.wizard.c): break ch's outgoing snoop link, if any.
fn stop_snooping(g: &mut GameState, ch: CharId) {
    let target = g.get_char(ch).and_then(|c| c.snooping);
    match target {
        None => g.send_to_char(ch, "You aren't snooping anyone.\r\n"),
        Some(victim) => {
            g.send_to_char(ch, "You stop snooping.\r\n");
            if let Some(v) = g.get_char_mut(victim) {
                v.snoop_by = None;
            }
            if let Some(c) = g.get_char_mut(ch) {
                c.snooping = None;
            }
        }
    }
}

/// Resolve the authenticated principal behind either half of a switched
/// session. The active NPC has the connection's forward `character` link,
/// while the detached PC is reachable only through the reverse `original`
/// link, so checking `Character::desc` alone is insufficient.
///
/// Persisted `trust` is the command authority. Invalid trust or asymmetric /
/// duplicate descriptor aliases fail closed instead of falling back to the
/// low-level body. Descriptorless NPCs retain their ordinary C `GET_LEVEL`
/// hierarchy semantics.
fn target_principal_authority(
    g: &GameState,
    target: CharId,
) -> Option<crate::state::PrincipalAuthority> {
    g.principal_authority(target)
}

/// Administrative callers must resolve to a live authenticated player
/// principal. Descriptorless PCs, descriptor-controlled NPCs without an
/// original, malformed aliases, and invalid persisted trust all fail closed.
fn authenticated_player_authority(
    g: &GameState,
    target: CharId,
) -> Option<crate::state::PrincipalAuthority> {
    let authority = target_principal_authority(g, target)
        .filter(|principal| principal.is_authenticated_player())?;
    let principal = g.get_char(authority.principal)?;
    (!g.authority_quarantine.contains(&principal.idnum)).then_some(authority)
}

/// A named PC target must represent its own account, not somebody else's
/// switched-to body. A detached original remains its own principal and is a
/// legitimate target.
fn exact_player_authority(
    g: &GameState,
    target: CharId,
) -> Option<crate::state::PrincipalAuthority> {
    target_principal_authority(g, target)
        .filter(|principal| principal.principal_is_player && principal.principal == target)
}

pub fn do_snoop(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return;
    }
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        stop_snooping(g, ch);
        return;
    }
    let victim = match get_char_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No such person around.\r\n");
            return;
        }
    };
    if g.get_char(victim).and_then(|c| c.desc).is_none() {
        g.send_to_char(ch, "There's no link.. nothing to snoop.\r\n");
        return;
    }
    if victim == ch {
        stop_snooping(g, ch);
        return;
    }
    if g.get_char(victim).and_then(|c| c.snoop_by).is_some() {
        g.send_to_char(ch, "Busy already. \r\n");
        return;
    }
    // C: victim->desc->snooping == ch->desc — already snooping us back.
    if g.get_char(victim).and_then(|c| c.snooping) == Some(ch) {
        g.send_to_char(ch, "Don't be stupid.\r\n");
        return;
    }
    if crate::interpreter::authenticated_input_authority(g, ch).is_none()
        || !g.can_start_snoop(ch, victim)
    {
        g.send_to_char(ch, "You can't.\r\n");
        return;
    }
    g.send_to_char(ch, OK);

    // Drop any prior outgoing snoop, then wire ch -> victim.
    let prior = g.get_char(ch).and_then(|c| c.snooping);
    if let Some(p) = prior {
        if let Some(pc) = g.get_char_mut(p) {
            pc.snoop_by = None;
        }
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.snooping = Some(victim);
    }
    if let Some(v) = g.get_char_mut(victim) {
        v.snoop_by = Some(ch);
    }
}

pub fn do_switch(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let Some(caller_principal) = target_principal_authority(g, ch) else {
        g.send_to_char(ch, "You can't do that right now.\r\n");
        return;
    };
    let already = g
        .descriptors
        .get(&conn)
        .map(|d| d.original.is_some())
        .unwrap_or(false);
    let (name, _rest) = one_argument(arg);
    if already {
        g.send_to_char(ch, "You're already switched.\r\n");
        return;
    }
    if name.is_empty() {
        g.send_to_char(ch, "Switch with who?\r\n");
        return;
    }
    let victim = match get_char_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No such character.\r\n");
            return;
        }
    };
    if ch == victim {
        g.send_to_char(ch, "Hee hee... we are jolly funny today, eh?\r\n");
        return;
    }
    let Some(victim_principal) = target_principal_authority(g, victim) else {
        g.send_to_char(ch, "You can't do that, the body state is invalid.\r\n");
        return;
    };
    if victim_principal.descriptor_controls_target || victim_principal.switched_session {
        g.send_to_char(ch, "You can't do that, the body is already in use!\r\n");
        return;
    }
    if caller_principal.authority < i32::from(LVL_IMPL) && !is_npc(g, victim) {
        g.send_to_char(ch, "You aren't holy enough to use a person's body.\r\n");
        return;
    }
    g.send_to_char(ch, OK);
    // Re-point the descriptor: character := victim, original := ch.
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.character = Some(victim);
        d.original = Some(ch);
    }
    if let Some(v) = g.get_char_mut(victim) {
        v.desc = Some(conn);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.desc = None;
    }
}

pub fn do_return(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let original = g.descriptors.get(&conn).and_then(|d| d.original);
    let original = match original {
        Some(o) => o,
        None => return,
    };
    g.send_to_char(ch, "You return to your original body.\r\n");
    // C act.wizard.c:1346-1347: "if someone switched into your original body,
    // disconnect them". Their descriptor is marked Close, which the game loop
    // treats exactly like do_quit.
    let occupant = g.get_char(original).and_then(|c| c.desc);
    if let Some(occ) = occupant {
        if let Some(d) = g.descriptors.get_mut(&occ) {
            d.state = ConState::Close;
        }
        if let Some(o) = g.get_char_mut(original) {
            o.desc = None;
        }
    }
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.character = Some(original);
        d.original = None;
    }
    if let Some(o) = g.get_char_mut(original) {
        o.desc = Some(conn);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.desc = None;
    }
}

// ===========================================================================
// do_load / do_aload
// ===========================================================================
pub fn do_load(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (kind, numstr, _rest) = two_arguments(arg);
    if kind.is_empty()
        || numstr.is_empty()
        || !numstr
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        g.send_to_char(ch, "Usage: load { obj | mob } <number>\r\n");
        return;
    }
    let Some(number) = command_atoi(g, ch, &numstr) else {
        return;
    };
    if number < 0 {
        g.send_to_char(ch, "A NEGATIVE number??\r\n");
        return;
    }
    // impboard (immortal board) protection + per-zone builder permission.
    let Some(ch_authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You are not holy enough for that!\r\n");
        return;
    };
    let ch_trust = ch_authority.authority;
    if number == IMPBOARD && ch_trust < i32::from(LVL_GRGOD) {
        g.send_to_char(ch, "You are not holy enough for that!\r\n");
        return;
    }
    if !can_edit_zone(g, ch, real_zone(g, number)) && ch_trust < i32::from(LVL_GRGOD) {
        g.send_to_char(ch, "You do not have permission to load from this zone.\r\n");
        return;
    }

    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let cname = name_of(g, ch);

    if is_abbrev(&kind, "mob") {
        if !g.mob_protos.contains_key(&number) {
            g.send_to_char(ch, "There is no monster with that number.\r\n");
            return;
        }
        let mob = match g.load_mobile(number) {
            Some(m) => m,
            None => {
                g.send_to_char(ch, "There is no monster with that number.\r\n");
                return;
            }
        };
        g.char_to_room(mob, rnum);
        let short = g
            .get_char(mob)
            .and_then(|c| c.short_desc.clone())
            .unwrap_or_default();
        let line = format!("[WATCHDOG] {} loads mobile {}: {}", cname, number, short);
        mudlog(g, &line, CMP, LVL_IMPL);
        act(
            g,
            "$n makes a quaint, magical gesture with one hand.",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        act(
            g,
            "$n has created $N!",
            false,
            ch,
            None,
            ActArg::Char(mob),
            To::Room,
        );
        act(
            g,
            "You create $N.",
            false,
            ch,
            None,
            ActArg::Char(mob),
            To::Char,
        );
        crate::dg_triggers::load_mtrigger(g, mob);
    } else if is_abbrev(&kind, "obj") {
        if !g.obj_protos.contains_key(&number) {
            g.send_to_char(ch, "There is no object with that number.\r\n");
            return;
        }
        let obj = match g.load_object(number) {
            Some(o) => o,
            None => {
                g.send_to_char(ch, "There is no object with that number.\r\n");
                return;
            }
        };
        if ch_trust < i32::from(LVL_IMMORT) {
            if let Some(o) = g.get_obj_mut(obj) {
                o.extra_flags =
                    crate::object::ExtraFlags::from_bits_retain(o.extra_flags.bits() | ITEM_NORENT);
            }
        }
        g.obj_to_char(obj, ch);
        let short = g
            .get_obj(obj)
            .map(|o| o.short_description.clone())
            .unwrap_or_default();
        let line = format!("[WATCHDOG] {} loads object {}: {}", cname, number, short);
        mudlog(g, &line, CMP, LVL_IMPL);
        act(
            g,
            "$n makes a strange magical gesture.",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        act(
            g,
            "$n has created $p!",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Room,
        );
        act(
            g,
            "You create $p.",
            false,
            ch,
            Some(obj),
            ActArg::None,
            To::Char,
        );
        crate::dg_triggers::load_otrigger(g, obj);
    } else {
        g.send_to_char(ch, "That'll have to be either 'obj' or 'mob'.\r\n");
    }
}

pub fn do_aload(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let (kind, from_s, rest) = two_arguments(arg);
    let (to_s, _r) = one_argument(&rest);
    let Some(to) = command_atoi(g, ch, &to_s) else {
        return;
    };
    let Some(frm) = command_atoi(g, ch, &from_s) else {
        return;
    };
    if frm <= 0 {
        g.send_to_char(ch, "You're missing the starting item number\r\n");
        return;
    }
    if to <= 0 {
        g.send_to_char(ch, "You're missing the ending item number\r\n");
        return;
    }
    if !(is_abbrev(&kind, "mob") || is_abbrev(&kind, "obj")) {
        g.send_to_char(
            ch,
            "Usage: aload { obj | mob } <startnumber> <endnumber>\r\n",
        );
        return;
    }
    if frm > to {
        g.send_to_char(ch, "Start number cannot be greater than Ending number\r\n");
        return;
    }
    for j in frm..=to {
        let line = format!(" {} {} ", kind, j);
        do_load(g, ch, &line, subcmd);
    }
}

// ===========================================================================
// do_vstat
// ===========================================================================
pub fn do_vstat(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (kind, numstr, _rest) = two_arguments(arg);
    if kind.is_empty()
        || numstr.is_empty()
        || !numstr
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        g.send_to_char(ch, "Usage: vstat { obj | mob } <number>\r\n");
        return;
    }
    let Some(number) = command_atoi(g, ch, &numstr) else {
        return;
    };
    if number < 0 {
        g.send_to_char(ch, "A NEGATIVE number??\r\n");
        return;
    }
    // C act.wizard.c:1481-1484: the builder gate fires before real_mobile() /
    // real_object(), so an out-of-zone vnum is refused without the mobile ever
    // being instantiated into room 0.
    let authority = authenticated_player_authority(g, ch)
        .map(|principal| principal.authority)
        .unwrap_or(-1);
    if authority < i32::from(LVL_IMMORT) && !can_edit_zone(g, ch, real_zone(g, number)) {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }
    if is_abbrev(&kind, "mob") {
        if !g.mob_protos.contains_key(&number) {
            g.send_to_char(ch, "There is no monster with that number.\r\n");
            return;
        }
        let mob = match g.load_mobile(number) {
            Some(m) => m,
            None => return,
        };
        g.char_to_room(mob, 0);
        do_stat_character(g, ch, mob);
        g.extract_char(mob);
    } else if is_abbrev(&kind, "obj") {
        if !g.obj_protos.contains_key(&number) {
            g.send_to_char(ch, "There is no object with that number.\r\n");
            return;
        }
        let obj = match g.load_object(number) {
            Some(o) => o,
            None => return,
        };
        do_stat_object(g, ch, obj);
        g.extract_obj(obj);
    } else {
        g.send_to_char(ch, "That'll have to be either 'obj' or 'mob'.\r\n");
    }
}

// ===========================================================================
// do_purge
// ===========================================================================
pub fn do_purge(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, _rest) = one_argument(arg);
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let ch_authority = target_principal_authority(g, ch);
    let ch_trust = ch_authority
        .map(|principal| principal.authority)
        .unwrap_or(-1);

    if !name.is_empty() {
        if let Some(vict) = g.get_char_room_vis(ch, &name) {
            let victim_authority = target_principal_authority(g, vict);
            let vict_npc = is_npc(g, vict);
            if victim_authority.is_none()
                || victim_authority.is_some_and(|target| target.switched_session)
            {
                g.send_to_char(ch, "No, no, no!\r\n");
                return;
            }
            if !vict_npc {
                let Some(authority) = ch_authority.filter(|principal| {
                    principal.is_authenticated_player()
                        && principal.authority >= i32::from(LVL_IMMORT)
                }) else {
                    g.send_to_char(ch, "No, no, no!\r\n");
                    return;
                };
                if authority.authority
                    <= victim_authority
                        .map(|principal| principal.authority)
                        .unwrap_or(i32::MAX)
                {
                    g.send_to_char(ch, "Fuuuuuuuuu!\r\n");
                    return;
                }
                let cname = name_of(g, ch);
                let vname = name_of(g, vict);
                mudlog(
                    g,
                    &format!("(GC) {} has purged {}.", cname, vname),
                    BRF,
                    LVL_GOD,
                );
                // close the player's socket: drop the descriptor link.
                if let Some(conn) = g.get_char(vict).and_then(|c| c.desc) {
                    if let Some(d) = g.descriptors.get_mut(&conn) {
                        d.state = ConState::Close;
                        d.character = None;
                    }
                    if let Some(c) = g.get_char_mut(vict) {
                        c.desc = None;
                    }
                }
            } else {
                // NPC: must own the zone (or be GRGOD+), unless it has no proto.
                let mob_vnum = g.get_char(vict).map(|c| c.nr).unwrap_or(NOBODY);
                if !can_edit_zone(g, ch, real_zone(g, mob_vnum))
                    && ch_trust < i32::from(LVL_GRGOD)
                    && mob_vnum != NOBODY
                {
                    g.send_to_char(
                        ch,
                        "You do not have permission to purge from this zone.\r\n",
                    );
                    return;
                }
            }
            act(
                g,
                "$n disintegrates $N.",
                false,
                ch,
                None,
                ActArg::Char(vict),
                To::NotVict,
            );
            g.extract_char(vict);
            g.send_to_char(ch, OK);
        } else {
            let room_objs = g.rooms[rnum].contents.clone();
            if let Some(obj) = g.get_obj_in_list_vis(ch, &name, &room_objs) {
                let obj_vnum = g.get_obj(obj).map(|o| o.item_number).unwrap_or(NOTHING);
                if !can_edit_zone(g, ch, real_zone(g, obj_vnum))
                    && ch_trust < i32::from(LVL_GRGOD)
                    && obj_vnum != NOTHING
                {
                    g.send_to_char(
                        ch,
                        "You do not have permission to purge from this zone.\r\n",
                    );
                    return;
                }
                act(
                    g,
                    "$n destroys $p.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Room,
                );
                g.extract_obj(obj);
                g.send_to_char(ch, OK);
            } else {
                g.send_to_char(ch, "Nothing here by that name.\r\n");
            }
        }
    } else {
        // No argument: clean the whole room (mobs + ground objects). Each
        // extraction is gated on can_edit_zone (or GRGOD+, or no proto).
        act(
            g,
            "$n gestures... You are surrounded by scorching flames!",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
        g.send_to_room(rnum, "The world seems a little cleaner.\r\n", None);

        let people = g.rooms[rnum].people.clone();
        for vict in people {
            if !is_npc(g, vict) {
                continue;
            }
            let Some(target) = target_principal_authority(g, vict) else {
                continue;
            };
            if target.switched_session || target.descriptor_controls_target {
                continue;
            }
            let mob_vnum = g.get_char(vict).map(|c| c.nr).unwrap_or(NOBODY);
            if can_edit_zone(g, ch, real_zone(g, mob_vnum))
                || ch_trust >= i32::from(LVL_GRGOD)
                || mob_vnum == NOBODY
            {
                g.extract_char(vict);
            }
        }
        let objs = g.rooms[rnum].contents.clone();
        for obj in objs {
            let obj_vnum = g.get_obj(obj).map(|o| o.item_number).unwrap_or(NOTHING);
            if can_edit_zone(g, ch, real_zone(g, obj_vnum))
                || ch_trust >= i32::from(LVL_GRGOD)
                || obj_vnum == NOTHING
            {
                g.extract_obj(obj);
            }
        }
    }
}

// ===========================================================================
// do_syslog
// ===========================================================================
const LOGTYPES: &[&str] = &["off", "brief", "normal", "perfect", "complete", "\n"];

pub fn do_syslog(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    };
    let principal = authority.principal;
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        let prf = g.get_char(principal).map(|c| c.prf_flags).unwrap_or(0);
        let tp = (if prf & PRF_LOG1 != 0 { 1 } else { 0 })
            + (if prf & PRF_LOG2 != 0 { 2 } else { 0 })
            + (if prf & PRF_LOG3 != 0 { 4 } else { 0 });
        g.send_to_char(
            ch,
            &format!("Your syslog is currently {}.\r\n", LOGTYPES[tp as usize]),
        );
        return;
    }
    let tp = match search_block(&name, LOGTYPES) {
        Some(i) if LOGTYPES[i] != "\n" => i as i64,
        _ => {
            g.send_to_char(
                ch,
                "Usage: syslog { Off | Brief | Normal | Perfect | Complete }\r\n",
            );
            return;
        }
    };
    if let Some(c) = g.get_char_mut(principal) {
        c.prf_flags &= !(PRF_LOG1 | PRF_LOG2 | PRF_LOG3);
        c.prf_flags |= if tp & 1 != 0 { PRF_LOG1 } else { 0 };
        c.prf_flags |= if tp & 2 != 0 { PRF_LOG2 } else { 0 };
        c.prf_flags |= if tp & 4 != 0 { PRF_LOG3 } else { 0 };
    }
    g.send_to_char(
        ch,
        &format!("Your syslog is now {}.\r\n", LOGTYPES[tp as usize]),
    );
    g.request_player_save(principal);
}

// ===========================================================================
// do_advance
// ===========================================================================
pub fn do_advance(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, levelstr, _rest) = two_arguments(arg);
    if name.is_empty() {
        g.send_to_char(ch, "Advance who?\r\n");
        return;
    }
    let victim = match get_char_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "That player is not here.\r\n");
            return;
        }
    };
    if is_npc(g, victim) {
        g.send_to_char(ch, "NO!  Not on NPC's.\r\n");
        return;
    }
    let Some(authorization) = crate::interpreter::authenticated_command_request(g, ch) else {
        g.send_to_char(ch, "Maybe that's not such a good idea.\r\n");
        return;
    };
    let Some(requester) = target_principal_authority(g, ch).filter(|principal| {
        principal.is_authenticated_player()
            && principal.principal == authorization.requester_principal
    }) else {
        g.send_to_char(ch, "Maybe that's not such a good idea.\r\n");
        return;
    };
    let requester_has_advance = g
        .get_char(requester.principal)
        .is_some_and(|principal| principal.godcmds1 & GCMD_ADVANCE != 0);
    if !requester_has_advance {
        g.send_to_char(ch, "Maybe that's not such a good idea.\r\n");
        return;
    }
    let Some(target) = target_principal_authority(g, victim)
        .filter(|principal| principal.principal_is_player && principal.principal == victim)
    else {
        g.send_to_char(ch, "Maybe that's not such a good idea.\r\n");
        return;
    };
    if requester.authority <= target.authority {
        g.send_to_char(ch, "Maybe that's not such a good idea.\r\n");
        return;
    }
    let Some(newlevel) = command_atoi(g, ch, &levelstr) else {
        return;
    };
    if levelstr.is_empty() || newlevel <= 0 {
        g.send_to_char(ch, "That's not a level!\r\n");
        return;
    }
    if newlevel > LVL_IMPL as i32 {
        g.send_to_char(
            ch,
            &format!("{} is the highest possible level.\r\n", LVL_IMPL),
        );
        return;
    }
    if newlevel > requester.authority {
        g.send_to_char(ch, "Yeah, right.\r\n");
        return;
    }
    let Some(character) = g.get_char(victim) else {
        return;
    };
    let expected = crate::PlayerAuthorityState {
        level: character.player.level,
        trust: character.trust,
        exp: character.points.exp,
        godcmds1: character.godcmds1,
        godcmds2: character.godcmds2,
        godcmds3: character.godcmds3,
        godcmds4: character.godcmds4,
    };
    let (godcmds1, godcmds2, godcmds3, godcmds4) =
        crate::gcmd::canonical_advance_grants(newlevel as u8, LVL_IMMORT, LVL_IMPL);
    let replacement = crate::PlayerAuthorityState {
        level: newlevel as u8,
        trust: newlevel,
        exp: exp_to_level(newlevel - 1),
        godcmds1,
        godcmds2,
        godcmds3,
        godcmds4,
    };
    if expected.level == replacement.level
        && expected.trust == replacement.trust
        && expected.godcmds1 == replacement.godcmds1
        && expected.godcmds2 == replacement.godcmds2
        && expected.godcmds3 == replacement.godcmds3
        && expected.godcmds4 == replacement.godcmds4
    {
        g.send_to_char(ch, "They are already at that level.\r\n");
        return;
    }
    let request = crate::state::AuthorityUpdateRequest {
        authorization,
        victim,
        idnum: character.idnum,
        name: character.get_name().to_string(),
        expected,
        replacement,
    };
    g.queue_authority_update(request);
    g.send_to_char(
        ch,
        "Authority change queued; it will be announced after durable confirmation.\r\n",
    );
}

/// Apply and announce a rank transition only after the async shell has
/// confirmed the exact durable authority tuple.
pub(crate) fn complete_advance(g: &mut GameState, request: &crate::state::AuthorityUpdateRequest) {
    if !g.authenticated_command_request_is_current(
        request.authorization,
        i32::from(LVL_IMMORT),
        1,
        GCMD_ADVANCE,
    ) {
        return;
    }
    let oldlevel = request.expected.level;
    let newlevel = request.replacement.level;
    if newlevel < oldlevel {
        g.send_to_char(
            request.victim,
            "You are momentarily enveloped by darkness!\r\nYou feel somewhat diminished.\r\n",
        );
    } else {
        act(
            g,
            "$n makes some strange gestures.\r\n\r\nA strange feeling comes upon you, like a giant hand, light comes\r\ndown from above, grabbing your body, that begins to pulse with\r\ncolored lights from inside.\r\n\r\nYour head seems to be filled with demons from another plane\r\nas your body dissolves to the elements of time and space itself.\r\nSuddenly a silent explosion of light snaps you back to reality.\r\n\r\nYou feel slightly different.",
            false,
            request.authorization.requester_body,
            None,
            ActArg::Char(request.victim),
            To::Vict,
        );
    }

    if let Some(victim) = g.get_char_mut(request.victim) {
        victim.player.level = newlevel;
        victim.trust = request.replacement.trust;
        victim.points.exp = request.replacement.exp;
        victim.godcmds1 = request.replacement.godcmds1;
        victim.godcmds2 = request.replacement.godcmds2;
        victim.godcmds3 = request.replacement.godcmds3;
        victim.godcmds4 = request.replacement.godcmds4;
        victim.invis_level = victim.invis_level.min(request.replacement.trust.max(0));
        if request.replacement.trust < i32::from(LVL_IMMORT) {
            victim.prf_flags &= !(PRF_NOHASSLE | PRF_HOLYLIGHT | PRF_ROOMFLAGS);
        }
    }

    g.send_to_char(request.authorization.requester_body, OK);
    let requester_name = name_of(g, request.authorization.requester_principal);
    let victim_name = name_of(g, request.victim);
    log_line(
        g,
        &format!(
            "(GC) {} has advanced {} to level {} (from {})",
            requester_name, victim_name, newlevel, oldlevel
        ),
    );
    g.send_to_char(
        request.authorization.requester_body,
        &format!(
            "(GC) {} has advanced {} to level {}.",
            requester_name, victim_name, newlevel
        ),
    );
    crate::autowiz::check_autowiz(g, request.victim);
}

// ===========================================================================
// do_restore
// ===========================================================================
pub fn do_restore(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "Whom do you wish to restore?\r\n");
        return;
    }
    let vict = match get_char_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, NOPERSON);
            return;
        }
    };
    let Some(ch_authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You are not godly enough for that!\r\n");
        return;
    };
    let Some(vict_authority) = target_principal_authority(g, vict) else {
        g.send_to_char(ch, "You are not godly enough for that!\r\n");
        return;
    };
    if !is_npc(g, vict) && exact_player_authority(g, vict).is_none() {
        g.send_to_char(ch, "You are not godly enough for that!\r\n");
        return;
    }
    let vict_level = level_of(g, vict);
    if let Some(v) = g.get_char_mut(vict) {
        v.points.hit = v.points.max_hit;
        v.points.mana = v.points.max_mana;
        v.points.move_points = v.points.max_move;
        if vict_level < LVL_IMMORT {
            v.conditions[FULL] = 24;
            v.conditions[THIRST] = 24;
        } else {
            v.conditions[FULL] = -100;
            v.conditions[THIRST] = -100;
        }
    }
    if ch_authority.authority >= i32::from(LVL_GRGOD)
        && vict_authority.authority >= i32::from(LVL_IMMORT)
    {
        if let Some(v) = g.get_char_mut(vict) {
            for i in 1..=(MAX_SKILLS as u16) {
                v.set_skill(i, 100);
            }
            v.real_abils.str_add = 100;
            v.real_abils.intel = MAX_STAT;
            v.real_abils.wis = MAX_STAT;
            v.real_abils.dex = MAX_STAT;
            v.real_abils.str = MAX_STAT;
            v.real_abils.con = MAX_STAT;
            v.real_abils.cha = MAX_STAT;
            v.aff_abils = v.real_abils;
        }
    }
    update_pos(g, vict);
    g.send_to_char(ch, OK);
    act(
        g,
        "You have been fully healed by $N!",
        false,
        vict,
        None,
        ActArg::Char(ch),
        To::Char,
    );
    let cname = name_of(g, ch);
    let vname = name_of(g, vict);
    mudlog(
        g,
        &format!("(GC) {} restored by {}", vname, cname),
        BRF,
        LVL_GOD,
    );
}

/// update_pos (fight.c): recompute position from hit points.
fn update_pos(g: &mut GameState, victim: CharId) {
    if let Some(v) = g.get_char_mut(victim) {
        if v.points.hit > 0 && v.position == Position::Standing {
            return;
        }
        if v.points.hit > 0 {
            v.position = Position::Standing;
        } else if v.points.hit <= -11 {
            v.position = Position::Dead;
        } else if v.points.hit <= -6 {
            v.position = Position::MortallyWounded;
        } else if v.points.hit <= -3 {
            v.position = Position::Incapacitated;
        } else {
            v.position = Position::Stunned;
        }
    }
}

// ===========================================================================
// do_invis (+ perform_immort_vis / perform_immort_invis)
// ===========================================================================
pub fn do_invis(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if is_npc(g, ch) {
        g.send_to_char(ch, "You can't do that!\r\n");
        return;
    }
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You can't do that!\r\n");
        return;
    };
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        if invis_lev(g, ch) > 0 {
            perform_immort_vis(g, ch);
        } else {
            perform_immort_invis(g, ch, authority.authority);
        }
    } else {
        let Some(level) = command_atoi(g, ch, &name) else {
            return;
        };
        if level > authority.authority {
            g.send_to_char(ch, "You can't go invisible above your own level.\r\n");
        } else if level < 1 {
            perform_immort_vis(g, ch);
        } else {
            perform_immort_invis(g, ch, level);
        }
    }
}

fn perform_immort_vis(g: &mut GameState, ch: CharId) {
    let (invis, hidden_or_invis) = g
        .get_char(ch)
        .map(|c| {
            (
                c.invis_level,
                c.affect_flags & (AFF_HIDE | AFF_INVISIBLE) != 0,
            )
        })
        .unwrap_or((0, false));
    if invis == 0 && !hidden_or_invis {
        g.send_to_char(ch, "You are already fully visible.\r\n");
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.invis_level = 0;
        c.affect_flags &= !(AFF_HIDE | AFF_INVISIBLE);
    }
    // appear(): show to the room.
    act(
        g,
        "$n slowly fades into existence.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    g.send_to_char(ch, "You are now fully visible.\r\n");
}

fn perform_immort_invis(g: &mut GameState, ch: CharId, level: i32) {
    if is_npc(g, ch) {
        return;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let cur_invis = invis_lev(g, ch);
    let people = g.rooms[rnum].people.clone();
    for tch in people {
        if tch == ch {
            continue;
        }
        let tlvl = target_principal_authority(g, tch)
            .map(|principal| principal.authority)
            .unwrap_or(-1);
        if tlvl >= cur_invis && tlvl < level {
            act(
                g,
                "You blink and suddenly realize that $n is gone.",
                false,
                ch,
                None,
                ActArg::Char(tch),
                To::Vict,
            );
        }
        if tlvl < cur_invis && tlvl >= level {
            act(
                g,
                "You suddenly realize that $n is standing beside you.",
                false,
                ch,
                None,
                ActArg::Char(tch),
                To::Vict,
            );
        }
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.invis_level = level;
    }
    g.send_to_char(ch, &format!("Your invisibility level is {}.\r\n", level));
}

// ===========================================================================
// do_gecho
// ===========================================================================
pub fn do_gecho(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = delete_doubledollar(arg.trim_start());
    if argument.is_empty() {
        g.send_to_char(ch, "That must be a mistake...\r\n");
        return;
    }
    let body = format!("{}\r\n", argument);
    let players: Vec<CharId> = g.players_by_name.values().copied().collect();
    for pt in players {
        if pt != ch {
            g.send_to_char(pt, &body);
        }
    }
    if g.get_char(ch)
        .map(|c| c.prf_flags & PRF_NOREPEAT != 0)
        .unwrap_or(false)
    {
        g.send_to_char(ch, OK);
    } else {
        g.send_to_char(ch, &body);
    }
    let cname = name_of(g, ch);
    mudlog(
        g,
        &format!("(GC) gecho by {}: {}", cname, argument),
        NRM,
        LVL_IMPL,
    );
}

/// delete_doubledollar(): collapse "$$" -> "$" (CircleMUD).
fn delete_doubledollar(s: &str) -> String {
    s.replace("$$", "$")
}

// ===========================================================================
// do_gplague / do_gcureplague
// ===========================================================================
pub fn do_gplague(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let players: Vec<CharId> = g.players_by_name.values().copied().collect();
    for pt in players {
        if pt != ch
            && exact_player_authority(g, pt)
                .is_some_and(|target| target.authority < i32::from(LVL_HERO))
        {
            g.send_to_char(pt, "&RYou have contracted the plague!&n\r\n");
            if let Some(c) = g.get_char_mut(pt) {
                c.affect_flags |= AFF_PLAGUED;
            }
        }
    }
    let cname = name_of(g, ch);
    mudlog(g, &format!("(GC) gplague by {}", cname), NRM, LVL_IMPL);
}

pub fn do_gcureplague(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let players: Vec<CharId> = g.players_by_name.values().copied().collect();
    for pt in players {
        if pt != ch
            && exact_player_authority(g, pt)
                .is_some_and(|target| target.authority < i32::from(LVL_HERO))
        {
            g.send_to_char(pt, "You have been cured of the plague!\r\n");
            if let Some(c) = g.get_char_mut(pt) {
                c.affect_flags &= !AFF_PLAGUED;
            }
        }
    }
    let cname = name_of(g, ch);
    mudlog(g, &format!("(GC) gcureplague by {}", cname), NRM, LVL_IMPL);
}

// ===========================================================================
// do_poofset
// ===========================================================================
pub fn do_poofset(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let argument = arg.trim_start();
    let value = if argument.is_empty() {
        None
    } else {
        Some(argument.to_string())
    };
    if let Some(c) = g.get_char_mut(ch) {
        match subcmd {
            SCMD_POOFIN => c.poofin = value,
            SCMD_POOFOUT => c.poofout = value,
            _ => return,
        }
    }
    g.send_to_char(ch, OK);
}

// ===========================================================================
// do_dc
// ===========================================================================
pub fn do_dc(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (numstr, _rest) = one_argument(arg);
    let Some(num_to_dc) = command_atoi(g, ch, &numstr) else {
        return;
    };
    if num_to_dc == 0 {
        g.send_to_char(ch, "Usage: DC <user number> (type USERS for a list)\r\n");
        return;
    }
    // Find the descriptor whose desc_num == num_to_dc.
    let conn = g
        .descriptors
        .values()
        .find(|d| d.id.0 == num_to_dc as u64)
        .map(|d| d.id);
    let conn = match conn {
        Some(c) => c,
        None => {
            g.send_to_char(ch, "No such connection.\r\n");
            return;
        }
    };
    let target = g
        .descriptors
        .get(&conn)
        .and_then(|d| d.character.or(d.original));
    if let Some(tch) = target {
        let authority = target_principal_authority(g, ch).map(|target| target.authority);
        let target_authority = target_principal_authority(g, tch).map(|target| target.authority);
        if authority.is_none() || target_authority.is_none() || target_authority >= authority {
            if !g.can_see(ch, tch) {
                g.send_to_char(ch, "No such connection.\r\n");
            } else {
                g.send_to_char(ch, "Umm.. maybe that's not such a good idea...\r\n");
            }
            return;
        }
    }
    // Mark the descriptor for closing (C sets d->close_me; the game loop here
    // observes ConState::Close like do_quit).
    if let Some(d) = g.descriptors.get_mut(&conn) {
        d.state = ConState::Close;
    }
    g.send_to_char(ch, &format!("Connection #{} closed.\r\n", num_to_dc));
    let cname = name_of(g, ch);
    log_line(g, &format!("(GC) Connection closed by {}.", cname));
}

// ===========================================================================
// do_wizlock
// ===========================================================================
// circle_restrict is a boot-loop global not in the contract; we keep a local
// thread-unsafe static mirror so the report path is faithful within a run.
// (Documented gap: the value is not persisted across reboot and not consulted
// at the login gate yet.)
use std::sync::atomic::{AtomicI32, Ordering};
static CIRCLE_RESTRICT: AtomicI32 = AtomicI32::new(0);

/// circle_restrict for the nanny login gates (C's boot-loop global;
/// interpreter.c:1826 refuses new characters, :1981 refuses
/// level < circle_restrict) (#202).
pub fn circle_restrict() -> i32 {
    CIRCLE_RESTRICT.load(Ordering::Relaxed)
}

pub fn do_wizlock(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Invalid wizlock value.\r\n");
        return;
    };
    let (name, _rest) = one_argument(arg);
    let when: &str;
    if !name.is_empty() {
        let Some(value) = command_atoi(g, ch, &name) else {
            return;
        };
        if value < 0 || value > authority.authority {
            g.send_to_char(ch, "Invalid wizlock value.\r\n");
            return;
        }
        CIRCLE_RESTRICT.store(value, Ordering::Relaxed);
        when = "now";
    } else {
        when = "currently";
    }
    let restrict = CIRCLE_RESTRICT.load(Ordering::Relaxed);
    let line = match restrict {
        0 => format!("The game is {} completely open.\r\n", when),
        1 => format!("The game is {} closed to new players.\r\n", when),
        _ => format!(
            "Only level {} and above may enter the game {}.\r\n",
            restrict, when
        ),
    };
    g.send_to_char(ch, &line);
}

// ===========================================================================
// do_date / do_uptime
// ===========================================================================
pub fn do_date(g: &mut GameState, ch: CharId, _arg: &str, subcmd: i32) {
    // The C command formats asctime(localtime(time(0)/boot_time)). We have the
    // wall clock via chrono; boot_time is not surfaced, so uptime is derived
    // from g.pulse (10 pulses/sec => seconds = pulse/10).
    let now = chrono::Local::now();
    let tmstr = now.format("%a %b %e %H:%M:%S %Y").to_string();
    if subcmd == SCMD_DATE {
        g.send_to_char(
            ch,
            &format!(
                "Current machine time: {}\r\nTo see the time in Deltanian format, type: time\r\n",
                tmstr
            ),
        );
    } else {
        let secs = (g.pulse / PASSES_PER_SEC) as i64;
        let d = secs / 86400;
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        g.send_to_char(
            ch,
            &format!(
                "Up since {}: {} day{}, {}:{:02}\r\n",
                tmstr,
                d,
                if d == 1 { "" } else { "s" },
                h,
                m
            ),
        );
    }
}

// ===========================================================================
// do_last
// ===========================================================================
pub fn do_last(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let Some(requester) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You are not sufficiently godly for that!\r\n");
        return;
    };
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "For whom do you wish to search?\r\n");
        return;
    }
    // C reads the player_main row from MySQL (pe_printf for idnum/level/class/
    // name/host/last_logon). The full row — notably `class` and the formatted
    // last_logon time — is only available synchronously for an ONLINE target;
    // for an OFFLINE target we render from the boot-loaded player_table index
    // (idnum/level/name/last_logon/host). Class is absent from the index (it is
    // not one of the index columns), so the offline line shows "---" for the
    // class abbreviation. (A full offline class/host-of-record render would
    // need an async player_main load — out of scope for the name<->id index.)
    let target = g.find_player_by_name(&name);
    match target {
        Some(p) => {
            let Some(target_authority) = exact_player_authority(g, p) else {
                g.send_to_char(ch, "You are not sufficiently godly for that!\r\n");
                return;
            };
            let plvl = level_of(g, p);
            if target_authority.authority > requester.authority {
                g.send_to_char(ch, "You are not sufficiently godly for that!\r\n");
                return;
            }
            let (idnum, class, pname) = g
                .get_char(p)
                .map(|c| (c.idnum, c.player.class, c.player.name.clone()))
                .unwrap_or((-1, Class::Warrior, String::new()));
            let cls = class_abbrev(class);
            let host = target_authority
                .descriptor
                .and_then(|conn| g.descriptors.get(&conn).map(|d| d.host.clone()))
                .unwrap_or_default();
            g.send_to_char(
                ch,
                &format!(
                    "[{:5}] [{:2} {}] {:<12} : {:<18} : {:<20}\r\n",
                    idnum, plvl, cls, pname, host, "online now"
                ),
            );
        }
        None => {
            // Offline: pull the index row (idnum/level/name/last_logon/host).
            let row = g.player_index(&name).cloned();
            match row {
                Some(p) => {
                    if !g.can_inspect_player_authority(requester.principal, p.trust) {
                        g.send_to_char(ch, "You are not sufficiently godly for that!\r\n");
                        return;
                    }
                    let when = ctime(p.last_logon);
                    g.send_to_char(
                        ch,
                        &format!(
                            "[{:5}] [{:2} {}] {:<12} : {:<18} : {:<20}\r\n",
                            p.idnum, p.level, "---", p.name, p.host, when
                        ),
                    );
                }
                None => g.send_to_char(ch, "There is no such player.\r\n"),
            }
        }
    }
}

/// ctime()-style rendering of a unix timestamp for the `last` line (C uses the
/// libc ctime(&last_logon), e.g. "Mon Jun 15 13:04:22 2026"). chrono's
/// "%a %b %e %T %Y" matches that fixed-width format.
fn ctime(unix: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|t| t.format("%a %b %e %T %Y").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn mud_age_parts(birth: i64) -> (i64, i64, i64, i64) {
    const SECS_PER_MUD_HOUR: i64 = 75;
    const SECS_PER_MUD_DAY: i64 = 24 * SECS_PER_MUD_HOUR;
    const SECS_PER_MUD_MONTH: i64 = 35 * SECS_PER_MUD_DAY;
    const SECS_PER_MUD_YEAR: i64 = 17 * SECS_PER_MUD_MONTH;

    let mut total = (chrono::Utc::now().timestamp() - birth).max(0);
    let hours = (total / SECS_PER_MUD_HOUR) % 24;
    total -= SECS_PER_MUD_HOUR * hours;
    let days = (total / SECS_PER_MUD_DAY) % 35;
    total -= SECS_PER_MUD_DAY * days;
    let months = (total / SECS_PER_MUD_MONTH) % 17;
    total -= SECS_PER_MUD_MONTH * months;
    let years = total / SECS_PER_MUD_YEAR + 17;
    (years, months, days, hours)
}

/// class_abbrevs[] (class.c) — 3-letter PC class codes.
fn class_abbrev(class: Class) -> &'static str {
    match class {
        Class::MagicUser => "Mag",
        Class::Cleric => "Cle",
        Class::Thief => "Thi",
        Class::Warrior => "War",
        Class::Artisan => "Art",
    }
}

// ===========================================================================
// do_force
// ===========================================================================
pub fn do_force(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (who, to_force) = half_chop(arg);
    let cmd_msg = format!("$n has forced you to '{}'.", to_force);

    if who.is_empty() || to_force.is_empty() {
        g.send_to_char(ch, "Whom do you wish to force do what?\r\n");
        return;
    }
    let Some(ch_authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "No, no, no!\r\n");
        return;
    };

    if ch_authority.authority < i32::from(LVL_GRGOD)
        || (!who.eq_ignore_ascii_case("all") && !who.eq_ignore_ascii_case("room"))
    {
        let vict = match get_char_vis(g, ch, &who) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, NOPERSON);
                return;
            }
        };
        let Some(victim_authority) = target_principal_authority(g, vict) else {
            g.send_to_char(ch, "No, no, no!\r\n");
            return;
        };
        if ch_authority.authority <= victim_authority.authority {
            g.send_to_char(ch, "No, no, no!\r\n");
            return;
        }
        g.send_to_char(ch, OK);
        act(g, &cmd_msg, true, ch, None, ActArg::Char(vict), To::Vict);
        let cname = name_of(g, ch_authority.principal);
        let vname = name_of(g, vict);
        let lvl = LVL_GOD.max(invis_lev(g, ch) as u8);
        mudlog(
            g,
            &format!("(GC) {} forced {} to {}", cname, vname, to_force),
            NRM,
            lvl,
        );
        command_interpreter(g, vict, &to_force);
    } else if who.eq_ignore_ascii_case("room") {
        g.send_to_char(ch, OK);
        let cname = name_of(g, ch_authority.principal);
        let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let rvnum = g.room(rnum).number;
        let lvl = LVL_GOD.max(invis_lev(g, ch) as u8);
        mudlog(
            g,
            &format!("(GC) {} forced room {} to {}", cname, rvnum, to_force),
            NRM,
            lvl,
        );
        let people = g.rooms[rnum].people.clone();
        for vict in people {
            let Some(victim_authority) = target_principal_authority(g, vict) else {
                continue;
            };
            if victim_authority.authority >= ch_authority.authority {
                continue;
            }
            act(g, &cmd_msg, true, ch, None, ActArg::Char(vict), To::Vict);
            command_interpreter(g, vict, &to_force);
        }
    } else {
        // force all
        g.send_to_char(ch, OK);
        let cname = name_of(g, ch_authority.principal);
        let lvl = LVL_GOD.max(invis_lev(g, ch) as u8);
        mudlog(
            g,
            &format!("(GC) {} forced all to {}", cname, to_force),
            NRM,
            lvl,
        );
        let targets: Vec<CharId> = g
            .descriptors
            .values()
            .filter(|descriptor| descriptor.state == ConState::Playing)
            .filter_map(|descriptor| descriptor.character)
            .collect();
        for vict in targets {
            let Some(victim_authority) = target_principal_authority(g, vict) else {
                continue;
            };
            if victim_authority.authority >= ch_authority.authority {
                continue;
            }
            act(g, &cmd_msg, true, ch, None, ActArg::Char(vict), To::Vict);
            command_interpreter(g, vict, &to_force);
        }
    }
}

// ===========================================================================
// do_wiznet
// ===========================================================================
/// Resolve each playing descriptor to its authenticated player principal and
/// active delivery body. Preferences and authority belong to the principal;
/// writing state and output belong to the body currently controlled by that
/// descriptor.
fn wiznet_participants(g: &GameState) -> Vec<(CharId, CharId, crate::state::PrincipalAuthority)> {
    let mut participants = Vec::new();
    let mut seen_descriptors = Vec::new();
    for &principal in g.players_by_name.values() {
        let Some(authority) = exact_player_authority(g, principal)
            .filter(|authority| authority.is_authenticated_player())
        else {
            continue;
        };
        let Some(conn) = authority.descriptor else {
            continue;
        };
        if seen_descriptors.contains(&conn) {
            continue;
        }
        let Some(descriptor) = g
            .descriptors
            .get(&conn)
            .filter(|descriptor| descriptor.state == ConState::Playing)
        else {
            continue;
        };
        let Some(principal_character) = g.get_char(principal) else {
            continue;
        };
        if g.authority_quarantine.contains(&principal_character.idnum) {
            continue;
        }
        let Some(body) = descriptor.character else {
            continue;
        };
        seen_descriptors.push(conn);
        participants.push((principal, body, authority));
    }
    participants
}

pub fn do_wiznet(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let Some(sender) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    };
    let sender_principal = sender.principal;
    let mut argument = delete_doubledollar(arg.trim_start());
    let mut emote = false;
    let mut level = LVL_IMMORT as i32;

    if argument.is_empty() {
        g.send_to_char(
            ch,
            "Usage: wiznet <text> | #<level> <text> | *<emotetext> |\r\n        wiznet @<level> *<emotetext> | wiz @\r\n",
        );
        return;
    }

    let first = argument.chars().next().unwrap();
    match first {
        '*' | '#' => {
            if first == '*' {
                emote = true;
            }
            let after = &argument[1..];
            let (firstw, _r) = one_argument(after);
            if is_number(&firstw) {
                let (numw, rest) = half_chop(after);
                let Some(parsed_level) = command_atoi(g, ch, &numw) else {
                    return;
                };
                level = parsed_level.max(LVL_IMMORT as i32);
                if level > sender.authority {
                    g.send_to_char(ch, "You can't wizline above your own level.\r\n");
                    return;
                }
                argument = rest;
            } else if emote {
                argument = after.to_string();
            }
        }
        '@' => {
            // List gods online / offline.
            let players = wiznet_participants(g);
            let ch_is_impl = sender.authority >= i32::from(LVL_IMPL);
            let mut out = String::new();
            let mut any = false;
            for &(principal, body, authority) in &players {
                let online_god = authority.authority >= i32::from(LVL_IMMORT)
                    && g.get_char(principal)
                        .map(|c| c.prf_flags & PRF_NOWIZ == 0)
                        .unwrap_or(false)
                    && (g.can_see(ch, body) || ch_is_impl);
                if online_god {
                    if !any {
                        out.push_str("Gods online:\r\n");
                        any = true;
                    }
                    let nm = name_of(g, principal);
                    let writing = g
                        .get_char(body)
                        .map(|c| c.act_flags & PLR_WRITING != 0)
                        .unwrap_or(false);
                    let mailing = g
                        .get_char(body)
                        .map(|c| c.act_flags & PLR_MAILING != 0)
                        .unwrap_or(false);
                    if writing {
                        out.push_str(&format!("  {} (Writing)\r\n", nm));
                    } else if mailing {
                        out.push_str(&format!("  {} (Writing mail)\r\n", nm));
                    } else {
                        out.push_str(&format!("  {}\r\n", nm));
                    }
                }
            }
            let mut any2 = false;
            for &(principal, body, authority) in &players {
                let offline_god = authority.authority >= i32::from(LVL_IMMORT)
                    && g.get_char(principal)
                        .map(|c| c.prf_flags & PRF_NOWIZ != 0)
                        .unwrap_or(false)
                    && (g.can_see(ch, body) || ch_is_impl);
                if offline_god {
                    if !any2 {
                        out.push_str("Gods offline:\r\n");
                        any2 = true;
                    }
                    out.push_str(&format!("  {}\r\n", name_of(g, principal)));
                }
            }
            g.send_to_char(ch, &out);
            return;
        }
        '\\' => {
            argument = argument[1..].to_string();
        }
        _ => {}
    }

    if g.get_char(sender_principal)
        .map(|c| c.prf_flags & PRF_NOWIZ != 0)
        .unwrap_or(false)
    {
        g.send_to_char(ch, "You are offline!\r\n");
        return;
    }
    let argument = argument.trim_start().to_string();
    if argument.is_empty() {
        g.send_to_char(ch, "Don't bother the gods like that!\r\n");
        return;
    }

    let cname = name_of(g, sender_principal);
    let (buf1, buf2);
    if level > LVL_IMMORT as i32 {
        buf1 = format!(
            "{}: <{}> {}{}\r\n",
            cname,
            level,
            if emote { "<--- " } else { "" },
            argument
        );
        buf2 = format!(
            "Someone: <{}> {}{}\r\n",
            level,
            if emote { "<--- " } else { "" },
            argument
        );
    } else {
        buf1 = format!(
            "{}: {}{}\r\n",
            cname,
            if emote { "<--- " } else { "" },
            argument
        );
        buf2 = format!(
            "Someone: {}{}\r\n",
            if emote { "<--- " } else { "" },
            argument
        );
    }

    let ch_norepeat = g
        .get_char(sender_principal)
        .map(|c| c.prf_flags & PRF_NOREPEAT != 0)
        .unwrap_or(false);
    let players = wiznet_participants(g);
    for (principal, body, authority) in players {
        let recv = authority.authority >= level
            && g.get_char(principal)
                .map(|c| c.prf_flags & PRF_NOWIZ == 0)
                .unwrap_or(false)
            && g.get_char(body)
                .map(|c| c.act_flags & (PLR_WRITING | PLR_MAILING) == 0)
                .unwrap_or(false);
        let is_self_norepeat = authority.descriptor == sender.descriptor && ch_norepeat;
        if recv && !is_self_norepeat {
            g.send_to_char(body, "&c");
            if g.can_see(body, ch) {
                g.send_to_char(body, &buf1);
            } else {
                g.send_to_char(body, &buf2);
            }
            g.send_to_char(body, "&n");
        }
    }
    if ch_norepeat {
        g.send_to_char(ch, OK);
    }
}

// ===========================================================================
// do_zreset
// ===========================================================================
pub fn do_zreset(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You are not holy enough to do that!\r\n");
        return;
    };
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "You must specify a zone.\r\n");
        return;
    }
    let cname = name_of(g, ch);
    if name.starts_with('*') {
        if authority.authority < i32::from(LVL_GRGOD) {
            g.send_to_char(ch, "You are not holy enough to do that!\r\n");
            return;
        }
        let zone_numbers: Vec<i32> = g.zones.iter().map(|z| z.number).collect();
        for zn in zone_numbers {
            g.reset_zone(zn);
        }
        g.send_to_char(ch, "Reset world.\r\n");
        let lvl = LVL_GRGOD.max(invis_lev(g, ch) as u8);
        mudlog(g, &format!("(GC) {} reset entire world.", cname), NRM, lvl);
        return;
    }

    let zone_idx: Option<usize> = if name.starts_with('.') {
        g.get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.rooms[r].zone as usize)
    } else {
        let Some(j) = command_atoi(g, ch, &name) else {
            return;
        };
        g.zones.iter().position(|z| z.number == j)
    };

    match zone_idx {
        Some(i) if i < g.zones.len() => {
            // Builders may only reset a zone they own (or be GRGOD+).
            if !can_edit_zone(g, ch, i as i32) && authority.authority < i32::from(LVL_GRGOD) {
                g.send_to_char(ch, "You do not have permission to reset this zone.\r\n");
                return;
            }
            let znum = g.zones[i].number;
            let zname = g.zones[i].name.clone();
            g.reset_zone(znum);
            g.send_to_char(ch, &format!("Reset zone {} (#{}): {}.\r\n", i, znum, zname));
            let lvl = LVL_GRGOD.max(invis_lev(g, ch) as u8);
            mudlog(
                g,
                &format!("(GC) {} reset zone {} ({})", cname, i, zname),
                NRM,
                lvl,
            );
        }
        _ => g.send_to_char(ch, "Invalid zone number.\r\n"),
    }
}

// ===========================================================================
// do_wizutil (reroll/pardon/notitle/squelch/freeze/thaw/unaffect)
// ===========================================================================
pub fn do_wizutil(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let (name, _rest) = one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "Yes, but for whom?!?\r\n");
        return;
    }
    let vict = match get_char_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "There is no such player.\r\n");
            return;
        }
    };
    if is_npc(g, vict) {
        g.send_to_char(ch, "You can't do that to a mob!\r\n");
        return;
    }
    let (Some(ch_authority), Some(vict_authority)) = (
        authenticated_player_authority(g, ch),
        exact_player_authority(g, vict),
    ) else {
        g.send_to_char(ch, "Hmmm...you'd better not.\r\n");
        return;
    };
    if vict_authority.authority > ch_authority.authority {
        g.send_to_char(ch, "Hmmm...you'd better not.\r\n");
        return;
    }

    let cname = name_of(g, ch_authority.principal);
    let vname = name_of(g, vict);
    let logmin = |g: &GameState| LVL_GOD.max(invis_lev(g, ch) as u8);

    match subcmd {
        SCMD_REROLL => {
            g.send_to_char(ch, "Rerolled...\r\n");
            crate::class::roll_real_abils(g, vict);
            log_line(g, &format!("(GC) {} has rerolled {}.", cname, vname));
            let a = g.get_char(vict).map(|c| c.real_abils).unwrap_or_default();
            g.send_to_char(
                ch,
                &format!(
                    "New stats: Str {}/{}, Int {}, Wis {}, Dex {}, Con {}, Cha {}\r\n",
                    a.str, a.str_add, a.intel, a.wis, a.dex, a.con, a.cha
                ),
            );
        }
        SCMD_PARDON => {
            let flagged = g
                .get_char(vict)
                .map(|c| c.act_flags & (PLR_THIEF | PLR_KILLER) != 0)
                .unwrap_or(false);
            if !flagged {
                g.send_to_char(ch, "Your victim is not flagged.\r\n");
                return;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.act_flags &= !(PLR_THIEF | PLR_KILLER);
            }
            g.send_to_char(ch, "Pardoned.\r\n");
            g.send_to_char(vict, "You have been pardoned by the gods!\r\n");
            let m = logmin(g);
            mudlog(g, &format!("(GC) {} pardoned by {}", vname, cname), BRF, m);
        }
        SCMD_NOTITLE => {
            let result = plr_tog_chk(g, vict, PLR_NOTITLE);
            let m = logmin(g);
            mudlog(
                g,
                &format!("(GC) Notitle {} for {} by {}.", onoff(result), vname, cname),
                NRM,
                m,
            );
            g.send_to_char(
                ch,
                &format!(
                    "(GC) Notitle {} for {} by {}.\r\n",
                    onoff(result),
                    vname,
                    cname
                ),
            );
        }
        SCMD_SQUELCH => {
            let result = plr_tog_chk(g, vict, PLR_NOSHOUT);
            let m = logmin(g);
            mudlog(
                g,
                &format!("(GC) Squelch {} for {} by {}.", onoff(result), vname, cname),
                BRF,
                m,
            );
            g.send_to_char(
                ch,
                &format!(
                    "(GC) Squelch {} for {} by {}.\r\n",
                    onoff(result),
                    vname,
                    cname
                ),
            );
        }
        SCMD_FREEZE => {
            if ch_authority.principal == vict_authority.principal {
                g.send_to_char(ch, "Oh, yeah, THAT'S real smart...\r\n");
                return;
            }
            if g.get_char(vict)
                .map(|c| c.act_flags & PLR_FROZEN != 0)
                .unwrap_or(false)
            {
                g.send_to_char(ch, "Your victim is already pretty cold.\r\n");
                return;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.act_flags |= PLR_FROZEN;
                v.freeze_level = ch_authority.authority as u8;
            }
            g.send_to_char(vict, "A bitter wind suddenly rises and drains every erg of heat from your body!\r\nYou feel frozen!\r\n");
            g.send_to_char(ch, "Frozen.\r\n");
            act(
                g,
                "A sudden cold wind conjured from nowhere freezes $n!",
                false,
                vict,
                None,
                ActArg::None,
                To::Room,
            );
            let m = logmin(g);
            mudlog(g, &format!("(GC) {} frozen by {}.", vname, cname), BRF, m);
            // A frozen immortal is filtered out of the autowiz roster (PLR_FROZEN);
            // regenerate so the list drops them. No-op for mortals (level gate).
            crate::autowiz::check_autowiz(g, vict);
        }
        SCMD_THAW => {
            if !g
                .get_char(vict)
                .map(|c| c.act_flags & PLR_FROZEN != 0)
                .unwrap_or(false)
            {
                g.send_to_char(
                    ch,
                    "Sorry, your victim is not morbidly encased in ice at the moment.\r\n",
                );
                return;
            }
            let freeze_lev = g.get_char(vict).map(|c| c.freeze_level).unwrap_or(0);
            if i32::from(freeze_lev) > ch_authority.authority {
                let hmhr = match g
                    .get_char(vict)
                    .map(|c| c.player.sex)
                    .unwrap_or(Gender::Neutral)
                {
                    Gender::Male => "him",
                    Gender::Female => "her",
                    Gender::Neutral => "it",
                };
                g.send_to_char(
                    ch,
                    &format!(
                        "Sorry, a level {} God froze {}... you can't unfreeze {}.\r\n",
                        freeze_lev, vname, hmhr
                    ),
                );
                return;
            }
            let m = logmin(g);
            mudlog(
                g,
                &format!("(GC) {} un-frozen by {}.", vname, cname),
                BRF,
                m,
            );
            if let Some(v) = g.get_char_mut(vict) {
                v.act_flags &= !PLR_FROZEN;
            }
            g.send_to_char(vict, "A fireball suddenly explodes in front of you, melting the ice!\r\nYou feel thawed.\r\n");
            g.send_to_char(ch, "Thawed.\r\n");
            act(
                g,
                "A sudden fireball conjured from nowhere thaws $n!",
                false,
                vict,
                None,
                ActArg::None,
                To::Room,
            );
            // Thawing an immortal restores them to the autowiz roster; regenerate.
            crate::autowiz::check_autowiz(g, vict);
        }
        SCMD_UNAFFECT => {
            let had = g
                .get_char(vict)
                .map(|c| !c.affected.is_empty())
                .unwrap_or(false);
            if had {
                if let Some(v) = g.get_char_mut(vict) {
                    v.affected.clear();
                }
                g.affect_total(vict);
                g.send_to_char(
                    vict,
                    "There is a brief flash of light!\r\nYou feel slightly different.\r\n",
                );
                g.send_to_char(ch, "All spells removed.\r\n");
            } else {
                g.send_to_char(ch, "Your victim does not have any affections!\r\n");
                return;
            }
        }
        _ => {}
    }
    // save_char(NOWHERE): no player-file layer yet (documented gap).
}

/// PLR_TOG_CHK(): toggle a player flag, return its new state.
fn plr_tog_chk(g: &mut GameState, id: CharId, flag: i64) -> bool {
    if let Some(c) = g.get_char_mut(id) {
        c.act_flags ^= flag;
        c.act_flags & flag != 0
    } else {
        false
    }
}

/// roll_real_abils (class.c): re-roll a PC's six stats. The exact class-weighted
/// distribution lives in class.c (not surfaced); use the same 3d6-style spread
/// CircleMUD's default roll_real_abils produces so the values are sane.

// ===========================================================================
// do_show
// ===========================================================================
fn print_zone_to_buf(buf: &mut String, z: &crate::world::Zone) {
    buf.push_str(&format!(
        "{:3} {:<30.30} Age: {:3}; Reset: {:3} ({:1}); Top: {:5}\r\n",
        z.number, z.name, z.age, z.lifespan, z.reset_mode, z.top
    ));
}

pub fn do_show(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // (cmd, min_level) table — order is load-bearing for the `case l` switch.
    let fields: [(&str, u8); 10] = [
        ("nothing", 0),
        ("zones", LVL_IMMORT),
        ("player", LVL_GOD),
        ("rent", LVL_GOD),
        ("stats", LVL_IMMORT),
        ("errors", LVL_IMPL),
        ("death", LVL_GOD),
        ("godrooms", LVL_GOD),
        ("shops", LVL_IMMORT),
        ("houses", LVL_GOD),
    ];

    let argument = arg.trim_start();
    let ch_trust = target_principal_authority(g, ch)
        .map(|principal| principal.authority)
        .unwrap_or(-1);

    if argument.is_empty() {
        let mut buf = String::from("Show options:\r\n");
        let mut j = 0;
        for (i, (cmd, lvl)) in fields.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if i32::from(*lvl) <= ch_trust {
                j += 1;
                buf.push_str(&format!(
                    "{:<15}{}",
                    cmd,
                    if j % 5 == 0 { "\r\n" } else { "" }
                ));
            }
        }
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
        return;
    }

    let (field, value, _rest) = two_arguments(argument);
    // Find first prefix match.
    let mut l = fields.len(); // sentinel = "not found" -> default arm
    for (i, (cmd, _)) in fields.iter().enumerate() {
        if cmd.starts_with(&field.to_lowercase()) {
            l = i;
            break;
        }
    }

    let needed = if l < fields.len() { fields[l].1 } else { 0 };
    if ch_trust < i32::from(needed) {
        g.send_to_char(ch, "You are not godly enough for that!\r\n");
        return;
    }
    let self_flag = value == ".";

    match l {
        1 => {
            // zone
            let mut buf = String::new();
            if self_flag {
                let zidx = g
                    .get_char(ch)
                    .and_then(|c| c.in_room)
                    .map(|r| g.rooms[r].zone as usize);
                if let Some(z) = zidx.and_then(|i| g.zones.get(i)).cloned() {
                    print_zone_to_buf(&mut buf, &z);
                }
            } else if !value.is_empty() && is_number(&value) {
                let Some(j) = command_atoi(g, ch, &value) else {
                    return;
                };
                match g.zones.iter().find(|z| z.number == j).cloned() {
                    Some(z) => print_zone_to_buf(&mut buf, &z),
                    None => {
                        g.send_to_char(ch, "That is not a valid zone.\r\n");
                        return;
                    }
                }
            } else {
                let zones: Vec<crate::world::Zone> = g.zones.clone();
                for z in &zones {
                    print_zone_to_buf(&mut buf, z);
                }
            }
            g.send_to_char(ch, &buf);
        }
        2 => {
            // show player: C retrieve_player_entry loads the offline row. The
            // online character is printed in full; for an OFFLINE name the read
            // is the async DB query (database::load_player), unreachable from
            // this sync path, so we defer through the async bridge — game.rs
            // loads the player into the world, replays `show player <name>`
            // (now resolving via find_player_by_name below), prints the record,
            // then extracts.
            if value.is_empty() {
                g.send_to_char(ch, "A name would help.\r\n");
                return;
            }
            let online = g.find_player_by_name(&value);
            let target_trust = match online {
                Some(target) => match exact_player_authority(g, target) {
                    Some(authority) => Some(authority.authority),
                    None => {
                        g.send_to_char(ch, PLAYER_INSPECTION_DENIED);
                        return;
                    }
                },
                None => g.player_index(&value).map(|entry| entry.trust),
            };
            if let Some(trust) = target_trust
                && !authorize_player_inspection(g, ch, trust)
            {
                return;
            }
            if online.is_none()
                && try_defer_offline(
                    g,
                    ch,
                    &value,
                    &format!("show player {}", value),
                    OfflineOpAuthority::InspectPlayer,
                )
            {
                return;
            }
            match g.find_player_by_name(&value) {
                Some(p) => {
                    let (nm, sex, lvl, cls, gold, bank, exp, align, lessons, birth, logon, played) =
                        g.get_char(p)
                            .map(|c| {
                                (
                                    c.player.name.clone(),
                                    c.player.sex as usize,
                                    c.player.level,
                                    c.player.class,
                                    c.points.gold,
                                    c.points.bank_gold,
                                    c.points.exp,
                                    c.alignment,
                                    c.spells_to_learn,
                                    c.player.time_birth,
                                    c.last_logon.timestamp(),
                                    c.player.time_played.max(0),
                                )
                            })
                            .unwrap();
                    let started = ctime(birth);
                    let last = ctime(logon);
                    let played_hours = played / 3600;
                    let played_minutes = (played % 3600) / 60;
                    let gname = constants::GENDERS.get(sex).copied().unwrap_or("Neutral");
                    let mut buf = format!(
                        "Player: {:<12} ({}) [{:2} {}]\r\n",
                        nm,
                        gname,
                        lvl,
                        class_abbrev(cls)
                    );
                    buf.push_str(&format!(
                        "Au: {:<8}  Bal: {:<8}  Exp: {:<8}  Align: {:<5}  Lessons: {:<3}\r\n",
                        gold, bank, exp, align, lessons
                    ));
                    buf.push_str(&format!(
                        "Started: {:<20.16}  Last: {:<20.16}  Played: {:3}h {:2}m\r\n",
                        started, last, played_hours, played_minutes
                    ));
                    g.send_to_char(ch, &buf);
                }
                None => {
                    g.send_to_char(ch, "There is no such player.\r\n");
                }
            }
        }
        3 => {
            // rent: Crash_listrent — dump the named player's stored rent file.
            let canonical = if value.len() >= 2
                && value.len() <= 20
                && value.chars().all(|c| c.is_ascii_alphabetic())
            {
                g.player_index(&value).map(|entry| entry.name.clone())
            } else {
                None
            };
            match canonical {
                Some(name) => crate::objsave::crash_listrent(g, ch, &name),
                None => g.send_to_char(ch, "There is no such player.\r\n"),
            }
        }
        4 => {
            // stats
            let mut players = 0;
            let mut mobiles = 0;
            let mut connected = 0;
            let chars: Vec<CharId> = g.char_ids();
            for vict in chars {
                if is_npc(g, vict) {
                    mobiles += 1;
                } else if g.can_see(ch, vict) {
                    players += 1;
                    if g.get_char(vict).and_then(|c| c.desc).is_some() {
                        connected += 1;
                    }
                }
            }
            let objs = g.objs.len();
            let mut buf = String::from("Current stats:\r\n");
            buf.push_str(&format!(
                "  {:5} players in game  {:5} connected\r\n",
                players, connected
            ));
            // C act.wizard.c:2800: top_of_p_table + 1 — the persistent player
            // index, not the set of currently instantiated PCs.
            buf.push_str(&format!("  {:5} registered\r\n", g.player_table.len()));
            buf.push_str(&format!(
                "  {:5} mobiles          {:5} prototypes\r\n",
                mobiles,
                g.mob_protos.len()
            ));
            buf.push_str(&format!(
                "  {:5} objects          {:5} prototypes\r\n",
                objs,
                g.obj_protos.len()
            ));
            buf.push_str(&format!(
                "  {:5} rooms            {:5} zones\r\n",
                g.rooms.len(),
                g.zones.len()
            ));
            buf.push_str(&format!("  {:5} large bufs\r\n", 0));
            buf.push_str(&format!("  {:5} buf switches     {:5} overflows\r\n", 0, 0));
            g.send_to_char(ch, &buf);
        }
        5 => {
            // errant rooms (exits pointing to rnum 0)
            let mut buf = String::from("Errant Rooms\r\n------------\r\n");
            let mut k = 0;
            for r in 0..g.rooms.len() {
                for j in 0..NUM_OF_DIRS {
                    if let Some(ex) = &g.rooms[r].exits[j] {
                        if g.real_room(ex.to_room) == Some(0) {
                            k += 1;
                            buf.push_str(&format!(
                                "{:2}: [{:5}] {}\r\n",
                                k, g.rooms[r].number, g.rooms[r].name
                            ));
                        }
                    }
                }
            }
            g.send_to_char(ch, &buf);
        }
        6 => {
            // death traps
            let mut buf = String::from("Death Traps\r\n-----------\r\n");
            let mut j = 0;
            for r in 0..g.rooms.len() {
                if g.rooms[r].room_flags.bits() & ROOM_DEATH_BIT != 0 {
                    j += 1;
                    buf.push_str(&format!(
                        "{:2}: [{:5}] {}\r\n",
                        j, g.rooms[r].number, g.rooms[r].name
                    ));
                }
            }
            g.send_to_char(ch, &buf);
        }
        7 => {
            // godrooms
            let mut buf = String::from("Godrooms\r\n--------------------------\r\n");
            let mut j = 0;
            for r in 0..g.rooms.len() {
                if g.rooms[r].room_flags.bits() & ROOM_GODROOM_BIT != 0 {
                    j += 1;
                    buf.push_str(&format!(
                        "{:2}: [{:5}] {}\r\n",
                        j, g.rooms[r].number, g.rooms[r].name
                    ));
                }
            }
            g.send_to_char(ch, &buf);
        }
        8 => {
            // shops: show_shops(ch, value) — the immortal shop listing.
            crate::shop::show_shops(g, ch, &value);
        }
        9 => {
            // houses: hcontrol_list_houses(ch) — the do_hcontrol "show" arm lists
            // every defined house (do_show declares this 1-arg, so it lists
            // without the guest column, i.e. showguests=false).
            crate::house::do_hcontrol(g, ch, "show", 0);
        }
        _ => {
            g.send_to_char(ch, "Sorry, I don't understand that.\r\n");
        }
    }
}

// ===========================================================================
// do_set / perform_set
// ===========================================================================
// set_fields[] mirrors the C table verbatim: (switchnum, name, level, pcnpc, type).
// pcnpc: 1=PC, 2=NPC, 3=BOTH. type: 0=MISC, 1=BINARY, 2=NUMBER.
const PC: u8 = 1;
const NPC: u8 = 2;
const BOTH: u8 = 3;
const T_MISC: u8 = 0;
const T_BINARY: u8 = 1;
const T_NUMBER: u8 = 2;

struct SetField {
    switchnum: i32,
    cmd: &'static str,
    level: u8,
    pcnpc: u8,
    typ: u8,
}

const fn sf(switchnum: i32, cmd: &'static str, level: u8, pcnpc: u8, typ: u8) -> SetField {
    SetField {
        switchnum,
        cmd,
        level,
        pcnpc,
        typ,
    }
}

static SET_FIELDS: &[SetField] = &[
    sf(18, "defense", LVL_GRGOD, BOTH, T_NUMBER),
    sf(49, "afk", LVL_DEMIGOD, PC, T_BINARY),
    sf(10, "align", LVL_DEMIGOD, BOTH, T_NUMBER),
    sf(20, "bank", LVL_GOD, PC, T_NUMBER),
    sf(0, "brief", LVL_DEMIGOD, PC, T_BINARY),
    sf(17, "cha", LVL_GRGOD, BOTH, T_NUMBER),
    sf(122, "citizen", LVL_IMPL, PC, T_NUMBER),
    sf(123, "mbuilder", LVL_GRGOD, PC, T_BINARY),
    sf(124, "cmdmap", LVL_GRGOD, PC, T_BINARY),
    sf(125, "cmdlweather", LVL_GRGOD, PC, T_BINARY),
    sf(126, "cmdpfileclean", LVL_IMPL, PC, T_BINARY),
    sf(39, "class", LVL_IMPL, BOTH, T_MISC),
    sf(55, "cmdadvance", LVL_GOD, PC, T_BINARY),
    sf(102, "cmdaload", LVL_GRGOD, PC, T_BINARY),
    sf(56, "cmdat", LVL_GOD, PC, T_BINARY),
    sf(100, "cmdattach", LVL_IMPL, PC, T_BINARY),
    sf(75, "cmdauctioneer", LVL_IMPL, PC, T_BINARY),
    sf(108, "cmdprophet", LVL_IMPL, PC, T_BINARY),
    sf(57, "cmdban", LVL_GRGOD, PC, T_BINARY),
    sf(84, "cmdsnow", LVL_IMPL, PC, T_BINARY),
    sf(58, "cmddc", LVL_GRGOD, PC, T_BINARY),
    sf(105, "cmdsage", LVL_GOD, PC, T_BINARY),
    sf(59, "cmdecho", LVL_GRGOD, PC, T_BINARY),
    sf(60, "cmdforce", LVL_GRGOD, PC, T_BINARY),
    sf(61, "cmdfreeze", LVL_GRGOD, PC, T_BINARY),
    sf(90, "cmdgecho", LVL_GRGOD, PC, T_BINARY),
    sf(54, "cmdgeneral", LVL_DEMIGOD, PC, T_BINARY),
    sf(106, "cmdseer", LVL_GRGOD, PC, T_BINARY),
    sf(62, "cmdhcontrol", LVL_IMPL, PC, T_BINARY),
    sf(104, "cmdimp", LVL_IMPL, PC, T_BINARY),
    sf(121, "cmdimpolc", LVL_IMPL, PC, T_BINARY),
    sf(86, "cmdinvis", LVL_GOD, PC, T_BINARY),
    sf(83, "cmdisay", LVL_GOD, PC, T_BINARY),
    sf(63, "cmdload", LVL_GRGOD, PC, T_BINARY),
    sf(87, "cmdmcasters", LVL_GRGOD, PC, T_BINARY),
    sf(88, "cmdmudheal", LVL_GRGOD, PC, T_BINARY),
    sf(64, "cmdmute", LVL_GOD, PC, T_BINARY),
    sf(93, "cmdnotitle", LVL_GOD, PC, T_BINARY),
    sf(85, "cmdolc", LVL_GRGOD, PC, T_BINARY),
    sf(94, "cmdpage", LVL_GOD, PC, T_BINARY),
    sf(66, "cmdpardon", LVL_GOD, PC, T_BINARY),
    sf(120, "cmdpeace", LVL_GOD, PC, T_BINARY),
    sf(79, "cmdplague", LVL_IMPL, PC, T_BINARY),
    sf(67, "cmdpurge", LVL_GRGOD, PC, T_BINARY),
    sf(95, "cmdqecho", LVL_GRGOD, PC, T_BINARY),
    sf(117, "cmdquestmobs", LVL_GRGOD, PC, T_BINARY),
    sf(130, "cmdrebalance", LVL_IMPL, PC, T_BINARY),
    sf(68, "cmdreload", LVL_IMPL, PC, T_BINARY),
    sf(69, "cmdreroll", LVL_IMPL, PC, T_BINARY),
    sf(112, "cmdrespec", LVL_IMPL, PC, T_BINARY),
    sf(70, "cmdrestore", LVL_GRGOD, PC, T_BINARY),
    sf(118, "cmdreward", LVL_GRGOD, PC, T_BINARY),
    sf(89, "cmdrewiz", LVL_IMPL, PC, T_BINARY),
    sf(92, "cmdrewww", LVL_IMPL, PC, T_BINARY),
    sf(71, "cmdsend", LVL_GRGOD, PC, T_BINARY),
    sf(72, "cmdset", LVL_GRGOD, PC, T_BINARY),
    sf(97, "cmdsetreboot", LVL_IMPL, PC, T_BINARY),
    sf(73, "cmdshutdown", LVL_IMPL, PC, T_BINARY),
    sf(74, "cmdskillset", LVL_GRGOD, PC, T_BINARY),
    sf(76, "cmdslowns", LVL_IMPL, PC, T_BINARY),
    sf(77, "cmdsnoop", LVL_GRGOD, PC, T_BINARY),
    sf(78, "cmdswitch", LVL_GRGOD, PC, T_BINARY),
    sf(65, "cmdsyslog", LVL_GRGOD, PC, T_BINARY),
    sf(98, "cmdtmobdie", LVL_IMPL, PC, T_BINARY),
    sf(80, "cmdtransfer", LVL_GOD, PC, T_BINARY),
    sf(81, "cmdunaffect", LVL_GOD, PC, T_BINARY),
    sf(101, "cmdusers", LVL_GRGOD, PC, T_BINARY),
    sf(82, "cmdwizlock", LVL_IMPL, PC, T_BINARY),
    sf(99, "cmdwrestrict", LVL_IMPL, PC, T_BINARY),
    sf(96, "cmdzreset", LVL_GRGOD, PC, T_BINARY),
    sf(43, "color", LVL_GOD, PC, T_BINARY),
    sf(16, "con", LVL_GRGOD, BOTH, T_NUMBER),
    sf(23, "mdefense", LVL_GRGOD, BOTH, T_NUMBER),
    sf(38, "deleted", LVL_GRGOD, PC, T_BINARY),
    sf(15, "dex", LVL_GRGOD, BOTH, T_NUMBER),
    sf(29, "drunk", LVL_GOD, BOTH, T_NUMBER),
    sf(21, "exp", LVL_GRGOD, BOTH, T_NUMBER),
    sf(26, "frozen", LVL_FREEZE, PC, T_BINARY),
    sf(19, "gold", LVL_GOD, BOTH, T_NUMBER),
    sf(7, "hit", LVL_GRGOD, BOTH, T_NUMBER),
    sf(22, "power", LVL_GRGOD, BOTH, T_NUMBER),
    sf(51, "hometown", LVL_GRGOD, PC, T_MISC),
    sf(30, "hunger", LVL_GRGOD, BOTH, T_NUMBER),
    sf(44, "idnum", LVL_IMPL, NPC, T_NUMBER),
    sf(13, "int", LVL_GRGOD, BOTH, T_NUMBER),
    sf(129, "intangible", LVL_GRGOD, PC, T_BINARY),
    sf(24, "invis", LVL_IMPL, PC, T_NUMBER),
    sf(1, "invstart", LVL_GOD, PC, T_BINARY),
    sf(32, "killer", LVL_GOD, PC, T_BINARY),
    sf(28, "lessons", LVL_GRGOD, PC, T_NUMBER),
    sf(34, "level", LVL_GRGOD, BOTH, T_NUMBER),
    sf(42, "loadroom", LVL_GRGOD, PC, T_NUMBER),
    sf(113, "lockout", LVL_IMPL, PC, T_BINARY),
    sf(111, "losses", LVL_GRGOD, PC, T_NUMBER),
    sf(8, "mana", LVL_GRGOD, BOTH, T_NUMBER),
    sf(4, "maxhit", LVL_GRGOD, BOTH, T_NUMBER),
    sf(5, "maxmana", LVL_GRGOD, BOTH, T_NUMBER),
    sf(6, "maxmove", LVL_GRGOD, BOTH, T_NUMBER),
    sf(9, "move", LVL_GRGOD, BOTH, T_NUMBER),
    sf(119, "multiok", LVL_IMPL, PC, T_BINARY),
    sf(127, "mpower", LVL_GRGOD, BOTH, T_NUMBER),
    sf(46, "nodelete", LVL_GOD, PC, T_BINARY),
    sf(25, "nohassle", LVL_GRGOD, PC, T_BINARY),
    sf(3, "nosummon", LVL_GRGOD, PC, T_BINARY),
    sf(40, "nowizlist", LVL_GOD, PC, T_BINARY),
    sf(45, "passwd", LVL_IMPL, PC, T_MISC),
    sf(27, "practices", LVL_GRGOD, PC, T_NUMBER),
    sf(41, "qchan", LVL_GOD, PC, T_BINARY),
    sf(115, "questnext", LVL_IMPL, PC, T_NUMBER),
    sf(114, "questor", LVL_IMPL, PC, T_BINARY),
    sf(116, "questpts", LVL_GRGOD, PC, T_NUMBER),
    sf(50, "race", LVL_IMPL, PC, T_MISC),
    sf(35, "room", LVL_IMPL, BOTH, T_NUMBER),
    sf(36, "roomflag", LVL_GRGOD, PC, T_BINARY),
    sf(103, "setall", LVL_IMPL, PC, T_BINARY),
    sf(47, "sex", LVL_GOD, BOTH, T_MISC),
    sf(37, "siteok", LVL_GRGOD, PC, T_BINARY),
    sf(11, "str", LVL_GRGOD, BOTH, T_NUMBER),
    sf(12, "stradd", LVL_GRGOD, BOTH, T_NUMBER),
    sf(128, "technique", LVL_GRGOD, BOTH, T_NUMBER),
    sf(33, "thief", LVL_GOD, PC, T_BINARY),
    sf(31, "thirst", LVL_GRGOD, BOTH, T_NUMBER),
    sf(2, "title", LVL_GOD, PC, T_MISC),
    sf(53, "trains", LVL_IMPL, PC, T_NUMBER),
    sf(52, "trust", LVL_IMPL, PC, T_NUMBER),
    sf(110, "wins", LVL_GRGOD, PC, T_NUMBER),
    sf(14, "wis", LVL_GRGOD, BOTH, T_NUMBER),
];

/// RANGE(low, high, value) clamp.
fn range_i32(low: i32, high: i32, value: i32) -> i32 {
    value.clamp(low, high)
}

/// parse_class(c): first-letter class parse (class.c). -1 == CLASS_UNDEFINED.
fn parse_class(c: char) -> i32 {
    match c.to_ascii_lowercase() {
        'm' => Class::MagicUser as i32,
        'c' => Class::Cleric as i32,
        't' => Class::Thief as i32,
        'w' => Class::Warrior as i32,
        'a' => Class::Artisan as i32,
        _ => -1,
    }
}

/// parse_race(c): first-letter race parse. -1 == RACE_UNDEFINED.
fn parse_race(c: char) -> i32 {
    // C act.wizard.c:3400 uses races.c parse_race (menu letters a..i); the
    // Rust copy invented name-initial letters and could not set Goblin/Drow.
    crate::races::parse_race(c)
}

/// Player level, trust, and command grants form one durable authority record
/// and must be committed atomically by `advance`; `set` may still change an
/// NPC's runtime level because NPC authority is not persistent account state.
fn set_field_changes_player_authority(field: &SetField) -> bool {
    matches!(field.switchnum, 34 | 52 | 103..=108) || field.cmd.starts_with("cmd")
}

/// perform_set: apply one set field. Returns true on a change that should be
/// saved. `ch` is None for recursive setall-style calls (no permission echo).
fn perform_set(
    g: &mut GameState,
    ch: Option<CharId>,
    vict: CharId,
    mode: usize,
    val_arg: &str,
) -> bool {
    let field = &SET_FIELDS[mode];
    let switchmode = field.switchnum;
    let mut on = false;
    let mut off = false;
    let mut value = 0i32;
    let mut output = String::new();

    if !is_npc(g, vict) && set_field_changes_player_authority(field) {
        if let Some(cch) = ch {
            g.send_to_char(
                cch,
                "Player authority changes must use 'advance <player> <level>' so they are durably committed.\r\n",
            );
        }
        return false;
    }

    if ch.is_none() {
        if val_arg == "on" || val_arg == "yes" {
            on = true;
        } else if val_arg == "off" || val_arg == "no" {
            off = true;
        }
    } else {
        let cch = ch.unwrap();
        let Some(caller) = authenticated_player_authority(g, cch) else {
            g.send_to_char(cch, "You are not godly enough for that!\r\n");
            return false;
        };
        let vnpc = is_npc(g, vict);
        let victim_authority = if vnpc {
            None
        } else {
            let Some(target) = exact_player_authority(g, vict) else {
                g.send_to_char(cch, "Maybe that's not such a great idea...\r\n");
                return false;
            };
            Some(target)
        };
        if caller.authority < i32::from(LVL_IMPL)
            && victim_authority.is_some_and(|target| {
                caller.principal != target.principal && caller.authority <= target.authority
            })
        {
            g.send_to_char(cch, "Maybe that's not such a great idea...\r\n");
            return false;
        }
        if caller.authority < i32::from(field.level) {
            g.send_to_char(cch, "You are not godly enough for that!\r\n");
            return false;
        }
        // PC/NPC correctness.
        if vnpc && (field.pcnpc & NPC) == 0 {
            g.send_to_char(cch, "You can't do that to a beast!\r\n");
            return false;
        }
        if !vnpc && (field.pcnpc & PC) == 0 {
            g.send_to_char(cch, "That can only be done to a beast!\r\n");
            return false;
        }
        let cname = name_of(g, caller.principal);
        let vname = name_of(g, vict);
        let m = LVL_GOD.max(invis_lev(g, cch) as u8);
        match field.typ {
            T_BINARY => {
                if val_arg == "on" || val_arg == "yes" {
                    on = true;
                } else if val_arg == "off" || val_arg == "no" {
                    off = true;
                }
                if !(on || off) {
                    g.send_to_char(cch, "Value must be 'on' or 'off'.\r\n");
                    return false;
                }
                mudlog(
                    g,
                    &format!(
                        "(GC) {} set {} {} for {}.",
                        cname,
                        field.cmd,
                        onoff(on),
                        vname
                    ),
                    BRF,
                    m,
                );
                output = format!("{}'s {} set {}.", vname, field.cmd, onoff(on));
            }
            T_NUMBER => {
                value = match crate::text::parse_i32_atoi(val_arg) {
                    Ok(value) => value,
                    Err(crate::text::ParseIntError::Overflow) => {
                        g.send_to_char(cch, "That number is outside the supported range.\r\n");
                        return false;
                    }
                    Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
                };
                mudlog(
                    g,
                    &format!("(GC) {} set {}'s {} to {}.", cname, vname, field.cmd, value),
                    BRF,
                    m,
                );
                output = format!("{}'s {} set to {}.", vname, field.cmd, value);
            }
            _ => {
                output = "Okay.".to_string();
            }
        }
    }

    // Apply.
    let set_or_remove_act = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.act_flags |= flag;
            } else if off {
                v.act_flags &= !flag;
            }
        }
    };
    let set_or_remove_prf = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.prf_flags |= flag;
            } else if off {
                v.prf_flags &= !flag;
            }
        }
    };
    let set_or_remove_prf2 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.prf2_flags |= flag;
            } else if off {
                v.prf2_flags &= !flag;
            }
        }
    };
    // SET_OR_REMOVE over the per-player god-command bitvectors (godcmds1..3).
    let set_or_remove_gcmd1 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.godcmds1 |= flag;
            } else if off {
                v.godcmds1 &= !flag;
            }
        }
    };
    let set_or_remove_gcmd2 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.godcmds2 |= flag;
            } else if off {
                v.godcmds2 &= !flag;
            }
        }
    };
    let set_or_remove_gcmd3 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.godcmds3 |= flag;
            } else if off {
                v.godcmds3 &= !flag;
            }
        }
    };

    let vict_immortal = !is_npc(g, vict)
        && exact_player_authority(g, vict)
            .is_some_and(|target| target.authority >= i32::from(LVL_IMMORT));

    match switchmode {
        0 => set_or_remove_prf(g, PRF_BRIEF),
        1 => set_or_remove_act(g, PLR_INVSTART),
        2 => {
            // set_title.
            if let Some(v) = g.get_char_mut(vict) {
                v.player.title = if val_arg.is_empty() {
                    None
                } else {
                    Some(val_arg.to_string())
                };
            }
            let vname = name_of(g, vict);
            let title = g
                .get_char(vict)
                .map(|c| c.get_title())
                .unwrap_or(vname.clone());
            output = format!("{}'s title is now: {}", vname, title);
        }
        3 => {
            set_or_remove_prf(g, PRF_SUMMONABLE);
            output = format!("Nosummon {} for {}.\r\n", onoff(!on), name_of(g, vict));
        }
        4 => {
            let nv = range_i32(1, 5000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.max_hit = nv;
            }
            g.affect_total(vict);
        }
        5 => {
            let nv = range_i32(1, 5000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.max_mana = nv;
            }
            g.affect_total(vict);
        }
        6 => {
            let nv = range_i32(1, 5000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.max_move = nv;
            }
            g.affect_total(vict);
        }
        7 => {
            let mh = g.get_char(vict).map(|c| c.points.max_hit).unwrap_or(0);
            let nv = range_i32(-9, mh, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.hit = nv;
            }
            g.affect_total(vict);
        }
        8 => {
            let mm = g.get_char(vict).map(|c| c.points.max_mana).unwrap_or(0);
            let nv = range_i32(0, mm, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.mana = nv;
            }
            g.affect_total(vict);
        }
        9 => {
            let mm = g.get_char(vict).map(|c| c.points.max_move).unwrap_or(0);
            let nv = range_i32(0, mm, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.move_points = nv;
            }
            g.affect_total(vict);
        }
        10 => {
            let nv = range_i32(-1000, 1000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.alignment = nv;
            }
            g.affect_total(vict);
        }
        11 => {
            let hi = if is_npc(g, vict) || vict_immortal {
                MAX_STAT
            } else {
                MAX_PLAYER_STAT
            };
            let nv = range_i32(3, hi as i32, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.str = nv;
                v.real_abils.str_add = 0;
            }
            g.affect_total(vict);
        }
        12 => {
            let nv = range_i32(0, 100, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.str_add = nv as i8;
                if value > 0 {
                    v.real_abils.str = MAX_PLAYER_STAT;
                }
            }
            g.affect_total(vict);
        }
        13 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.intel = nv;
            }
            g.affect_total(vict);
        }
        14 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.wis = nv;
            }
            g.affect_total(vict);
        }
        15 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.dex = nv;
            }
            g.affect_total(vict);
        }
        16 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.con = nv;
            }
            g.affect_total(vict);
        }
        17 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.cha = nv;
            }
            g.affect_total(vict);
        }
        18 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.defense = nv;
            }
            g.affect_total(vict);
        }
        19 => {
            let nv = range_i32(0, 100_000_000, value);
            if let Some(v) = g.get_char_mut(vict) {
                crate::gold::set(v, crate::gold::Account::Carried, i64::from(nv));
            }
        }
        20 => {
            let nv = range_i32(0, 100_000_000, value);
            if let Some(v) = g.get_char_mut(vict) {
                crate::gold::set(v, crate::gold::Account::Bank, i64::from(nv));
            }
        }
        21 => {
            let nv = range_i32(0, 50_000_000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.exp = nv as i64;
            }
        }
        22 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.power = nv;
            }
            g.affect_total(vict);
        }
        23 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.mdefense = nv;
            }
            g.affect_total(vict);
        }
        24 => {
            let Some(cch) = ch else {
                return false;
            };
            let (Some(caller), Some(target)) = (
                authenticated_player_authority(g, cch),
                exact_player_authority(g, vict),
            ) else {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            };
            if caller.authority < i32::from(LVL_IMPL) && caller.principal != target.principal {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            }
            let nv = range_i32(0, target.authority, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.invis_level = nv;
            }
        }
        25 => {
            let Some(cch) = ch else {
                return false;
            };
            let (Some(caller), Some(target)) = (
                authenticated_player_authority(g, cch),
                exact_player_authority(g, vict),
            ) else {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            };
            if caller.authority < i32::from(LVL_IMPL) && caller.principal != target.principal {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            }
            set_or_remove_prf(g, PRF_NOHASSLE);
        }
        26 => {
            if let Some(cch) = ch {
                if cch == vict {
                    g.send_to_char(cch, "Better not -- could be a long winter!\r\n");
                    return false;
                }
            }
            set_or_remove_act(g, PLR_FROZEN);
        }
        27 | 28 => {
            let nv = range_i32(0, 100, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.spells_to_learn = nv;
            }
        }
        29 => {
            let nv = range_i32(-100, 24, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.conditions[DRUNK] = nv as i8;
            }
        }
        30 => {
            let nv = range_i32(-100, 24, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.conditions[FULL] = nv as i8;
            }
        }
        31 => {
            let nv = range_i32(-100, 24, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.conditions[THIRST] = nv as i8;
            }
        }
        32 => set_or_remove_act(g, PLR_KILLER),
        33 => set_or_remove_act(g, PLR_THIEF),
        34 => {
            let ch_trust = ch
                .and_then(|caller| authenticated_player_authority(g, caller))
                .map(|caller| caller.authority)
                .unwrap_or(i32::from(LVL_IMPL));
            if value > ch_trust || value > i32::from(LVL_IMPL) {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "You can't do that.\r\n");
                }
                return false;
            }
            let nv = range_i32(0, LVL_IMPL as i32, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.player.level = nv as u8;
            }
        }
        35 => match g.real_room(value) {
            Some(i) => {
                g.char_from_room(vict);
                g.char_to_room(vict, i);
            }
            None => {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "No room exists with that number.\r\n");
                }
                return false;
            }
        },
        36 => set_or_remove_prf(g, PRF_ROOMFLAGS),
        37 => set_or_remove_act(g, PLR_SITEOK),
        38 => set_or_remove_act(g, PLR_DELETED),
        39 => {
            let i = parse_class(val_arg.chars().next().unwrap_or(' '));
            if i < 0 {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "That is not a class.\r\n");
                }
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.player.class = Class::from_u8(i as u8);
            }
        }
        40 => set_or_remove_act(g, PLR_NOWIZLIST),
        41 => set_or_remove_prf2(g, PRF2_QCHAN),
        42 => {
            if is_number(val_arg) {
                value = match crate::text::parse_i32_strict(val_arg) {
                    Ok(value) => value,
                    Err(_) => {
                        if let Some(cch) = ch {
                            g.send_to_char(
                                cch,
                                "Must be a room's virtual number in the supported range.\r\n",
                            );
                        }
                        return false;
                    }
                };
                if g.real_room(value).is_some() || value == -1 {
                    if let Some(v) = g.get_char_mut(vict) {
                        v.load_room = value;
                    }
                    let vname = name_of(g, vict);
                    if value == -1 {
                        output = format!("{}'s loadroom turned off.", vname);
                    } else {
                        output = format!("{} will enter at room #{}.", vname, value);
                    }
                } else {
                    if let Some(cch) = ch {
                        g.send_to_char(cch, "That room does not exist!\r\n");
                    }
                    return false;
                }
            } else {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "Must be a room's virtual number.\r\n");
                }
                return false;
            }
        }
        43 => set_or_remove_prf(g, PRF_COLOR_1 | PRF_COLOR_2),
        44 => {
            // idnum: an Implementor role may change NPC runtime identities.
            // Durable player id 1 is historical data, not an authorization
            // credential; trusting it let a mortal impostor bypass this gate.
            let caller_is_implementor = ch.is_some_and(|caller| {
                target_principal_authority(g, caller)
                    .is_some_and(|principal| principal.authority >= i32::from(LVL_IMPL))
            });
            if !caller_is_implementor || !is_npc(g, vict) {
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.idnum = value as i64;
            }
        }
        45 => {
            let Some(target) = exact_player_authority(g, vict) else {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "You cannot change that.\r\n");
                }
                return false;
            };
            if target.authority >= i32::from(LVL_GRGOD) {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "You cannot change that.\r\n");
                }
                return false;
            }
            if !(3..=crate::password::MAX_PASSWORD_INPUT_BYTES).contains(&val_arg.len()) {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "Password must be between 3 and 64 bytes.\r\n");
                }
                return false;
            }
            let Some(authorization) =
                ch.and_then(|caller| crate::interpreter::authenticated_command_request(g, caller))
            else {
                return false;
            };
            let Some((idnum, name)) = g
                .get_char(vict)
                .filter(|victim| !victim.is_npc && victim.idnum > 0)
                .map(|victim| (victim.idnum, victim.get_name().to_string()))
            else {
                g.send_to_char(
                    authorization.requester_body,
                    "That player has no durable identity.\r\n",
                );
                return false;
            };
            g.queue_password_update(authorization, vict, idnum, &name, val_arg.to_owned());
            output = format!("Password change for {} queued.", name);
        }
        46 => set_or_remove_act(g, PLR_NODELETE),
        47 => {
            let sex = if val_arg.eq_ignore_ascii_case("male") {
                Gender::Male
            } else if val_arg.eq_ignore_ascii_case("female") {
                Gender::Female
            } else if val_arg.eq_ignore_ascii_case("neutral") {
                Gender::Neutral
            } else {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "Must be 'male', 'female', or 'neutral'.\r\n");
                }
                return false;
            };
            if let Some(v) = g.get_char_mut(vict) {
                v.player.sex = sex;
            }
        }
        49 => set_or_remove_prf(g, PRF_AFK),
        50 => {
            let i = parse_race(val_arg.chars().next().unwrap_or(' '));
            if i < 0 {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "That is not a race.\r\n");
                }
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.player.race = Race::from_u8(i as u8);
            }
        }
        51 => {
            // hometown: parse_town(*val_arg) — the home-town menu letter (a..c).
            let i = crate::class::parse_town(val_arg.chars().next().unwrap_or(' '));
            if i == -1 {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "That is not a hometown.\r\n");
                }
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.player.hometown = i;
            }
        }
        52 => {
            if let Some(cch) = ch {
                g.send_to_char(
                    cch,
                    "Player authority changes must use 'advance <player> <level>' so they are durably committed.\r\n",
                );
            }
            return false;
        }
        53 => {
            let nv = range_i32(0, 100, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.spells_to_learn = nv;
            }
        }
        // 54..=108: per-player god-command bitvectors (godcmds1..4). Each cmd*
        // field flips one GCMD bit; the grant/revoke aggregates (103..=108) set
        // or recursively walk the set_fields table.
        54 => set_or_remove_gcmd1(g, GCMD_GEN),
        55 => set_or_remove_gcmd1(g, GCMD_ADVANCE),
        56 => set_or_remove_gcmd1(g, GCMD_AT),
        57 => set_or_remove_gcmd1(g, GCMD_BAN),
        58 => set_or_remove_gcmd1(g, GCMD_DC),
        59 => set_or_remove_gcmd1(g, GCMD_ECHO),
        60 => set_or_remove_gcmd1(g, GCMD_FORCE),
        61 => set_or_remove_gcmd1(g, GCMD_FREEZE),
        62 => set_or_remove_gcmd1(g, GCMD_HCONTROL),
        63 => set_or_remove_gcmd1(g, GCMD_LOAD),
        64 => set_or_remove_gcmd1(g, GCMD_MUTE),
        65 => set_or_remove_gcmd1(g, GCMD_SYSLOG),
        66 => set_or_remove_gcmd1(g, GCMD_PARDON),
        67 => set_or_remove_gcmd1(g, GCMD_PURGE),
        68 => set_or_remove_gcmd1(g, GCMD_RELOAD),
        69 => set_or_remove_gcmd1(g, GCMD_REROLL),
        70 => set_or_remove_gcmd1(g, GCMD_RESTORE),
        71 => set_or_remove_gcmd1(g, GCMD_SEND),
        72 => set_or_remove_gcmd1(g, GCMD_SET),
        73 => set_or_remove_gcmd1(g, GCMD_SHUTDOWN),
        74 => set_or_remove_gcmd1(g, GCMD_SKILLSET),
        75 => set_or_remove_gcmd1(g, GCMD_AUCTIONEER),
        76 => set_or_remove_gcmd1(g, GCMD_SLOWNS),
        77 => set_or_remove_gcmd1(g, GCMD_SNOOP),
        78 => set_or_remove_gcmd1(g, GCMD_SWITCH),
        79 => set_or_remove_gcmd1(g, GCMD_PLAGUE),
        80 => set_or_remove_gcmd1(g, GCMD_TRANS),
        81 => set_or_remove_gcmd1(g, GCMD_UNAFFECT),
        82 => set_or_remove_gcmd1(g, GCMD_WIZLOCK),
        83 => set_or_remove_gcmd1(g, GCMD_ISAY),
        84 => {
            set_or_remove_gcmd3(g, GCMD3_ADDSNOW);
            set_or_remove_gcmd3(g, GCMD3_DELSNOW);
        }
        85 => set_or_remove_gcmd2(g, GCMD2_OLC),
        86 => set_or_remove_gcmd2(g, GCMD2_INVIS),
        87 => set_or_remove_gcmd2(g, GCMD2_MCASTERS),
        88 => set_or_remove_gcmd2(g, GCMD2_MUDHEAL),
        89 => set_or_remove_gcmd2(g, GCMD2_REWIZ),
        90 => set_or_remove_gcmd2(g, GCMD2_GECHO),
        92 => set_or_remove_gcmd2(g, GCMD2_REWWW),
        93 => set_or_remove_gcmd2(g, GCMD2_NOTITLE),
        94 => set_or_remove_gcmd2(g, GCMD2_PAGE),
        95 => set_or_remove_gcmd2(g, GCMD2_QECHO),
        96 => set_or_remove_gcmd2(g, GCMD2_ZRESET),
        97 => set_or_remove_gcmd2(g, GCMD2_SETREBOOT),
        98 => set_or_remove_gcmd2(g, GCMD2_TMOBDIE),
        99 => set_or_remove_gcmd2(g, GCMD2_WRESTRICT),
        100 => set_or_remove_gcmd2(g, GCMD2_ATTACH),
        101 => set_or_remove_gcmd2(g, GCMD2_USERS),
        102 => set_or_remove_gcmd2(g, GCMD2_ALOAD),
        103 => {
            // imp: grant everything (bar GCMD_CMDSET) or revoke everything.
            if val_arg.eq_ignore_ascii_case("on") {
                if let Some(v) = g.get_char_mut(vict) {
                    v.godcmds1 = (!GCMD_CMDSET) | v.godcmds1;
                    v.godcmds2 = !0;
                    v.godcmds3 = !0;
                    v.godcmds4 = !0;
                }
            } else if val_arg.eq_ignore_ascii_case("off") {
                if let Some(v) = g.get_char_mut(vict) {
                    v.godcmds1 = 0;
                    v.godcmds2 = 0;
                    v.godcmds3 = 0;
                    v.godcmds4 = 0;
                }
            }
        }
        104 => {
            if val_arg.eq_ignore_ascii_case("on") {
                if let Some(v) = g.get_char_mut(vict) {
                    for i in 0..=32 {
                        v.godcmds1 |= 1i64 << i;
                        v.godcmds2 |= 1i64 << i;
                        v.godcmds3 |= 1i64 << i;
                    }
                }
            } else {
                grant_cmd_tier(g, vict, LVL_IMPL, val_arg);
            }
        }
        105 => grant_cmd_tier(g, vict, LVL_DEMIGOD, val_arg),
        106 => grant_cmd_tier(g, vict, LVL_GOD, val_arg),
        107 | 108 => grant_cmd_tier(g, vict, LVL_GRGOD, val_arg),
        109 => {}
        110 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.wins = value as u8;
            }
        }
        111 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.losses = value as u8;
            }
        }
        112 => set_or_remove_gcmd2(g, GCMD2_RESPEC),
        113 => set_or_remove_prf2(g, PRF2_LOCKOUT),
        114 => {
            if on {
                if let Some(cch) = ch {
                    g.send_to_char(
                        cch,
                        "Sorry. But setting QUESTOR flag ON for a player will cause problems.\r\n",
                    );
                }
                return false;
            }
            set_or_remove_act(g, PLR_QUESTOR);
            if let Some(v) = g.get_char_mut(vict) {
                v.quest_mob = 0;
                v.quest_obj = 0;
            }
        }
        115 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.next_quest = value;
            }
        }
        116 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.quest_points = value;
            }
        }
        117 => set_or_remove_gcmd2(g, GCMD2_QUESTMOBS),
        118 => set_or_remove_gcmd2(g, GCMD2_REWARD),
        119 => set_or_remove_act(g, PLR_MULTIOK),
        120 => set_or_remove_gcmd3(g, GCMD3_PEACE),
        121 => set_or_remove_gcmd3(g, GCMD3_IMPOLC),
        122 => {
            // C: RANGE(1,7); citizen = value - 1 (stored 0..6).
            let nv = range_i32(1, 7, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.citizen = (nv - 1) as u8;
            }
        }
        123 => set_or_remove_act(g, PLR_MBUILDER),
        124 => set_or_remove_gcmd3(g, GCMD3_MAP),
        125 => set_or_remove_gcmd3(g, GCMD3_LWEATHER),
        126 => set_or_remove_gcmd3(g, GCMD3_PFILECLEAN),
        127 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.mpower = nv;
            }
            g.affect_total(vict);
        }
        128 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.technique = nv;
            }
            g.affect_total(vict);
        }
        129 => set_or_remove_prf2(g, PRF2_INTANGIBLE),
        130 => set_or_remove_gcmd3(g, GCMD3_REBALANCE),
        _ => {
            if let Some(cch) = ch {
                g.send_to_char(cch, "Can't set that!\r\n");
            }
            return false;
        }
    }

    output.push_str("\r\n");
    if let Some(cch) = ch {
        g.send_to_char(cch, &cap(&output));
    }
    true
}

/// The level-tier god-command grant/revoke aggregates (set cmddemigod/cmdgod/
/// cmdgreatergod/cmdimpcmds-off, do_set cases 104..=108). Walks the set_fields
/// table and recursively perform_set's every `cmd*` field at `tier`, skipping
/// the aggregate switches (104..=108) and the two multi-bit fields (54 cmdgeneral
/// / 84 cmdsnow) exactly as C does. `val_arg` ("on"/"off") drives each flip.
fn grant_cmd_tier(g: &mut GameState, vict: CharId, tier: u8, val_arg: &str) {
    for i in 0..SET_FIELDS.len() {
        let f = &SET_FIELDS[i];
        if f.level == tier
            && f.cmd.starts_with("cmd")
            && !(104..=108).contains(&f.switchnum)
            && f.switchnum != 54
            && f.switchnum != 84
        {
            perform_set(g, None, vict, i, val_arg);
        }
    }
}

/// Stat ceiling for int/wis/dex/con/cha: NPC or >= GRGOD gets MAX_STAT.
fn stat_hi(g: &GameState, vict: CharId) -> i32 {
    if is_npc(g, vict)
        || exact_player_authority(g, vict)
            .is_some_and(|target| target.authority >= i32::from(LVL_GRGOD))
    {
        MAX_STAT as i32
    } else {
        MAX_PLAYER_STAT as i32
    }
}

pub fn do_set(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (mut name, mut rest) = half_chop(arg);
    let mut is_player = false;
    let mut is_mob = false;

    let Some(ch_authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    };
    let principal_has_god_commands = g.get_char(ch_authority.principal).is_some_and(|principal| {
        principal.godcmds1 != 0
            || principal.godcmds2 != 0
            || principal.godcmds3 != 0
            || principal.godcmds4 != 0
    });
    // C act.wizard.c:3895: `if (!IS_GOD(ch) && GET_LEVEL(ch) < LVL_IMMORT)`.
    // IS_GOD is the granted-command test, so a sub-immortal holding bits is
    // admitted where a plain trust check would reject them. Both properties
    // come from the authenticated principal, never the active body's level.
    if !principal_has_god_commands && ch_authority.authority < i32::from(LVL_IMMORT) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }

    if name == "file" {
        // set file <name> <field> <value>: load-edit-save an OFFLINE player
        // (retrieve_player_entry + save_char). The load + write-back are async
        // DB ops (database::load_player / save_player), unreachable from this
        // sync path — so when the named player exists in the index, defer the
        // command through the async bridge (game.rs loads the player into the
        // world, replays it, saves + extracts). We replay as `set player <…>`
        // rather than `set file <…>` so the replayed pass takes the online
        // get_player_vis path (the char is now present) instead of re-entering
        // this `file` branch and deferring forever.
        let (fname, frest) = half_chop(&rest);
        if !fname.is_empty()
            && g.find_player_by_name(&fname).is_none()
            && g.get_id_by_name(&fname).is_some()
        {
            g.send_to_char(
                ch,
                &format!("[ Loading {} from the player file... ]\r\n", fname),
            );
            g.queue_offline_op(
                ch,
                &fname,
                &format!("set player {} {}", fname, frest),
                OfflineOpAuthority::ReplayHandler,
            );
            return;
        }
        g.send_to_char(ch, "There is no such player.\r\n");
        return;
    } else if name.eq_ignore_ascii_case("player") {
        is_player = true;
        let (n, r) = half_chop(&rest);
        name = n;
        rest = r;
    } else if name.eq_ignore_ascii_case("mob") {
        is_mob = true;
        let (n, r) = half_chop(&rest);
        name = n;
        rest = r;
    } else if name.eq_ignore_ascii_case("Legal_PKS")
        && ch_authority.authority >= i32::from(LVL_GRGOD)
    {
        // C act.wizard.c:3914-3921: this really flips the pk_allowed global that
        // do_hit/do_kill/murder, fight.c's killer flagging and the PvP spell
        // guards all read.
        let (mode, _r) = half_chop(&rest);
        let mut allowed = g.pk_allowed;
        if mode.eq_ignore_ascii_case("OFF") {
            allowed = false;
        }
        if mode.eq_ignore_ascii_case("ON") {
            allowed = true;
        }
        g.pk_allowed = allowed;
        g.send_to_char(
            ch,
            &format!(
                "Legal PKs are now {}.\r\n",
                if allowed { "Allowed" } else { "Disallowed" }
            ),
        );
        return;
    }
    let _ = is_mob;

    let (field, val_arg) = half_chop(&rest);

    if name.is_empty() || field.is_empty() {
        let mut buf = String::from("Usage: set <victim> <field> <value>\r\nFields:\r\n");
        let mut k = 0;
        for f in SET_FIELDS {
            if i32::from(f.level) > ch_authority.authority {
                continue;
            }
            k += 1;
            if f.cmd.starts_with("cmd") {
                buf.push_str(&format!("&Ycmd&n{:<12}", &f.cmd[3..]));
            } else {
                buf.push_str(&format!("{:<15}", f.cmd));
            }
            if k % 5 == 0 {
                buf.push_str("\r\n");
            }
        }
        buf.push_str(&format!(
            "\r\nThere are {} set fields available to you.\r\n",
            k
        ));
        g.send_to_char(ch, &buf);
        return;
    }

    // Find target. An offline player is resolved through the async bridge: if
    // the name isn't in the world but IS in the player_table, defer the WHOLE
    // command (game.rs loads the player, replays this verbatim so the online
    // path below runs against the now-in-world char, then saves + extracts).
    let vict = if is_player {
        match get_player_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                if try_defer_offline(
                    g,
                    ch,
                    &name,
                    &format!("set {}", arg),
                    OfflineOpAuthority::ReplayHandler,
                ) {
                    return;
                }
                g.send_to_char(ch, "There is no such player.\r\n");
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                if try_defer_offline(
                    g,
                    ch,
                    &name,
                    &format!("set {}", arg),
                    OfflineOpAuthority::ReplayHandler,
                ) {
                    return;
                }
                g.send_to_char(ch, "There is no such creature.\r\n");
                return;
            }
        }
    };

    // Find the field by prefix.
    let mut mode = SET_FIELDS.len();
    for (i, f) in SET_FIELDS.iter().enumerate() {
        if f.cmd.starts_with(&field.to_lowercase()) {
            mode = i;
            break;
        }
    }
    if mode >= SET_FIELDS.len() {
        // No match -> the C loop lands on the "\n" terminator -> default arm.
        g.send_to_char(ch, "Can't set that!\r\n");
        return;
    }

    let changed = perform_set(g, Some(ch), vict, mode, &val_arg);
    if changed
        && !is_npc(g, vict)
        && SET_FIELDS[mode].switchnum != 45
        && !set_field_changes_player_authority(&SET_FIELDS[mode])
    {
        g.request_player_save(vict);
    }
}

// ===========================================================================
// do_rewiz
// ===========================================================================
pub fn do_rewiz(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // C do_rewiz: when use_autowiz (YES in config.c), regenerate the wiz/imm
    // lists via check_autowiz(ch). The native autowiz is always available, so
    // this build takes the online branch.
    g.send_to_char(ch, "You have reloaded the autowiz system.\r\n");
    let cname = name_of(g, ch);
    let m = LVL_GOD.max(invis_lev(g, ch) as u8); // C MAX(LVL_GOD, GET_INVIS_LEV(ch))
    mudlog(
        g,
        &format!("(GC) {} initiated reload of the autowiz system.", cname),
        BRF,
        m,
    );
    crate::autowiz::check_autowiz(g, ch);
}

// ===========================================================================
// do_rlist / do_mlist / do_olist
// ===========================================================================
const MAX_ROOM_VNUM: i32 = 99999;

pub fn do_rlist(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, b, _r) = two_arguments(arg);
    if a.is_empty() || b.is_empty() {
        g.send_to_char(ch, "Usage: rlist <begining number> <ending number>\r\n");
        return;
    }
    let Some(first) = command_atoi(g, ch, &a) else {
        return;
    };
    let Some(last) = command_atoi(g, ch, &b) else {
        return;
    };
    // C act.wizard.c:4091-4100: a mortal builder may only enumerate the zone(s)
    // they own, checked on both arguments before any range validation.
    let authority = authenticated_player_authority(g, ch)
        .map(|principal| principal.authority)
        .unwrap_or(-1);
    if authority < i32::from(LVL_IMMORT) {
        if !can_edit_zone(g, ch, real_zone(g, first)) {
            g.send_to_char(
                ch,
                "You can't edit the zone supplied by the first argument.\r\n",
            );
            return;
        }
        if !can_edit_zone(g, ch, real_zone(g, last)) {
            g.send_to_char(
                ch,
                "You can't edit the zone supplied by the second argument.\r\n",
            );
            return;
        }
    }
    if first < 0 || first > MAX_ROOM_VNUM || last < 0 || last > MAX_ROOM_VNUM {
        g.send_to_char(
            ch,
            "Values must be between 0 and highest possible vnum.\n\r",
        );
        return;
    }
    if first >= last {
        g.send_to_char(ch, "Second value must be greater than first.\n\r");
        return;
    }
    let mut rows: Vec<(RoomVnum, i32, String)> = g
        .rooms
        .iter()
        .map(|r| (r.number, r.zone, r.name.clone()))
        .collect();
    rows.sort_by_key(|r| r.0);
    let mut found = 0;
    let mut out = String::new();
    for (vnum, zone, name) in rows {
        if vnum > last {
            break;
        }
        if vnum >= first {
            found += 1;
            out.push_str(&format!(
                "{:5}. [{:5}] ({:3}) {}\r\n",
                found, vnum, zone, name
            ));
        }
    }
    g.send_to_char(ch, &out);
    if found == 0 {
        g.send_to_char(ch, "No rooms were found in those parameters.\n\r");
    }
}

pub fn do_mlist(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, b, _r) = two_arguments(arg);
    if a.is_empty() || b.is_empty() {
        g.send_to_char(ch, "Usage: mlist <begining number> <ending number>\r\n");
        return;
    }
    let Some(first) = command_atoi(g, ch, &a) else {
        return;
    };
    let Some(last) = command_atoi(g, ch, &b) else {
        return;
    };
    // C act.wizard.c:4146-4155: same two-gate builder check as rlist.
    let authority = authenticated_player_authority(g, ch)
        .map(|principal| principal.authority)
        .unwrap_or(-1);
    if authority < i32::from(LVL_IMMORT) {
        if !can_edit_zone(g, ch, real_zone(g, first)) {
            g.send_to_char(
                ch,
                "You can't edit the zone supplied by the first argument.\r\n",
            );
            return;
        }
        if !can_edit_zone(g, ch, real_zone(g, last)) {
            g.send_to_char(
                ch,
                "You can't edit the zone supplied by the second argument.\r\n",
            );
            return;
        }
    }
    if first < 0 || first > MAX_ROOM_VNUM || last < 0 || last > MAX_ROOM_VNUM {
        g.send_to_char(
            ch,
            "Values must be between 0 and highest possible vnum.\n\r",
        );
        return;
    }
    if first >= last {
        g.send_to_char(ch, "Second value must be greater than first.\n\r");
        return;
    }
    let mut rows: Vec<(MobVnum, String)> = g
        .mob_protos
        .values()
        .map(|m| (m.vnum, m.short_desc.clone()))
        .collect();
    rows.sort_by_key(|r| r.0);
    let mut found = 0;
    let mut out = String::new();
    for (vnum, short) in rows {
        if vnum > last {
            break;
        }
        if vnum >= first {
            found += 1;
            out.push_str(&format!("{:5}. [{:5}] {}\r\n", found, vnum, short));
        }
    }
    g.send_to_char(ch, &out);
    if found == 0 {
        g.send_to_char(ch, "No mobiles were found in those parameters.\n\r");
    }
}

pub fn do_olist(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, b, _r) = two_arguments(arg);
    if a.is_empty() || b.is_empty() {
        g.send_to_char(ch, "Usage: olist <begining number> <ending number>\r\n");
        return;
    }
    let Some(first) = command_atoi(g, ch, &a) else {
        return;
    };
    let Some(last) = command_atoi(g, ch, &b) else {
        return;
    };
    // C act.wizard.c:4199-4208: same two-gate builder check as rlist.
    let authority = authenticated_player_authority(g, ch)
        .map(|principal| principal.authority)
        .unwrap_or(-1);
    if authority < i32::from(LVL_IMMORT) {
        if !can_edit_zone(g, ch, real_zone(g, first)) {
            g.send_to_char(
                ch,
                "You can't edit the zone supplied by the first argument.\r\n",
            );
            return;
        }
        if !can_edit_zone(g, ch, real_zone(g, last)) {
            g.send_to_char(
                ch,
                "You can't edit the zone supplied by the second argument.\r\n",
            );
            return;
        }
    }
    if first < 0 || first > MAX_ROOM_VNUM || last < 0 || last > MAX_ROOM_VNUM {
        g.send_to_char(
            ch,
            "Values must be between 0 and highest possible vnum.\n\r",
        );
        return;
    }
    if first >= last {
        g.send_to_char(ch, "Second value must be greater than first.\n\r");
        return;
    }
    let mut rows: Vec<(ObjVnum, String)> = g
        .obj_protos
        .values()
        .map(|o| (o.vnum, o.short_desc.clone()))
        .collect();
    rows.sort_by_key(|r| r.0);
    let mut found = 0;
    let mut out = String::new();
    for (vnum, short) in rows {
        if vnum > last {
            break;
        }
        if vnum >= first {
            found += 1;
            out.push_str(&format!("{:5}. [{:5}] {}\r\n", found, vnum, short));
        }
    }
    g.send_to_char(ch, &out);
    if found == 0 {
        g.send_to_char(ch, "No objects were found in those parameters.\n\r");
    }
}

// ===========================================================================
// do_whoupd / do_isay / do_mcasters
// ===========================================================================
pub fn do_whoupd(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // C act.wizard.c:4238 + comm.c:2566. C's guard (`if (!(www_who) > 0)`)
    // was buggy and www_who shipped NO; the finish-the-game port repairs the
    // guard and makes the generator live behind the www_who config flag
    // (registered divergence).
    if !g.config.www_who {
        g.send_to_char(ch, "The WWW who is currently deactivated in the code.\r\n");
        return;
    }
    match crate::whohtml::make_who2html(g) {
        Ok(()) => g.send_to_char(ch, "Updating the web who list...\r\n"),
        Err(e) => {
            crate::syslog::mudlog(
                g,
                &format!("ERROR: who2html: {}", e),
                crate::syslog::NRM,
                LVL_GOD,
            );
            g.send_to_char(ch, "The WWW who update failed; see syslog.\r\n");
        }
    }
}

pub fn do_isay(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if arg.trim().is_empty() {
        g.send_to_char(ch, "YES! But what do you want to say?!\r\n");
        return;
    }
    let line = format!("&m[&YINFO&m]&n{}\r\n", arg);
    send_to_all(g, &line);
    let cname = name_of(g, ch);
    mudlog(
        g,
        &format!("(GC) Isay by {}: {}", cname, line.trim_end()),
        NRM,
        LVL_IMPL,
    );
}

pub fn do_mcasters(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    const MOB_CASTER: i64 = 1 << 21;

    let magic_user = crate::spec_procs::magic_user as crate::spec_assign::SpecFn;
    // C act.wizard.c:4489-4494 filters on mob_index[i].func == magic_user alone.
    // That binding comes from two places: the static ASSIGNMOB table, and
    // db.c:1346-1349, which sets `mob_index[i].func = magic_user` for EVERY
    // MOB_CASTER prototype while the mob file loads. So C lists flag-only
    // casters too — the bit merely picks the "(Type: CASTER)" label — and the
    // disjunction below is the faithful rendering of that combined binding, not
    // an extra filter.
    let mut casters: Vec<_> = g
        .mob_protos
        .values()
        .filter(|proto| {
            proto.act_flags & MOB_CASTER != 0
                || crate::spec_assign::get_mob_spec(g, proto.vnum)
                    .is_some_and(|func| std::ptr::fn_addr_eq(func, magic_user))
        })
        .collect();
    casters.sort_by_key(|proto| proto.vnum);

    let mut out = String::from("Spellcasting mobs:\r\n");
    for proto in casters {
        let caster_type = if proto.act_flags & MOB_CASTER != 0 {
            "CASTER"
        } else {
            "ASSIGNED"
        };
        out.push_str(&format!(
            "[{}] {} (Type: {})\r\n",
            proto.vnum, proto.short_desc, caster_type
        ));
    }

    g.send_to_char(ch, &out);
}

// ===========================================================================
// do_setreboot
// ===========================================================================
// reboot_hr/min + warn_hr/min are boot-loop globals. Mirror locally so the
// report path is faithful within a run (documented gap: not consulted yet).
static REBOOT_HR: AtomicI32 = AtomicI32::new(-1);

/// The (reboot_hr, reboot_min, warn_hr, warn_min) schedule for the
/// heartbeat's auto-reboot clock (-1 = disabled).
pub fn reboot_schedule() -> (i32, i32, i32, i32) {
    (
        REBOOT_HR.load(Ordering::Relaxed),
        REBOOT_MIN.load(Ordering::Relaxed),
        WARN_HR.load(Ordering::Relaxed),
        WARN_MIN.load(Ordering::Relaxed),
    )
}
static REBOOT_MIN: AtomicI32 = AtomicI32::new(0);
static WARN_HR: AtomicI32 = AtomicI32::new(0);
static WARN_MIN: AtomicI32 = AtomicI32::new(0);

pub fn do_setreboot(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, b, _r) = two_arguments(arg);
    let Some(hr) = command_atoi(g, ch, &a) else {
        return;
    };
    let Some(min) = command_atoi(g, ch, &b) else {
        return;
    };

    if a.is_empty() {
        g.send_to_char(ch, "Usage: setreboot <reboothr> <rebootmin>\r\n");
        let rh = REBOOT_HR.load(Ordering::Relaxed);
        if rh == -1 {
            g.send_to_char(ch, "Reboot time is currently DISABLED.\r\n");
        } else {
            g.send_to_char(
                ch,
                &format!(
                    "Reboot time is currently set for {}:{}(reminder at {}:{})\r\n",
                    rh,
                    REBOOT_MIN.load(Ordering::Relaxed),
                    WARN_HR.load(Ordering::Relaxed),
                    WARN_MIN.load(Ordering::Relaxed)
                ),
            );
        }
        return;
    }

    if (-1..=23).contains(&hr) {
        REBOOT_HR.store(hr, Ordering::Relaxed);
        if (0..=59).contains(&min) {
            REBOOT_MIN.store(min, Ordering::Relaxed);
        }
        let cname = name_of(g, ch);
        let logline = if hr == -1 {
            format!("(GC) {} has DISABLED auto reboot time.", cname)
        } else {
            let mut warn_min = REBOOT_MIN.load(Ordering::Relaxed) - 10;
            let mut warn_hr = hr;
            if warn_min < 0 {
                warn_min += 60;
                warn_hr -= 1;
                if warn_hr < 0 {
                    warn_hr = 23;
                }
            }
            WARN_MIN.store(warn_min, Ordering::Relaxed);
            WARN_HR.store(warn_hr, Ordering::Relaxed);
            format!(
                "(GC) {} has set auto reboot time for {}:{}",
                cname,
                hr,
                REBOOT_MIN.load(Ordering::Relaxed)
            )
        };
        mudlog(g, &logline, NRM, LVL_GOD);
    }
}

// ===========================================================================
// do_esave / do_copyto / do_dig / do_tedit / do_areas / do_vwear
// (all OLC-dependent). OLC isn't ported; reproduce the guard/usage messages
// and the parts that don't require the OLC save machinery.
// ===========================================================================
pub fn do_esave(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, _b, _r) = two_arguments(arg);
    if a.is_empty() {
        g.send_to_char(
            ch,
            "You must supply a zone number or '*' for all zones.\r\n",
        );
        return;
    }
    let cname = name_of(g, ch);
    // do_esave runs `do_olc save N 1` for each component j=0..4 (room/obj/zone/
    // mob/shop) — olc::olc_save_to_disk is that arm: it rewrites the .wld/.obj
    // and drops the zone/mob/shop save-list entries (mob/shop .mob/.shp are
    // autosaved on edit-quit inside their editors, so the explicit save there
    // only clears the dirty flag — that delegation lives in olc.rs).
    let lvl = LVL_BUILDER.max(invis_lev(g, ch) as u8);
    if a.starts_with('*') {
        mudlog(
            g,
            &format!("OLC: {} saves ALL info for ALL zones.", cname),
            PFT,
            lvl,
        );
        for zr in 0..g.zones.len() {
            let (named, has_top) = {
                let z = &g.zones[zr];
                (!z.name.is_empty(), z.top > 0)
            };
            if named && has_top {
                for kind in 0..=4 {
                    crate::olc::olc_save_to_disk(g, zr, kind);
                }
            }
        }
    } else {
        mudlog(
            g,
            &format!("OLC: {} saves ALL info for zone {}.", cname, a),
            PFT,
            lvl,
        );
        // do_olc resolves the save target as real_zone(atoi(arg)*100).
        let Some(znum) = command_atoi(g, ch, &a) else {
            return;
        };
        let Some((zone_vnum, _)) = zone_vnum_bounds(znum) else {
            g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
            return;
        };
        if let Some(zr) = crate::olc::real_zone(g, zone_vnum) {
            for kind in 0..=4 {
                crate::olc::olc_save_to_disk(g, zr, kind);
            }
        }
    }
}

pub fn do_copyto(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, _r) = one_argument(arg);
    if a.is_empty() {
        g.send_to_char(ch, "Format: copyto <room number>\r\n");
        return;
    }
    let Some(iroom) = command_atoi(g, ch, &a) else {
        return;
    };
    let rroom = match g.real_room(iroom) {
        Some(r) => r,
        None => {
            g.send_to_char(
                ch,
                &format!("There is no room with the number {}.\r\n", iroom),
            );
            return;
        }
    };
    if !can_edit_zone(g, ch, real_zone(g, iroom)) {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }
    let cur = g.get_char(ch).and_then(|c| c.in_room);
    let desc = cur
        .map(|r| g.rooms[r].description.clone())
        .unwrap_or_default();
    if !desc.is_empty() {
        g.rooms[rroom].description = desc;
        // olc_add_to_save_list(zone_table[real_zone(iroom)].number, OLC_SAVE_ROOM)
        let zr = real_zone(g, iroom);
        if let Some(znum) = g.zones.get(zr as usize).map(|z| z.number) {
            crate::olc::olc_add_to_save_list(znum, crate::olc::OLC_SAVE_ROOM);
        }
        g.send_to_char(
            ch,
            &format!("You copy the description to room {}.\r\n", iroom),
        );
    } else {
        g.send_to_char(ch, "This room has no description!\r\n");
    }
}

pub fn do_dig(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (dir_s, room_s, _r) = two_arguments(arg);
    if dir_s.is_empty() || room_s.is_empty() {
        g.send_to_char(ch, "Format: dig <dir> <room number>\r\n");
        return;
    }
    let Some(iroom) = command_atoi(g, ch, &room_s) else {
        return;
    };
    let rroom = match g.real_room(iroom) {
        Some(r) if r > 0 => r,
        _ => {
            g.send_to_char(ch, &format!("There is no room with the number {}", iroom));
            return;
        }
    };
    let zr = real_zone(g, iroom);
    if !can_edit_zone(g, ch, zr) {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }
    let dir = match dir_s.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('n') => NORTH,
        Some('e') => EAST,
        Some('s') => SOUTH,
        Some('w') => WEST,
        Some('u') => UP,
        Some('d') => DOWN,
        _ => NORTH,
    };
    let cur = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let here_vnum = g.rooms[cur].number;
    let there_vnum = g.rooms[rroom].number;
    let rev = constants::REV_DIR[dir] as usize;
    // Dig the back-exit (rroom -> here) and the forward exit (here -> rroom).
    g.rooms[rroom].exits[rev] = Some(crate::room::Exit {
        description: None,
        keyword: None,
        exit_info: 0,
        key: NOTHING,
        to_room: here_vnum,
    });
    g.rooms[cur].exits[dir] = Some(crate::room::Exit {
        description: None,
        keyword: None,
        exit_info: 0,
        key: NOTHING,
        to_room: there_vnum,
    });
    if let Some(znum) = g.zones.get(zr as usize).map(|z| z.number) {
        crate::olc::olc_add_to_save_list(znum, crate::olc::OLC_SAVE_ROOM);
    }
    g.send_to_char(
        ch,
        &format!("You make an exit {} to room {}.\r\n", dir_s, iroom),
    );
}

pub fn do_areas(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = arg.trim_start();
    if !argument.is_empty() {
        return;
    }
    let zones: Vec<crate::world::Zone> = g.zones.clone();
    let mut buf = String::new();
    for z in &zones {
        // display_zone_to_buf: skip zones with lvl1==0 (uses min_level here).
        if z.min_level == 0 {
            continue;
        }
        buf.push_str(&format!(
            " &W{:<30.30}&n &b(&Y{} to {}&b)&n {}\r\n",
            z.name,
            z.min_level,
            z.max_level,
            if z.reset_mode != 0 {
                "Open."
            } else {
                "Closed."
            }
        ));
    }
    g.send_to_char(ch, &buf);
}

pub fn do_vwear(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // (cmd, level) listing table.
    let fields: [(&str, u8); 40] = [
        ("nothing", LVL_GOD),
        ("finger", LVL_GOD),
        ("neck", LVL_GOD),
        ("body", LVL_GOD),
        ("head", LVL_GOD),
        ("legs", LVL_GOD),
        ("feet", LVL_GOD),
        ("hands", LVL_GOD),
        ("shield", LVL_GOD),
        ("arms", LVL_GOD),
        ("about", LVL_GOD),
        ("waist", LVL_GOD),
        ("wrist", LVL_GOD),
        ("wield", LVL_GOD),
        ("hold", LVL_GOD),
        ("shoulders", LVL_GOD),
        ("ankle", LVL_GOD),
        ("face", LVL_GOD),
        ("light", LVL_GOD),
        ("scroll", LVL_GOD),
        ("wand", LVL_GOD),
        ("staff", LVL_GOD),
        ("treasure", LVL_GOD),
        ("armor", LVL_GOD),
        ("potion", LVL_GOD),
        ("worn", LVL_GOD),
        ("other", LVL_GOD),
        ("trash", LVL_GOD),
        ("container", LVL_GOD),
        ("liquid", LVL_GOD),
        ("key", LVL_GOD),
        ("food", LVL_GOD),
        ("money", LVL_GOD),
        ("pen", LVL_GOD),
        ("boat", LVL_GOD),
        ("fountain", LVL_GOD),
        ("portal", LVL_GOD),
        ("hpregen", LVL_GOD),
        ("mpregen", LVL_GOD),
        ("mvregen", LVL_GOD),
    ];
    let argument = arg.trim_start();
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You are not godly enough for that!\r\n");
        return;
    };
    if argument.is_empty() {
        let mut buf = String::from("&cItem Listing Options&n:\r\n");
        let mut j = 0;
        for (i, (cmd, lvl)) in fields.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if i32::from(*lvl) <= authority.authority {
                j += 1;
                buf.push_str(&format!(
                    "{:<15}{}",
                    cmd,
                    if j % 5 == 0 { "\r\n" } else { "" }
                ));
            }
        }
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
        return;
    }
    let (field, _value, _r) = two_arguments(argument);
    let mut l = fields.len();
    for (i, (cmd, _)) in fields.iter().enumerate() {
        if cmd.starts_with(&field.to_lowercase()) {
            l = i;
            break;
        }
    }
    if l >= fields.len() {
        g.send_to_char(ch, "Come again?\r\n");
        return;
    }
    if authority.authority < i32::from(fields[l].1) {
        g.send_to_char(ch, "You are not godly enough for that!\r\n");
        return;
    }
    // Indices 1..=17 list by wear-bit; 18..=39 list by item type.
    let wear_bits: [u32; 18] = [0, 1, 2, 3, 4, 5, 6, 7, 9, 8, 10, 11, 12, 13, 14, 15, 16, 17];
    if (1..=17).contains(&l) {
        let bit = 1u32 << wear_bits[l];
        vwear_object(g, ch, bit);
    } else {
        let item_type = match l {
            18 => ObjectType::Light,
            19 => ObjectType::Scroll,
            20 => ObjectType::Wand,
            21 => ObjectType::Staff,
            22 => ObjectType::Treasure,
            23 => ObjectType::Armor,
            24 => ObjectType::Potion,
            25 => ObjectType::Worn,
            26 => ObjectType::Other,
            27 => ObjectType::Trash,
            28 => ObjectType::Container,
            29 => ObjectType::LiqContainer,
            30 => ObjectType::Key,
            31 => ObjectType::Food,
            32 => ObjectType::Money,
            33 => ObjectType::Pen,
            34 => ObjectType::Boat,
            35 => ObjectType::Fountain,
            36 => ObjectType::Portal,
            37 => ObjectType::HpRegen,
            38 => ObjectType::MpRegen,
            39 => ObjectType::MvRegen,
            _ => ObjectType::Other,
        };
        vwear_obj(g, ch, item_type);
    }
}

fn vwear_object(g: &mut GameState, ch: CharId, wear_bit: u32) {
    let mut rows: Vec<(ObjVnum, u32, String)> = g
        .obj_protos
        .values()
        .map(|o| (o.vnum, o.wear_flags.bits(), o.short_desc.clone()))
        .collect();
    rows.sort_by_key(|r| r.0);
    let mut found = 0;
    let mut out = String::new();
    for (vnum, wf, short) in rows {
        if wf & wear_bit != 0 {
            found += 1;
            out.push_str(&format!("{:3}. [{:5}] {}\r\n", found, vnum, short));
        }
    }
    g.send_to_char(ch, &out);
}

fn vwear_obj(g: &mut GameState, ch: CharId, item_type: ObjectType) {
    let mut rows: Vec<(ObjVnum, ObjectType, String)> = g
        .obj_protos
        .values()
        .map(|o| (o.vnum, o.obj_type, o.short_desc.clone()))
        .collect();
    rows.sort_by_key(|r| r.0);
    let mut found = 0;
    let mut out = String::new();
    for (vnum, ot, short) in rows {
        if ot == item_type {
            found += 1;
            out.push_str(&format!("{:3}. [{:5}] {}\r\n", found, vnum, short));
        }
    }
    g.send_to_char(ch, &out);
}

pub fn do_tedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // act.wizard.c do_tedit: edit one of the static text files via the modify.c
    // CON_TEXTED string editor (EditTarget::TextFile here). Each entry mirrors
    // the C fields[] table: (command, level, relative path, buffer size).
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => {
            g.send_to_char(ch, "Get outta here you linkdead head!\r\n");
            return;
        }
    };
    let Some(authorization) = crate::olc::capture_olc_authorization(g, ch) else {
        g.send_to_char(ch, "You do not have text editor permissions.\r\n");
        return;
    };
    if !g.authenticated_command_request_is_current(
        authorization,
        i32::from(LVL_GRGOD),
        3,
        crate::gcmd::GCMD3_IMPOLC,
    ) {
        g.send_to_char(ch, "You do not have text editor permissions.\r\n");
        return;
    }
    let authority = g
        .principal_authority(ch)
        .map(|principal| principal.authority)
        .unwrap_or(-1);
    let (field, _rest) = half_chop(arg);
    // (command, min level, relative path under lib, editor max size). Paths and
    // sizes match db.h *_FILE constants / fields[].size in act.wizard.c.
    let files: [(&str, u8, &str, usize); 11] = [
        ("credits", LVL_IMPL, "text/credits", 2400),
        ("news", LVL_GRGOD, "text/news", 8192),
        ("motd", LVL_GRGOD, "text/motd", 2400),
        ("imotd", LVL_IMPL, "text/imotd", 2400),
        ("help", LVL_GRGOD, "text/help/screen", 2400),
        ("info", LVL_GRGOD, "text/info", 8192),
        ("background", LVL_IMPL, "text/background", 8192),
        ("handbook", LVL_IMPL, "text/handbook", 8192),
        ("policies", LVL_IMPL, "text/policies", 8192),
        ("circlemud", LVL_IMPL, "text/circlemud", 2400),
        ("startup", LVL_IMPL, "text/startup", 8192),
    ];
    if field.is_empty() {
        let mut buf = String::from("Files available to be edited:\r\n");
        let mut i = 1;
        let mut any = false;
        for (cmd, lvl, _, _) in &files {
            if authority >= i32::from(*lvl) {
                buf.push_str(&format!("{:<11.11}", cmd));
                if i % 7 == 0 {
                    buf.push_str("\r\n");
                }
                i += 1;
                any = true;
            }
        }
        if (i - 1) % 7 != 0 {
            buf.push_str("\r\n");
        }
        if !any {
            buf.push_str("None.\r\n");
        }
        g.send_to_char(ch, &buf);
        return;
    }
    let l = files
        .iter()
        .position(|(c, _, _, _)| c.starts_with(&field.to_lowercase()));
    match l {
        Some(i) if authority >= i32::from(files[i].1) => {
            let (_, minimum_authority, rel, size) = files[i];
            if !g.authenticated_command_request_is_current(
                authorization,
                i32::from(minimum_authority),
                3,
                crate::gcmd::GCMD3_IMPOLC,
            ) {
                g.send_to_char(ch, "You are not godly enough for that!\r\n");
                return;
            }
            let path = std::path::Path::new(&g.config.lib_path).join(rel);
            if crate::modify::textfile_edit_busy(&path, conn) {
                g.send_to_char(ch, "That text file is currently being edited.\r\n");
                return;
            }
            // C echoes the current file contents into the editor and seeds it as
            // the abort-restore buffer (backstr); read what is on disk now.
            let current = match std::fs::read_to_string(&path) {
                Ok(current) => current,
                Err(error) => {
                    log::warn!(
                        "SYSERR: TEDIT refused to open '{}': {}",
                        path.display(),
                        error
                    );
                    g.send_to_char(
                        ch,
                        "That text file could not be read safely; no editor was opened.\r\n",
                    );
                    return;
                }
            };
            g.send_to_char(ch, "\x1B[H\x1B[J");
            g.send_to_char(ch, "Edit file below: (/s saves /h for help)\r\n");
            if !current.is_empty() {
                g.send_to_char(ch, &current);
            }
            act(
                g,
                "$n begins editing a scroll.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            crate::modify::start_textfile_editing(
                g,
                conn,
                path,
                &current,
                size,
                authorization,
                i32::from(minimum_authority),
            );
        }
        Some(_) => g.send_to_char(ch, "You are not godly enough for that!\r\n"),
        None => g.send_to_char(ch, "Invalid text editor option.\r\n"),
    }
}

// ===========================================================================
// do_rename
// ===========================================================================
pub fn do_rename(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if is_npc(g, ch) {
        return;
    }
    let (arg1, arg2, _rest) = two_arguments(arg);
    if arg1.is_empty() || arg2.is_empty() {
        g.send_to_char(ch, "Usage: rename <player name> <new name>\r\n");
        return;
    }
    // is_playing(): only an online player can be renamed (the offline rename
    // path is intentionally disabled in C too).
    let victim = match g.find_player_by_name(&arg1) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "That player is not playing at the moment.\r\n");
            return;
        }
    };
    let Some(authorization) = crate::interpreter::authenticated_command_request(g, ch) else {
        g.send_to_char(ch, "You don't have permission to change that name.");
        return;
    };
    let (Some(requester), Some(target)) = (
        authenticated_player_authority(g, ch)
            .filter(|principal| principal.principal == authorization.requester_principal),
        exact_player_authority(g, victim),
    ) else {
        g.send_to_char(ch, "You don't have permission to change that name.");
        return;
    };
    if requester.authority <= target.authority {
        g.send_to_char(ch, "You don't have permission to change that name.");
        return;
    }
    // Name validation (_parse_name / Valid_Name / reserved_word / fill_word):
    // require 2..=MAX_NAME_LENGTH alphabetic chars (structs.h: 20).
    let tmp = cap(&arg2);
    let valid = (2..=20).contains(&tmp.len()) && tmp.chars().all(|c| c.is_ascii_alphabetic());
    if !valid {
        g.send_to_char(ch, "The new name is invalid.\r\n");
        return;
    }
    // Uniqueness: live players AND the offline player index. Without the
    // offline check, the next REPLACE INTO player_main (keyed unique on name)
    // silently DELETES the offline player's entire row (issue #384).
    if g.find_player_by_name(&tmp)
        .map(|id| id != victim)
        .unwrap_or(false)
        || g.player_table.iter().any(|p| {
            p.name.eq_ignore_ascii_case(&tmp)
                && p.idnum != g.get_char(victim).map(|c| c.idnum).unwrap_or(-1)
        })
    {
        g.send_to_char(ch, "There is already a player with that name.\r\n");
        return;
    }
    let oldname = name_of(g, victim);
    let victim_idnum = g.get_char(victim).map(|c| c.idnum).unwrap_or(-1);
    if oldname.eq_ignore_ascii_case(&tmp) {
        g.send_to_char(ch, "That player already has that name.\r\n");
        return;
    }
    if g.player_rename_requests
        .iter()
        .any(|request| request.victim == victim || request.idnum == victim_idnum)
    {
        g.send_to_char(
            ch,
            "A durable rename for that player is already pending.\r\n",
        );
        return;
    }

    // The command path is synchronous, while a correct rename needs a
    // conditional SQL commit plus two name-keyed sidecars.  Queue the exact
    // identities and publish nothing here.  The async Game drain rechecks
    // authority/collisions and reports success only after all durable pieces
    // have committed (#411/#413).
    g.queue_player_rename(authorization, victim, victim_idnum, &oldname, &tmp);
}

// ===========================================================================
// do_peace
// ===========================================================================
pub fn do_peace(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let Some(ch_authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    };
    act(
        g,
        "$n decides that everyone should just be friends.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    g.send_to_room(rnum, "Everything is quite peaceful now.\r\n", None);
    let people = g.rooms[rnum].people.clone();
    for vict in people {
        let fighting = g
            .get_char(vict)
            .map(|c| c.fighting.is_some())
            .unwrap_or(false);
        let target_authority = target_principal_authority(g, vict)
            .map(|target| target.authority)
            .unwrap_or(i32::MAX);
        if fighting && target_authority <= ch_authority.authority {
            if let Some(v) = g.get_char_mut(vict) {
                v.fighting = None;
                if v.position == Position::Fighting {
                    v.position = Position::Standing;
                }
            }
        }
    }
}

// ===========================================================================
// do_citizen
// ===========================================================================
pub fn do_citizen(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, lvl_s, _r) = two_arguments(arg);
    if name.is_empty() || lvl_s.is_empty() {
        g.send_to_char(ch, "Usage: citizen <player> <level>\r\n");
        return;
    }
    let vict = match get_char_vis(g, ch, &name) {
        Some(v) if !is_npc(g, v) => v,
        _ => {
            g.send_to_char(ch, "Who is that?\r\n");
            return;
        }
    };
    let Some(i) = command_atoi(g, ch, &lvl_s) else {
        return;
    };
    if i == 0 || i > 7 {
        g.send_to_char(ch, "Valid levels are 1..7!\r\n");
        return;
    }
    // C: GET_CITIZEN(vict) = i-1 (stored 0..6). Persistence mirrors every other
    // wizard mutation in this build: the in-memory field is written here, and
    // the async disconnect/save loop (game.rs) snapshots the Character and calls
    // db.save_player — which writes the citizen column (database_compat.rs) — so
    // C's save_char(vict, NOWHERE) is honoured when the victim next saves/quits.
    if let Some(v) = g.get_char_mut(vict) {
        v.citizen = (i - 1) as u8;
    }
    g.send_to_char(vict, "You feel different, something has changed.\r\n");
    g.send_to_char(ch, OK);
}

// ===========================================================================
// do_addsnow / do_delsnow
// ===========================================================================
pub fn do_addsnow(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "Okay.\r\n");
    let n = g.rooms.len();
    for i in 0..n.saturating_sub(1) {
        if g.rooms[i].snow < 10 {
            g.rooms[i].snow += 1;
        }
    }
}

pub fn do_delsnow(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "Okay.\r\n");
    let n = g.rooms.len();
    for i in 0..n.saturating_sub(1) {
        if g.rooms[i].snow > 0 {
            g.rooms[i].snow -= 1;
        }
    }
}

// ===========================================================================
// do_tmobdie / do_wrestrict / do_respec / do_questmobs / do_reward
// ===========================================================================
static MOBDIE_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static WEAPONRESTRICTIONS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn do_tmobdie(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let cname = name_of(g, ch);
    if MOBDIE_ENABLED.load(Ordering::Relaxed) {
        MOBDIE_ENABLED.store(false, Ordering::Relaxed);
        let lvl = LVL_GRGOD.max(invis_lev(g, ch) as u8);
        mudlog(g, &format!("(GC) {} has disabled mobdie.", cname), PFT, lvl);
        g.send_to_char(ch, "Mobdie now disabled\r\n");
    } else {
        MOBDIE_ENABLED.store(true, Ordering::Relaxed);
        let lvl = LVL_GRGOD.max(invis_lev(g, ch) as u8);
        mudlog(g, &format!("(GC) {} has enabled mobdie.", cname), PFT, lvl);
        g.send_to_char(ch, "Mobdie now enabled\r\n");
    }
}

pub fn do_wrestrict(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let cname = name_of(g, ch);
    if WEAPONRESTRICTIONS.load(Ordering::Relaxed) {
        WEAPONRESTRICTIONS.store(false, Ordering::Relaxed);
        let lvl = LVL_IMPL.max(invis_lev(g, ch) as u8);
        mudlog(
            g,
            &format!("(GC) {} has disabled weapon restrictions.", cname),
            PFT,
            lvl,
        );
        g.send_to_char(ch, "Weapon restrictions now disabled\r\n");
    } else {
        WEAPONRESTRICTIONS.store(true, Ordering::Relaxed);
        let lvl = LVL_IMPL.max(invis_lev(g, ch) as u8);
        mudlog(
            g,
            &format!("(GC) {} has enabled weapon restrictions.", cname),
            PFT,
            lvl,
        );
        g.send_to_char(ch, "Weapon restrictions now enabled\r\n");
    }
}

pub fn do_respec(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    };
    if authority.authority < i32::from(LVL_IMMORT) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    let cname = name_of(g, authority.principal);
    mudlog(g, &format!("(GC) {} has respec'd.", cname), PFT, LVL_GOD);
    g.send_to_char(ch, "Mob hardcoded SPECS reassigned\r\n");
    // C re-walks mob_index[] re-binding each func pointer. The Rust spec-proc
    // table (spec_assign) is built once and resolved per-mob on demand via
    // special(), so the binding is always live; assign_specs() just asserts the
    // table exists (idempotent OnceLock) — no per-mob pointer to refresh.
    crate::spec_assign::assign_specs(g);
}

pub fn do_questmobs(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (a, b, _r) = two_arguments(arg);
    if a.is_empty() || b.is_empty() {
        g.send_to_char(ch, "Usage: questmobs <from vnum> <to vnum>\r\n");
        return;
    }
    let Some(from) = command_atoi(g, ch, &a) else {
        return;
    };
    let Some(to) = command_atoi(g, ch, &b) else {
        return;
    };
    if from < 0 || to < 0 {
        g.send_to_char(ch, "A NEGATIVE number??\r\n");
        return;
    }
    if to > MAX_ROOM_VNUM {
        g.send_to_char(ch, "Too high a to_number!\r\n");
        return;
    }
    if to < from {
        g.send_to_char(ch, "to_vnum less than from_vnum??\r\n");
        return;
    }
    // MOB_QUEST (1<<19) on a mob's act_flags marks a quest target. read_mobile
    // copies the proto act_flags onto the live mob, so the proto's flag is the
    // live mob's flag — list each prototype in [from,to] that carries it.
    const MOB_QUEST: i64 = 1 << 19;
    let mut buf = String::from("QuestMobs:\r\n\r\n");
    for i in from..=to {
        if let Some(m) = g.mob_protos.get(&i) {
            if m.act_flags & MOB_QUEST != 0 {
                buf.push_str(&format!("({}) {}\r\n", i, m.short_desc));
            }
        }
    }
    g.send_to_char(ch, &buf);
}

pub fn do_reward(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (target, vnum_s, _r) = two_arguments(arg);
    if target.is_empty()
        || vnum_s.is_empty()
        || !vnum_s
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        g.send_to_char(ch, "Usage: reward <playername|room|all> <object vnum>\r\n");
        return;
    }
    let roomonly = target == "room";
    let all = target == "all";
    let playeronly = !roomonly && !all;

    let Some(obj_vnum) = command_atoi(g, ch, &vnum_s) else {
        return;
    };
    if obj_vnum < 0 {
        g.send_to_char(ch, "A NEGATIVE number??\r\n");
        return;
    }
    if !g.obj_protos.contains_key(&obj_vnum) {
        g.send_to_char(ch, "There is no object with that number.\r\n");
        return;
    }

    let cname = name_of(g, ch);

    if playeronly {
        let victim = match g.get_char_room_vis(ch, &target) {
            Some(v) => v,
            None => {
                g.send_to_char(ch, "You don't see anyone by that name here.\r\n");
                return;
            }
        };
        if is_npc(g, victim) {
            g.send_to_char(ch, "Reward a mobile?!?\r\n");
            return;
        }
        if let Some(obj) = g.load_object(obj_vnum) {
            act(
                g,
                "$n rewards $N with $p.",
                false,
                ch,
                Some(obj),
                ActArg::Char(victim),
                To::NotVict,
            );
            act(
                g,
                "$n rewards you for your efforts with $p.",
                false,
                ch,
                Some(obj),
                ActArg::Char(victim),
                To::Vict,
            );
            g.obj_to_char(obj, victim);
            let short = g
                .get_obj(obj)
                .map(|o| o.short_description.clone())
                .unwrap_or_default();
            let vname = name_of(g, victim);
            mudlog(
                g,
                &format!(
                    "[WATCHDOG] {} rewards {} with {} ({})",
                    cname, vname, short, obj_vnum
                ),
                CMP,
                LVL_IMPL,
            );
            g.send_to_char(ch, &format!("You reward {} to {}.\r\n", short, vname));
        }
        return;
    }

    if roomonly {
        let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        let people = g.rooms[rnum].people.clone();
        let mut rewardbuf = String::from("You reward");
        let mut rewardcount = 0;
        for victim in people {
            if victim == ch || is_npc(g, victim) {
                continue;
            }
            if let Some(obj) = g.load_object(obj_vnum) {
                act(
                    g,
                    "$n rewards you for your efforts with $p.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::Char(victim),
                    To::Vict,
                );
                g.obj_to_char(obj, victim);
                let short = g
                    .get_obj(obj)
                    .map(|o| o.short_description.clone())
                    .unwrap_or_default();
                let vname = name_of(g, victim);
                mudlog(
                    g,
                    &format!(
                        "[WATCHDOG] {} rewards {} (room) with {} ({})",
                        cname, vname, short, obj_vnum
                    ),
                    CMP,
                    LVL_IMPL,
                );
                if rewardcount == 0 {
                    rewardbuf.push_str(&format!(" {} to:\r\n{}, ", short, vname));
                } else {
                    rewardbuf.push_str(&format!("{}, ", vname));
                }
                rewardcount += 1;
                if rewardcount % 10 == 0 {
                    rewardbuf.push_str("\r\n");
                }
            }
        }
        rewardbuf.push_str(&format!("a total of {} players.\r\n", rewardcount));
        g.send_to_char(ch, &rewardbuf);
        return;
    }

    if all {
        let players: Vec<CharId> = g.players_by_name.values().copied().collect();
        let mut rewardbuf = String::from("You reward");
        let mut rewardcount = 0;
        for victim in players {
            if is_npc(g, victim)
                || exact_player_authority(g, victim)
                    .is_none_or(|target| target.authority >= i32::from(LVL_IMMORT))
            {
                continue;
            }
            if let Some(obj) = g.load_object(obj_vnum) {
                act(
                    g,
                    "$N rewards $n with $p.",
                    false,
                    victim,
                    Some(obj),
                    ActArg::Char(ch),
                    To::Room,
                );
                act(
                    g,
                    "$n rewards you for your efforts with $p.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::Char(victim),
                    To::Vict,
                );
                g.obj_to_char(obj, victim);
                let short = g
                    .get_obj(obj)
                    .map(|o| o.short_description.clone())
                    .unwrap_or_default();
                let vname = name_of(g, victim);
                mudlog(
                    g,
                    &format!(
                        "[WATCHDOG] {} rewards {} (all) with {} ({})",
                        cname, vname, short, obj_vnum
                    ),
                    CMP,
                    LVL_IMPL,
                );
                if rewardcount == 0 {
                    rewardbuf.push_str(&format!(" {} to:\r\n{}, ", short, vname));
                } else {
                    rewardbuf.push_str(&format!("{}, ", vname));
                }
                rewardcount += 1;
                if rewardcount % 10 == 0 {
                    rewardbuf.push_str("\r\n");
                }
            }
        }
        rewardbuf.push_str(&format!("a total of {} players.\r\n", rewardcount));
        g.send_to_char(ch, &rewardbuf);
    }
}

// ===========================================================================
// do_copyover (Erwin S. Andreasen's seamless reboot)
// ===========================================================================
// Faithful port of act.wizard.c do_copyover. The trick that makes a seamless
// reboot possible despite the async runtime: execv() replaces the process image
// IMMEDIATELY, so no Rust/tokio destructor ever runs and TcpStream::drop never
// closes the live sockets. We clear FD_CLOEXEC on the listener + every playing
// socket so the kernel keeps them open across the exec, write the copyover state
// file (listener fd, then `fd name host` per playing descriptor, then -1), do a
// final synchronous flush to each player (the async writer task dies at exec),
// then exec the same binary with `--copyover <port> <listener_fd>`. The fresh
// process inherits the fds and re-wraps them in main.rs's copyover-recovery path
// (comm.c copyover_recover).
pub fn do_copyover(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    if g.copyover_requested.is_some() {
        g.send_to_char(ch, "A copyover is already being prepared.\n\r");
        return;
    }
    let Some(request) = crate::interpreter::authenticated_command_request(g, ch) else {
        g.send_to_char(ch, "Copyover requires direct authenticated input.\n\r");
        return;
    };
    g.copyover_requested = Some(request);
    g.send_to_char(ch, "Preparing a durable copyover snapshot...\n\r");
}

fn validate_copyover_executable(
    candidate: std::path::PathBuf,
    require_absolute: bool,
    require_root_trust: bool,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::{Context, bail};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if require_absolute && !candidate.is_absolute() {
        bail!("MUD_EXEC_PATH must be absolute");
    }
    let executable = candidate
        .canonicalize()
        .with_context(|| format!("resolve copyover executable {}", candidate.display()))?;
    let metadata = executable
        .metadata()
        .with_context(|| format!("inspect copyover executable {}", executable.display()))?;
    if !metadata.is_file() {
        bail!("copyover executable is not a regular file");
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        bail!("copyover executable has no execute bit");
    }
    let executable_c = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
        executable.as_os_str(),
    ))
    .context("copyover executable path contains a NUL byte")?;
    if unsafe { libc::access(executable_c.as_ptr(), libc::X_OK) } != 0 {
        bail!(
            "copyover executable is not executable by the service identity: {}",
            std::io::Error::last_os_error()
        );
    }
    if require_root_trust {
        if metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
            bail!("production copyover executable must be root-owned and not group/world writable");
        }

        // Canonicalizing closes symlink ambiguity only if an unprivileged
        // process cannot replace a symlink or ancestor afterward. Check both
        // the configured path and resolved path chains; the trusted root owner
        // may still activate releases, while the service identity cannot race
        // validation-to-exec by replacing a component.
        let mut ancestors = std::collections::BTreeSet::new();
        for path in [candidate.as_path(), executable.as_path()] {
            for ancestor in path.ancestors().skip(1) {
                ancestors.insert(ancestor.to_path_buf());
            }
        }
        for ancestor in ancestors {
            let ancestor_metadata = ancestor
                .metadata()
                .with_context(|| format!("inspect copyover path {}", ancestor.display()))?;
            if !ancestor_metadata.is_dir()
                || ancestor_metadata.uid() != 0
                || ancestor_metadata.permissions().mode() & 0o022 != 0
            {
                bail!(
                    "production copyover path component {} is not a trusted root-owned directory",
                    ancestor.display()
                );
            }
        }
    }
    Ok(executable)
}

fn resolve_copyover_executable_from(
    configured_path: Option<std::ffi::OsString>,
    use_mock_db: bool,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::{Context, bail};

    match configured_path {
        Some(path) => validate_copyover_executable(std::path::PathBuf::from(path), true, true),
        None if use_mock_db => validate_copyover_executable(
            std::env::current_exe().context("resolve current executable")?,
            false,
            false,
        ),
        None => bail!("MUD_EXEC_PATH is required when using the real database"),
    }
}

/// Production copyover must execute an explicitly configured, trusted release
/// path. Only mock-DB development/test runs may fall back to the current test
/// or development binary.
fn resolve_copyover_executable(
    config: &crate::config::Config,
) -> anyhow::Result<std::path::PathBuf> {
    resolve_copyover_executable_from(
        config.exec_path.as_ref().map(std::ffi::OsString::from),
        config.use_mock_db,
    )
}

/// Execute the filesystem/socket half only after the async Game shell has
/// confirmed every player record is durable in the database.
pub fn perform_copyover(g: &mut GameState, ch: CharId) {
    use std::os::unix::io::RawFd;

    // C act.wizard.c:1927-1990: flush the OLC save list to disk before the
    // exec, so redit/oedit work survives the reboot (#262).
    if let Err(error) = crate::olc::flush_save_list_to_disk(g) {
        log::warn!("Copyover aborted because pending OLC could not be saved: {error}");
        g.send_to_char(
            ch,
            "Copyover OLC save failed; reboot aborted. Unsaved OLC entries remain pending.\n\r",
        );
        return;
    }

    let listener_fd = crate::state::listener_fd();
    if listener_fd < 3 {
        // No inherited listener => seamless reboot impossible; abort like C's
        // unwritable-file path rather than dropping everyone.
        g.send_to_char(ch, "Copyover unavailable (no listener fd).\n\r");
        return;
    }

    // Resolve the binary to exec FIRST. Production sets MUD_EXEC_PATH to the
    // release-aware `current` path; resolving it here deliberately selects the
    // newly activated release instead of re-executing the old process image.
    // Only explicit mock/development runs retain current_exe() as a fallback;
    // a real-DB service fails closed when MUD_EXEC_PATH is absent.
    let exe = match resolve_copyover_executable(&g.config) {
        Ok(p) => p,
        Err(error) => {
            log::warn!("copyover executable validation failed: {error:#}");
            g.send_to_char(ch, "Copyover file not writeable, aborted.\n\r");
            return;
        }
    };

    let copyover_path = std::path::Path::new(&g.config.lib_path).join("copyover.dat");
    // Build and validate the complete recovery set before mutating descriptors
    // or inheriting a single fd. The structured snapshot carries its record
    // count, completion bit, and checksum; JSON escaping handles arbitrary
    // titles/hosts without delimiter assumptions.
    let conns: Vec<ConnId> = g.descriptors.keys().copied().collect();
    let mut inherit_fds: Vec<RawFd> = Vec::new();
    let mut entries = Vec::new();
    let mut nonplaying = Vec::new();
    for &conn in &conns {
        let (recovery_character, state, fd, host) = match g.descriptors.get(&conn) {
            Some(d) => (
                d.original.or(d.character),
                d.state,
                d.raw_fd,
                d.host.clone(),
            ),
            None => continue,
        };
        if recovery_character.is_none() || state != ConState::Playing {
            nonplaying.push((conn, fd));
            continue;
        }
        let cid = recovery_character.expect("checked copyover recovery character");
        let (player_name, player_id, character_snapshot) = match g.get_char(cid) {
            Some(character) if !character.is_npc => {
                let mut process_exit = character.clone();
                if let Some(room) = character.in_room.and_then(|room| g.rooms.get(room)) {
                    if let Some((x, y)) = room.map_x.zip(room.map_y) {
                        process_exit.tloadroom = -1;
                        process_exit.mapx = i64::from(x);
                        process_exit.mapy = i64::from(y);
                    } else {
                        process_exit.tloadroom = i64::from(room.number);
                        process_exit.mapx = -1;
                        process_exit.mapy = -1;
                    }
                }
                crate::arena::apply_process_exit_state_to_snapshot(g, cid, &mut process_exit);
                (
                    character.get_name().to_string(),
                    character.idnum,
                    crate::copyover::CharacterSnapshot::from_character(&process_exit),
                )
            }
            _ => {
                g.send_to_char(ch, "Copyover found an invalid playing body; aborted.\n\r");
                return;
            }
        };
        let was_crash_dirty = g
            .get_char(cid)
            .is_some_and(|character| character.act_flags & crate::objsave::PLR_CRASH != 0);
        if !crate::objsave::crash_save(g, cid, &g.config.lib_path.clone()) {
            g.send_to_char(ch, "Copyover could not save player objects; aborted.\n\r");
            return;
        }
        if was_crash_dirty {
            // Copyover preflight must be retryable. The crash file is durable,
            // but only a successful exec may discard the live dirty marker.
            if let Some(character) = g.get_char_mut(cid) {
                character.act_flags |= crate::objsave::PLR_CRASH;
            }
        }
        if let Err(error) = crate::alias::write_aliases(&g.config.lib_path, &player_name, player_id)
        {
            log::warn!("copyover alias save failed: {error}");
            g.send_to_char(ch, "Copyover could not save player aliases; aborted.\n\r");
            return;
        }
        entries.push(crate::copyover::ConnectionSnapshot {
            fd,
            host,
            character: character_snapshot,
        });
        inherit_fds.push(fd);
    }
    let payload = crate::copyover::SnapshotPayload {
        listener_fd,
        entries,
    };
    if let Err(error) = crate::copyover::validate_inherited_fds(&payload) {
        log::warn!("copyover fd validation failed: {error:#}");
        g.send_to_char(
            ch,
            "Copyover found an invalid socket set; reboot aborted.\n\r",
        );
        return;
    }
    if let Err(error) = crate::copyover::write_atomic(&copyover_path, payload) {
        log::warn!("copyover snapshot failed: {error:#}");
        g.send_to_char(ch, "Copyover snapshot failed; reboot aborted.\n\r");
        return;
    }

    // Clear FD_CLOEXEC on the listener and every inherited playing fd so the
    // kernel keeps them open across execv (the whole point — without this the
    // exec closes them and the sockets die). Do this before publishing reboot
    // text to sockets; the guard rolls every flag back on any later refusal.
    let inheritance_fds: Vec<RawFd> = std::iter::once(listener_fd)
        .chain(inherit_fds.iter().copied())
        .collect();
    let inheritance_guard =
        match crate::copyover::InheritedFdGuard::clear_for_exec(&inheritance_fds) {
            Ok(guard) => guard,
            Err(error) => {
                log::warn!("copyover could not prepare inherited sockets: {error:#}");
                let _ = std::fs::remove_file(&copyover_path);
                g.send_to_char(
                    ch,
                    "Copyover could not inherit sockets; reboot aborted.\n\r",
                );
                return;
            }
        };

    let cname = name_of(g, ch);
    let reboot_notice = format!(
        "\n\rThe server is being rebooted by {}. Please standby..\n\r",
        cname
    );
    for (_conn, fd) in nonplaying {
        let bye = b"\n\rSorry, we are rebooting. Come back in a minute.\n\r";
        if fd >= 0 {
            let _ = write_fd_all(fd, bye);
        }
    }
    for &conn in &conns {
        let (fd, playing, buffered) = match g.descriptors.get(&conn) {
            Some(descriptor) => (
                descriptor.raw_fd,
                descriptor.state == ConState::Playing
                    && descriptor.original.or(descriptor.character).is_some(),
                descriptor.outbuf.clone(),
            ),
            None => continue,
        };
        if !playing {
            continue;
        }
        // Clone rather than consume the descriptor buffer. Successful exec
        // discards the old process; a failed write/exec leaves the live session
        // byte-for-byte retryable (at worst the client sees duplicate text).
        let mut tail = buffered;
        tail.push_str(&reboot_notice);
        tail.push_str("\n\rRestoring from copyover...\n\r");
        let rendered = crate::connection::render_color(&tail);
        if let Err(error) = write_fd_all(fd, rendered.as_bytes()) {
            log::warn!("copyover socket flush failed for fd {fd}: {error}");
            let _ = std::fs::remove_file(&copyover_path);
            if let Err(rollback_error) = inheritance_guard.rollback() {
                log::error!("copyover fd rollback failed after socket error: {rollback_error:#}");
            }
            g.send_to_char(ch, "Copyover socket flush failed; reboot aborted.\n\r");
            return;
        }
    }

    // Exec the validated release: argv = [exe, "--copyover", "<port>",
    // "<listener_fd>"].
    // CommandExt::exec() is execvp with no fork: on success it never returns.
    let err = std::os::unix::process::CommandExt::exec(
        std::process::Command::new(&exe)
            .arg("--copyover")
            .arg(g.config.port.to_string())
            .arg(listener_fd.to_string()),
    );

    // Only reached if exec failed (C: perror + "Copyover FAILED!").
    eprintln!("do_copyover: exec failed: {}", err);
    if let Err(rollback_error) = inheritance_guard.rollback() {
        log::error!("copyover fd rollback failed after exec error: {rollback_error:#}");
    }
    let _ = std::fs::remove_file(&copyover_path);
    g.send_to_char(ch, "Copyover FAILED!\n\r");
}

fn write_fd_all(fd: std::os::unix::io::RawFd, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written =
            unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if written < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "copyover socket write returned zero",
            ));
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::object::{ExtraFlags, Object, WearFlags};
    use crate::room::Room;
    use crate::world::{MobileProto, ObjectProto, Zone};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn shutdown_options_have_explicit_restart_or_stop_dispositions() {
        for option in ["", "reboot", "REBOOT", "now"] {
            assert_eq!(
                requested_shutdown_disposition(option),
                Some(ProcessDisposition::Restart),
                "{option:?} must request a supervisor restart"
            );
        }
        for option in ["die", "DIE", "pause"] {
            assert_eq!(
                requested_shutdown_disposition(option),
                Some(ProcessDisposition::Stop),
                "{option:?} must stop cleanly"
            );
        }
        assert_eq!(requested_shutdown_disposition("later"), None);
    }

    #[test]
    fn copyover_executable_must_be_absolute_regular_and_executable() {
        assert!(
            validate_copyover_executable(std::path::PathBuf::from("relative/mud"), true, false)
                .is_err()
        );

        let dir = std::env::temp_dir().join(format!(
            "deltamud-copyover-exe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("deltamud");
        std::fs::write(&file, b"test executable").unwrap();

        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&file, permissions.clone()).unwrap();
        assert!(validate_copyover_executable(file.clone(), true, false).is_err());

        permissions.set_mode(0o700);
        std::fs::set_permissions(&file, permissions).unwrap();
        assert_eq!(
            validate_copyover_executable(file.clone(), true, false).unwrap(),
            file.canonicalize().unwrap()
        );
        assert!(
            validate_copyover_executable(file.clone(), true, true).is_err(),
            "a binary below world-writable /tmp must never be a production exec target"
        );

        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn copyover_requires_configured_release_for_real_database() {
        let error = resolve_copyover_executable_from(None, false).unwrap_err();
        assert!(error.to_string().contains("MUD_EXEC_PATH is required"));

        let current = resolve_copyover_executable_from(None, true).unwrap();
        assert_eq!(
            current,
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }

    use chrono::TimeZone;
    fn lock_olc_save_list() -> crate::olc::TestSaveListGuard {
        crate::olc::test_save_list_guard()
    }

    #[test]
    fn low_level_copyover_aborts_before_listener_work_when_olc_flush_fails() {
        let _guard = lock_olc_save_list();
        const MISSING_ZONE: i32 = 29_994;
        crate::olc::olc_add_to_save_list(MISSING_ZONE, crate::olc::OLC_SAVE_ZONE);
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(199), "Builder", LVL_IMPL);

        perform_copyover(&mut g, requester);

        let output = &g.descriptors[&ConnId(199)].outbuf;
        assert!(output.contains("Copyover OLC save failed"));
        assert!(!output.contains("Copyover unavailable"));
        assert!(crate::olc::flush_save_list_to_disk(&mut g).is_err());

        crate::olc::olc_remove_from_save_list(MISSING_ZONE, crate::olc::OLC_SAVE_ZONE);
    }

    fn connected_player(g: &mut GameState, conn: ConnId, name: &str, level: Level) -> CharId {
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        g.descriptors.get_mut(&conn).unwrap().state = ConState::Playing;
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.idnum = conn.0 as i64;
        ch.player.level = level;
        ch.trust = i32::from(level);
        let grants = crate::gcmd::canonical_advance_grants(level, LVL_IMMORT, LVL_IMPL);
        ch.godcmds1 = grants.0;
        ch.godcmds2 = grants.1;
        ch.godcmds3 = grants.2;
        ch.godcmds4 = grants.3;
        let id = g.create_char(ch);
        g.descriptors.get_mut(&conn).unwrap().character = Some(id);
        g.players_by_name.insert(name.to_lowercase(), id);
        id
    }

    #[test]
    fn tedit_uses_persisted_trust_and_refuses_unreadable_source() {
        let root = std::env::temp_dir().join(format!("deltamud-tedit-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("text")).unwrap();
        let mut config = Config::default();
        config.lib_path = root.to_string_lossy().into_owned();
        let mut g = GameState::new(config);

        let demoted_conn = ConnId(7_301);
        let demoted = connected_player(&mut g, demoted_conn, "Demoted", LVL_IMPL);
        {
            let character = g.get_char_mut(demoted).unwrap();
            character.trust = 1;
            character.godcmds3 |= crate::gcmd::GCMD3_IMPOLC;
        }
        do_tedit(&mut g, demoted, "news", 0);
        assert!(
            g.descriptors[&demoted_conn]
                .outbuf
                .contains("do not have text editor permissions")
        );
        assert!(!crate::modify::editing(&g, demoted_conn));

        let trusted_conn = ConnId(7_302);
        let trusted = connected_player(&mut g, trusted_conn, "Trusted", 1);
        {
            let character = g.get_char_mut(trusted).unwrap();
            character.trust = i32::from(LVL_GRGOD);
            character.godcmds3 |= crate::gcmd::GCMD3_IMPOLC;
        }
        do_tedit(&mut g, trusted, "news", 0);
        assert!(
            g.descriptors[&trusted_conn]
                .outbuf
                .contains("could not be read safely")
        );
        assert!(!crate::modify::editing(&g, trusted_conn));

        std::fs::write(root.join("text/news"), "Current news.\n").unwrap();
        g.descriptors.get_mut(&trusted_conn).unwrap().outbuf.clear();
        do_tedit(&mut g, trusted, "news", 0);
        assert!(crate::modify::editing(&g, trusted_conn));
        assert!(
            g.descriptors[&trusted_conn]
                .outbuf
                .contains("Current news.")
        );

        let other_conn = ConnId(7_303);
        let other = connected_player(&mut g, other_conn, "Other", LVL_GRGOD);
        g.get_char_mut(other).unwrap().godcmds3 |= crate::gcmd::GCMD3_IMPOLC;
        do_tedit(&mut g, other, "news", 0);
        assert!(
            g.descriptors[&other_conn]
                .outbuf
                .contains("currently being edited")
        );
        assert!(!crate::modify::editing(&g, other_conn));

        crate::modify::abort_conn(&mut g, trusted_conn);
        g.descriptors.get_mut(&trusted_conn).unwrap().editors.pop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn do_advance_queues_exact_transition_without_publishing_or_mutating() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let vict = connected_player(&mut g, ConnId(2), "Mort", 2);
        g.get_char_mut(imm).unwrap().godcmds1 |= GCMD_ADVANCE;
        g.char_to_room(imm, room);
        g.char_to_room(vict, room);

        crate::interpreter::run_authenticated_command(&mut g, imm, "advance Mort 10");

        let victim = g.get_char(vict).unwrap();
        assert_eq!(victim.player.level, 2);
        assert_ne!(victim.points.exp, exp_to_level(9));
        assert_eq!(g.authority_update_requests.len(), 1);
        let request = &g.authority_update_requests[0];
        assert_eq!(request.expected.level, 2);
        assert_eq!(request.replacement.level, 10);
        assert_eq!(request.replacement.trust, 10);
        assert_eq!(request.replacement.exp, exp_to_level(9));
        assert_eq!(
            (
                request.replacement.godcmds1,
                request.replacement.godcmds2,
                request.replacement.godcmds3,
                request.replacement.godcmds4,
            ),
            (0, 0, 0, 0)
        );

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("queued"));
        assert!(!out.contains("&YOkay.&n\r\n"));
        assert!(!out.contains("has advanced"));
    }

    #[test]
    fn mulder_name_does_not_bypass_snoop_authority() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(1), "Mulder", LVL_GOD);
        let target = connected_player(&mut g, ConnId(2), "Target", LVL_IMPL);

        do_snoop(&mut g, requester, "Target", 0);

        assert_eq!(g.get_char(requester).unwrap().snooping, None);
        assert_eq!(g.get_char(target).unwrap().snoop_by, None);
        assert_eq!(g.descriptors[&ConnId(1)].outbuf, "You can't.\r\n");
    }

    #[test]
    fn switched_low_level_body_does_not_hide_target_principal_from_snoop_gate() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(11), "Requester", LVL_GOD);
        let principal = connected_player(&mut g, ConnId(12), "Principal", LVL_GRGOD);
        let mut vessel = Character::new_npc(9_901);
        vessel.player.name = "Vessel".to_string();
        vessel.player.level = 1;
        let vessel = g.create_char(vessel);

        do_switch(&mut g, principal, "Vessel", 0);
        assert_eq!(g.descriptors[&ConnId(12)].original, Some(principal));
        assert_eq!(g.descriptors[&ConnId(12)].character, Some(vessel));

        do_snoop(&mut g, requester, "Vessel", 0);

        assert_eq!(g.get_char(requester).unwrap().snooping, None);
        assert_eq!(g.get_char(vessel).unwrap().snoop_by, None);
        assert!(
            g.descriptors[&ConnId(11)]
                .outbuf
                .ends_with("You can't.\r\n")
        );
    }

    #[test]
    fn snoop_revalidates_grant_before_each_disclosure() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(21), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(22), "Target", 1);
        g.get_char_mut(requester).unwrap().godcmds1 |= GCMD_SNOOP;

        crate::interpreter::run_authenticated_command(&mut g, requester, "snoop Target");
        assert_eq!(g.get_char(requester).unwrap().snooping, Some(target));
        assert_eq!(g.get_char(target).unwrap().snoop_by, Some(requester));

        g.get_char_mut(requester).unwrap().godcmds1 &= !GCMD_SNOOP;
        g.send_to_char(target, "private output\r\n");

        assert_eq!(g.get_char(requester).unwrap().snooping, None);
        assert_eq!(g.get_char(target).unwrap().snoop_by, None);
        assert!(
            !g.descriptors[&ConnId(21)].outbuf.contains("private output"),
            "revoked snoopers must not receive one final relayed message"
        );
    }

    #[test]
    fn switch_pc_body_exception_uses_exact_principal_trust_not_level() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 1, "Room".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(91), "Requester", LVL_IMPL);
        g.get_char_mut(requester).unwrap().trust = 1;
        let mut victim = Character::new_player(
            "Victim".to_string(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        victim.player.level = 1;
        victim.trust = 1;
        let victim = g.create_char(victim);
        g.char_to_room(requester, room);
        g.char_to_room(victim, room);

        do_switch(&mut g, requester, "Victim", 0);
        assert_eq!(g.descriptors[&ConnId(91)].character, Some(requester));
        assert_eq!(g.descriptors[&ConnId(91)].original, None);

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_IMPL);
        }
        do_switch(&mut g, requester, "Victim", 0);
        assert_eq!(g.descriptors[&ConnId(91)].character, Some(victim));
        assert_eq!(g.descriptors[&ConnId(91)].original, Some(requester));
    }

    #[test]
    fn dc_cannot_disconnect_a_higher_trust_principal_switched_into_a_low_npc() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(21), "Requester", LVL_GOD);
        let principal = connected_player(&mut g, ConnId(22), "Principal", LVL_GRGOD);
        let mut vessel = Character::new_npc(9_902);
        vessel.player.name = "Vessel".to_string();
        vessel.player.level = 1;
        let vessel = g.create_char(vessel);

        do_switch(&mut g, principal, "Vessel", 0);
        let original_state = g.descriptors[&ConnId(22)].state;

        do_dc(&mut g, requester, "22", 0);

        assert_eq!(g.descriptors[&ConnId(22)].state, original_state);
        assert_eq!(g.descriptors[&ConnId(22)].character, Some(vessel));
        assert_eq!(g.descriptors[&ConnId(22)].original, Some(principal));
        assert!(
            g.descriptors[&ConnId(21)]
                .outbuf
                .ends_with("Umm.. maybe that's not such a good idea...\r\n")
        );
    }

    #[test]
    fn switched_principal_authority_blocks_force_transfer_and_teleport() {
        let mut g = GameState::new(Config::default());
        let source = g.add_room(Room::new(100, 1, "Source".into(), String::new()));
        let destination = g.add_room(Room::new(200, 2, "Destination".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(31), "Requester", LVL_GOD);
        let principal = connected_player(&mut g, ConnId(32), "Principal", LVL_GRGOD);
        let mut vessel = Character::new_npc(9_903);
        vessel.player.name = "Vessel".to_string();
        vessel.player.level = 1;
        let vessel = g.create_char(vessel);
        g.char_to_room(requester, destination);
        g.char_to_room(principal, source);
        g.char_to_room(vessel, source);
        do_switch(&mut g, principal, "Vessel", 0);

        do_force(&mut g, requester, "Vessel stand", 0);
        do_trans(&mut g, requester, "Vessel", 0);
        do_teleport(&mut g, requester, "Vessel 200", 0);

        assert_eq!(g.get_char(vessel).unwrap().in_room, Some(source));
        let output = &g.descriptors[&ConnId(31)].outbuf;
        assert!(output.contains("No, no, no!\r\n"));
        assert!(output.contains("Go transfer someone your own size.\r\n"));
        assert!(output.ends_with("Maybe you shouldn't do that.\r\n"));
    }

    #[test]
    fn room_force_skips_a_higher_principal_switched_body() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 1, "Room".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(35), "Requester", LVL_GRGOD);
        let principal = connected_player(&mut g, ConnId(36), "Principal", LVL_IMPL);
        let mut vessel = Character::new_npc(9_905);
        vessel.player.name = "Vessel".to_string();
        vessel.position = Position::Resting;
        let vessel = g.create_char(vessel);
        let mut ordinary = Character::new_npc(NOBODY);
        ordinary.player.name = "Ordinary".to_string();
        ordinary.position = Position::Resting;
        let ordinary = g.create_char(ordinary);
        for character in [requester, principal, vessel, ordinary] {
            g.char_to_room(character, room);
        }
        do_switch(&mut g, principal, "Vessel", 0);

        do_force(&mut g, requester, "room stand", 0);

        assert_eq!(g.get_char(vessel).unwrap().position, Position::Resting);
        assert_eq!(g.get_char(ordinary).unwrap().position, Position::Standing);
    }

    #[test]
    fn purge_preserves_both_roles_of_a_switched_session_and_ordinary_npcs() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 1, "Room".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(41), "Requester", LVL_IMPL);
        let principal = connected_player(&mut g, ConnId(42), "Principal", LVL_GRGOD);
        let mut vessel = Character::new_npc(9_904);
        vessel.player.name = "Vessel".to_string();
        let vessel = g.create_char(vessel);
        let mut ordinary = Character::new_npc(NOBODY);
        ordinary.player.name = "Ordinary".to_string();
        let ordinary = g.create_char(ordinary);
        g.char_to_room(requester, room);
        g.char_to_room(principal, room);
        g.char_to_room(vessel, room);
        g.char_to_room(ordinary, room);
        do_switch(&mut g, principal, "Vessel", 0);

        do_purge(&mut g, requester, "Vessel", 0);
        do_purge(&mut g, requester, "Principal", 0);

        assert!(g.get_char(vessel).is_some());
        assert!(g.get_char(principal).is_some());
        assert_eq!(g.descriptors[&ConnId(42)].character, Some(vessel));
        assert_eq!(g.descriptors[&ConnId(42)].original, Some(principal));

        do_purge(&mut g, requester, "", 0);

        assert!(g.get_char(vessel).is_some());
        assert!(g.get_char(principal).is_some());
        assert!(g.get_char(ordinary).is_none());
        assert_eq!(g.descriptors[&ConnId(42)].character, Some(vessel));
        assert_eq!(g.descriptors[&ConnId(42)].original, Some(principal));
    }

    #[test]
    fn ordinary_descriptorless_npc_keeps_wizard_target_behavior() {
        let mut g = GameState::new(Config::default());
        let source = g.add_room(Room::new(100, 1, "Source".into(), String::new()));
        let destination = g.add_room(Room::new(200, 2, "Destination".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(51), "Requester", LVL_GOD);
        let mut ordinary = Character::new_npc(NOBODY);
        ordinary.player.name = "Ordinary".to_string();
        ordinary.position = Position::Resting;
        let ordinary = g.create_char(ordinary);
        g.char_to_room(requester, destination);
        g.char_to_room(ordinary, source);

        do_force(&mut g, requester, "Ordinary stand", 0);
        assert_eq!(g.get_char(ordinary).unwrap().position, Position::Standing);

        do_trans(&mut g, requester, "Ordinary", 0);
        assert_eq!(g.get_char(ordinary).unwrap().in_room, Some(destination));

        g.char_from_room(ordinary);
        g.char_to_room(ordinary, source);
        do_teleport(&mut g, requester, "Ordinary 200", 0);
        assert_eq!(g.get_char(ordinary).unwrap().in_room, Some(destination));

        do_purge(&mut g, requester, "Ordinary", 0);
        assert!(g.get_char(ordinary).is_none());
    }

    #[test]
    fn purge_keeps_ordinary_connected_player_close_and_extract_behavior() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 1, "Room".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(61), "Requester", LVL_IMPL);
        let victim = connected_player(&mut g, ConnId(62), "Victim", 10);
        g.char_to_room(requester, room);
        g.char_to_room(victim, room);

        do_purge(&mut g, requester, "Victim", 0);

        assert!(g.get_char(victim).is_none());
        assert_eq!(g.descriptors[&ConnId(62)].state, ConState::Close);
        assert_eq!(g.descriptors[&ConnId(62)].character, None);
        assert_eq!(g.descriptors[&ConnId(62)].original, None);
    }

    #[test]
    fn purge_player_hierarchy_uses_authenticated_trust_for_every_caller_level() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 1, "Room".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(63), "Requester", LVL_IMPL);
        let victim = connected_player(&mut g, ConnId(64), "Victim", 1);
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_IMMORT);
        g.get_char_mut(victim).unwrap().trust = i32::from(LVL_GOD);
        g.char_to_room(requester, room);
        g.char_to_room(victim, room);

        do_purge(&mut g, requester, "Victim", 0);

        assert!(
            g.char_exists(victim),
            "display level must not let lower trust purge a higher-trust player"
        );
        assert!(
            g.descriptors[&ConnId(63)]
                .outbuf
                .ends_with("Fuuuuuuuuu!\r\n")
        );

        g.get_char_mut(requester).unwrap().player.level = 1;
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_IMPL);
        g.get_char_mut(victim).unwrap().trust = 1;
        do_purge(&mut g, requester, "Victim", 0);
        assert!(
            !g.char_exists(victim),
            "authenticated Implementor trust must work independently of display level"
        );
    }

    #[test]
    fn malformed_descriptor_alias_fails_closed_at_the_shared_authority_gate() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(71), "Requester", LVL_IMPL);
        let victim = connected_player(&mut g, ConnId(72), "Victim", 1);
        g.get_char_mut(victim).unwrap().desc = Some(ConnId(999));
        let original_state = g.descriptors[&ConnId(72)].state;

        do_dc(&mut g, requester, "72", 0);

        assert_eq!(g.descriptors[&ConnId(72)].state, original_state);
        assert!(
            g.descriptors[&ConnId(71)]
                .outbuf
                .ends_with("Umm.. maybe that's not such a good idea...\r\n")
        );
    }

    #[test]
    fn descriptor_controlled_npc_without_an_original_principal_fails_closed() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(75), "Requester", LVL_IMPL);
        let conn = ConnId(76);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        g.descriptors.get_mut(&conn).unwrap().state = ConState::Playing;
        let mut npc = Character::new_npc(9_906);
        npc.player.name = "Broken".to_string();
        npc.desc = Some(conn);
        let npc = g.create_char(npc);
        g.descriptors.get_mut(&conn).unwrap().character = Some(npc);

        do_dc(&mut g, requester, "76", 0);

        assert_eq!(g.descriptors[&conn].state, ConState::Playing);
        assert_eq!(g.descriptors[&conn].character, Some(npc));
        assert_eq!(g.descriptors[&conn].original, None);
    }

    #[test]
    fn bulk_force_and_transfer_operate_on_the_active_switched_body() {
        let mut g = GameState::new(Config::default());
        let source = g.add_room(Room::new(100, 1, "Source".into(), String::new()));
        let destination = g.add_room(Room::new(200, 2, "Destination".into(), String::new()));
        let requester = connected_player(&mut g, ConnId(81), "Requester", LVL_IMPL);
        let principal = connected_player(&mut g, ConnId(82), "Principal", LVL_GRGOD);
        let mut vessel = Character::new_npc(9_907);
        vessel.player.name = "Vessel".to_string();
        vessel.position = Position::Resting;
        let vessel = g.create_char(vessel);
        g.char_to_room(requester, destination);
        g.char_to_room(principal, source);
        g.char_to_room(vessel, source);
        do_switch(&mut g, principal, "Vessel", 0);

        do_force(&mut g, requester, "all stand", 0);
        assert_eq!(g.get_char(vessel).unwrap().position, Position::Standing);

        do_trans(&mut g, requester, "all", 0);
        assert_eq!(g.get_char(vessel).unwrap().in_room, Some(destination));
        assert_eq!(g.get_char(principal).unwrap().in_room, Some(source));
        assert_eq!(g.descriptors[&ConnId(82)].character, Some(vessel));
        assert_eq!(g.descriptors[&ConnId(82)].original, Some(principal));
    }

    #[test]
    fn admin_teleport_uses_forced_arena_departure_without_penalty() {
        let mut g = GameState::new(Config::default());
        let arena_room = g.add_room(Room::new(4801, 48, "Arena".into(), String::new()));
        let outside = g.add_room(Room::new(3001, 30, "Temple".into(), String::new()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let victim = connected_player(&mut g, ConnId(2), "Victim", 10);
        g.char_to_room(imm, outside);
        g.char_to_room(victim, arena_room);
        if let Some(c) = g.get_char_mut(victim) {
            c.wimp_level = 12;
            c.recall_level = 34;
            c.affect_flags = crate::flags::AFF_INVISIBLE;
            crate::gold::set(c, crate::gold::Account::Carried, 9_999);
        }
        crate::arena::set_stat_for_test(&mut g, victim, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut g, victim);
        g.get_char_mut(victim).unwrap().affect_flags = crate::flags::AFF_BLIND;

        do_teleport(&mut g, imm, "Victim 3001", 0);

        let victim_state = g.get_char(victim).unwrap();
        assert_eq!(victim_state.in_room, Some(outside));
        assert_eq!(victim_state.affect_flags, crate::flags::AFF_INVISIBLE);
        assert_eq!(victim_state.wimp_level, 12);
        assert_eq!(victim_state.recall_level, 34);
        assert_eq!(victim_state.points.gold, 9_999);
        assert_eq!(
            crate::arena::arena_stat(&g, victim),
            crate::arena::ARENA_NOT
        );
        assert_eq!(g.player_save_requests, vec![victim]);
    }

    #[test]
    fn admin_transfer_uses_forced_arena_departure_without_penalty() {
        let mut g = GameState::new(Config::default());
        let arena_room = g.add_room(Room::new(4801, 48, "Arena".into(), String::new()));
        let outside = g.add_room(Room::new(3001, 30, "Temple".into(), String::new()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let victim = connected_player(&mut g, ConnId(2), "Victim", 10);
        g.char_to_room(imm, outside);
        g.char_to_room(victim, arena_room);
        if let Some(c) = g.get_char_mut(victim) {
            c.wimp_level = 12;
            c.recall_level = 34;
            c.affect_flags = crate::flags::AFF_INVISIBLE;
            crate::gold::set(c, crate::gold::Account::Carried, 9_999);
        }
        crate::arena::set_stat_for_test(&mut g, victim, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut g, victim);
        g.get_char_mut(victim).unwrap().affect_flags = crate::flags::AFF_BLIND;

        do_trans(&mut g, imm, "Victim", 0);

        let victim_state = g.get_char(victim).unwrap();
        assert_eq!(victim_state.in_room, Some(outside));
        assert_eq!(victim_state.affect_flags, crate::flags::AFF_INVISIBLE);
        assert_eq!(victim_state.wimp_level, 12);
        assert_eq!(victim_state.recall_level, 34);
        assert_eq!(victim_state.points.gold, 9_999);
        assert_eq!(
            crate::arena::arena_stat(&g, victim),
            crate::arena::ARENA_NOT
        );
        assert_eq!(g.player_save_requests, vec![victim]);
    }

    fn object_proto(vnum: ObjVnum, obj_type: ObjectType, short: &str) -> ObjectProto {
        ObjectProto {
            vnum,
            name: short.to_string(),
            short_desc: short.to_string(),
            description: format!("{} is here.", short),
            obj_type,
            wear_flags: WearFlags::TAKE,
            extra_flags: ExtraFlags::empty(),
            weight: 1,
            cost: 0,
            rent: 0,
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

    fn mobile_proto(vnum: MobVnum, short: &str, act_flags: i64) -> MobileProto {
        MobileProto {
            vnum,
            name: short.to_string(),
            short_desc: short.to_string(),
            long_desc: format!("{} is here.\r\n", short),
            description: String::new(),
            level: 1,
            hitpoints: 1,
            hit_dice: (0, 0, 1),
            experience: 0,
            gold: 0,
            position: Position::Standing,
            default_pos: Position::Standing,
            sex: Gender::Neutral,
            alignment: 0,
            act_flags,
            affect_flags: 0,
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 1,
            power: 0,
            mpower: 0,
            defense: 0,
            mdefense: 0,
            technique: 0,
            abilities: None,
            attack_type: 0,
        }
    }

    fn test_zone(number: i32, top: RoomVnum, builders: &str) -> Zone {
        Zone {
            number,
            name: format!("Zone {}", number),
            builders: builders.to_string(),
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
        }
    }

    #[test]
    fn do_stat_object_reports_object_bitvector() {
        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let mut obj = Object::new(1234, "cloak".to_string(), "a shimmering cloak".to_string());
        obj.bitvector = crate::flags::AFF_INVISIBLE;
        let obj = g.create_obj(obj);

        do_stat_object(&mut g, imm, obj);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Set char bits : INVIS"));
        assert!(!out.contains("Set char bits : NOBITS"));
    }

    #[test]
    fn do_vwear_maps_boat_keyword_to_boat_objects() {
        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.obj_protos
            .insert(2222, object_proto(2222, ObjectType::Boat, "a river boat"));
        g.obj_protos
            .insert(3333, object_proto(3333, ObjectType::Other, "a plain thing"));

        do_vwear(&mut g, imm, "boat", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("[ 2222] a river boat"));
        assert!(!out.contains("[ 3333] a plain thing"));
    }

    #[test]
    fn do_mcasters_lists_magic_user_specs_with_caster_flag_type() {
        const MOB_CASTER: i64 = 1 << 21;

        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.mob_protos
            .insert(4000, mobile_proto(4000, "a flagged caster", MOB_CASTER));
        // Mob 1 (puff) carries a spec proc that is NOT magic_user, and no flag.
        g.mob_protos.insert(1, mobile_proto(1, "puff", 0));

        do_mcasters(&mut g, imm, "", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.starts_with("Spellcasting mobs:\r\n"));
        // db.c:1346-1349 binds magic_user to every MOB_CASTER prototype at load,
        // so C's `mob_index[i].func == magic_user` lists it (label CASTER).
        assert!(out.contains("[4000] a flagged caster (Type: CASTER)\r\n"));
        // A mob with neither the flag nor a magic_user binding is not listed.
        assert!(!out.contains("puff"));
    }

    // ---- #215: `show stats` counts registered players from the index -------

    #[test]
    fn show_stats_counts_registered_players_from_the_persistent_index() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.char_to_room(imm, room);
        // Three persistent rows, of which only `Imm` is instantiated.
        g.update_player_index(1, "Imm", LVL_IMPL, 0, "test");
        g.update_player_index(2, "Offline", 12, 0, "test");
        g.update_player_index(3, "Gone", 20, 0, "test");

        show_stats_case_4(&mut g, imm);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Current stats:"), "out: {}", out);
        assert!(out.contains("players in game"), "out: {}", out);
        let line = out
            .lines()
            .find(|l| l.contains("registered"))
            .expect("registered line");
        assert!(line.contains("3"), "expected 3 registered, got: {}", line);
    }

    const INSPECTION_ROUTES: [(&str, &str); 3] = [
        ("stat-player", "IDNum:"),
        ("stat-file", "IDNum:"),
        ("show-player", "Player:"),
    ];

    fn run_inspection_route(g: &mut GameState, requester: CharId, route: &str, target: &str) {
        match route {
            "stat-player" => do_stat(g, requester, &format!("player {target}"), 0),
            "stat-file" => do_stat(g, requester, &format!("file {target}"), 0),
            "show-player" => do_show(g, requester, &format!("player {target}"), 0),
            _ => panic!("unknown inspection route {route}"),
        }
    }

    #[test]
    fn online_inspection_authority_matrix_allows_lower_and_equal_but_denies_higher() {
        for (route, record_marker) in INSPECTION_ROUTES {
            for (target_level, allowed) in
                [(LVL_GOD - 1, true), (LVL_GOD, true), (LVL_GOD + 1, false)]
            {
                let mut g = GameState::new(Config::default());
                let requester = connected_player(&mut g, ConnId(1), "Requester", LVL_GOD);
                let target = connected_player(&mut g, ConnId(2), "Target", target_level);
                g.get_char_mut(target).unwrap().idnum = 9_409_001;

                run_inspection_route(&mut g, requester, route, "Target");

                let output = &g.descriptors[&ConnId(1)].outbuf;
                assert_eq!(
                    output.contains(record_marker),
                    allowed,
                    "route={route} target_level={target_level} output={output:?}"
                );
                assert_eq!(
                    output.contains(PLAYER_INSPECTION_DENIED.trim()),
                    !allowed,
                    "route={route} target_level={target_level} output={output:?}"
                );
            }
        }
    }

    #[test]
    fn offline_inspection_queue_matrix_uses_the_same_authority_rule() {
        for (route, _) in INSPECTION_ROUTES {
            for (target_level, allowed) in
                [(LVL_GOD - 1, true), (LVL_GOD, true), (LVL_GOD + 1, false)]
            {
                let mut g = GameState::new(Config::default());
                let requester = connected_player(&mut g, ConnId(1), "Requester", LVL_GOD);
                g.update_player_index(9_409_002, "Target", target_level, 0, "test");

                run_inspection_route(&mut g, requester, route, "Target");

                let output = &g.descriptors[&ConnId(1)].outbuf;
                assert_eq!(
                    g.offline_ops.len(),
                    usize::from(allowed),
                    "route={route} target_level={target_level} output={output:?}"
                );
                assert_eq!(
                    output.contains(PLAYER_INSPECTION_DENIED.trim()),
                    !allowed,
                    "route={route} target_level={target_level} output={output:?}"
                );
                if allowed {
                    assert_eq!(
                        g.offline_ops[0].authority,
                        OfflineOpAuthority::InspectPlayer
                    );
                }
            }
        }
    }

    #[test]
    fn show_rent_rejects_paths_and_requires_an_indexed_player() {
        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);

        do_show(&mut g, imm, "rent ../../etc/passwd", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert_eq!(out, "There is no such player.\r\n");
    }

    /// `show stats` case 4 (act.wizard.c:2780-2811) — the dispatcher arm that
    /// prints the "Current stats:" block.
    fn show_stats_case_4(g: &mut GameState, ch: CharId) {
        do_show(g, ch, "stats", 0);
    }

    #[test]
    fn do_stat_character_uses_player_time_and_practice_fields() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(2), "Target", 20);
        g.char_to_room(imm, room);
        g.char_to_room(target, room);
        {
            let c = g.get_char_mut(target).unwrap();
            c.player.time_birth = chrono::Utc::now().timestamp() - 2 * 17 * 35 * 24 * 75;
            c.player.time_played = 2 * 3600 + 5 * 60;
            c.last_logon = chrono::Utc
                .timestamp_opt(1_700_000_000, 0)
                .single()
                .unwrap();
            c.spells_to_learn = 7;
        }

        do_stat_character(&mut g, imm, target);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(!out.contains("Created: [Unknown]"));
        assert!(!out.contains("Last Logon: [Unknown]"));
        assert!(out.contains("Played [2h 5m]"));
        assert!(out.contains("Age [19]"));
        assert!(out.contains("STL[7]"));
    }

    #[test]
    fn do_show_player_uses_player_time_and_lessons() {
        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(2), "Target", 20);
        {
            let c = g.get_char_mut(target).unwrap();
            c.player.time_birth = 1_600_000_000;
            c.player.time_played = 3 * 3600 + 12 * 60;
            c.last_logon = chrono::Utc
                .timestamp_opt(1_700_000_000, 0)
                .single()
                .unwrap();
            c.spells_to_learn = 9;
        }

        do_show(&mut g, imm, "player Target", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Lessons: 9"));
        assert!(!out.contains("Started: Unknown"));
        assert!(!out.contains("Last: Unknown"));
        assert!(out.contains("Played:   3h 12m"));
    }

    #[test]
    fn set_passwd_queues_a_typed_hash_without_claiming_durable_success() {
        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(2), "Mort", 20);
        g.get_char_mut(imm).unwrap().idnum = 101;
        g.get_char_mut(target).unwrap().idnum = 202;

        crate::interpreter::run_authenticated_command(&mut g, imm, "set Mort passwd newpass");

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Password change for Mort queued.\r\n"));
        assert!(!out.contains("newpass"));
        assert!(g.get_char(target).unwrap().pending_password_hash.is_none());
        assert_eq!(g.password_update_requests.len(), 1);
        let request = &g.password_update_requests[0];
        assert_eq!(request.authorization.requester_body, imm);
        assert_eq!(request.victim, target);
        assert_eq!(request.idnum, 202);
        assert_eq!(request.name, "Mort");
        assert_eq!(request.plaintext_password, "newpass");

        crate::interpreter::run_authenticated_command(&mut g, imm, "set Mort passwd xy");
        assert_eq!(g.password_update_requests.len(), 1);
        assert!(
            g.descriptors[&ConnId(1)]
                .outbuf
                .contains("Password must be between 3 and 64 bytes.")
        );
        crate::interpreter::run_authenticated_command(
            &mut g,
            imm,
            &format!(
                "set Mort passwd {}",
                "x".repeat(crate::password::MAX_PASSWORD_INPUT_BYTES + 1)
            ),
        );
        assert_eq!(g.password_update_requests.len(), 1);
    }

    #[test]
    fn set_passwd_rejects_grgod_target() {
        let mut g = GameState::new(Config::default());
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(2), "Greater", LVL_GRGOD);

        crate::interpreter::run_authenticated_command(&mut g, imm, "set Greater passwd newpass");

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("You cannot change that.\r\n"));
        assert!(g.get_char(target).unwrap().pending_password_hash.is_none());
        assert!(g.password_update_requests.is_empty());
    }

    #[test]
    fn set_idnum_authorizes_the_implementor_role_not_player_id_one() {
        let mut g = GameState::new(Config::default());
        let implementor = connected_player(&mut g, ConnId(1), "Admin", LVL_IMPL);
        g.get_char_mut(implementor).unwrap().idnum = 42;
        let mut mobile = Character::new_npc(9001);
        mobile.player.name = "test mobile".to_string();
        mobile.idnum = 9001;
        let target = g.create_char(mobile);

        do_set(&mut g, implementor, "mobile idnum 777", 0);

        assert_eq!(g.get_char(target).unwrap().idnum, 777);
    }

    #[test]
    fn set_idnum_rejects_an_id_one_non_implementor_impostor() {
        let mut g = GameState::new(Config::default());
        let impostor = connected_player(&mut g, ConnId(1), "Impostor", 1);
        {
            let impostor = g.get_char_mut(impostor).unwrap();
            impostor.idnum = 1;
            // Pass do_set's historical IS_GOD admission so this specifically
            // exercises role/field authorization rather than command lookup.
            impostor.godcmds1 = crate::gcmd::GCMD_GEN;
        }
        let mut mobile = Character::new_npc(9002);
        mobile.player.name = "test mobile".to_string();
        mobile.idnum = 9002;
        let target = g.create_char(mobile);

        let idnum_mode = SET_FIELDS
            .iter()
            .position(|field| field.switchnum == 44)
            .unwrap();
        assert!(!perform_set(
            &mut g,
            Some(impostor),
            target,
            idnum_mode,
            "888"
        ));

        assert_eq!(g.get_char(target).unwrap().idnum, 9002);
        assert!(
            g.descriptors[&ConnId(1)]
                .outbuf
                .contains("You are not godly enough for that!")
        );
    }

    #[test]
    fn do_dig_denies_unowned_destination_zone() {
        let _guard = lock_olc_save_list();
        let mut g = GameState::new(Config::default());
        g.zones.push(test_zone(1, 199, "Builder"));
        g.zones.push(test_zone(2, 299, "Other"));
        let here = g.add_room(Room::new(
            100,
            0,
            "Here".to_string(),
            "The starting room.".to_string(),
        ));
        let there = g.add_room(Room::new(
            200,
            1,
            "There".to_string(),
            "The target room.".to_string(),
        ));
        let builder = connected_player(&mut g, ConnId(1), "Builder", LVL_GRGOD);
        g.char_to_room(builder, here);

        do_dig(&mut g, builder, "east 200", 0);

        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("You don't have permissions to that zone.\r\n")
        );
        assert!(g.room(here).exits[EAST].is_none());
        assert!(g.room(there).exits[WEST].is_none());
    }

    #[test]
    fn do_dig_marks_destination_zone_room_save() {
        let _guard = lock_olc_save_list();
        crate::olc::olc_remove_from_save_list(2, crate::olc::OLC_SAVE_ROOM);
        let mut g = GameState::new(Config::default());
        g.zones.push(test_zone(1, 199, "Builder"));
        g.zones.push(test_zone(2, 299, "Builder"));
        let here = g.add_room(Room::new(
            100,
            0,
            "Here".to_string(),
            "The starting room.".to_string(),
        ));
        let there = g.add_room(Room::new(
            200,
            1,
            "There".to_string(),
            "The target room.".to_string(),
        ));
        let builder = connected_player(&mut g, ConnId(1), "Builder", LVL_GRGOD);
        g.char_to_room(builder, here);

        do_dig(&mut g, builder, "east 200", 0);

        assert_eq!(
            g.room(here).exits[EAST].as_ref().map(|e| e.to_room),
            Some(200)
        );
        assert_eq!(
            g.room(there).exits[WEST].as_ref().map(|e| e.to_room),
            Some(100)
        );
        crate::olc::olc_saveinfo(&mut g, builder);
        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("Rooms for zone 2")
        );
        crate::olc::olc_remove_from_save_list(2, crate::olc::OLC_SAVE_ROOM);
    }

    // ---- #195: sprintbit fidelity (utils.c:402-423) -----------------------

    /// A table whose names run out well before bit 63 ("\n"-terminated, as the
    /// C `*_bits[]` tables are).
    const SHORT_TABLE: &[&str] = &["ALPHA", "BETA", "\n"];

    #[test]
    fn sprintbit_negative_vector_is_invalid_bitvector() {
        assert_eq!(sprintbit(-1, SHORT_TABLE), "<INVALID BITVECTOR>");
        assert_eq!(sprintbit(-(1i64 << 40), SHORT_TABLE), "<INVALID BITVECTOR>");
    }

    #[test]
    fn sprintbit_set_bits_above_the_table_are_undefined() {
        // Bits 0 and 1 have names; bit 40 is past the terminator and must still
        // render (C keeps shifting until the vector is exhausted).
        let bits = (1i64) | (1i64 << 1) | (1i64 << 40);
        assert_eq!(sprintbit(bits, SHORT_TABLE), "ALPHA BETA UNDEFINED ");
        // A lone out-of-table bit: no name, but not NOBITS either.
        assert_eq!(sprintbit(1i64 << 40, SHORT_TABLE), "UNDEFINED ");
        // Bit 63 sets the sign bit of C's signed `long`, so it takes the
        // negative-vector branch there too.
        assert_eq!(sprintbit(1i64 << 63, SHORT_TABLE), "<INVALID BITVECTOR>");
    }

    #[test]
    fn sprintbit_zero_is_nobits() {
        assert_eq!(sprintbit(0, SHORT_TABLE), "NOBITS ");
    }

    // ---- #200: cdsr() map-coordinate addressing in find_target_room -------

    fn lib_with_worldmap(name: &str, size: usize) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!(
            "deltamud-wiz-{}-{}-{}",
            name,
            std::process::id(),
            unique
        ));
        std::fs::create_dir_all(dir.join("world")).unwrap();
        let row = ".".repeat(size);
        let mut map = String::from(
            "NewSector: .\n\
SectName: Field\n\
SectShow: .\n\
SectMove: 1\n\
SectSect: Field\n\
EndSector\n\
WorldMap:\n",
        );
        for _ in 0..size {
            map.push_str(&row);
            map.push('\n');
        }
        map.push_str("~\n");
        std::fs::write(dir.join("world").join("worldmap"), map).unwrap();
        dir
    }

    #[test]
    fn goto_with_map_coordinates_lands_on_the_surface_room() {
        let dir = lib_with_worldmap("cdsr", 12);
        let cfg = Config {
            lib_path: dir.to_string_lossy().to_string(),
            ..Config::default()
        };
        let mut g = GameState::new(cfg);
        crate::maputils::integrate_map_rooms(&mut g);
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let start = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        g.char_to_room(imm, start);

        let want = g.map_coords_to_rnum(3, 7).expect("map room spliced in");
        do_goto(&mut g, imm, "3x7", 0);

        assert_eq!(g.get_char(imm).unwrap().in_room, Some(want));
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(!out.contains("No room exists with that number."));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn goto_still_rejects_a_nonexistent_numeric_vnum() {
        let dir = lib_with_worldmap("cdsr-vnum", 8);
        let cfg = Config {
            lib_path: dir.to_string_lossy().to_string(),
            ..Config::default()
        };
        let mut g = GameState::new(cfg);
        crate::maputils::integrate_map_rooms(&mut g);
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let start = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        g.char_to_room(imm, start);

        do_goto(&mut g, imm, "999999", 0);

        assert_eq!(g.get_char(imm).unwrap().in_room, Some(start));
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("No room exists with that number."));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn goto_rejects_numeric_overflow_instead_of_resolving_room_zero() {
        let mut g = GameState::new(Config::default());
        let zero = g.add_room(Room::new(0, 0, "Void".to_string(), "Void.".to_string()));
        let start = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.char_to_room(imm, start);

        do_goto(&mut g, imm, "999999999999999999999999999999", 0);

        assert_eq!(g.get_char(imm).unwrap().in_room, Some(start));
        assert_ne!(g.get_char(imm).unwrap().in_room, Some(zero));
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Invalid or out-of-range room number."));
    }

    // ---- #204: stat's MaxWeapon / practices-per / regen rates --------------

    #[test]
    fn do_stat_character_reports_maxweapon_practices_per_and_regen() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let vict = connected_player(&mut g, ConnId(2), "Mort", 10);
        g.char_to_room(imm, room);
        g.char_to_room(vict, room);
        {
            let c = g.get_char_mut(vict).unwrap();
            c.spells_to_learn = 7;
            c.aff_abils.intel = 18; // int_app[18].learn == 50
            c.aff_abils.wis = 18; // wis_app[18].bonus == 5
        }
        let hit_regen = crate::limits::hit_gain(&g, vict);

        do_stat_character(&mut g, imm, vict);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        // lvl_maxdmg_weapon[10] == 20 (config.c).
        assert!(out.contains("MaxWeapon: [20]"), "out: {}", out);
        assert!(out.contains("(STL[7]/per[50]/NSTL[5])"), "out: {}", out);
        // The regen column is the live rate, not a literal 0.
        assert!(hit_regen != 0, "expected a non-zero hit regen");
        assert!(out.contains(&format!("+{}", hit_regen)), "out: {}", out);
    }

    // ---- #213: stat's Spec-Proc / attack-type / connected lookups ----------

    #[test]
    fn do_stat_character_reports_mob_spec_proc_attack_type_and_connection() {
        let mut g = GameState::new(Config::default());
        crate::spec_assign::assign_specs(&mut g);
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.char_to_room(imm, room);
        // Mob vnum 1 (puff) carries a statically assigned spec proc.
        g.mob_protos.insert(1, mobile_proto(1, "puff", 0));
        g.mob_protos.get_mut(&1).unwrap().attack_type = 4; // attack_hit_text[4] == "bite"
        let mob = g.create_char(Character::new_npc(1));
        g.char_to_room(mob, room);

        do_stat_character(&mut g, imm, mob);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Mob Spec-Proc: Exists"), "out: {}", out);
        assert!(out.contains(", Attack type: bite"), "out: {}", out);
        // C only prints the Connected field when the char has a descriptor; a
        // freshly loaded mobile does not.
        assert!(!out.contains(", Connected:"), "out: {}", out);
    }

    #[test]
    fn do_stat_character_reports_connected_state_for_a_live_player() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.char_to_room(imm, room);
        g.descriptors.get_mut(&ConnId(1)).unwrap().state = ConState::Menu;

        do_stat_character(&mut g, imm, imm);

        // sprinttype(d->connected, connected_types): CON_MENU -> "Main Menu".
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains(", Connected: Main Menu"), "out: {}", out);
    }

    #[test]
    fn stat_reports_room_and_object_spec_procs() {
        let mut g = GameState::new(Config::default());
        crate::spec_assign::assign_specs(&mut g);
        // vnum 34000 is the (production-inert) pet-shop registration; 3031 is
        // now zone 30's Tower Magazine (COMPATIBILITY.md collisions table).
        let plain = g.add_room(Room::new(100, 0, "Plain".to_string(), "Plain.".to_string()));
        let petshop = g.add_room(Room::new(
            34000,
            30,
            "Pets".to_string(),
            "Pets.".to_string(),
        ));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        g.char_to_room(imm, petshop);
        do_stat_room(&mut g, imm);
        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("SpecProc: Exists")
        );

        // Object vnum 20 is the generic portal (ASSIGNOBJ 20 portal).
        let obj = g.create_obj(Object::new(
            20,
            "portal".to_string(),
            "a shimmering portal".to_string(),
        ));
        g.descriptors.get_mut(&ConnId(1)).unwrap().outbuf.clear();
        g.char_to_room(imm, plain);
        do_stat_object(&mut g, imm, obj);
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("SpecProc: Exists"), "out: {}", out);
    }

    // ---- #205: IS_GOD is a granted-command test, not a level test ----------

    #[test]
    fn set_admits_a_bit_holding_mortal_and_stat_reports_god_commands() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let mortal = connected_player(&mut g, ConnId(2), "Granted", 100);
        g.char_to_room(imm, room);
        g.char_to_room(mortal, room);
        g.get_char_mut(mortal).unwrap().godcmds4 = 1; // set cmdgeneral on

        // (a) A sub-immortal with bits reaches set's field list.
        do_set(&mut g, mortal, "Granted God_Commands on", 0);
        let out = &g.descriptors.get(&ConnId(2)).unwrap().outbuf;
        assert!(!out.contains("Huh?!?"), "out: {}", out);

        // (b) stat prints God-Commands from the bits, not the level.
        g.descriptors.get_mut(&ConnId(1)).unwrap().outbuf.clear();
        do_stat_character(&mut g, imm, mortal);
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("God-Commands: &YYes&n"), "out: {}", out);
    }

    #[test]
    fn stat_god_commands_is_no_for_a_bitless_immortal() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_GOD);
        {
            let imm = g.get_char_mut(imm).unwrap();
            imm.godcmds1 = 0;
            imm.godcmds2 = 0;
            imm.godcmds3 = 0;
            imm.godcmds4 = 0;
        }
        g.char_to_room(imm, room);

        do_stat_character(&mut g, imm, imm);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("God-Commands: No\r\n"), "out: {}", out);
    }

    fn rename_test_state(label: &str) -> (GameState, std::path::PathBuf, CharId, CharId) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let lib = std::env::temp_dir().join(format!(
            "deltamud-rename-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&lib).unwrap();
        let mut config = Config::default();
        config.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let admin = connected_player(&mut g, ConnId(1), "Admin", LVL_IMPL);
        let victim = connected_player(&mut g, ConnId(2), "Oldname", 20);
        g.get_char_mut(victim).unwrap().idnum = 9_413_101;
        g.update_player_index(9_413_101, "Oldname", 20, 0, "test");
        (g, lib, admin, victim)
    }

    fn seed_rename_sidecars(g: &GameState, idnum: i64) -> (std::path::PathBuf, std::path::PathBuf) {
        let rent = crate::objsave::crash_filename(&g.config.lib_path, "Oldname").unwrap();
        std::fs::create_dir_all(rent.parent().unwrap()).unwrap();
        std::fs::write(&rent, b"rent owned by Oldname").unwrap();
        crate::alias::set_aliases(
            idnum,
            vec![crate::alias::AliasEntry {
                alias: "waveall".into(),
                replacement: "wave all".into(),
                atype: 0,
            }],
        );
        crate::alias::write_aliases(&g.config.lib_path, "Oldname", idnum).unwrap();
        let alias = crate::alias::alias_filename(&g.config.lib_path, "Oldname").unwrap();
        (rent, alias)
    }

    #[test]
    fn rename_queues_a_durable_operation_without_publishing_any_identity() {
        let (mut g, lib, admin, victim) = rename_test_state("queue");
        let idnum = g.get_char(victim).unwrap().idnum;
        let (old_rent, old_alias) = seed_rename_sidecars(&g, idnum);
        let new_rent = crate::objsave::crash_filename(&g.config.lib_path, "Newname").unwrap();
        let new_alias = crate::alias::alias_filename(&g.config.lib_path, "Newname").unwrap();

        crate::interpreter::run_authenticated_command(&mut g, admin, "rename Oldname Newname");

        assert_eq!(g.get_char(victim).unwrap().get_name(), "Oldname");
        assert_eq!(g.find_player_by_name("Oldname"), Some(victim));
        assert!(g.find_player_by_name("Newname").is_none());
        assert!(old_rent.is_file() && old_alias.is_file());
        assert!(!new_rent.exists() && !new_alias.exists());
        assert!(g.player_save_requests.is_empty());
        assert_eq!(g.player_rename_requests.len(), 1);
        let queued = &g.player_rename_requests[0];
        assert_eq!(queued.authorization.requester_body, admin);
        assert_eq!(queued.victim, victim);
        assert_eq!(queued.idnum, idnum);
        assert_eq!(queued.old_name, "Oldname");
        assert_eq!(queued.new_name, "Newname");
        assert!(!g.descriptors[&ConnId(1)].outbuf.contains("renamed"));
        assert!(!g.descriptors[&ConnId(2)].outbuf.contains("renamed"));

        crate::alias::clear_aliases(idnum);
        std::fs::remove_dir_all(lib).unwrap();
    }

    #[test]
    fn rename_rejects_a_second_request_for_the_same_identity() {
        let (mut g, lib, admin, victim) = rename_test_state("duplicate");
        crate::interpreter::run_authenticated_command(&mut g, admin, "rename Oldname Newname");
        crate::interpreter::run_authenticated_command(&mut g, admin, "rename Oldname Anothername");

        assert_eq!(g.player_rename_requests.len(), 1);
        assert_eq!(g.get_char(victim).unwrap().get_name(), "Oldname");
        let output = &g.descriptors[&ConnId(1)].outbuf;
        assert!(output.contains("already pending"), "output={output:?}");
        assert!(!output.contains("You have renamed"), "output={output:?}");

        std::fs::remove_dir_all(lib).unwrap();
    }

    // ---- #201: `stat file` refuses a target above the requester ------------

    #[test]
    fn stat_file_refuses_a_higher_level_target() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMMORT);
        let imp = connected_player(&mut g, ConnId(2), "Imp", LVL_IMPL);
        g.char_to_room(imm, room);
        g.char_to_room(imp, room);
        g.update_player_index(imp_idnum(&g, imp), "Imp", LVL_IMPL, 0, "test");

        do_stat(&mut g, imm, "file Imp", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("Sorry, you can't do that."), "out: {}", out);
        assert!(!out.contains("IDNum:"), "no record was rendered: {}", out);
    }

    fn imp_idnum(g: &GameState, id: CharId) -> i64 {
        g.get_char(id).map(|c| c.idnum).unwrap_or(0)
    }

    #[test]
    fn stat_file_still_renders_an_equal_or_lower_level_target() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMPL);
        let mort = connected_player(&mut g, ConnId(2), "Mort", 10);
        g.char_to_room(imm, room);
        g.char_to_room(mort, room);

        do_stat(&mut g, imm, "file Mort", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("IDNum:"), "out: {}", out);
        assert!(!out.contains("Sorry, you can't do that."), "out: {}", out);
    }

    // ---- #209: vstat refuses an out-of-zone vnum before instantiating ------

    #[test]
    fn vstat_denies_an_out_of_zone_vnum_for_a_builder() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        // Zone 0 belongs to "Bob"; the builder "Sally" may not edit it.
        g.zones.push(test_zone(0, 99, "Bob"));
        let builder = connected_player(&mut g, ConnId(1), "Sally", 1);
        g.char_to_room(builder, room);
        g.mob_protos
            .insert(1200, mobile_proto(1200, "receptionist", 0));

        do_vstat(&mut g, builder, "mob 1200", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(
            out.contains("You don't have permissions to that zone."),
            "out: {}",
            out
        );
        // The mobile was never instantiated into room 0.
        assert!(g.char_ids().iter().all(|c| !is_npc(&g, *c)));
    }

    // ---- #203: rlist/mlist/olist builder gates -----------------------------

    #[test]
    fn rlist_mlist_olist_deny_a_zone_the_builder_does_not_own() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        g.zones.push(test_zone(0, 99, "Bob"));
        let builder = connected_player(&mut g, ConnId(1), "Sally", 1);
        g.char_to_room(builder, room);

        do_rlist(&mut g, builder, "0 99", 0);
        do_mlist(&mut g, builder, "0 99", 0);
        do_olist(&mut g, builder, "0 99", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("You can't edit the zone supplied by the first argument."));
        assert!(!out.contains("No rooms were found"));
        assert!(!out.contains("No mobiles were found"));
        assert!(!out.contains("No objects were found"));
    }

    #[test]
    fn rlist_lets_an_immortal_enumerate_any_zone() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Hall".to_string(), "A hall.".to_string()));
        g.zones.push(test_zone(0, 99, "Bob"));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMMORT);
        g.char_to_room(imm, room);

        do_rlist(&mut g, imm, "0 200", 0);

        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(out.contains("[  100]"), "out: {}", out);
        assert!(!out.contains("You can't edit the zone"));
    }

    // ---- #206: `set Legal_PKS` flips the live PvP gate ---------------------

    #[test]
    fn set_legal_pks_flips_the_live_pvp_gate() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let god = connected_player(&mut g, ConnId(1), "God", LVL_GRGOD);
        let alpha = connected_player(&mut g, ConnId(2), "Alpha", 20);
        let beta = connected_player(&mut g, ConnId(3), "Beta", 20);
        g.char_to_room(god, room);
        g.char_to_room(alpha, room);
        g.char_to_room(beta, room);

        do_set(&mut g, god, "Legal_PKS ON", 0);
        assert!(g.pk_allowed, "the gate must actually open");
        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("Legal PKs are now Allowed.")
        );

        // The do_hit murder-redirect is `if (!pk_allowed)` in C — it must not
        // fire once the gate is open.
        crate::cmd_offensive::do_hit(&mut g, alpha, "beta", 0);
        let out = &g.descriptors.get(&ConnId(2)).unwrap().outbuf;
        assert!(
            !out.contains("Use 'murder' to hit another player."),
            "out: {}",
            out
        );

        do_set(&mut g, god, "Legal_PKS OFF", 0);
        assert!(!g.pk_allowed, "the gate must actually close");
        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("Legal PKs are now Disallowed.")
        );
    }

    #[test]
    fn set_legal_pks_requires_grgod() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let imm = connected_player(&mut g, ConnId(1), "Imm", LVL_IMMORT);
        g.char_to_room(imm, room);

        do_set(&mut g, imm, "Legal_PKS ON", 0);

        assert!(!g.pk_allowed, "a sub-GRGOD immortal cannot open the gate");
        let out = &g.descriptors.get(&ConnId(1)).unwrap().outbuf;
        assert!(!out.contains("Legal PKs are now"), "out: {}", out);
    }

    // ---- #208: emote's PRF2_INTANGIBLE recipient filter --------------------

    #[test]
    fn intangible_non_builder_emote_is_hidden_from_mortals() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let ghost = connected_player(&mut g, ConnId(1), "Ghost", LVL_IMMORT);
        let mort = connected_player(&mut g, ConnId(2), "Mort", 10);
        g.char_to_room(ghost, room);
        g.char_to_room(mort, room);
        g.get_char_mut(ghost).unwrap().prf2_flags |= PRF2_INTANGIBLE;

        do_echo(&mut g, ghost, "drifts through the wall.", SCMD_EMOTE);

        // The sender still sees it; the mortal does not.
        assert!(
            g.descriptors
                .get(&ConnId(1))
                .unwrap()
                .outbuf
                .contains("drifts through the wall.")
        );
        assert!(
            !g.descriptors
                .get(&ConnId(2))
                .unwrap()
                .outbuf
                .contains("drifts through the wall.")
        );

        // A builder ghost (PRF2_MBUILDING) is delivered to mortals again.
        g.get_char_mut(ghost).unwrap().prf2_flags |= PRF2_MBUILDING;
        g.descriptors.get_mut(&ConnId(2)).unwrap().outbuf.clear();
        do_echo(&mut g, ghost, "drifts through the wall.", SCMD_EMOTE);
        assert!(
            g.descriptors
                .get(&ConnId(2))
                .unwrap()
                .outbuf
                .contains("drifts through the wall.")
        );

        // So is an emote seen by a mortal who is themselves intangible.
        g.get_char_mut(ghost).unwrap().prf2_flags &= !PRF2_MBUILDING;
        g.get_char_mut(mort).unwrap().prf2_flags |= PRF2_INTANGIBLE;
        g.descriptors.get_mut(&ConnId(2)).unwrap().outbuf.clear();
        do_echo(&mut g, ghost, "drifts through the wall.", SCMD_EMOTE);
        assert!(
            g.descriptors
                .get(&ConnId(2))
                .unwrap()
                .outbuf
                .contains("drifts through the wall.")
        );
    }

    // ---- #212: return disconnects an occupant of the original body ---------

    #[test]
    fn return_disconnects_a_body_snatcher() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".to_string(), "A room.".to_string()));
        let owner = connected_player(&mut g, ConnId(1), "Owner", LVL_IMPL);
        let body = connected_player(&mut g, ConnId(2), "Snatcher", LVL_IMPL);
        g.char_to_room(owner, room);
        g.char_to_room(body, room);

        // `owner` is currently possessing `host`; `body` has meanwhile switched
        // into `host` (the immortal's original body) with ConnId(2).
        let host = g.create_char(Character::new_npc(1234));
        g.char_to_room(host, room);
        g.get_char_mut(host).unwrap().desc = Some(ConnId(2));
        {
            let d = g.descriptors.get_mut(&ConnId(1)).unwrap();
            d.state = ConState::Playing;
            d.character = Some(owner);
            d.original = Some(host);
        }
        {
            let d = g.descriptors.get_mut(&ConnId(2)).unwrap();
            d.state = ConState::Playing;
            d.character = Some(host);
            d.original = Some(body);
        }
        g.get_char_mut(owner).unwrap().desc = Some(ConnId(1));
        g.get_char_mut(body).unwrap().desc = Some(ConnId(2));

        do_return(&mut g, owner, "", 0);

        assert_eq!(
            g.descriptors.get(&ConnId(2)).unwrap().state,
            ConState::Close,
            "the occupant's connection must be dropped"
        );
        // The original body is re-attached to the returning player's connection.
        assert_eq!(g.get_char(host).unwrap().desc, Some(ConnId(1)));
        assert_eq!(g.get_char(owner).unwrap().desc, None);
        assert_eq!(g.descriptors.get(&ConnId(1)).unwrap().character, Some(host));
        assert_eq!(g.descriptors.get(&ConnId(1)).unwrap().original, None);
    }

    #[test]
    fn force_mass_scope_uses_persisted_trust_not_display_level() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".into(), "A room.".into()));
        let requester = connected_player(&mut g, ConnId(91_001), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(91_002), "Target", 1);
        g.char_to_room(requester, room);
        g.char_to_room(target, room);
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GOD);

        do_force(&mut g, requester, "all stand", 0);
        assert!(
            !g.descriptors[&ConnId(91_002)]
                .outbuf
                .contains("has forced you")
        );

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_GRGOD);
        }
        do_force(&mut g, requester, "all stand", 0);
        assert!(
            g.descriptors[&ConnId(91_002)]
                .outbuf
                .contains("has forced you")
        );
    }

    #[test]
    fn rename_queue_gate_uses_persisted_trust_not_display_level() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(91_011), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(91_012), "Oldname", 1);
        g.get_char_mut(target).unwrap().idnum = 91_012;
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GOD);
        g.get_char_mut(target).unwrap().trust = i32::from(LVL_GRGOD);

        crate::interpreter::run_authenticated_command(&mut g, requester, "rename Oldname Newname");
        assert!(g.player_rename_requests.is_empty());

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_IMPL);
        }
        {
            let target = g.get_char_mut(target).unwrap();
            target.player.level = LVL_IMPL;
            target.trust = i32::from(LVL_GOD);
        }
        crate::interpreter::run_authenticated_command(&mut g, requester, "rename Oldname Newname");
        assert_eq!(g.player_rename_requests.len(), 1);
        assert_eq!(
            g.player_rename_requests[0].authorization.requester_body,
            requester
        );
    }

    #[test]
    fn set_rejects_player_authority_fields_and_saves_normal_changes() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(91_021), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(91_022), "Target", 20);
        let before = {
            let target = g.get_char(target).unwrap();
            (
                target.player.level,
                target.trust,
                target.godcmds1,
                target.godcmds2,
                target.godcmds3,
                target.godcmds4,
            )
        };

        for command in [
            "Target level 50",
            "Target trust 50",
            "Target cmdadvance on",
            "Target setall on",
        ] {
            g.descriptors
                .get_mut(&ConnId(91_021))
                .unwrap()
                .outbuf
                .clear();
            do_set(&mut g, requester, command, 0);
            assert!(
                g.descriptors[&ConnId(91_021)]
                    .outbuf
                    .contains("advance <player> <level>"),
                "command={command:?}"
            );
            let target = g.get_char(target).unwrap();
            assert_eq!(
                (
                    target.player.level,
                    target.trust,
                    target.godcmds1,
                    target.godcmds2,
                    target.godcmds3,
                    target.godcmds4,
                ),
                before
            );
            assert!(g.player_save_requests.is_empty());
        }

        do_set(&mut g, requester, "Target title Persisted", 0);
        assert_eq!(
            g.get_char(target).unwrap().player.title.as_deref(),
            Some("Persisted")
        );
        assert_eq!(g.player_save_requests, vec![target]);
    }

    #[test]
    fn set_password_target_cap_uses_trust_not_display_level() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(91_031), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(91_032), "Target", 1);
        g.get_char_mut(requester).unwrap().idnum = 91_031;
        g.get_char_mut(target).unwrap().idnum = 91_032;
        g.get_char_mut(target).unwrap().trust = i32::from(LVL_GRGOD);

        crate::interpreter::run_authenticated_command(
            &mut g,
            requester,
            "set Target passwd durable-pass",
        );
        assert!(g.password_update_requests.is_empty());

        {
            let target = g.get_char_mut(target).unwrap();
            target.player.level = LVL_GRGOD;
            target.trust = 1;
        }
        crate::interpreter::run_authenticated_command(
            &mut g,
            requester,
            "set Target passwd durable-pass",
        );
        assert_eq!(g.password_update_requests.len(), 1);
        assert!(g.player_save_requests.is_empty());
    }

    #[test]
    fn set_invis_and_nohassle_caps_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(91_041), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(91_042), "Target", LVL_IMPL);
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GRGOD);
        g.get_char_mut(target).unwrap().trust = 1;

        do_set(&mut g, requester, "Target nohassle on", 0);
        assert_eq!(g.get_char(target).unwrap().prf_flags & PRF_NOHASSLE, 0);

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_IMPL);
        }
        do_set(&mut g, requester, "Target invis 105", 0);
        do_set(&mut g, requester, "Target nohassle on", 0);
        assert_eq!(g.get_char(target).unwrap().invis_level, 1);
        assert_ne!(g.get_char(target).unwrap().prf_flags & PRF_NOHASSLE, 0);
        assert_eq!(g.player_save_requests, vec![target]);
    }

    #[test]
    fn wiznet_recipients_and_listing_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        let sender = connected_player(&mut g, ConnId(91_051), "Sender", 1);
        let spoofed = connected_player(&mut g, ConnId(91_052), "Spoofed", LVL_IMPL);
        let trusted = connected_player(&mut g, ConnId(91_053), "Trusted", 1);
        g.get_char_mut(sender).unwrap().trust = i32::from(LVL_IMPL);
        g.get_char_mut(spoofed).unwrap().trust = 1;
        g.get_char_mut(trusted).unwrap().trust = i32::from(LVL_IMMORT);

        do_wiznet(&mut g, sender, "trust boundary", 0);
        assert!(
            !g.descriptors[&ConnId(91_052)]
                .outbuf
                .contains("trust boundary")
        );
        assert!(
            g.descriptors[&ConnId(91_053)]
                .outbuf
                .contains("trust boundary")
        );

        g.descriptors
            .get_mut(&ConnId(91_051))
            .unwrap()
            .outbuf
            .clear();
        do_wiznet(&mut g, sender, "@", 0);
        let listing = &g.descriptors[&ConnId(91_051)].outbuf;
        assert!(listing.contains("Trusted"), "listing={listing:?}");
        assert!(!listing.contains("Spoofed"), "listing={listing:?}");

        g.authority_quarantine.insert(91_053);
        g.descriptors
            .get_mut(&ConnId(91_051))
            .unwrap()
            .outbuf
            .clear();
        do_wiznet(&mut g, sender, "@", 0);
        let listing = &g.descriptors[&ConnId(91_051)].outbuf;
        assert!(!listing.contains("Trusted"), "listing={listing:?}");
    }

    #[test]
    fn wizutil_hierarchy_and_freeze_provenance_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(91_056), "Requester", 1);
        let target = connected_player(&mut g, ConnId(91_057), "Target", LVL_HERO - 1);
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GRGOD);
        g.get_char_mut(target).unwrap().trust = 1;

        do_wizutil(&mut g, requester, "Target", SCMD_FREEZE);
        assert_ne!(g.get_char(target).unwrap().act_flags & PLR_FROZEN, 0);
        assert_eq!(g.get_char(target).unwrap().freeze_level, LVL_GRGOD);

        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GOD);
        do_wizutil(&mut g, requester, "Target", SCMD_THAW);
        assert_ne!(g.get_char(target).unwrap().act_flags & PLR_FROZEN, 0);

        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GRGOD);
        do_wizutil(&mut g, requester, "Target", SCMD_THAW);
        assert_eq!(g.get_char(target).unwrap().act_flags & PLR_FROZEN, 0);
    }

    #[test]
    fn live_and_offline_inspection_and_last_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        let requester = connected_player(&mut g, ConnId(91_061), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(91_062), "Target", 1);
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GOD);
        g.get_char_mut(target).unwrap().trust = i32::from(LVL_GRGOD);
        g.descriptors.get_mut(&ConnId(91_062)).unwrap().host = "secret.example".into();

        do_stat(&mut g, requester, "player Target", 0);
        do_last(&mut g, requester, "Target", 0);
        let output = &g.descriptors[&ConnId(91_061)].outbuf;
        assert!(output.contains(PLAYER_INSPECTION_DENIED.trim()));
        assert!(!output.contains("secret.example"));

        g.update_player_index(91_063, "Offline", 1, 1_700_000_000, "offline-secret");
        g.player_table
            .iter_mut()
            .find(|entry| entry.idnum == 91_063)
            .unwrap()
            .trust = i32::from(LVL_GRGOD);
        g.descriptors
            .get_mut(&ConnId(91_061))
            .unwrap()
            .outbuf
            .clear();
        do_last(&mut g, requester, "Offline", 0);
        assert!(
            !g.descriptors[&ConnId(91_061)]
                .outbuf
                .contains("offline-secret")
        );

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_IMPL);
        }
        g.descriptors
            .get_mut(&ConnId(91_061))
            .unwrap()
            .outbuf
            .clear();
        do_last(&mut g, requester, "Offline", 0);
        assert!(
            g.descriptors[&ConnId(91_061)]
                .outbuf
                .contains("offline-secret")
        );
    }

    #[test]
    fn transfer_all_uses_persisted_trust_not_display_level() {
        let mut g = GameState::new(Config::default());
        let source = g.add_room(Room::new(100, 0, "Source".into(), "Source.".into()));
        let destination = g.add_room(Room::new(
            200,
            0,
            "Destination".into(),
            "Destination.".into(),
        ));
        let requester = connected_player(&mut g, ConnId(92_001), "Requester", LVL_IMPL);
        let target = connected_player(&mut g, ConnId(92_002), "Target", 1);
        g.char_to_room(requester, destination);
        g.char_to_room(target, source);
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GOD);

        do_trans(&mut g, requester, "all", 0);
        assert_eq!(g.get_char(target).unwrap().in_room, Some(source));

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_GRGOD);
        }
        do_trans(&mut g, requester, "all", 0);
        assert_eq!(g.get_char(target).unwrap().in_room, Some(destination));
    }

    #[test]
    fn stat_vstat_and_zone_lists_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        g.zones.push(test_zone(1, 199, "Other"));
        let room = g.add_room(Room::new(100, 0, "Audited room".into(), "Room.".into()));
        let requester = connected_player(&mut g, ConnId(92_011), "Requester", LVL_IMPL);
        g.char_to_room(requester, room);
        g.get_char_mut(requester).unwrap().trust = 1;

        g.obj_protos
            .insert(100, object_proto(100, ObjectType::Boat, "an audited boat"));
        g.mob_protos
            .insert(100, mobile_proto(100, "an audited mobile", 0));
        let object = g.create_obj(Object::new(
            100,
            "audited boat".into(),
            "an audited boat".into(),
        ));
        let mut mobile = Character::new_npc(100);
        mobile.player.name = "audited mobile".into();
        let mobile = g.create_char(mobile);

        do_stat_room(&mut g, requester);
        do_stat_object(&mut g, requester, object);
        do_stat_character(&mut g, requester, mobile);
        do_vstat(&mut g, requester, "mob 100", 0);
        let denied = &g.descriptors[&ConnId(92_011)].outbuf;
        assert_eq!(
            denied
                .matches("You don't have permissions to that zone.")
                .count(),
            4,
            "output={denied:?}"
        );

        let list_routes: [fn(&mut GameState, CharId, &str, i32); 3] =
            [do_rlist, do_mlist, do_olist];
        for route in list_routes {
            g.descriptors
                .get_mut(&ConnId(92_011))
                .unwrap()
                .outbuf
                .clear();
            route(&mut g, requester, "100 101", 0);
            assert!(
                g.descriptors[&ConnId(92_011)]
                    .outbuf
                    .contains("You can't edit the zone supplied by the first argument."),
            );
        }

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_IMMORT);
        }
        for route in list_routes {
            g.descriptors
                .get_mut(&ConnId(92_011))
                .unwrap()
                .outbuf
                .clear();
            route(&mut g, requester, "100 101", 0);
            let output = &g.descriptors[&ConnId(92_011)].outbuf;
            assert!(!output.contains("can't edit the zone"), "output={output:?}");
            assert!(output.contains("100"), "output={output:?}");
        }
    }

    #[test]
    fn invisibility_visibility_and_vwear_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".into(), "Room.".into()));
        let requester = connected_player(&mut g, ConnId(92_021), "Requester", LVL_IMPL);
        let spoofed_viewer = connected_player(&mut g, ConnId(92_022), "Spoofed", LVL_IMPL);
        let trusted_viewer = connected_player(&mut g, ConnId(92_023), "Trusted", 1);
        for player in [requester, spoofed_viewer, trusted_viewer] {
            g.char_to_room(player, room);
        }
        g.get_char_mut(requester).unwrap().trust = 1;
        g.get_char_mut(spoofed_viewer).unwrap().trust = 1;
        g.get_char_mut(trusted_viewer).unwrap().trust = i32::from(LVL_IMPL);
        g.obj_protos.insert(
            500,
            object_proto(500, ObjectType::Boat, "an authority boat"),
        );

        do_invis(&mut g, requester, "105", 0);
        do_vwear(&mut g, requester, "boat", 0);
        do_respec(&mut g, requester, "", 0);
        assert_eq!(g.get_char(requester).unwrap().invis_level, 0);
        assert!(
            g.descriptors[&ConnId(92_021)]
                .outbuf
                .contains("You are not godly enough for that!")
        );

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_GOD);
        }
        for conn in [ConnId(92_021), ConnId(92_022), ConnId(92_023)] {
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        }
        do_invis(&mut g, requester, "", 0);
        do_vwear(&mut g, requester, "boat", 0);
        do_respec(&mut g, requester, "", 0);
        assert_eq!(
            g.get_char(requester).unwrap().invis_level,
            i32::from(LVL_GOD)
        );
        assert!(
            g.descriptors[&ConnId(92_021)]
                .outbuf
                .contains("an authority boat")
        );
        assert!(
            g.descriptors[&ConnId(92_021)]
                .outbuf
                .contains("Mob hardcoded SPECS reassigned")
        );
        assert!(
            g.descriptors[&ConnId(92_022)]
                .outbuf
                .contains("suddenly realize")
        );
        assert!(
            !g.descriptors[&ConnId(92_023)]
                .outbuf
                .contains("suddenly realize")
        );

        perform_immort_vis(&mut g, requester);
        g.get_char_mut(requester).unwrap().prf2_flags |= PRF2_INTANGIBLE;
        for conn in [ConnId(92_022), ConnId(92_023)] {
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        }
        do_echo(&mut g, requester, "passes unseen.", SCMD_EMOTE);
        assert!(
            !g.descriptors[&ConnId(92_022)]
                .outbuf
                .contains("passes unseen")
        );
        assert!(
            g.descriptors[&ConnId(92_023)]
                .outbuf
                .contains("passes unseen")
        );
    }

    #[test]
    fn bare_stat_and_administrative_target_scopes_use_persisted_trust() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 0, "Room".into(), "Room.".into()));
        let requester = connected_player(&mut g, ConnId(92_031), "Requester", LVL_IMPL);
        let lower = connected_player(&mut g, ConnId(92_032), "Lower", LVL_IMPL);
        let higher = connected_player(&mut g, ConnId(92_033), "Higher", 1);
        for player in [requester, lower, higher] {
            g.char_to_room(player, room);
        }
        g.get_char_mut(requester).unwrap().trust = i32::from(LVL_GOD);
        g.get_char_mut(lower).unwrap().trust = 1;
        g.get_char_mut(higher).unwrap().trust = i32::from(LVL_GRGOD);

        do_stat(&mut g, requester, "Higher", 0);
        assert!(
            g.descriptors[&ConnId(92_031)]
                .outbuf
                .contains(PLAYER_INSPECTION_DENIED.trim())
        );

        g.get_char_mut(lower).unwrap().fighting = Some(higher);
        g.get_char_mut(higher).unwrap().fighting = Some(lower);
        do_peace(&mut g, requester, "", 0);
        assert!(g.get_char(lower).unwrap().fighting.is_none());
        assert!(g.get_char(higher).unwrap().fighting.is_some());

        do_gplague(&mut g, requester, "", 0);
        assert_ne!(g.get_char(lower).unwrap().affect_flags & AFF_PLAGUED, 0);
        assert_eq!(g.get_char(higher).unwrap().affect_flags & AFF_PLAGUED, 0);

        g.obj_protos.insert(
            600,
            object_proto(600, ObjectType::Treasure, "an authority reward"),
        );
        do_reward(&mut g, requester, "all 600", 0);
        assert_eq!(g.get_char(lower).unwrap().carrying.len(), 1);
        assert!(g.get_char(higher).unwrap().carrying.is_empty());

        {
            let requester = g.get_char_mut(requester).unwrap();
            requester.player.level = 1;
            requester.trust = i32::from(LVL_GRGOD);
        }
        g.descriptors
            .get_mut(&ConnId(92_031))
            .unwrap()
            .outbuf
            .clear();
        do_stat(&mut g, requester, "Higher", 0);
        assert!(g.descriptors[&ConnId(92_031)].outbuf.contains("IDNum:"));
    }
}
