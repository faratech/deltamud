// arena.rs — DeltaMUD Arena PvP subsystem (port of src/arena.c, plus the
// arena fragments scattered across utils.c, comm.c, spec_procs.c,
// act.other.c, act.movement.c, act.offensive.c, spells.c, limits.c and
// fight.c that together make the arena work).
//
// CircleMUD stores arena state in fields on char_data (player_specials.saved.
// arena/wins/losses, char_specials.flee_timer/last_fighting/observing/
// observe_by, char_specials.bup_*) and in two global char_data pointers
// (`arenamaster`, `defaultobserve`). The Rust Character carries `wins`/
// `losses` (used directly), but not the rest of the arena-only fields, and we
// are not allowed to grow Character or GameState. So every arena-private bit
// of per-character state lives in a module-owned side table keyed by CharId,
// exactly as shop.rs / mail.rs / boards.rs keep their own runtime state in a
// `static OnceLock<Mutex<…>>`. The id keys make the C observer linked list a
// pair of `Option<CharId>` links (`observing` / `observe_by`).
//
// Public surface (the C externs other modules call):
//   spec proc        arenaentrancemaster(g, ch, me, cmd, arg) -> bool
//   command          do_observe(g, ch, arg, subcmd)                 (ACMD)
//   combat hooks     match_over, arena_combat_death
//   flee/recall      arena_flee_start, arena_recall, arena_flee_pulse
//   movement hook    arena_leave_via_exit
//   life-cycle       restore_bup_affects, bup_affects, clearobservers,
//                    deobserve, on_link_lost
//   queries          is_arena_combatant, arena_stat, arena_flee_timer

use crate::act::{act, ActArg, To};
use crate::flags::AFF_INVISIBLE;
use crate::interpreter::{is_abbrev, one_argument};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// ARENA_* status codes (structs.h). Stored per-character in ArenaWorld.
// ---------------------------------------------------------------------------
pub const ARENA_NOT: u8 = 0;
pub const ARENA_COMBATANT1: u8 = 1; // about to do match #1
pub const ARENA_COMBATANT1W: u8 = 2; // has done > 1 match
pub const ARENA_COMBATANT2: u8 = 3; // about to do match #2
pub const ARENA_COMBATANT3: u8 = 4; // about to do match #3
pub const ARENA_COMBATANTZ: u8 = 99; // has done all matches
pub const ARENA_OBSERVER: u8 = 100;

// ---------------------------------------------------------------------------
// Arena config (config.c). Fees are multipliers: fee = level * fee_mult.
// ---------------------------------------------------------------------------
const ARENA_ENTRANCE: RoomVnum = 4800;
const ARENA_PREPROOM: RoomVnum = 4801;
const ARENA_OBSERVEROOM: RoomVnum = 4899;
const ARENA_LEAVE_PENALTY_MULT: i32 = 100;
const ARENA_FLEE_TIMEOUT: i32 = 3; // # tics for the flee-recall timeout
const ARENA_COMBATANT_FEE: i32 = 1000; // combatant entrance fee multiplier
const ARENA_OBSERVER_FEE: i32 = 0; // observer entrance fee multiplier

// SCMD_ARENA for do_gen_comm (cmd_comm.rs SCMD_ARENA = 6).
const SCMD_ARENA: i32 = 6;

// ---------------------------------------------------------------------------
// Per-character arena bookkeeping (the C fields we can't store on Character).
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct BupAffects {
    aff_flags: i64,
    affected: Vec<crate::character::Affect>,
    wimp_level: i32,
    recall_level: i32, // BUP_RECALL_LEV — GET_RECALL_LEV stashed while in the arena
}

#[derive(Default, Clone)]
struct ArenaChar {
    /// GET_ARENASTAT — ARENA_NOT when absent.
    stat: u8,
    /// char_specials.flee_timer (GET_ARENAFLEETIMER).
    flee_timer: i32,
    /// char_specials.last_fighting (LASTFIGHTING).
    last_fighting: Option<CharId>,
    /// char_specials.observing — the combatant this observer watches.
    observing: Option<CharId>,
    /// char_specials.observe_by — next link in the observer chain.
    observe_by: Option<CharId>,
    /// Backed-up affects while inside the arena (None until bup_affects()).
    bup: Option<BupAffects>,
}

struct ArenaWorld {
    chars: HashMap<CharId, ArenaChar>,
    /// `arenamaster` global — the entrance-master mob (set by the spec proc).
    arenamaster: Option<CharId>,
    /// `defaultobserve` global — combatant new observers latch onto.
    defaultobserve: Option<CharId>,
}

static ARENA: OnceLock<Mutex<ArenaWorld>> = OnceLock::new();

fn arena() -> &'static Mutex<ArenaWorld> {
    ARENA.get_or_init(|| {
        Mutex::new(ArenaWorld {
            chars: HashMap::new(),
            arenamaster: None,
            defaultobserve: None,
        })
    })
}

// ---- tiny accessors over the side table (lock-scoped) ---------------------

