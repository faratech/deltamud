// auction.rs — the live auction house (CircleMUD/DeltaMUD auction.c), ported
// 1:1 to the id-indexed GameState.
//
// A single global auction runs at a time. A seller puts an item up with
// `auction <item> [minimum]`; the object is pulled out of their inventory into
// escrow (held by the runtime auction table, not in any room/inventory).
// Bidders place bids with `bid <amount>`; their gold is taken immediately and
// the previous high bidder is refunded. `auction_update(g)` is ticked from the
// heartbeat: with a high bidder it walks the going-once / going-twice /
// for-the-last-call / SOLD cascade, transferring the item to the winner and the
// gold to the seller. With no bidder it counts the item down and hands it back
// to the seller (or destroys it if the seller has gone). `stopauc` (immortal)
// aborts and refunds; `auctioneer` (immortal) renames the auction crier.
//
// The auction channel itself is broadcast by `auction_output`, which mirrors
// C's per-recipient color selection: connected players who aren't writing,
// aren't PRF_NOAUCT, and aren't in a SOUNDPROOF room each receive either the
// colored or the plain rendering depending on their COLOR level.
//
// Persistence: the auction is purely runtime state (C kept it in a single
// `struct auction_data auction;` global, reset on boot — there is no auction
// file). `boot_auction` therefore just installs a fresh empty table. The escrow
// object lives in GameState.objs with ObjLoc::Nowhere while it is up for sale,
// exactly as C's detached `auction.obj` pointer.

use crate::act::{act, ActArg, To};
use crate::state::GameState;
use crate::syslog::{mudlog, CMP};
use crate::types::*;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// auction.h #defines for the `ticks` state machine.
// ---------------------------------------------------------------------------
const AUC_NONE: i32 = -1; // no auction running
#[allow(dead_code)]
const AUC_NEW: i32 = 0; // (unused transient in C)
const AUC_BID: i32 = 1; // a fresh bid / item just listed
const AUC_ONCE: i32 = 2; // "going once"
const AUC_TWICE: i32 = 3; // "going twice"
const AUC_SOLD: i32 = 4; // sold / final

// auction.h: the default crier name.
const DEFAULT_AUCTIONEER: &str = "Arius";

// ---------------------------------------------------------------------------
// Flag/level constants used by the channel gate. These mirror structs.h /
// screen.h bits not present in the shared flags.rs subset; kept private here.
// ---------------------------------------------------------------------------
const PRF_NOAUCT: i64 = 1 << 18; // structs.h: can't hear the auction channel
const PLR_WRITING: i64 = 1 << 4; // structs.h: writing a board/mail/olc message
const PRF_COLOR_1: i64 = 1 << 13; // screen.h color bit 1
const PRF_COLOR_2: i64 = 1 << 14; // screen.h color bit 2
const ROOM_SOUNDPROOF: u32 = 1 << 5; // structs.h: shouts/channels blocked

const C_NRM: i32 = 2; // screen.h color level "normal"
const LVL_IMMORT: u8 = crate::types::LVL_IMMORT;
const LVL_IMPL: u8 = crate::types::LVL_IMPL;

// ---------------------------------------------------------------------------
// Runtime auction table (the C `struct auction_data auction;` global).
// seller/bidder are persistent player idnums (long in C); -1 means "none".
// `obj` is the escrowed object id (None == no item). Behind one mutex so the
// command handlers and the heartbeat update share a single canonical copy.
// ---------------------------------------------------------------------------
struct AuctionData {
    seller: i64,
    bidder: i64,
    obj: Option<ObjId>,
    bid: i64,
    ticks: i32,
    auctioneer: String,
}

impl AuctionData {
    fn empty() -> Self {
        AuctionData {
            seller: -1,
            bidder: -1,
            obj: None,
            bid: 0,
            ticks: AUC_NONE,
            auctioneer: DEFAULT_AUCTIONEER.to_string(),
        }
    }
}

static AUCTION: OnceLock<Mutex<AuctionData>> = OnceLock::new();

fn auction() -> &'static Mutex<AuctionData> {
    AUCTION.get_or_init(|| Mutex::new(AuctionData::empty()))
}

/// CircleMUD auction_reset(): return the auction to a non-bidding state. Does
/// NOT touch the escrowed object — callers decide what happens to it first.
fn auction_reset() {
    let mut a = crate::lock_ok::lock(&auction());
    a.bidder = -1;
    a.seller = -1;
    a.obj = None;
    a.ticks = AUC_NONE;
    a.bid = 0;
}

/// Boot hook (integrator calls once at startup, e.g. `boot_auction(&config.lib_path)`).
/// There is no auction file in C — the global simply starts empty — so this
/// installs (or, on a copyover/double-boot, resets) a fresh empty table. The
/// `lib_path` argument is accepted to match the other `boot_*` hooks; it is
/// unused because the auction has no on-disk state.
pub fn boot_auction(_lib_path: &str) {
    match AUCTION.get() {
        Some(m) => *crate::lock_ok::lock(&m) = AuctionData::empty(),
        None => {
            let _ = AUCTION.set(Mutex::new(AuctionData::empty()));
        }
    }
}

// ---------------------------------------------------------------------------
// Small predicate helpers (utils.h macros / screen.h color level), private to
// this module so we add nothing to GameState.
// ---------------------------------------------------------------------------

fn is_npc(g: &GameState, id: CharId) -> bool {
    g.get_char(id).map(|c| c.is_npc).unwrap_or(false)
}

fn prf_flagged(g: &GameState, id: CharId, flag: i64) -> bool {
    g.get_char(id)
        .map(|c| c.prf_flags & flag != 0)
        .unwrap_or(false)
}

fn get_idnum(g: &GameState, id: CharId) -> i64 {
    g.get_char(id).map(|c| c.idnum).unwrap_or(-1)
}

fn get_gold(g: &GameState, id: CharId) -> i32 {
    g.get_char(id).map(|c| c.points.gold).unwrap_or(0)
}

fn get_level(g: &GameState, id: CharId) -> u8 {
    g.get_char(id).map(|c| c.player.level).unwrap_or(0)
}

fn get_name(g: &GameState, id: CharId) -> String {
    g.get_char(id)
        .map(|c| c.player.name.clone())
        .unwrap_or_default()
}

/// obj->short_description, with the "something" fallback C guards against.
fn obj_short(g: &GameState, oid: ObjId) -> String {
    match g.get_obj(oid) {
        Some(o) if !o.short_description.is_empty() => o.short_description.clone(),
        _ => "something".to_string(),
    }
}

/// COLOR_LEV(ch): _clrlevel = (PRF_COLOR_1?1:0) + (PRF_COLOR_2?2:0). (screen.h)
fn color_lev(g: &GameState, id: CharId) -> i32 {
    let c = match g.get_char(id) {
        Some(c) => c,
        None => return 0,
    };
    let mut lev = 0;
    if c.prf_flags & PRF_COLOR_1 != 0 {
        lev += 1;
    }
    if c.prf_flags & PRF_COLOR_2 != 0 {
        lev += 2;
    }
    lev
}

/// True if the character is mid-edit (board/mail/olc). C tests PLR_WRITING;
/// the Rust port also expresses "writing" as a non-empty editor stack on the
/// descriptor, so we honor either signal.
fn is_writing(g: &GameState, id: CharId) -> bool {
    let c = match g.get_char(id) {
        Some(c) => c,
        None => return false,
    };
    if !c.is_npc && c.act_flags & PLR_WRITING != 0 {
        return true;
    }
    match c.desc.and_then(|conn| g.descriptors.get(&conn)) {
        Some(d) => !d.editors.is_empty(),
        None => false,
    }
}