fn get_stat(id: CharId) -> u8 {
    arena()
        .lock()
        .ok()
        .and_then(|w| w.chars.get(&id).map(|c| c.stat))
        .unwrap_or(ARENA_NOT)
}

fn set_stat(id: CharId, stat: u8) {
    if let Ok(mut w) = arena().lock() {
        let e = w.chars.entry(id).or_default();
        e.stat = stat;
        // ARENA_NOT means "gone from arena" — drop the side-table entry once it
        // holds nothing of interest, mirroring how C just clears the fields.
        if stat == ARENA_NOT
            && e.flee_timer == 0
            && e.last_fighting.is_none()
            && e.observing.is_none()
            && e.observe_by.is_none()
            && e.bup.is_none()
        {
            w.chars.remove(&id);
        }
    }
}

fn observing_of(id: CharId) -> Option<CharId> {
    arena().lock().ok().and_then(|w| w.chars.get(&id).and_then(|c| c.observing))
}
fn observe_by_of(id: CharId) -> Option<CharId> {
    arena().lock().ok().and_then(|w| w.chars.get(&id).and_then(|c| c.observe_by))
}
fn set_observing(id: CharId, to: Option<CharId>) {
    if let Ok(mut w) = arena().lock() {
        w.chars.entry(id).or_default().observing = to;
    }
}
fn set_observe_by(id: CharId, to: Option<CharId>) {
    if let Ok(mut w) = arena().lock() {
        w.chars.entry(id).or_default().observe_by = to;
    }
}

fn get_flee_timer(id: CharId) -> i32 {
    arena().lock().ok().and_then(|w| w.chars.get(&id).map(|c| c.flee_timer)).unwrap_or(0)
}
fn set_flee_timer(id: CharId, v: i32) {
    if let Ok(mut w) = arena().lock() {
        w.chars.entry(id).or_default().flee_timer = v;
    }
}
fn get_last_fighting(id: CharId) -> Option<CharId> {
    arena().lock().ok().and_then(|w| w.chars.get(&id).and_then(|c| c.last_fighting))
}
fn set_last_fighting(id: CharId, v: Option<CharId>) {
    if let Ok(mut w) = arena().lock() {
        w.chars.entry(id).or_default().last_fighting = v;
    }
}

fn get_arenamaster() -> Option<CharId> {
    arena().lock().ok().and_then(|w| w.arenamaster)
}
fn set_arenamaster(id: CharId) {
    if let Ok(mut w) = arena().lock() {
        w.arenamaster = Some(id);
    }
}
fn get_defaultobserve() -> Option<CharId> {
    arena().lock().ok().and_then(|w| w.defaultobserve)
}
fn set_defaultobserve(id: Option<CharId>) {
    if let Ok(mut w) = arena().lock() {
        w.defaultobserve = id;
    }
}

// ---------------------------------------------------------------------------
// Public queries (the C macros IS_ARENACOMBATANT / GET_ARENASTAT etc.).
// ---------------------------------------------------------------------------

/// IS_ARENACOMBATANT(ch): ARENA_COMBATANT1 <= stat <= ARENA_COMBATANTZ.
pub fn is_arena_combatant(id: CharId) -> bool {
    let s = get_stat(id);
    s >= ARENA_COMBATANT1 && s <= ARENA_COMBATANTZ
}

/// GET_ARENASTAT(ch).
pub fn arena_stat(id: CharId) -> u8 {
    get_stat(id)
}

/// GET_ARENAFLEETIMER(ch).
pub fn arena_flee_timer(id: CharId) -> i32 {
    get_flee_timer(id)
}

/// OBSERVING(ch) — the combatant this observer is watching, if any.
pub fn arena_observing(id: CharId) -> Option<CharId> {
    observing_of(id)
}

/// LASTFIGHTING(ch) — the last opponent fought in the arena, if any.
pub fn arena_last_fighting(id: CharId) -> Option<CharId> {
    get_last_fighting(id)
}

// ---------------------------------------------------------------------------
// Small helpers in the house style.
// ---------------------------------------------------------------------------

fn is_npc(g: &GameState, id: CharId) -> bool {
    g.get_char(id).map(|c| c.is_npc).unwrap_or(false)
}
fn level(g: &GameState, id: CharId) -> u8 {
    g.get_char(id).map(|c| c.player.level).unwrap_or(0)
}
fn get_name(g: &GameState, id: CharId) -> String {
    g.get_char(id).map(|c| c.player.name.clone()).unwrap_or_default()
}
fn get_gold(g: &GameState, id: CharId) -> i32 {
    g.get_char(id).map(|c| c.points.gold).unwrap_or(0)
}
fn in_room(g: &GameState, id: CharId) -> Option<RoomRnum> {
    g.get_char(id).and_then(|c| c.in_room)
}
fn fighting(g: &GameState, id: CharId) -> Option<CharId> {
    g.get_char(id).and_then(|c| c.fighting)
}

/// numdisplay(): comma-group a signed integer ("1234567" -> "1,234,567").
fn numdisplay(val: i64) -> String {
    let neg = val < 0;
    let digits = val.unsigned_abs().to_string();
    let mut out = String::new();
    let n = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if neg {
        format!("-{}", out)
    } else {
        out
    }
}

/// stop_fighting(ch): clear the fighting target and drop out of POS_FIGHTING.
fn stop_fighting(g: &mut GameState, id: CharId) {
    if let Some(c) = g.get_char_mut(id) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Standing;
        }
    }
}

/// The arenamaster speaks a private tell to a player (C do_tell(arenamaster,
/// "<Name> <text>")). The C buffer is "<recipient name> <message>"; we already
/// have the two pieces split, so deliver straight through perform-tell shape.
fn master_tell(g: &mut GameState, to: CharId, message: &str) {
    let master = match get_arenamaster() {
        Some(m) if g.char_exists(m) => m,
        // No arenamaster known/loaded: degrade to a plain send, as C would
        // simply send via the (null-checked) channel — the player still hears.
        _ => {
            g.send_to_char(to, &format!("{}\r\n", message));
            return;
        }
    };
    let to_vict = format!("$n tells you, '{}'", message);
    act(g, &to_vict, false, master, None, ActArg::Char(to), To::Vict);
}

/// do_gen_comm(arenamaster, msg, 1, SCMD_ARENA): broadcast on the arena
/// channel. If the arenamaster isn't around, the broadcast is simply dropped,
/// exactly as C would with a null arenamaster pointer (the log still happens).
fn arena_channel(g: &mut GameState, msg: &str) {
    if let Some(master) = get_arenamaster() {
        if g.char_exists(master) {
            crate::cmd_comm::do_gen_comm(g, master, msg, SCMD_ARENA);
        }
    }
}

// ===========================================================================
// inc_matchcount (arena.c) — advance a combatant's match counter and tell
// them how many remain.
// ===========================================================================
fn inc_matchcount(g: &mut GameState, ch: CharId) {
    match get_stat(ch) {
        ARENA_COMBATANT1 => {
            set_stat(ch, ARENA_COMBATANT2);
            g.send_to_char(
                ch,
                "\r\nYou've used up one of your arena matches. Two left.\r\n\r\n",
            );
        }
        ARENA_COMBATANT1W => {
            set_stat(ch, ARENA_COMBATANT2);
            g.send_to_char(
                ch,
                "\r\nYou've used up one of your arena matches. One left.\r\n\r\n",
            );
        }
        ARENA_COMBATANT2 => {
            set_stat(ch, ARENA_COMBATANT3);
            g.send_to_char(
                ch,
                "\r\nYou've used up two of your arena matches. One left.\r\n\r\n",
            );
        }
        ARENA_COMBATANT3 => {
            set_stat(ch, ARENA_COMBATANTZ);
            g.send_to_char(
                ch,
                "\r\nYou've used up all three of your arena matches!\r\nThank you. Come again.\r\n\r\n",
            );
        }
        _ => {
            // DEBUG: arena combatant but not flagged as such? (C mudlogs.)
            g.send_to_char(ch, "Hmmm, your arena matches are screwed!\r\n");
        }
    }
}

// ===========================================================================
// trans_to_preproom (arena.c) — sling a loser back to the prep room at 1 HP.
// ===========================================================================
fn trans_to_preproom(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        c.points.hit = 1;
    }
    g.char_from_room(ch);
    if let Some(rnum) = g.real_room(ARENA_PREPROOM) {
        g.char_to_room(ch, rnum);
    }
    act(g, "$n has entered the Arena Prep Room.", false, ch, None, ActArg::None, To::NotVict);
    crate::cmd_informative::look_at_room(g, ch, false);
}