/// ROOM_FLAGGED(IN_ROOM(ch), ROOM_SOUNDPROOF).
fn in_soundproof(g: &GameState, id: CharId) -> bool {
    let rnum = match g.get_char(id).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    g.room_opt(rnum)
        .map(|r| r.room_flags.bits() & ROOM_SOUNDPROOF != 0)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// auction.c get_ch_by_id / get_ch_by_id_desc — find a player by persistent
// idnum. `_by_id` scans every PC still in the world (including link-dead);
// `_by_id_desc` requires an attached descriptor. We walk char_list for parity
// with the C character_list traversal order. idnum < 0 never matches (and is
// the "no seller/bidder" sentinel).
// ---------------------------------------------------------------------------
fn get_ch_by_id(g: &GameState, idnum: i64) -> Option<CharId> {
    if idnum < 0 {
        return None;
    }
    g.chars.keys().copied().find(|&cid| {
        g.get_char(cid)
            .map(|c| !c.is_npc && c.idnum == idnum)
            .unwrap_or(false)
    })
}

fn get_ch_by_id_desc(g: &GameState, idnum: i64) -> Option<CharId> {
    if idnum < 0 {
        return None;
    }
    g.chars.keys().copied().find(|&cid| {
        g.get_char(cid)
            .map(|c| !c.is_npc && c.idnum == idnum && c.desc.is_some())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// auction_output : dispense an auction-channel line to everyone connected.
//
// C builds, per recipient, "[AUCTION] <crier>: '<body>'", where the crier name
// is framed in CCMAG/CCCYN/CCNRM (gated at C_NRM) and the body is the colored
// rendering when COLOR_LEV > C_NRM, else the plain rendering.
//
// Here `color` already carries the colored body (as &-codes); `black` carries
// the plain body. Because the Rust output layer always renders &-codes, we
// pick &-coded framing per recipient with their color level: those whose level
// reaches C_NRM get the magenta/cyan crier framing, and those above C_NRM get
// the colored body; everyone else gets the plain text. The recipient gate
// (connected, not writing, not PRF_NOAUCT, not in a soundproof room) matches C.
// ---------------------------------------------------------------------------
fn auction_output(g: &mut GameState, color: &str, black: &str) {
    let crier = crate::lock_ok::lock(&auction()).auctioneer.clone();

    // Recipient set: every playing descriptor with an attached character.
    let recipients: Vec<CharId> = g
        .descriptors
        .values()
        .filter(|d| matches!(d.state, crate::connection::ConState::Playing))
        .filter_map(|d| d.character)
        .collect();

    for cid in recipients {
        if is_writing(g, cid) || prf_flagged(g, cid, PRF_NOAUCT) || in_soundproof(g, cid) {
            continue;
        }
        let lev = color_lev(g, cid);
        // CCMAG / CCCYN / CCNRM are active only at COLOR_LEV >= C_NRM.
        let (mag, cyn, nrm) = if lev >= C_NRM {
            ("&m", "&c", "&n")
        } else {
            ("", "", "")
        };
        // Body: colored rendering when above C_NRM (C: COLOR_LEV > C_NRM).
        let body = if lev > C_NRM { color } else { black };
        let line = format!(
            "&c[&YAUCTION&c]&n {}{}{}{}: '{}{}{}'{}\r\n",
            mag, crier, cyn, nrm, body, mag, nrm, ""
        );
        g.send_to_char(cid, &line);
    }
}

// Build the colored / plain body pair for the going-once cascade. The colored
// rendering uses bold-white (&W) for the highlighted tokens and magenta (&m)
// for the connectives, matching C's \x1B[1;37m / \x1B[35m sequences.
fn going_phase(ticks: i32) -> &'static str {
    match ticks {
        AUC_BID => "once",
        AUC_ONCE => "twice",
        AUC_TWICE => "for the last call",
        _ => "",
    }
}

// "coin" vs "coins" suffix. C uses "s" for !=1 and a trailing space for ==1 in
// the cascade lines, but "s"/"" in the bid/list lines; we reproduce each at the
// call site to stay byte-faithful.
fn coin_s(bid: i64) -> &'static str {
    if bid != 1 {
        "s"
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// auction_update : the heartbeat tick. Drives the going-once/twice/last-call/
// SOLD cascade and performs the escrow/gold transfer on sale. The integrator
// calls this once per relevant pulse (C called it from heartbeat() every
// PULSE_AUCTION). Returns immediately when no auction is running.
// ---------------------------------------------------------------------------
pub fn auction_update(g: &mut GameState) {
    // Snapshot the table (copy locals before any send/act, per borrow rules).
    let (ticks, seller_id, bidder_id, obj, bid) = {
        let a = crate::lock_ok::lock(&auction());
        (a.ticks, a.seller, a.bidder, a.obj, a.bid)
    };

    if ticks == AUC_NONE {
        return; // No auction.
    }

    // Seller left the game entirely (not in world, not on a descriptor)?
    // C (auction.c:57-63) destroys the lot here WITHOUT refunding the high
    // bidder's debited gold -- real money destruction. Refund first.
    if get_ch_by_id_desc(g, seller_id).is_none() && get_ch_by_id(g, seller_id).is_none() {
        let (bid, bidder_id) = (bid, bidder_id); // already snapshotted above
        if bid > 0 {
            if let Some(b) = get_ch_by_id(g, bidder_id) {
                if let Some(c) = g.get_char_mut(b) {
                    c.points.gold = c.points.gold.saturating_add(bid as i32);
                }
                g.send_to_char(
                    b,
                    "The auction has been cancelled (seller left).  Your bid has been returned.\r\n",
                );
            }
        }
        if let Some(oid) = obj {
            g.extract_obj(oid);
        }
        auction_reset();
        return;
    }

    // Only the bidding states drive the cascade.
    if !(AUC_BID..=AUC_SOLD).contains(&ticks) {
        return;
    }

    let bidder = get_ch_by_id(g, bidder_id);
    let seller = get_ch_by_id(g, seller_id);
    let oid = match obj {
        Some(o) => o,
        // No object but a live auction state — nothing sensible to crier; bail.
        None => {
            auction_reset();
            return;
        }
    };
    let short = obj_short(g, oid);
    let s = coin_s(bid);

    // --- There IS a bidder and it's not yet sold: going once/twice/last call.
    if bidder.is_some() && ticks < AUC_SOLD {
        let bidder = bidder.unwrap();
        let phase = going_phase(ticks);
        let bname = get_name(g, bidder);
        // " " trailing for ==1, "s" otherwise (C: bid != 1 ? "s" : " ").
        let cs = if bid != 1 { "s" } else { " " };
        let plain = format!(
            "{} is going {} to {} for {} coin{}.",
            short, phase, bname, bid, cs
        );
        let color = format!(
            "&W{}&m is going &W{}&m to &W{}&m for &W{}&m coin{}.",
            short, phase, bname, bid, cs
        );
        auction_output(g, &color, &plain);
        bump_ticks();
        return;
    }

    // --- No bidder, and we ARE in the sold state: SOLD to no one.
    if bidder.is_none() && ticks == AUC_SOLD {
        let plain = format!("{} is SOLD to no one for {} coin{}.", short, bid, s);
        let color = format!(
            "&W{}&m is &WSOLD&m to &Wno one&m for &W{}&m coin{}.",
            short, bid, s
        );
        auction_output(g, &color, &plain);
        // Give the seller his unsold goods back, or destroy them if he's gone.
        match seller {
            Some(sid) => {
                g.obj_to_char(oid, sid);
            }
            None => g.extract_obj(oid),
        }
        auction_reset();
        return;
    }

    // --- No bidder, not yet sold: going once/twice/last call to no one.
    if bidder.is_none() && ticks < AUC_SOLD {
        let phase = going_phase(ticks);
        let plain = format!(
            "{} is going {} to no one for {} coin{}.",
            short, phase, bid, s
        );
        let color = format!(
            "&W{}&m is going &W{}&m to &Wno one&m for &W{}&m coin{}.",
            short, phase, bid, s
        );
        auction_output(g, &color, &plain);
        bump_ticks();
        return;
    }

    // --- There IS a bidder and we are in the sold state: SOLD to the bidder.
    if bidder.is_some() && ticks >= AUC_SOLD {
        let bidder = bidder.unwrap();
        let bname = {
            let n = get_name(g, bidder);
            if n.is_empty() {
                "someone".to_string()
            } else {
                n
            }
        };
        let plain = format!("{} is SOLD to {} for {} coin{}.", short, bname, bid, s);
        let color = format!(
            "&W{}&m is &WSOLD&m to &W{}&m for &W{}&m coin{}.",
            short, bname, bid, s
        );
        auction_output(g, &color, &plain);

        // Pay the seller (if still around) and watchdog-log immortal trades.
        if let Some(sid) = seller {
            let sname = {
                let n = get_name(g, sid);
                if n.is_empty() {
                    "someone".to_string()
                } else {
                    n
                }
            };
            let watch = format!(
                "[WATCHDOG] {} auctions {} to {} for {} coin{}.",
                sname, short, bname, bid, s
            );
            if get_level(g, bidder) >= LVL_IMMORT || get_level(g, sid) >= LVL_IMMORT {
                mudlog(g, &watch, CMP, LVL_IMPL);
            }
            if let Some(c) = g.get_char_mut(sid) {
                c.points.gold += bid as i32;
            }
            act(
                g,
                "Congrats! You have sold $p!",
                false,
                sid,
                Some(oid),
                ActArg::None,
                To::Char,
            );
        }
        // Hand the object to the winner (the bidder is here by definition).
        g.obj_to_char(oid, bidder);
        act(
            g,
            "Congrats! You now have $p!",
            false,
            bidder,
            Some(oid),
            ActArg::None,
            To::Char,
        );

        auction_reset();
    }
}

/// Increment the auction timer (C: auction.ticks++).
fn bump_ticks() {
    let mut a = crate::lock_ok::lock(&auction());
    a.ticks += 1;
}

// ---------------------------------------------------------------------------
// do_bid : user interface to place a bid.
// ---------------------------------------------------------------------------
pub fn do_bid(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    // NPCs lack an idnum, so they can neither bid nor auction.
    if is_npc(g, ch) {
        g.send_to_char(ch, "You aren't unique enough to bid.\r\n");
        return;
    }

    // Snapshot the auction state.
    let (ticks, seller_id, bidder_id, obj, cur_bid) = {
        let a = crate::lock_ok::lock(&auction());
        (a.ticks, a.seller, a.bidder, a.obj, a.bid)
    };

    if ticks == AUC_NONE {
        g.send_to_char(ch, "Nothing is up for sale.\r\n");
        return;
    }

    if prf_flagged(g, ch, PRF_NOAUCT) {
        g.send_to_char(ch, "You can't bid when you're not on the channel\r\n");
        return;
    }

    let (word, _) = crate::interpreter::one_argument(argument);
    let bid: i64 = word.parse::<i64>().unwrap_or(0);

    // No argument — report the current bid.
    if word.is_empty() {
        let s = if cur_bid != 1 { "s." } else { "." };
        g.send_to_char(ch, &format!("Current bid: {} coin{}\r\n", cur_bid, s));
        return;
    }

    // The current high bidder is this person.
    if Some(ch) == get_ch_by_id_desc(g, bidder_id) {
        g.send_to_char(ch, "You're trying to outbid yourself.\r\n");
        return;
    }
    // The seller is the one trying to bid.
    if Some(ch) == get_ch_by_id_desc(g, seller_id) {
        g.send_to_char(ch, "You can't bid on your own item.\r\n");
        return;
    }
    // Below the minimum where there is no bidder yet.
    if bid < cur_bid && bidder_id < 0 {
        g.send_to_char(
            ch,
            &format!("The minimum is currently {} coins.\r\n", cur_bid),
        );
        return;
    }
    // Below the 5%-over threshold where there IS a bidder, or a zero bid.
    if (bid as f64) < cur_bid as f64 * 1.05 && bidder_id >= 0 || bid == 0 {
        let need = (cur_bid as f64 * 1.05 + 1.0).round() as i64;
        g.send_to_char(
            ch,
            &format!(
                "Try bidding at least 5% over the current bid of {}. ({} coins).\r\n",
                cur_bid, need
            ),
        );
        return;
    }
    // Not enough gold on hand.
    if (get_gold(g, ch) as i64) < bid {
        g.send_to_char(
            ch,
            &format!("You have only {} coins on hand.\r\n", get_gold(g, ch)),
        );
        return;
    }

    // It's a valid bid. Refund the previous bidder if they're still around.
    if bidder_id >= 0 {
        if let Some(prev) = get_ch_by_id(g, bidder_id) {
            if let Some(c) = g.get_char_mut(prev) {
                c.points.gold += cur_bid as i32;
            }
        }
    }

    // Record the new bid, take the gold, and reset to first-chance.
    let my_id = get_idnum(g, ch);
    {
        let mut a = crate::lock_ok::lock(&auction());
        a.bid = bid;
        a.bidder = my_id;
        a.ticks = AUC_BID;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.points.gold -= bid as i32;
    }

    let bname = get_name(g, ch);
    let short = match obj {
        Some(oid) => obj_short(g, oid),
        None => "something".to_string(),
    };
    let s = coin_s(bid);
    let plain = format!("{} bids {} coin{} on {}.", bname, bid, s, short);
    let color = format!("&W{}&m bids &W{}&m coin{} on &W{}&m.", bname, bid, s, short);
    auction_output(g, &color, &plain);
}

// ---------------------------------------------------------------------------
// do_auction : user interface for placing an item up for sale.
// ---------------------------------------------------------------------------
pub fn do_auction(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if is_npc(g, ch) {
        g.send_to_char(ch, "You're not unique enough to auction.\r\n");
        return;
    }

    let (arg1, arg2) = two_arguments(argument);

    if prf_flagged(g, ch, PRF_NOAUCT) {
        g.send_to_char(ch, "You can't auction when you're not on the channel\r\n");
        return;
    }

    let (ticks, seller_id, obj, cur_bid) = {
        let a = crate::lock_ok::lock(&auction());
        (a.ticks, a.seller, a.obj, a.bid)
    };

    if arg1.is_empty() {
        g.send_to_char(ch, "Auction what for what minimum?\r\n");
        return;
    }

    // An auction is already running — report its status instead of listing.
    if ticks != AUC_NONE {
        let short = match obj {
            Some(oid) => obj_short(g, oid),
            None => "something".to_string(),
        };
        let line = match get_ch_by_id(g, seller_id) {
            Some(sid) => format!(
                "{} is currently auctioning {} for {} coins.\r\n",
                get_name(g, sid),
                short,
                cur_bid
            ),
            None => format!(
                "No one is currently auctioning {} for {} coins.\r\n",
                short, cur_bid
            ),
        };
        g.send_to_char(ch, &line);
        return;
    }

    // Find the item in the seller's inventory.
    let carrying = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    let obj = match g.get_obj_in_list_vis(ch, &arg1, &carrying) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You don't seem to have that to sell.\r\n");
            return;
        }
    };

    // Corpses (containers with a non-zero value[3]) may decompose — refuse.
    if let Some(o) = g.get_obj(obj) {
        if o.obj_type == crate::object::ObjectType::Container && o.values[3] != 0 {
            g.send_to_char(ch, "You can not auction corpses.\n\r");
            return;
        }
    }

    // It's valid: list it. Minimum can not drop below 1 coin.
    let minimum = {
        let v = arg2.parse::<i64>().unwrap_or(0);
        if v > 0 {
            v
        } else {
            1
        }
    };
    let seller_idnum = get_idnum(g, ch);

    // Pull the object out of the character so it can't be dropped (escrow).
    g.obj_from_anywhere(obj);

    {
        let mut a = crate::lock_ok::lock(&auction());
        a.ticks = AUC_BID;
        a.seller = seller_idnum;
        a.bid = minimum;
        a.obj = Some(obj);
        a.bidder = -1;
    }

    let sname = get_name(g, ch);
    let short = obj_short(g, obj);
    let s = if minimum != 1 { "s." } else { "." };
    let plain = format!(
        "{} puts {} up for sale, minimum bid {} coin{}",
        sname, short, minimum, s
    );
    let color = format!(
        "&W{}&m puts &W{}&m up for sale, minimum bid &W{}&m coin{}",
        sname, short, minimum, s
    );
    auction_output(g, &color, &plain);
}

// ---------------------------------------------------------------------------
// do_auctioneer : change the name used on the auction channel (immortal).
// ---------------------------------------------------------------------------
pub fn do_auctioneer(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let name = argument.trim();
    if name.is_empty() {
        g.send_to_char(ch, "Must have a name!\r\n");
        return;
    }
    {
        let mut a = crate::lock_ok::lock(&auction());
        a.auctioneer = name.to_string();
    }
    g.send_to_char(ch, "&YOkay.&n\r\n");
}

// ---------------------------------------------------------------------------
// do_stop_auction : abort the running auction, refunding bidder and returning
// (or destroying) the item (immortal "stopauc").
// ---------------------------------------------------------------------------
pub fn do_stop_auction(g: &mut GameState, ch: CharId, _argument: &str, _subcmd: i32) {
    let (seller_id, bidder_id, obj, bid) = {
        let a = crate::lock_ok::lock(&auction());
        (a.seller, a.bidder, a.obj, a.bid)
    };

    // Return the escrowed object to the seller, or destroy it if he's gone.
    if let Some(oid) = obj {
        match get_ch_by_id(g, seller_id) {
            Some(sid) => g.obj_to_char(oid, sid),
            None => g.extract_obj(oid),
        }
    }

    // Refund the high bidder if there was a live bid.
    if bid != 0 {
        if let Some(bid_ch) = get_ch_by_id(g, bidder_id) {
            if let Some(c) = g.get_char_mut(bid_ch) {
                c.points.gold += bid as i32;
            }
        }
    }

    auction_reset();
    g.send_to_char(ch, "&YOkay.&n\r\n");
}

// ---------------------------------------------------------------------------
// two_arguments(): one_argument(one_argument(...)). Local copy (the shared
// interpreter exposes one_argument/half_chop; two_arguments lives per-module).
// ---------------------------------------------------------------------------
fn two_arguments(argument: &str) -> (String, String) {
    let (a1, rest) = crate::interpreter::one_argument(argument);
    let (a2, _) = crate::interpreter::one_argument(rest);
    (a1, a2)
}