// ===========================================================================
// match_over (arena.c) — settle a finished match: pay/credit the winner, debit
// a loss, advance the loser's match count, announce, optionally rebound the
// loser to the prep room.
// ===========================================================================
pub fn match_over(
    g: &mut GameState,
    winner: Option<CharId>,
    loser: Option<CharId>,
    msg: &str,
    loser_to_preproom: bool,
) {
    let winner = match winner {
        Some(w) => w,
        None => return,
    };
    let loser = match loser {
        Some(l) => l,
        None => return,
    };
    if is_npc(g, winner) || is_npc(g, loser) {
        return;
    }
    if !is_arena_combatant(winner) {
        // DEBUG: match_over called but winner not flagged (C mudlogs GRGOD).
        return;
    }
    if !is_arena_combatant(loser) {
        // DEBUG: match_over called but loser not flagged.
        return;
    }

    // winnings = (int)(loser_level * arena_combatant_fee * number(5,15) * 0.1)
    let lvl = level(g, loser) as i32;
    let roll = g.rng.number(5, 15);
    let winnings = ((lvl * ARENA_COMBATANT_FEE * roll) as f64 * 0.1) as i32;

    act(g, "$n has WON this match!", false, winner, None, ActArg::None, To::NotVict);
    g.send_to_char(
        winner,
        &format!(
            "\r\n&RYou are victorious!!! You have been rewarded {} coins for winning.&n\r\n\r\n",
            winnings
        ),
    );
    if let Some(c) = g.get_char_mut(winner) {
        c.points.gold += winnings;
        if c.wins < 254 {
            c.wins += 1;
        }
    }
    set_flee_timer(winner, 0);

    act(g, "$n has lost this match!", false, loser, None, ActArg::None, To::NotVict);
    g.send_to_char(loser, "\r\n&RYou have lost the match!  Sorry...&n\r\n\r\n");
    if let Some(c) = g.get_char_mut(loser) {
        if c.losses < 254 {
            c.losses += 1;
        }
    }
    set_flee_timer(loser, 0);

    // stop_fighting(winner); stop_fighting(FIGHTING(loser)).
    if fighting(g, winner).is_some() {
        stop_fighting(g, winner);
    }
    if let Some(loser_target) = fighting(g, loser) {
        stop_fighting(g, loser_target);
    }

    if let Some(c) = g.get_char_mut(loser) {
        c.position = Position::Standing;
    }

    // A winner who's only done their first match graduates to COMBATANT1w.
    if get_stat(winner) == ARENA_COMBATANT1 {
        set_stat(winner, ARENA_COMBATANT1W);
    }

    inc_matchcount(g, loser);

    let announce = format!("{} has won a match against {}! {}", get_name(g, winner), get_name(g, loser), msg);
    arena_channel(g, &announce);
    log::info!("{}", announce);

    if loser_to_preproom {
        trans_to_preproom(g, loser);
    }
}

// ===========================================================================
// bup_affects / restore_bup_affects (arena.c) — stash and restore a PC's spell
// affects + wimp/recall levels while they're inside the arena (so arena buffs
// don't leak out and pre-arena buffs don't carry in).
// ===========================================================================

/// Back up the PC's affects + wimp/recall levels and strip them for the arena.
pub fn bup_affects(g: &mut GameState, ch: CharId) {
    if is_npc(g, ch) {
        return;
    }
    let saved = if let Some(c) = g.get_char_mut(ch) {
        let saved = BupAffects {
            aff_flags: c.affect_flags,
            affected: std::mem::take(&mut c.affected),
            wimp_level: c.wimp_level,
            recall_level: c.recall_level, // BUP_RECALL_LEV(ch) = GET_RECALL_LEV(ch)
        };
        c.affect_flags = 0;
        c.wimp_level = 0;
        c.recall_level = 0;
        saved
    } else {
        return;
    };
    if let Ok(mut w) = arena().lock() {
        w.chars.entry(ch).or_default().bup = Some(saved);
    }
    g.affect_total(ch);
}

/// Restore the affects saved by bup_affects (clearing any arena-acquired ones).
pub fn restore_bup_affects(g: &mut GameState, ch: CharId) {
    if is_npc(g, ch) {
        return;
    }
    let saved = arena().lock().ok().and_then(|mut w| w.chars.get_mut(&ch).and_then(|c| c.bup.take()));
    if let Some(c) = g.get_char_mut(ch) {
        // First clear off the arena affects.
        c.affected.clear();
        c.affect_flags = 0;
        if let Some(s) = saved {
            c.affect_flags = s.aff_flags;
            c.affected = s.affected;
            c.wimp_level = s.wimp_level;
            c.recall_level = s.recall_level; // GET_RECALL_LEV = BUP_RECALL_LEV
        }
    }
    g.affect_total(ch);
}

// ===========================================================================
// Observer chain (utils.c deobserve/linkobserve/clearobservers,
// comm.c send_to_observers/findanyinarena). The C struct links become
// Option<CharId> links in the side table.
// ===========================================================================

/// deobserve(who): remove this observer from the chain of whomever they watch.
pub fn deobserve(who: CharId) {
    let obswho = observing_of(who);
    if obswho.is_none() || get_stat(who) != ARENA_OBSERVER {
        return;
    }

    // Walk the chain rooted at OBSERVING(who), find `who`, splice it out.
    // C starts curr=prev=OBSERVING(who) then advances until curr==who.
    let head = obswho; // OBSERVING(who)
    let mut curr = head;
    let mut prev = head;
    while let Some(c) = curr {
        if c == who {
            break;
        }
        prev = curr;
        curr = observe_by_of(c);
    }
    if curr == Some(who) {
        let next = observe_by_of(who);
        if let Some(p) = prev {
            set_observe_by(p, next);
        }
        set_observe_by(who, None);
    }
    set_observing(who, None);
}

/// linkobserve(who, to): append `who` to the tail of `to`'s observer chain.
pub fn linkobserve(who: CharId, to: CharId) {
    let mut curr = to;
    while let Some(next) = observe_by_of(curr) {
        curr = next;
    }
    set_observing(who, Some(to));
    set_observe_by(curr, Some(who));
    set_observe_by(who, None);
}

/// clearobservers(who): detach every observer hanging off a combatant who's
/// leaving the arena. No-op for non-combatants / observers.
pub fn clearobservers(who: CharId) {
    let s = get_stat(who);
    if s == ARENA_NOT || s == ARENA_OBSERVER {
        return;
    }
    let mut tmp = Some(who);
    while let Some(clear) = tmp {
        tmp = observe_by_of(clear);
        set_observing(clear, None);
        set_observe_by(clear, None);
    }
}

/// send_to_observers(messg, who): relay a combatant's action line to everyone
/// observing them. Walks OBSERVE_BY links; only ARENA_OBSERVERs with a desc
/// receive it.
pub fn send_to_observers(g: &mut GameState, messg: &str, who: CharId) {
    let s = get_stat(who);
    if s == ARENA_NOT || s == ARENA_OBSERVER {
        return;
    }
    let mut tmp = observe_by_of(who);
    while let Some(t) = tmp {
        if get_stat(t) == ARENA_OBSERVER && g.get_char(t).map(|c| c.desc.is_some()).unwrap_or(false) {
            g.send_to_char(t, messg);
        }
        tmp = observe_by_of(t);
    }
}

/// findanyinarena(): the first connected combatant, used to repoint
/// `defaultobserve` when the current default leaves.
fn findanyinarena(g: &GameState) -> Option<CharId> {
    // C walks descriptor_list (connection order). Iterate descriptors' chars.
    for d in g.descriptors.values() {
        if let Some(cid) = d.character {
            if is_arena_combatant(cid) {
                return Some(cid);
            }
        }
    }
    None
}

// ===========================================================================
// arenaentrancemaster (spec_procs.c) — the arena entrance-master mob. Handles
// the `arena [combatant|observer]` command and shuttles players into the prep
// room / observatory, charging fees and managing arena status.
//
// Spec-proc signature: returns true if the command was consumed.
// ===========================================================================
pub fn arenaentrancemaster(g: &mut GameState, ch: CharId, me: CharId, cmd: &str, arg: &str) -> bool {
    // arenamaster = me; (set every call, like C.)
    set_arenamaster(me);

    // if (IS_NPC(ch) || !CMD_IS("arena")) return 0;
    if is_npc(g, ch) || !cmd.eq_ignore_ascii_case("arena") {
        return false;
    }

    let argument = arg.trim_start();
    let lvl = level(g, ch) as i32;
    let name = get_name(g, ch);

    // No argument: explain the fees and reset to ARENA_NOT.
    if argument.is_empty() {
        let mut mybuf = format!(
            "{} Welcome to my Arena! Your fee as a combatant will be {} coins,",
            name,
            numdisplay((lvl * ARENA_COMBATANT_FEE) as i64)
        );
        let ofee = lvl * ARENA_OBSERVER_FEE;
        if ofee == 0 {
            mybuf = format!("{} and as an observer it's FREE!", mybuf);
        } else {
            mybuf = format!("{} and as an observer it's {} coins.", mybuf, numdisplay(ofee as i64));
        }
        master_tell(g, ch, &mybuf);
        deobserve(ch);
        clearobservers(ch);
        set_stat(ch, ARENA_NOT);
        return true;
    }

    if is_abbrev(argument, "combatant") {
        let fee = lvl * ARENA_COMBATANT_FEE;
        if get_gold(g, ch) < fee {
            let mybuf = format!(
                "{} The fee for you is {} coins. You don't have enough gold!",
                name,
                numdisplay(fee as i64)
            );
            master_tell(g, ch, &mybuf);
            crate::cmd_social::do_action_named(g, me, "puke", &name);
            return true;
        }
        let mybuf = format!("{} That'll be {} coins, thanks!", name, numdisplay(fee as i64));
        master_tell(g, ch, &mybuf);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.gold -= fee;
        }
        set_stat(ch, ARENA_COMBATANT1);
        act(g, "$n admits $N as a combatant into the arena.", false, me, None, ActArg::Char(ch), To::NotVict);
        act(g, "$n admits you as a combatant into the arena.", false, me, None, ActArg::Char(ch), To::Vict);
        g.char_from_room(ch);
        if let Some(rnum) = g.real_room(ARENA_PREPROOM) {
            g.char_to_room(ch, rnum);
        }
        act(g, "$n has arrived.", false, ch, None, ActArg::None, To::NotVict);
        crate::cmd_informative::look_at_room(g, ch, false);

        // Maintain defaultobserve: keep it if it's a live combatant, else use ch.
        match get_defaultobserve() {
            Some(d) if is_arena_combatant(d) => {}
            _ => set_defaultobserve(Some(ch)),
        }

        if level(g, ch) < LVL_IMMORT {
            let mybuf = format!("{} has entered the arena as a combatant.", name);
            log::info!("{}", mybuf);
            arena_channel(g, &mybuf);
        }

        bup_affects(g, ch);
        return true;
    } else if is_abbrev(argument, "observer") {
        let fee = lvl * ARENA_OBSERVER_FEE;
        if get_gold(g, ch) < fee {
            let mybuf = format!(
                "{} The fee for you is {} coins. You don't have enough gold!",
                name,
                numdisplay(fee as i64)
            );
            master_tell(g, ch, &mybuf);
            crate::cmd_social::do_action_named(g, me, "puke", &name);
            return true;
        }

        // No combatants to watch -> refuse.
        let no_combatants = match get_defaultobserve() {
            None => true,
            Some(d) => !is_arena_combatant(d),
        };
        if no_combatants {
            let mybuf = format!("{} Looks like there's currently no combatants there.", name);
            master_tell(g, ch, &mybuf);
            return true;
        }
        let default = get_defaultobserve().unwrap();

        if fee == 0 {
            master_tell(g, ch, &format!("{} It's free to observe now!", name));
        } else {
            master_tell(g, ch, &format!("{} That'll be {} coins, thanks!", name, fee));
        }
        master_tell(
            g,
            ch,
            &format!("{} You're currently observing the actions of {}.", name, get_name(g, default)),
        );
        // Now let's link.
        linkobserve(ch, default);

        if let Some(c) = g.get_char_mut(ch) {
            c.points.gold -= fee;
        }
        set_stat(ch, ARENA_OBSERVER);
        act(g, "$n admits $N into the arena observatory.", false, me, None, ActArg::Char(ch), To::NotVict);
        act(g, "$n admits you into the arena observatory.", false, me, None, ActArg::Char(ch), To::Vict);
        g.char_from_room(ch);
        if let Some(rnum) = g.real_room(ARENA_OBSERVEROOM) {
            g.char_to_room(ch, rnum);
        }
        act(g, "$n has arrived.", false, ch, None, ActArg::None, To::NotVict);
        crate::cmd_informative::look_at_room(g, ch, false);

        if level(g, ch) < LVL_IMMORT {
            let mybuf = format!("{} has entered the arena as an observer.", name);
            arena_channel(g, &mybuf);
        }
        return true;
    }

    // Anything else: greet and reset to ARENA_NOT.
    master_tell(g, ch, &format!("{} Welcome to my Arena! Combatant or Observer?\r\n", name));
    deobserve(ch);
    clearobservers(ch);
    set_stat(ch, ARENA_NOT);
    true
}

// ===========================================================================
// do_observe (act.other.c) — an observer re-targets which combatant they watch.
// ===========================================================================
pub fn do_observe(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let in_observatory = in_room(g, ch) == g.real_room(ARENA_OBSERVEROOM);
    if get_stat(ch) != ARENA_OBSERVER || !in_observatory {
        g.send_to_char(ch, "You can't do that now! Get to the observatory!\r\n");
        return;
    }

    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        let who = match observing_of(ch) {
            Some(v) => get_name(g, v),
            None => "nobody".to_string(),
        };
        g.send_to_char(ch, &format!("You're currently observing the actions of {}.\r\n", who));
        return;
    }

    let victim = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No such person around.\r\n");
            return;
        }
    };

    if level(g, victim) >= LVL_IMMORT && victim != ch {
        g.send_to_char(ch, "You dare not.\r\n");
        return;
    }

    if victim == ch {
        deobserve(ch);
        g.send_to_char(ch, "Ok. You're observing nobody now.\r\n");
        return;
    }

    if !is_arena_combatant(victim) {
        g.send_to_char(ch, "Hey! That person's not an arena combatant!\r\n");
        return;
    }

    deobserve(ch);
    linkobserve(ch, victim);
    g.send_to_char(ch, &format!("You're now observing the actions of {}.\r\n", get_name(g, victim)));
}

// ===========================================================================
// Combat hooks.
// ===========================================================================

/// fight.c: when a victim hits POS_DEAD and is an arena combatant, the kill is
/// a match win (Fatality) rather than a real death. Returns true if the death
/// was consumed by the arena (caller must NOT run normal death handling).
pub fn arena_combat_death(g: &mut GameState, killer: CharId, victim: CharId) -> bool {
    if is_arena_combatant(victim) {
        match_over(g, Some(killer), Some(victim), "(Fatality)", true);
        true
    } else {
        false
    }
}

// ===========================================================================
// Flee / recall (act.offensive.c do_flee + spells.c spell_recall +
// limits.c point_update tic).
// ===========================================================================

/// act.offensive.c do_flee: an arena combatant who flees combat starts the
/// flee-recall timer instead of losing experience. Returns true if the flee
/// was handled as an arena flee (so the caller skips the exp-loss path).
/// `was_fighting` is the opponent the fleer was fighting.
pub fn arena_flee_start(g: &mut GameState, ch: CharId, was_fighting: CharId) -> bool {
    if is_npc(g, ch) || !is_arena_combatant(ch) {
        return false;
    }
    let loss = {
        let max = g.get_char(was_fighting).map(|c| c.points.max_hit).unwrap_or(0);
        let cur = g.get_char(was_fighting).map(|c| c.points.hit).unwrap_or(0);
        let wlvl = g.get_char(was_fighting).map(|c| c.player.level as i64).unwrap_or(0);
        (max - cur) as i64 * wlvl
    };
    g.send_to_char(
        ch,
        &format!(
            "&RYou would have lost {} experience points for fleeing if this wasn't an arena.&n\r\n",
            numdisplay(loss)
        ),
    );
    set_last_fighting(ch, Some(was_fighting));
    set_flee_timer(ch, 1); // start flee timer
    g.send_to_char(
        ch,
        "Starting Flee-Recall timer. If you recall before the timer expires, you concede the match!\r\n",
    );
    true
}

/// spells.c spell_recall: an arena participant who recalls. Combatants either
/// concede (if the flee timer is still running) or get rebounded to the prep
/// room; observers bounce to the observatory. Returns true if the recall was
/// fully handled by the arena (the caller must not run the normal recall).
pub fn arena_recall(g: &mut GameState, victim: CharId) -> bool {
    if is_npc(g, victim) {
        return false;
    }

    if is_arena_combatant(victim) {
        act(g, "$n disappears.", true, victim, None, ActArg::None, To::Room);

        let mut victor = fighting(g, victim);
        let mut msg = "(Recalled)".to_string();
        let ft = get_flee_timer(victim);
        if ft >= 1 && ft <= 1 + ARENA_FLEE_TIMEOUT {
            victor = get_last_fighting(victim);
            g.send_to_char(
                victim,
                "You recalled before the flee-recall timer expired.\r\nYou have conceded the match!\r\n",
            );
            msg = "(Fled & Recalled)".to_string();
            set_flee_timer(victim, 0);
        }
        match_over(g, victor, Some(victim), &msg, false);
        g.char_from_room(victim);
        if let Some(rnum) = g.real_room(ARENA_PREPROOM) {
            g.char_to_room(victim, rnum);
        }
        act(g, "$n appears in the middle of the room.", true, victim, None, ActArg::None, To::Room);
        crate::cmd_informative::look_at_room(g, victim, false);
        return true;
    }

    if get_stat(victim) == ARENA_OBSERVER {
        act(g, "$n disappears.", true, victim, None, ActArg::None, To::Room);
        g.char_from_room(victim);
        if let Some(rnum) = g.real_room(ARENA_OBSERVEROOM) {
            g.char_to_room(victim, rnum);
        }
        act(g, "$n appears in the middle of the room.", true, victim, None, ActArg::None, To::Room);
        crate::cmd_informative::look_at_room(g, victim, false);
        return true;
    }

    false
}

/// limits.c point_update: advance the flee-recall timer one tic for every
/// arena combatant. Called once per regen tic (only for awake, >= POS_STUNNED
/// combatants — the caller already gates on that). When the timer expires the
/// player may recall freely again.
pub fn arena_flee_pulse(g: &mut GameState, ch: CharId) {
    if !is_arena_combatant(ch) {
        return;
    }
    let ft = get_flee_timer(ch);
    if ft >= 1 + ARENA_FLEE_TIMEOUT {
        g.send_to_char(
            ch,
            "Flee-Recall timer expired. You may nowrecall without conceding the match.\r\n",
        );
        set_flee_timer(ch, 0);
    } else if ft >= 1 {
        let nft = ft + 1;
        set_flee_timer(ch, nft);
        g.send_to_char(
            ch,
            &format!(
                "Flee-Recall timer in tic #{}. {} tic(s) to go.\r\n",
                nft - 1,
                (ARENA_FLEE_TIMEOUT + 1) - nft
            ),
        );
    }
}

// ===========================================================================
// Movement hook (act.movement.c do_simple_move): leaving the prep room or
// observatory back through the arena entrance tears down arena state. Called
// AFTER the move's target room is known but BEFORE the char actually moves, so
// it can veto the move (returns false to block, as C `return 0`).
//
// `from` is the room the char is leaving; `to_vnum` is the destination vnum.
// ===========================================================================
pub fn arena_leave_via_exit(g: &mut GameState, ch: CharId, from: RoomRnum, to_vnum: RoomVnum) -> bool {
    let obs_room = g.real_room(ARENA_OBSERVEROOM);
    let prep_room = g.real_room(ARENA_PREPROOM);
    let name = get_name(g, ch);
    let lvl = level(g, ch);

    // Leaving the observatory toward the entrance: tear down observer state.
    if Some(from) == obs_room && to_vnum == ARENA_ENTRANCE {
        if lvl < LVL_IMMORT {
            arena_channel(g, &format!("{} has left the arena observatory.", name));
        }
        deobserve(ch);
        clearobservers(ch);
        set_stat(ch, ARENA_NOT);
        return true;
    }

    // Leaving the prep room.
    if Some(from) == prep_room {
        if to_vnum == ARENA_ENTRANCE {
            act(g, "$n has left the arena.", false, ch, None, ActArg::None, To::NotVict);
            if lvl < LVL_IMMORT {
                arena_channel(g, &format!("{} has left the arena.", name));
            }
            restore_bup_affects(g, ch);

            // Penalty for leaving a never-matched combatant.
            if get_stat(ch) == ARENA_COMBATANT1 && lvl < LVL_IMMORT {
                g.send_to_char(
                    ch,
                    "There's a penalty for leaving the arena without matching at least once!\r\n",
                );
                let penalty = lvl as i32 * ARENA_LEAVE_PENALTY_MULT;
                if penalty == 0 {
                    g.send_to_char(
                        ch,
                        "Setting your current move points to and mana points 1.\r\n\r\n",
                    );
                } else {
                    g.send_to_char(
                        ch,
                        &format!(
                            "Deducting {} coins from you and setting your current move points to and mana points 1.\r\n\r\n",
                            numdisplay(penalty as i64)
                        ),
                    );
                }
                // GET_MOVE = 1; GET_MANA = 1; then deduct gold, then bank.
                if let Some(c) = g.get_char_mut(ch) {
                    c.points.move_points = 1;
                    c.points.mana = 1;
                }
                if penalty > 0 {
                    apply_leave_penalty(g, ch, penalty);
                }
            } else {
                g.send_to_char(ch, "&GHope to see you soon!\r\n\r\n");
            }

            deobserve(ch);
            clearobservers(ch);
            set_stat(ch, ARENA_NOT);
            g.request_player_save(ch);

            // Repoint defaultobserve if the leaver was it.
            if get_defaultobserve() == Some(ch) {
                set_defaultobserve(findanyinarena(g));
            }
            return true;
        } else if get_stat(ch) == ARENA_COMBATANTZ {
            // Out of matches: only the entrance exit is allowed.
            g.send_to_char(
                ch,
                "Sorry, you've used up all your matches.\r\nYou may only leave. If you wish to play on, please leave and reenter\r\n",
            );
            return false; // veto the move
        }
    }

    true
}

/// Deduct the leave penalty: gold first, then bank (C act.movement.c).
fn apply_leave_penalty(g: &mut GameState, ch: CharId, penalty: i32) {
    if let Some(c) = g.get_char_mut(ch) {
        if penalty > c.points.gold {
            let mut rem = penalty - c.points.gold;
            c.points.gold = 0;
            if rem > c.points.bank_gold {
                c.points.bank_gold = 0;
            } else {
                c.points.bank_gold -= rem;
                rem = 0;
            }
            let _ = rem;
        } else {
            c.points.gold -= penalty;
        }
    }
}

/// act.movement.c: after an arena combatant moves between arena rooms, relay
/// the move to their observers. `dir` is the direction index (DIR_NAMES). Call
/// once the move has completed and `ch` is in its new room.
pub fn arena_relay_move(g: &mut GameState, ch: CharId, dir: usize) {
    if !is_arena_combatant(ch) {
        return;
    }
    let name = get_name(g, ch);
    let dirname = DIR_NAMES.get(dir).copied().unwrap_or("somewhere");
    let roomname = in_room(g, ch).map(|r| g.room(r).name.clone()).unwrap_or_default();
    let msg = format!("{} heads {} to: {}.\r\n", name, dirname, roomname);
    send_to_observers(g, &msg, ch);
}

// ===========================================================================
// Life-cycle teardown (comm.c close_socket on link-loss): an arena participant
// who goes linkdead has their affects restored and observer links cleared.
// ===========================================================================
pub fn on_link_lost(g: &mut GameState, ch: CharId) {
    if is_arena_combatant(ch) {
        restore_bup_affects(g, ch);
    }
    deobserve(ch);
    clearobservers(ch);
    // If the defaulted-on combatant just dropped, repoint it.
    if get_defaultobserve() == Some(ch) {
        set_defaultobserve(findanyinarena(g));
    }
}

/// Drop every trace of a character from the arena tables. Call from
/// extract_char so a removed entity can't leave dangling observer links or a
/// stale defaultobserve / arenamaster pointer.
pub fn forget_char(ch: CharId) {
    deobserve(ch);
    clearobservers(ch);
    if let Ok(mut w) = arena().lock() {
        w.chars.remove(&ch);
        if w.defaultobserve == Some(ch) {
            w.defaultobserve = None;
        }
        if w.arenamaster == Some(ch) {
            w.arenamaster = None;
        }
        // Scrub any lingering links that pointed at this char.
        for c in w.chars.values_mut() {
            if c.observing == Some(ch) {
                c.observing = None;
            }
            if c.observe_by == Some(ch) {
                c.observe_by = None;
            }
            if c.last_fighting == Some(ch) {
                c.last_fighting = None;
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    if let Ok(mut w) = arena().lock() {
        w.chars.clear();
        w.arenamaster = None;
        w.defaultobserve = None;
    }
}

// Keep the AFF_INVISIBLE import meaningful for affect bookkeeping parity with
// the C bup/restore (the affect flag set is round-tripped wholesale, so no
// per-flag handling is needed; this silences the unused-import lint while
// documenting that backed-up affect flags include AFF_INVISIBLE et al.).
const _: i64 = AFF_INVISIBLE;
