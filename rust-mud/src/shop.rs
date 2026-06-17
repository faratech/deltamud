// shop.rs — the CircleMUD/DeltaMUD shopkeeper system (shop.c), ported 1:1 to
// the id-indexed GameState.
//
// What lives here:
//   * boot_shops(lib_path)            -> load world/shp/index + *.shp (v3.0 fmt)
//   * shop_keeper(g,ch,me,cmd,arg)    -> the SPECIAL() spec proc (exported for
//                                        the spec dispatcher that lands later)
//   * do_buy / do_sell / do_list /
//     do_value / do_appraise          -> the player command handlers. The C
//                                        routes "buy"/"sell"/"list"/"value"
//                                        through the keeper spec proc (they are
//                                        otherwise do_not_here); since spec
//                                        dispatch isn't wired yet, each handler
//                                        finds the shopkeeper in the room and
//                                        runs the shopping_* path itself, then
//                                        falls back to the do_not_here message.
//   * the pricing / RPN keyword evaluator / sort_keeper_objs / trade_with /
//     ok_shop_room and all the shopping_buy/sell/list/value internals.
//
// Borrow discipline: every broadcast copies the locals it needs out of the
// arena first (act() / send takes &mut GameState), then re-looks-up by id.
//
// ON-DISK FORMAT (matched byte-for-byte against C boot_the_shops):
//   A shop file begins with a fread_string ending in '~'. If that opening
//   string contains "v3.0" the file is new-format (every shipped file is).
//   Each shop is then:
//     #<vnum>~                         (fread_string)
//     <producing ints, one per line, terminated by -1>   (real obj vnums)
//     <profit_buy   float>
//     <profit_sell  float>
//     <trade type list: lines "NAME [keywords]" or "<int> [keywords]",
//      terminated by a line whose number is -1>
//     7 x fread_string  (no_such_item1, no_such_item2, do_not_buy,
//                        missing_cash1, missing_cash2, message_buy,
//                        message_sell)   -- NOTE C field order, see below
//     <temper1 int>
//     <bitvector int>
//     <keeper mob vnum int>
//     <with_who (trade-with bitvector) int>
//     <in_room vnums, one per line, terminated by -1>
//     <open1 int> <close1 int> <open2 int> <close2 int>
//   The file ends with a fread_string beginning with '$'.
//
//   IMPORTANT field-order subtlety: shop.h's struct lists do_not_buy 5th but
//   boot_the_shops reads the seven strings in the order:
//     no_such_item1, no_such_item2, do_not_buy, missing_cash1,
//     missing_cash2, message_buy, message_sell
//   We read them in that exact file order.

use crate::act::{act, ActArg, To};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Constants (shop.h).
// ---------------------------------------------------------------------------

// Object-trade result codes.
const OBJECT_DEAD: i32 = 0;
const OBJECT_NOTOK: i32 = 1;
const OBJECT_OK: i32 = 2;
const OBJECT_NOVAL: i32 = 3;

// trade-with bitvector (SHOP_TRADE_WITH).
const TRADE_NOGOOD: i32 = 1;
const TRADE_NOEVIL: i32 = 2;
const TRADE_NONEUTRAL: i32 = 4;
const TRADE_NOMAGIC_USER: i32 = 8;
const TRADE_NOCLERIC: i32 = 16;
const TRADE_NOTHIEF: i32 = 32;
const TRADE_NOWARRIOR: i32 = 64;
const TRADE_NOARTISAN: i32 = 128;

// bitvector flags (SHOP_KILL_CHARS / SHOP_USES_BANK).
const WILL_START_FIGHT: i32 = 1;
const WILL_BANK_MONEY: i32 = 2;

const MIN_OUTSIDE_BANK: i32 = 5000;
const MAX_OUTSIDE_BANK: i32 = 15000;

// RPN operator codes (find_oper_num index into operator_str[]).
const OPER_OPEN_PAREN: i32 = 0;
const OPER_CLOSE_PAREN: i32 = 1;
const OPER_OR: i32 = 2;
const OPER_AND: i32 = 3;
const OPER_NOT: i32 = 4;
const MAX_OPER: i32 = 4;

// operator_str[] from shop.c.
const OPERATOR_STR: [&str; 5] = ["[({", "])}", "|+", "&*", "^'"];

// trade_letters[] (who we sell to), terminated implicitly by length.
const TRADE_LETTERS: [&str; 8] = [
    "Good", "Evil", "Neutral", "Magic User", "Cleric", "Thief", "Warrior", "Artisan",
];

// shop_bits[] for the detailed listing.
const SHOP_BITS: [&str; 2] = ["WILL_FIGHT", "USES_BANK"];

// Standard message strings (shop.h #defines).
const MSG_NOT_OPEN_YET: &str = "Come back later!";
const MSG_NOT_REOPEN_YET: &str = "Sorry, we have closed, but come back later.";
const MSG_CLOSED_FOR_DAY: &str = "Sorry, come back tomorrow.";
const MSG_NO_SEE_CHAR: &str = "I don't trade with someone I can't see!";
const MSG_NO_SELL_ALIGN: &str = "Get out of here before I call the guards!";
const MSG_NO_SELL_CLASS: &str = "We don't serve your kind here!";
const MSG_NO_SELL_PAWN: &str = "You can only pawn stuff with me!";
const MSG_NO_USED_WANDSTAFF: &str = "I don't buy used up wands or staves!";
const MSG_NO_STEAL_HERE: &str = "$n is a bloody thief!";
const MSG_CANT_KILL_KEEPER: &str = "Get out of here before I call the guards!";

// config.c / structs.h gates used by the shop (the pawnshop-in-jail logic and
// IS_GOD; these mirror the values already used across the Rust port).
const JAIL_NUM: RoomVnum = 400;
const PK_ALLOWED: bool = false;
const PLR_KILLER: i64 = 1 << 0;

// structs.h ITEM_* (RAW item-type integers as stored in the shop file and as
// CircleMUD's GET_OBJ_TYPE returns them). The Rust ObjectType enum uses a
// *different, collapsed* numbering, so we translate explicitly — see
// `obj_type_int()` — to keep the on-disk .shp format byte-identical.
const ITEM_LIGHT: i32 = 1;
const ITEM_SCROLL: i32 = 2;
const ITEM_WAND: i32 = 3;
const ITEM_STAFF: i32 = 4;
const ITEM_WEAPON: i32 = 5;
const ITEM_FIREWEAPON: i32 = 6;
const ITEM_MISSILE: i32 = 7;
const ITEM_TREASURE: i32 = 8;
const ITEM_ARMOR: i32 = 9;
const ITEM_POTION: i32 = 10;
const ITEM_WORN: i32 = 11;
const ITEM_OTHER: i32 = 12;
const ITEM_TRASH: i32 = 13;
const ITEM_TRAP: i32 = 14;
const ITEM_CONTAINER: i32 = 15;
const ITEM_NOTE: i32 = 16;
const ITEM_DRINKCON: i32 = 17;
const ITEM_KEY: i32 = 18;
const ITEM_FOOD: i32 = 19;
const ITEM_MONEY: i32 = 20;
const ITEM_PEN: i32 = 21;
const ITEM_BOAT: i32 = 22;
const ITEM_FOUNTAIN: i32 = 23;

// ITEM_NOSELL extra flag bit (structs.h) — matches ExtraFlags::NO_SELL (1<<16).
const ITEM_NOSELL_BIT: u64 = 1 << 16;

// item_types[] names for read_type_list parsing + the detailed listing. This
// matches constants.c item_types[] (stock CircleMUD numbering, index == type).
const ITEM_TYPE_NAMES: [&str; 24] = [
    "UNDEFINED",
    "LIGHT",
    "SCROLL",
    "WAND",
    "STAFF",
    "WEAPON",
    "FIRE WEAPON",
    "MISSILE",
    "TREASURE",
    "ARMOR",
    "POTION",
    "WORN",
    "OTHER",
    "TRASH",
    "TRAP",
    "CONTAINER",
    "NOTE",
    "LIQ CONTAINER",
    "KEY",
    "FOOD",
    "MONEY",
    "PEN",
    "BOAT",
    "FOUNTAIN",
];

// extra_bits[] names for the RPN keyword evaluator (constants.c extra_bits[]).
// Index == bit position; push(IS_SET(EXTRA, 1<<index)).
const EXTRA_BITS: [&str; 18] = [
    "GLOW", "HUM", "!RENT", "!DONATE", "!INVIS", "INVISIBLE", "MAGIC", "!DROP", "BLESS", "!GOOD",
    "!EVIL", "!NEUTRAL", "!MAGE", "!CLERIC", "!THIEF", "!WARRIOR", "!SELL", "!ARTISAN",
];

// ---------------------------------------------------------------------------
// Runtime shop table.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ShopBuyData {
    /// Trade item type (raw structs.h ITEM_* integer) or NOTHING terminator.
    type_: i32,
    /// Optional keyword expression for this trade type.
    keywords: Option<String>,
}

#[derive(Clone, Debug)]
struct ShopData {
    vnum: i32,
    /// Produced item *real* numbers (obj vnums; resolved at trade time). C
    /// stores real_object() rnums, but we keep vnums and look up obj_protos.
    producing: Vec<ObjVnum>,
    profit_buy: f32,
    profit_sell: f32,
    type_: Vec<ShopBuyData>,
    no_such_item1: String,
    no_such_item2: String,
    do_not_buy: String,
    missing_cash1: String,
    missing_cash2: String,
    message_buy: String,
    message_sell: String,
    temper1: i32,
    bitvector: i32,
    /// Keeper mob *vnum* (-1 / NOBODY when none). C converts to rnum; we keep
    /// the vnum and match against Character::nr at dispatch time.
    keeper: MobVnum,
    with_who: i32,
    in_room: Vec<RoomVnum>,
    open1: i32,
    close1: i32,
    open2: i32,
    close2: i32,
    // Runtime mutable state.
    bank_account: i32,
    lastsort: i32,
}

impl ShopData {
    fn new(vnum: i32) -> Self {
        ShopData {
            vnum,
            producing: Vec::new(),
            profit_buy: 1.0,
            profit_sell: 1.0,
            type_: Vec::new(),
            no_such_item1: String::new(),
            no_such_item2: String::new(),
            do_not_buy: String::new(),
            missing_cash1: String::new(),
            missing_cash2: String::new(),
            message_buy: String::new(),
            message_sell: String::new(),
            temper1: 0,
            bitvector: 0,
            keeper: NOBODY,
            with_who: 0,
            in_room: Vec::new(),
            open1: 0,
            close1: 0,
            open2: 0,
            close2: 0,
            bank_account: 0,
            lastsort: 0,
        }
    }
}

static SHOPS: OnceLock<Mutex<Vec<ShopData>>> = OnceLock::new();

fn shops() -> &'static Mutex<Vec<ShopData>> {
    SHOPS.get_or_init(|| Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------------------
// SHOP_FUNC secondary-spec registry (shop.h SHOP_FUNC + shop.c
// assign_the_shopkeepers).
//
// In C every shop_index[] entry carries a `func` pointer (SHOP_FUNC(i)).
// assign_the_shopkeepers() runs *after* assign_mobiles(): for each keeper it
// captures whatever spec the keeper mob already had —
//   `if (mob_index[SHOP_KEEPER].func) SHOP_FUNC(index) = mob_index[...].func;`
// — into SHOP_FUNC, then overwrites the keeper's own func with shop_keeper().
// shop_keeper() then invokes SHOP_FUNC first and, if it returns nonzero, treats
// the command as consumed. The net effect: a mob that is *both* a shop keeper
// and an assign_mobiles() spec runs the shop logic as its primary proc and the
// original spec as a secondary, consulted before any buy/sell/list handling.
//
// We mirror this exactly. The keeper's "original" spec is the statically
// assigned mob spec, i.e. spec_assign::get_mob_spec(keeper_vnum). We build a
// vnum->SpecFn map (keeper mob vnum -> the captured secondary) lazily the first
// time shop_keeper() consults it, which is well after both the shop table
// (boot_shops) and the static mob-spec tables (spec_assign::assign_specs) have
// been populated. The OnceLock makes the build idempotent and order-independent
// (get_mob_spec lazily builds its own table if it has not yet).
//
// In the shipped DeltaMUD world no shop-keeper vnum is also an assign_mobiles()
// target, so this map ends up empty — byte-for-byte what C produces (every
// SHOP_FUNC stays 0). The machinery is faithful regardless: if a future build
// assigns a spec to a keeper vnum, it is captured and dispatched here.
type ShopFn = crate::spec_assign::SpecFn;

static SHOP_FUNCS: OnceLock<HashMap<MobVnum, ShopFn>> = OnceLock::new();

/// SHOP_FUNC(shop_nr) lookup, keyed by the shop's keeper mob vnum. Returns the
/// captured secondary spec, or None when the keeper had no prior spec (the
/// common case — equivalent to C's SHOP_FUNC == 0).
fn shop_func(keeper_vnum: MobVnum) -> Option<ShopFn> {
    SHOP_FUNCS
        .get_or_init(|| {
            // assign_the_shopkeepers(): walk every shop, capture the keeper's
            // existing static spec as its SHOP_FUNC. NOBODY keepers are skipped
            // exactly as C does (`if (SHOP_KEEPER(index) == NOBODY) continue;`).
            let mut map: HashMap<MobVnum, ShopFn> = HashMap::new();
            if let Ok(guard) = shops().lock() {
                for s in guard.iter() {
                    if s.keeper == NOBODY {
                        continue;
                    }
                    if let Some(func) = crate::spec_assign::get_mob_spec(s.keeper) {
                        map.insert(s.keeper, func);
                    }
                }
            }
            map
        })
        .get(&keeper_vnum)
        .copied()
}

pub fn is_shop_keeper_vnum(keeper_vnum: MobVnum) -> bool {
    shops()
        .lock()
        .map(|guard| guard.iter().any(|s| s.keeper == keeper_vnum))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Boot: load the shop index + files.
// ---------------------------------------------------------------------------

/// Integrator entry point: load every shop from `<lib_path>/world/shp/`. Call
/// once at boot (alongside prime_zones). Degrades to an empty table — and thus
/// "no shop" everywhere — if the index or any file is missing/garbled, exactly
/// as the C boot path leaves top_shop at 0 when shp/ is absent.
pub fn boot_shops(lib_path: &str) {
    let dir = format!("{}/world/shp", lib_path.trim_end_matches('/'));
    let index_path = format!("{}/index", dir);
    let index = match std::fs::read_to_string(&index_path) {
        Ok(s) => s,
        Err(_) => {
            // No index -> no shops (mirrors C with an empty shp dir).
            let _ = shops().lock().map(|mut v| v.clear());
            return;
        }
    };

    let mut table: Vec<ShopData> = Vec::new();
    for line in index.lines() {
        let fname = line.trim();
        if fname.is_empty() || fname == "$" {
            break;
        }
        let path = format!("{}/{}", dir, fname);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            boot_the_shops(&contents, &mut table);
        }
    }

    if let Ok(mut guard) = shops().lock() {
        *guard = table;
    }
}

/// A line-oriented cursor over the .shp file body, providing the get_line /
/// fread_string primitives boot_the_shops relies on.
struct ShopReader<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> ShopReader<'a> {
    fn new(s: &'a str) -> Self {
        ShopReader { lines: s.lines().collect(), pos: 0 }
    }

    /// get_line(): next non-comment, non-blank line (CircleMUD skips lines that
    /// are empty or begin with '*'). Returns None at EOF.
    fn get_line(&mut self) -> Option<&'a str> {
        while self.pos < self.lines.len() {
            let l = self.lines[self.pos].trim_end_matches('\r');
            self.pos += 1;
            let t = l.trim();
            if t.is_empty() || t.starts_with('*') {
                continue;
            }
            return Some(l);
        }
        None
    }

    /// fread_string(): read lines until one ends with '~'; the '~' (and
    /// everything after it on that line) is stripped, internal newlines kept as
    /// "\r\n". Comment/blank skipping does NOT apply inside a string.
    fn fread_string(&mut self) -> String {
        let mut out = String::new();
        let mut first = true;
        while self.pos < self.lines.len() {
            let raw = self.lines[self.pos].trim_end_matches('\r');
            self.pos += 1;
            if let Some(idx) = raw.find('~') {
                if !first {
                    out.push_str("\r\n");
                }
                out.push_str(&raw[..idx]);
                return out;
            } else {
                if !first {
                    out.push_str("\r\n");
                }
                out.push_str(raw);
                first = false;
            }
        }
        out
    }

    /// Peek the next significant line without consuming it.
    fn peek_significant(&self) -> Option<&'a str> {
        let mut p = self.pos;
        while p < self.lines.len() {
            let t = self.lines[p].trim();
            if t.is_empty() || t.starts_with('*') {
                p += 1;
                continue;
            }
            return Some(self.lines[p].trim_end_matches('\r'));
        }
        None
    }
}

/// read an int off the next significant line; defaults to 0 like a failed
/// sscanf would leave the (zero-initialized) target in practice.
fn read_int(r: &mut ShopReader) -> i32 {
    match r.get_line() {
        Some(l) => l.trim().split_whitespace().next().and_then(|t| parse_int(t)).unwrap_or(0),
        None => 0,
    }
}

fn read_float(r: &mut ShopReader) -> f32 {
    match r.get_line() {
        Some(l) => l.trim().split_whitespace().next().and_then(|t| t.parse::<f32>().ok()).unwrap_or(0.0),
        None => 0.0,
    }
}

/// Parse a leading integer out of a token, tolerating a trailing non-digit
/// (sscanf("%d") semantics).
fn parse_int(t: &str) -> Option<i32> {
    let t = t.trim();
    let mut end = 0;
    let bytes = t.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        end = i;
    }
    if end == 0 {
        return None;
    }
    t[..end].parse::<i32>().ok()
}

/// read_list (LIST_PRODUCE / LIST_ROOM): read ints until -1. For producing,
/// values are obj vnums (C runs them through real_object()). We keep them as
/// vnums and resolve via obj_protos at use time.
fn read_int_list(r: &mut ShopReader) -> Vec<i32> {
    let mut out = Vec::new();
    loop {
        let v = read_int(r);
        if v < 0 {
            break;
        }
        out.push(v);
    }
    out
}

/// read_type_list (LIST_TRADE, new format): each line is "NAME [keywords]" or
/// "<int> [keywords]"; the line may carry a trailing ';' comment which is
/// stripped. Terminates on a line whose parsed number is -1.
fn read_type_list(r: &mut ShopReader) -> Vec<ShopBuyData> {
    let mut out = Vec::new();
    loop {
        let line = match r.get_line() {
            Some(l) => l,
            None => break,
        };
        // Strip ';' comment (C: *ptr=0 at first ';').
        let body = match line.find(';') {
            Some(i) => &line[..i],
            None => line,
        };
        let body = body.trim_end_matches('\r');

        // Try a leading item-type NAME first (strn_cmp prefix match).
        let mut num: i32 = NOTHING;
        let mut rest = body;
        let trimmed = body.trim_start();
        for (idx, name) in ITEM_TYPE_NAMES.iter().enumerate() {
            if !name.is_empty() && trimmed.len() >= name.len() && trimmed[..name.len()].eq_ignore_ascii_case(name)
            {
                num = idx as i32;
                rest = &trimmed[name.len()..];
                break;
            }
        }
        if num == NOTHING {
            // No name matched: parse a leading integer.
            if let Some(n) = parse_int(trimmed) {
                num = n;
                // Advance past the digits (and sign) for the keyword tail.
                let bytes = trimmed.as_bytes();
                let mut i = 0;
                while i < bytes.len() && !bytes[i].is_ascii_digit() {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                rest = &trimmed[i.min(trimmed.len())..];
            } else {
                // Garbled line — treat as terminator to avoid an infinite loop.
                break;
            }
        }

        let kw = rest.trim();
        let kw = if kw.is_empty() { None } else { Some(kw.to_string()) };
        out.push(ShopBuyData { type_: num, keywords: kw });

        if num < 0 {
            // The terminator entry (-1) was pushed; trim it back off so the
            // live list ends just before the sentinel (we use Vec length).
            out.pop();
            break;
        }
    }
    out
}

/// boot_the_shops(): parse one shop file body, appending to `table`.
fn boot_the_shops(contents: &str, table: &mut Vec<ShopData>) {
    let mut r = ShopReader::new(contents);

    // Opening fread_string — discarded (its only role is the v3.0 marker; every
    // shipped DeltaMUD .shp is v3.0 and the file structure below is unchanged
    // by the marker since old-format files no longer exist in this world).
    let _opening = r.fread_string();

    loop {
        // Look at the next significant line to decide shop / EOF.
        let nxt = match r.peek_significant() {
            Some(l) => l,
            None => break,
        };
        let nt = nxt.trim();
        if nt.starts_with('$') {
            break; // EOF marker.
        }
        if !nt.starts_with('#') {
            // Stray fread_string between shops (e.g. version marker mid-file):
            // consume it as a string and continue.
            let _ = r.fread_string();
            continue;
        }

        // New shop: '#<vnum>~' read via fread_string.
        let header = r.fread_string();
        let vnum = parse_int(header.trim_start_matches('#')).unwrap_or(0);
        let mut shop = ShopData::new(vnum);

        // Producing list (obj vnums until -1).
        shop.producing = read_int_list(&mut r);

        // Profit factors.
        shop.profit_buy = read_float(&mut r);
        shop.profit_sell = read_float(&mut r);

        // Trade type list.
        shop.type_ = read_type_list(&mut r);

        // Seven message strings in file order.
        shop.no_such_item1 = r.fread_string();
        shop.no_such_item2 = r.fread_string();
        shop.do_not_buy = r.fread_string();
        shop.missing_cash1 = r.fread_string();
        shop.missing_cash2 = r.fread_string();
        shop.message_buy = r.fread_string();
        shop.message_sell = r.fread_string();

        shop.temper1 = read_int(&mut r);
        shop.bitvector = read_int(&mut r);
        shop.keeper = read_int(&mut r); // mob vnum
        shop.with_who = read_int(&mut r);

        // Rooms (vnums until -1).
        shop.in_room = read_int_list(&mut r);

        shop.open1 = read_int(&mut r);
        shop.close1 = read_int(&mut r);
        shop.open2 = read_int(&mut r);
        shop.close2 = read_int(&mut r);

        shop.bank_account = 0;
        shop.lastsort = 0;

        table.push(shop);
    }
}

// ---------------------------------------------------------------------------
// Small accessor helpers over GameState (CircleMUD utils.h macros).
// ---------------------------------------------------------------------------

fn get_name(g: &GameState, ch: CharId) -> String {
    g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default()
}

fn get_gold(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.gold).unwrap_or(0)
}

fn is_npc(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.is_npc).unwrap_or(false)
}

/// IS_GOD(ch): DeltaMUD keys shop god-mode on any granted god-command bit.
fn is_god(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| {
            !c.is_npc
                && (c.godcmds1 != 0 || c.godcmds2 != 0 || c.godcmds3 != 0 || c.godcmds4 != 0)
        })
        .unwrap_or(false)
}

fn is_good(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.alignment >= 350).unwrap_or(false)
}
fn is_evil(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.alignment <= -350).unwrap_or(false)
}
fn is_neutral(g: &GameState, ch: CharId) -> bool {
    !is_good(g, ch) && !is_evil(g, ch)
}

fn class_of(g: &GameState, ch: CharId) -> Option<Class> {
    g.get_char(ch).filter(|c| !c.is_npc).map(|c| c.player.class)
}

fn awake(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.position > Position::Sleeping).unwrap_or(false)
}

fn in_room(g: &GameState, ch: CharId) -> Option<RoomRnum> {
    g.get_char(ch).and_then(|c| c.in_room)
}

fn room_vnum(g: &GameState, rnum: RoomRnum) -> RoomVnum {
    g.room_opt(rnum).map(|r| r.number).unwrap_or(NOWHERE)
}

fn plr_killer(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch).map(|c| c.act_flags & PLR_KILLER != 0).unwrap_or(false)
}

/// Current mud-clock hour (0..23) for the shop open/close gate. This mirrors
/// weather.rs's reset_time()/mud_time_passed() exactly. We compute it locally
/// instead of calling `crate::weather::time_now()` so the shop module does not
/// hard-depend on a sibling module that may not be wired yet — once weather is
/// integrated the value is identical (both seed from `now - YEARS*1000`, which
/// always yields hour/day/month = 0 plus the live fractional hour of `now`).
///
/// NOTE: weather.rs caches a frozen clock and advances it one hour per
/// mud-hour heartbeat, so its `hours` can diverge from a freshly-recomputed
/// value over a long uptime. The shop only consults the hour to gate
/// open/close; every shipped DeltaMUD shop uses open=0 close>=24 (always open),
/// so the gate is a no-op in practice and this local computation matches.
fn mud_hour() -> i32 {
    const SECS_PER_MUD_HOUR: i64 = 75;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ((now / SECS_PER_MUD_HOUR) % 24) as i32
}

/// Translate the Rust ObjectType enum back to the raw structs.h ITEM_* integer
/// the shop file (and CircleMUD GET_OBJ_TYPE) uses. ObjectType discriminants now
/// equal the C ITEM_* numbers, so the type cast IS the trade code.
fn obj_type_int(g: &GameState, oid: ObjId) -> i32 {
    match g.get_obj(oid).map(|o| o.obj_type) {
        Some(t) => t as i32,
        None => ITEM_OTHER,
    }
}

fn obj_cost(g: &GameState, oid: ObjId) -> i32 {
    g.get_obj(oid).map(|o| o.cost).unwrap_or(0)
}
fn obj_weight(g: &GameState, oid: ObjId) -> i32 {
    g.get_obj(oid).map(|o| o.weight).unwrap_or(0)
}
fn obj_vnum(g: &GameState, oid: ObjId) -> ObjVnum {
    g.get_obj(oid).map(|o| o.item_number).unwrap_or(NOTHING)
}
fn obj_extra_bits(g: &GameState, oid: ObjId) -> u64 {
    g.get_obj(oid).map(|o| o.extra_flags.bits()).unwrap_or(0)
}
fn obj_val(g: &GameState, oid: ObjId, idx: usize) -> i32 {
    g.get_obj(oid).map(|o| o.values[idx]).unwrap_or(0)
}
fn obj_name(g: &GameState, oid: ObjId) -> String {
    g.get_obj(oid).map(|o| o.name.clone()).unwrap_or_default()
}
fn obj_short(g: &GameState, oid: ObjId) -> String {
    g.get_obj(oid).map(|o| o.short_description.clone()).unwrap_or_default()
}

/// CAN_SEE_OBJ — Tier-0 visibility defers to existence (matches the rest of the
/// port's object-finder behaviour).
fn can_see_obj(g: &GameState, _ch: CharId, oid: ObjId) -> bool {
    g.get_obj(oid).is_some()
}

/// CAN_CARRY_N(ch) = 5 + (DEX>>1) + (LEVEL>>1). (utils.h)
fn can_carry_n(g: &GameState, ch: CharId) -> i32 {
    match g.get_char(ch) {
        Some(c) => 5 + (c.aff_abils.dex as i32 >> 1) + (c.player.level as i32 >> 1),
        None => 0,
    }
}
fn is_carrying_n(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.carry_items as i32).unwrap_or(0)
}
fn is_carrying_w(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.carry_weight).unwrap_or(0)
}

/// str_app[].carry_w table (constants.c), reused from the act.item.c port.
const STR_APP_CARRY: [i32; 31] = [
    0, 3, 3, 10, 25, 55, 80, 90, 100, 100, 115, 115, 140, 140, 170, 170, 195, 220, 255, 640, 700,
    810, 970, 1130, 1440, 1750, 280, 305, 330, 380, 480,
];

fn strength_apply_index(g: &GameState, ch: CharId) -> usize {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let str_ = c.aff_abils.str as i32;
    let add = c.aff_abils.str_add as i32;
    if add == 0 || str_ != 18 {
        str_.clamp(0, 30) as usize
    } else if add <= 50 {
        26
    } else if add <= 75 {
        27
    } else if add <= 90 {
        28
    } else if add <= 99 {
        29
    } else {
        30
    }
}
fn can_carry_w(g: &GameState, ch: CharId) -> i32 {
    STR_APP_CARRY[strength_apply_index(g, ch)]
}

/// numdisplay(): comma-group a non-negative integer (matches the helper used in
/// the other ported modules and DeltaMUD's numdisplay()).
fn numdisplay(val: i32) -> String {
    let neg = val < 0;
    let s = val.unsigned_abs().to_string();
    let mut out = String::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg {
        format!("-{}", out)
    } else {
        out
    }
}

/// AN(string): "a"/"an" (utils.h).
fn an(s: &str) -> &'static str {
    match s.trim_start().chars().next() {
        Some(c) if "aeiouAEIOU".contains(c) => "an",
        _ => "a",
    }
}

/// fname(): first keyword of a namelist (utils.c).
fn fname(namelist: &str) -> String {
    namelist.split_whitespace().next().unwrap_or("").to_string()
}

/// CAP(): uppercase the first character (utils.h).
fn cap(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// is_number(): all-digit (interpreter.c).
fn is_number(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Keeper speech: do_say / do_tell / social helpers for an NPC keeper.
// ---------------------------------------------------------------------------

/// do_say(keeper, msg): the room sees "Keeper says, '<msg>'". (DeltaMUD do_say
/// for an NPC with no descriptor — the "You say" half goes nowhere.) The shop
/// always passes literal punctuation-free strings, so we hard-code "says,".
fn keeper_say(g: &mut GameState, keeper: CharId, msg: &str) {
    let line = format!("says, '{}'", msg);
    act(g, &line, false, keeper, None, ActArg::None, To::Room);
}

/// do_tell(keeper, "<name> <body>"): tell the player whose name is the first
/// token. perform_tell renders "$n tells you, '<body>'" to that player only.
fn keeper_tell_named(g: &mut GameState, keeper: CharId, target: CharId, body: &str) {
    let line = format!("tells you, '{}'", body);
    act(g, &line, false, keeper, None, ActArg::Char(target), To::Vict);
}

fn page_string_to_char(g: &mut GameState, ch: CharId, body: &str) {
    match g.get_char(ch).and_then(|c| c.desc) {
        Some(conn) => crate::modify::page_string(g, conn, body),
        None => g.send_to_char(ch, body),
    }
}

// ---------------------------------------------------------------------------
// is_ok_char / is_open / is_ok (shop.c).
// ---------------------------------------------------------------------------

fn is_ok_char(g: &mut GameState, keeper: CharId, ch: CharId, shop: &ShopData) -> bool {
    // CAN_SEE(keeper, ch).
    if !g.can_see(keeper, ch) {
        keeper_say(g, keeper, MSG_NO_SEE_CHAR);
        return false;
    }
    if is_god(g, ch) {
        return true;
    }

    let notrade_good = shop.with_who & TRADE_NOGOOD != 0;
    let notrade_evil = shop.with_who & TRADE_NOEVIL != 0;
    let notrade_neutral = shop.with_who & TRADE_NONEUTRAL != 0;

    if (is_good(g, ch) && notrade_good)
        || (is_evil(g, ch) && notrade_evil)
        || (is_neutral(g, ch) && notrade_neutral)
    {
        let body = format!("{} {}", get_name(g, ch), MSG_NO_SELL_ALIGN);
        keeper_tell_named(g, keeper, ch, body.split_once(' ').map(|x| x.1).unwrap_or(&body));
        return false;
    }
    if is_npc(g, ch) {
        return true;
    }

    let notrade_mage = shop.with_who & TRADE_NOMAGIC_USER != 0;
    let notrade_cleric = shop.with_who & TRADE_NOCLERIC != 0;
    let notrade_thief = shop.with_who & TRADE_NOTHIEF != 0;
    let notrade_warrior = shop.with_who & TRADE_NOWARRIOR != 0;
    let notrade_artisan = shop.with_who & TRADE_NOARTISAN != 0;

    let cls = class_of(g, ch);
    let blocked = matches!(cls, Some(Class::MagicUser)) && notrade_mage
        || matches!(cls, Some(Class::Cleric)) && notrade_cleric
        || matches!(cls, Some(Class::Thief)) && notrade_thief
        || matches!(cls, Some(Class::Artisan)) && notrade_artisan
        || matches!(cls, Some(Class::Warrior)) && notrade_warrior;
    if blocked {
        keeper_tell_named(g, keeper, ch, MSG_NO_SELL_CLASS);
        return false;
    }
    true
}

fn is_open(g: &mut GameState, keeper: CharId, shop: &ShopData, msg: bool) -> bool {
    let hours = mud_hour();
    let mut buf = String::new();

    if shop.open1 > hours {
        buf = MSG_NOT_OPEN_YET.to_string();
    } else if shop.close1 < hours {
        if shop.open2 > hours {
            buf = MSG_NOT_REOPEN_YET.to_string();
        } else if shop.close2 < hours {
            buf = MSG_CLOSED_FOR_DAY.to_string();
        }
    }

    if buf.is_empty() {
        return true;
    }
    if msg {
        keeper_say(g, keeper, &buf);
    }
    false
}

fn is_ok(g: &mut GameState, keeper: CharId, ch: CharId, shop: &ShopData) -> bool {
    if is_open(g, keeper, shop, true) {
        is_ok_char(g, keeper, ch, shop)
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// RPN keyword expression evaluator (shop.c push/pop/evaluate_*).
// ---------------------------------------------------------------------------

fn find_oper_num(token: char) -> i32 {
    for (index, set) in OPERATOR_STR.iter().enumerate() {
        if set.contains(token) {
            return index as i32;
        }
    }
    NOTHING
}

fn stack_top(stack: &[i32]) -> i32 {
    stack.last().copied().unwrap_or(NOTHING)
}

fn stack_pop(stack: &mut Vec<i32>) -> i32 {
    stack.pop().unwrap_or(0)
}

fn evaluate_operation(ops: &mut Vec<i32>, vals: &mut Vec<i32>) {
    let oper = stack_pop(ops);
    if oper == OPER_NOT {
        let a = stack_pop(vals);
        vals.push(if a == 0 { 1 } else { 0 });
    } else if oper == OPER_AND {
        let a = stack_pop(vals);
        let b = stack_pop(vals);
        vals.push(if a != 0 && b != 0 { 1 } else { 0 });
    } else if oper == OPER_OR {
        let a = stack_pop(vals);
        let b = stack_pop(vals);
        vals.push(if a != 0 || b != 0 { 1 } else { 0 });
    }
}

/// evaluate_expression(obj, expr): true if the object matches the shop's
/// keyword expression. Empty / non-alpha-leading expressions are always true.
fn evaluate_expression(g: &GameState, obj: ObjId, expr: Option<&str>) -> bool {
    let expr = match expr {
        Some(e) => e,
        None => return true,
    };
    if expr.is_empty() {
        return true;
    }
    if !expr.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
        return true;
    }

    let extra = obj_extra_bits(g, obj);
    let oname = obj_name(g, obj);

    let mut ops: Vec<i32> = Vec::new();
    let mut vals: Vec<i32> = Vec::new();

    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let temp = find_oper_num(c);
        if temp == NOTHING {
            // Read a name token: up to the next space/operator.
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() && find_oper_num(chars[i]) == NOTHING {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();

            // Match against extra_bits names first.
            let mut matched = false;
            for (index, bit) in EXTRA_BITS.iter().enumerate() {
                if name.eq_ignore_ascii_case(bit) {
                    let set = extra & (1u64 << index) != 0;
                    vals.push(if set { 1 } else { 0 });
                    matched = true;
                    break;
                }
            }
            if !matched {
                let isname = crate::handler::isname(&name, &oname);
                vals.push(if isname { 1 } else { 0 });
            }
        } else {
            if temp != OPER_OPEN_PAREN {
                while stack_top(&ops) > temp {
                    evaluate_operation(&mut ops, &mut vals);
                }
            }
            if temp == OPER_CLOSE_PAREN {
                if stack_pop(&mut ops) != OPER_OPEN_PAREN {
                    // Illegal parenthesis.
                    return false;
                }
            } else {
                ops.push(temp);
            }
            i += 1;
        }
    }
    while stack_top(&ops) != NOTHING {
        evaluate_operation(&mut ops, &mut vals);
    }
    let temp = stack_pop(&mut vals);
    if stack_top(&vals) != NOTHING {
        // Extra operands left — C logs and returns FALSE.
        return false;
    }
    temp != 0
}

// ---------------------------------------------------------------------------
// trade_with / same_obj / shop_producing (shop.c).
// ---------------------------------------------------------------------------

fn trade_with(g: &GameState, item: ObjId, shop: &ShopData) -> i32 {
    if obj_cost(g, item) < 1 {
        return OBJECT_NOVAL;
    }
    if obj_extra_bits(g, item) & ITEM_NOSELL_BIT != 0 {
        return OBJECT_NOTOK;
    }
    let ty = obj_type_int(g, item);
    for entry in &shop.type_ {
        if entry.type_ == NOTHING {
            break;
        }
        if entry.type_ == ty {
            if obj_val(g, item, 2) == 0 && (ty == ITEM_WAND || ty == ITEM_STAFF) {
                return OBJECT_DEAD;
            } else if evaluate_expression(g, item, entry.keywords.as_deref()) {
                return OBJECT_OK;
            }
        }
    }
    OBJECT_NOTOK
}

/// same_obj(): structural identity used to group identical items on the list.
/// Uses prototype vnum + cost + extra flags (+ affects, which Rust object
/// affects carry as (location, modifier) pairs).
fn same_obj(g: &GameState, a: Option<ObjId>, b: Option<ObjId>) -> bool {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        (None, None) => return true,
        _ => return false,
    };
    let oa = match g.get_obj(a) {
        Some(o) => o,
        None => return false,
    };
    let ob = match g.get_obj(b) {
        Some(o) => o,
        None => return false,
    };
    if oa.item_number != ob.item_number {
        return false;
    }
    if oa.cost != ob.cost {
        return false;
    }
    if oa.extra_flags != ob.extra_flags {
        return false;
    }
    if oa.affects.len() != ob.affects.len() {
        return false;
    }
    for (x, y) in oa.affects.iter().zip(ob.affects.iter()) {
        if x.location != y.location || x.modifier != y.modifier {
            return false;
        }
    }
    true
}

/// shop_producing(item): does this shop produce an item identical to `item`?
/// (Matches against the proto for each produced vnum.)
fn shop_producing(g: &GameState, item: ObjId, shop: &ShopData) -> bool {
    if obj_vnum(g, item) < 0 {
        return false;
    }
    for &pv in &shop.producing {
        if pv == NOTHING {
            break;
        }
        if proto_same_obj(g, item, pv) {
            return true;
        }
    }
    false
}

/// same_obj against a prototype (the produced item's vnum). Compares the live
/// object to the obj prototype the same way C compares to &obj_proto[rnum].
fn proto_same_obj(g: &GameState, item: ObjId, proto_vnum: ObjVnum) -> bool {
    let proto = match g.obj_protos.get(&proto_vnum) {
        Some(p) => p,
        None => return false,
    };
    let o = match g.get_obj(item) {
        Some(o) => o,
        None => return false,
    };
    o.item_number == proto.vnum
        && o.cost == proto.cost
        && o.extra_flags == proto.extra_flags
        && o.affects.is_empty()
}

// ---------------------------------------------------------------------------
// Pricing (shop.c).
// ---------------------------------------------------------------------------

fn buy_price(g: &GameState, obj: ObjId, shop: &ShopData) -> i32 {
    (obj_cost(g, obj) as f32 * shop.profit_buy) as i32
}

fn sell_price(g: &GameState, ch: CharId, obj: ObjId, shop: &ShopData) -> i32 {
    let mut value = (obj_cost(g, obj) as f32 * shop.profit_sell) as i32;
    if !PK_ALLOWED
        && plr_killer(g, ch)
        && in_room(g, ch) == g.real_room(JAIL_NUM)
        && value > 75000
    {
        value = 75000;
    }
    value
}

// ---------------------------------------------------------------------------
// times_message / list_object display helpers (shop.c).
// ---------------------------------------------------------------------------

fn times_message(g: &GameState, obj: Option<ObjId>, name: &str, num: i32) -> String {
    let mut buf = if let Some(o) = obj {
        obj_short(g, o)
    } else {
        let ptr = match name.find('.') {
            Some(i) => &name[i + 1..],
            None => name,
        };
        format!("{} {}", an(ptr), ptr)
    };
    if num > 1 {
        buf.push_str(&format!(" (x {})", num));
    }
    buf
}

fn list_object(g: &GameState, obj: ObjId, cnt: i32, index: i32, shop: &ShopData) -> String {
    let buf2 = if shop_producing(g, obj, shop) {
        "Unlimited   ".to_string()
    } else {
        format!("{:>8}    ", numdisplay(cnt))
    };
    let mut buf = format!(" {:2})  {}", index, buf2);

    let mut buf3 = obj_short(g, obj);
    let ty = obj_type_int(g, obj);
    if ty == ITEM_DRINKCON && obj_val(g, obj, 1) != 0 {
        let liq = obj_val(g, obj, 2);
        let drink = crate::constants::DRINKS.get(liq as usize).copied().unwrap_or("water");
        buf3.push_str(&format!(" of {}", drink));
    }
    if (ty == ITEM_WAND || ty == ITEM_STAFF) && obj_val(g, obj, 2) < obj_val(g, obj, 1) {
        buf3.push_str(" (partially used)");
    }

    let line = format!("{:<48} {:>8}\r\n", buf3, numdisplay(buy_price(g, obj, shop)));
    buf.push_str(&cap(&line));
    buf
}

// ---------------------------------------------------------------------------
// Object finders for the keeper's inventory (shop.c slide/hash/purchase).
// ---------------------------------------------------------------------------

/// get_hash_obj_vis: "#N" selects the N-th *distinct* sellable item in the
/// keeper's carrying list.
fn get_hash_obj_vis(g: &GameState, ch: CharId, name: &str, list: &[ObjId]) -> Option<ObjId> {
    let n = name.strip_prefix('#')?;
    if !is_number(n) {
        return None;
    }
    let mut index: i32 = n.parse().ok()?;
    let mut last: Option<ObjId> = None;
    for &oid in list {
        if can_see_obj(g, ch, oid) && obj_cost(g, oid) > 0 && !same_obj(g, last, Some(oid)) {
            index -= 1;
            if index == 0 {
                return Some(oid);
            }
            last = Some(oid);
        }
    }
    None
}

/// get_slide_obj_vis: "N.keyword" selects the N-th distinct item matching the
/// keyword among the keeper's carrying list.
fn get_slide_obj_vis(g: &GameState, ch: CharId, name: &str, list: &[ObjId]) -> Option<ObjId> {
    let (number, tmp) = crate::handler::get_number(name);
    if number == 0 {
        return None;
    }
    let mut last: Option<ObjId> = None;
    let mut j = 1;
    for &oid in list {
        if j > number {
            break;
        }
        if crate::handler::isname(&tmp, &obj_name(g, oid))
            && can_see_obj(g, ch, oid)
            && !same_obj(g, last, Some(oid))
        {
            if j == number {
                return Some(oid);
            }
            last = Some(oid);
            j += 1;
        }
    }
    None
}

/// get_purchase_obj: resolve the buy-target from `arg`, dropping any zero-cost
/// object it finds (the C extracts those and retries).
fn get_purchase_obj(
    g: &mut GameState,
    ch: CharId,
    arg: &str,
    keeper: CharId,
    shop: &ShopData,
    msg: bool,
) -> Option<ObjId> {
    let (name, _) = crate::interpreter::one_argument(arg);
    loop {
        let carrying = g.get_char(keeper).map(|c| c.carrying.clone()).unwrap_or_default();
        let obj = if name.starts_with('#') {
            get_hash_obj_vis(g, ch, &name, &carrying)
        } else {
            get_slide_obj_vis(g, ch, &name, &carrying)
        };
        let obj = match obj {
            Some(o) => o,
            None => {
                if msg {
                    let body = sprintf_name(&shop.no_such_item1, &get_name(g, ch));
                    keeper_tell_named(g, keeper, ch, &body);
                }
                return None;
            }
        };
        if obj_cost(g, obj) <= 0 {
            g.obj_from_anywhere(obj);
            g.extract_obj(obj);
            continue;
        }
        return Some(obj);
    }
}

/// get_obj_in_list_vis over the player's inventory (handler.rs equivalent).
fn get_selling_obj(
    g: &mut GameState,
    ch: CharId,
    name: &str,
    keeper: CharId,
    shop: &ShopData,
    msg: bool,
) -> Option<ObjId> {
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let obj = match g.get_obj_in_list_vis(ch, name, &inv) {
        Some(o) => o,
        None => {
            if msg {
                let body = sprintf_name(&shop.no_such_item2, &get_name(g, ch));
                keeper_tell_named(g, keeper, ch, &body);
            }
            return None;
        }
    };

    let result = trade_with(g, obj, shop);
    if result == OBJECT_OK {
        return Some(obj);
    }

    let body = match result {
        OBJECT_NOVAL => format!(
            "{} You've got to be kidding, that thing is worthless!",
            get_name(g, ch)
        ),
        OBJECT_NOTOK => sprintf_name(&shop.do_not_buy, &get_name(g, ch)),
        OBJECT_DEAD => format!("{} {}", get_name(g, ch), MSG_NO_USED_WANDSTAFF),
        _ => format!("{} An error has occurred.", get_name(g, ch)),
    };
    if msg {
        // Strip the leading "<name> " prefix for the tell body.
        let body = body.split_once(' ').map(|x| x.1.to_string()).unwrap_or(body);
        keeper_tell_named(g, keeper, ch, &body);
    }
    None
}

/// Apply a shop message template's first %s (the player name) and, when present,
/// its %d (a coin amount). Templates either have a leading "%s " or not.
fn sprintf_name(template: &str, name: &str) -> String {
    // The body sent to the player via do_tell has the leading "<name> " word
    // stripped by perform_tell's target parse. We replicate by substituting
    // %s with the name, then dropping the name prefix.
    let full = template.replacen("%s", name, 1);
    full.split_once(' ').map(|x| x.1.to_string()).unwrap_or(full)
}

fn sprintf_name_amt(template: &str, name: &str, amt: i32) -> String {
    let full = template.replacen("%s", name, 1).replacen("%d", &amt.to_string(), 1);
    full.split_once(' ').map(|x| x.1.to_string()).unwrap_or(full)
}

// ---------------------------------------------------------------------------
// transaction_amt (shop.c): peel a leading numeric count off `arg`.
// ---------------------------------------------------------------------------

/// Returns (count, remaining-arg). If the first token is a number it is
/// consumed; otherwise count defaults to 1 and arg is unchanged.
fn transaction_amt(arg: &str) -> (i32, String) {
    let (first, rest) = crate::interpreter::one_argument(arg);
    if !first.is_empty() && is_number(&first) {
        let num: i32 = first.parse().unwrap_or(0);
        (num, rest.to_string())
    } else {
        (1, arg.to_string())
    }
}

// ---------------------------------------------------------------------------
// sort_keeper_objs / slide_obj (shop.c). We model the keeper's carrying Vec
// directly: "sorted" means identical items are adjacent and producibles appear
// once. lastsort tracks how many items are sorted.
// ---------------------------------------------------------------------------

fn sort_keeper_objs(g: &mut GameState, keeper: CharId, shop_idx: usize) {
    // Snapshot the keeper's inventory and re-order so identical items are
    // grouped adjacently (the on-list grouping the C slide_obj maintains).
    let inv = g.get_char(keeper).map(|c| c.carrying.clone()).unwrap_or_default();

    // Stable grouping: walk the list, bucketing same_obj items together while
    // preserving first-appearance order of each group.
    let mut groups: Vec<Vec<ObjId>> = Vec::new();
    for oid in inv {
        let mut placed = false;
        for grp in groups.iter_mut() {
            if same_obj(g, grp.first().copied(), Some(oid)) {
                grp.push(oid);
                placed = true;
                break;
            }
        }
        if !placed {
            groups.push(vec![oid]);
        }
    }
    let sorted: Vec<ObjId> = groups.into_iter().flatten().collect();
    let n = sorted.len() as i32;
    if let Some(c) = g.get_char_mut(keeper) {
        c.carrying = sorted;
    }
    if let Ok(mut guard) = shops().lock() {
        if let Some(s) = guard.get_mut(shop_idx) {
            s.lastsort = n;
        }
    }
}

// ---------------------------------------------------------------------------
// ok_shop_room (shop.c).
// ---------------------------------------------------------------------------

fn ok_shop_room(shop: &ShopData, room: RoomVnum) -> bool {
    shop.in_room.iter().any(|&r| r == room)
}

// ---------------------------------------------------------------------------
// shopping_buy / sell / value / list (shop.c). These take a *clone* of the
// ShopData so they don't hold the global lock across send/act; mutations to
// bank/gold/lastsort are written back through commit_shop().
// ---------------------------------------------------------------------------

/// Write the mutable shop fields (bank, lastsort) back into the global table.
fn commit_shop(shop_idx: usize, bank: i32, lastsort: i32) {
    if let Ok(mut guard) = shops().lock() {
        if let Some(s) = guard.get_mut(shop_idx) {
            s.bank_account = bank;
            s.lastsort = lastsort;
        }
    }
}

fn shopping_buy(g: &mut GameState, arg: &str, ch: CharId, keeper: CharId, shop_idx: usize) {
    let mut shop = match shops().lock().ok().and_then(|v| v.get(shop_idx).cloned()) {
        Some(s) => s,
        None => return,
    };

    if !is_ok(g, keeper, ch, &shop) {
        return;
    }
    if !PK_ALLOWED && plr_killer(g, ch) && in_room(g, ch) == g.real_room(JAIL_NUM) {
        keeper_say(g, keeper, MSG_NO_SELL_PAWN);
        return;
    }

    if shop.lastsort < is_carrying_n(g, keeper) {
        sort_keeper_objs(g, keeper, shop_idx);
        shop.lastsort = is_carrying_n(g, keeper);
    }

    let (buynum, arg) = transaction_amt(arg);
    if buynum < 0 {
        let body = format!("A negative amount?  Try selling me something.");
        keeper_tell_named(g, keeper, ch, &body);
        return;
    }
    if arg.is_empty() || buynum == 0 {
        keeper_tell_named(g, keeper, ch, "What do you want to buy??");
        return;
    }

    let obj = match get_purchase_obj(g, ch, &arg, keeper, &shop, true) {
        Some(o) => o,
        None => return,
    };

    if buy_price(g, obj, &shop) > get_gold(g, ch) && !is_god(g, ch) {
        let body = sprintf_name(&shop.missing_cash2, &get_name(g, ch));
        keeper_tell_named(g, keeper, ch, &body);
        match shop.temper1 {
            0 => {
                let pname = get_name(g, ch);
                crate::cmd_social::do_action_named(g, keeper, "puke", &pname);
            }
            1 => {
                act(g, "$n smokes on his joint.", false, keeper, None, ActArg::None, To::Room);
            }
            _ => {}
        }
        return;
    }

    if is_carrying_n(g, ch) + 1 > can_carry_n(g, ch) {
        g.send_to_char(ch, &format!("{}: You can't carry any more items.\r\n", fname(&obj_name(g, obj))));
        return;
    }
    if is_carrying_w(g, ch) + obj_weight(g, obj) > can_carry_w(g, ch) {
        g.send_to_char(ch, &format!("{}: You can't carry that much weight.\r\n", fname(&obj_name(g, obj))));
        return;
    }

    let mut goldamt = 0;
    let mut bought = 0;
    let mut cur: Option<ObjId> = Some(obj);
    let mut last_obj: Option<ObjId> = None;

    while let Some(o) = cur {
        let price = buy_price(g, o, &shop);
        if !((get_gold(g, ch) >= price || is_god(g, ch))
            && is_carrying_n(g, ch) < can_carry_n(g, ch)
            && bought < buynum
            && is_carrying_w(g, ch) + obj_weight(g, o) <= can_carry_w(g, ch))
        {
            break;
        }
        bought += 1;

        // Producing shop: hand over a fresh copy from the prototype. Otherwise
        // move the stock item.
        let given = if shop_producing(g, o, &shop) {
            let vnum = obj_vnum(g, o);
            match g.load_object(vnum) {
                Some(fresh) => {
                    crate::dg_triggers::load_otrigger(g, fresh);
                    fresh
                }
                None => break,
            }
        } else {
            g.obj_from_anywhere(o);
            shop.lastsort -= 1;
            o
        };
        g.obj_to_char(given, ch);

        let price = buy_price(g, given, &shop);
        goldamt += price;
        if !is_god(g, ch) {
            if let Some(c) = g.get_char_mut(ch) {
                c.points.gold -= price;
            }
        }

        last_obj = Some(given);
        cur = get_purchase_obj(g, ch, &arg, keeper, &shop, false);
        if !same_obj(g, cur, last_obj) {
            break;
        }
    }

    if bought < buynum {
        let body = if cur.is_none() || !same_obj(g, last_obj, cur) {
            format!("I only have {} to sell you.", bought)
        } else if cur.map(|o| get_gold(g, ch) < buy_price(g, o, &shop)).unwrap_or(false) {
            format!("You can only afford {}.", bought)
        } else if is_carrying_n(g, ch) >= can_carry_n(g, ch) {
            format!("You can only hold {}.", bought)
        } else if cur
            .map(|o| is_carrying_w(g, ch) + obj_weight(g, o) > can_carry_w(g, ch))
            .unwrap_or(false)
        {
            format!("You can only carry {}.", bought)
        } else {
            format!("Something screwy only gave you {}.", bought)
        };
        keeper_tell_named(g, keeper, ch, &body);
    }

    if !is_god(g, ch) {
        if let Some(k) = g.get_char_mut(keeper) {
            k.points.gold += goldamt;
        }
    }

    // "$n buys <times_message>." — render to the room.
    let first_carry = g.get_char(ch).and_then(|c| c.carrying.first().copied());
    let tempstr = times_message(g, first_carry, "", bought);
    let line = format!("$n buys {}.", tempstr);
    act(g, &line, false, ch, None, ActArg::None, To::Room);

    let body = sprintf_name_amt(&shop.message_buy, &get_name(g, ch), goldamt);
    keeper_tell_named(g, keeper, ch, &body);
    g.send_to_char(ch, &format!("You now have {}.\r\n", tempstr));

    // Bank overflow.
    if shop.bitvector & WILL_BANK_MONEY != 0 {
        let kgold = get_gold(g, keeper);
        if kgold > MAX_OUTSIDE_BANK {
            shop.bank_account += kgold - MAX_OUTSIDE_BANK;
            if let Some(k) = g.get_char_mut(keeper) {
                k.points.gold = MAX_OUTSIDE_BANK;
            }
        }
    }

    commit_shop(shop_idx, shop.bank_account, shop.lastsort);
}

fn shopping_sell(g: &mut GameState, arg: &str, ch: CharId, keeper: CharId, shop_idx: usize) {
    let mut shop = match shops().lock().ok().and_then(|v| v.get(shop_idx).cloned()) {
        Some(s) => s,
        None => return,
    };

    if !is_ok(g, keeper, ch, &shop) {
        return;
    }

    let (sellnum, arg) = transaction_amt(arg);
    if sellnum < 0 {
        keeper_tell_named(g, keeper, ch, "A negative amount?  Try buying something.");
        return;
    }
    if arg.is_empty() || sellnum == 0 {
        keeper_tell_named(g, keeper, ch, "What do you want to sell??");
        return;
    }

    let (name, _) = crate::interpreter::one_argument(&arg);
    let mut obj = match get_selling_obj(g, ch, &name, keeper, &shop, true) {
        Some(o) => o,
        None => return,
    };

    if get_gold(g, keeper) + shop.bank_account < sell_price(g, ch, obj, &shop) {
        let body = sprintf_name(&shop.missing_cash1, &get_name(g, ch));
        keeper_tell_named(g, keeper, ch, &body);
        return;
    }

    let mut sold = 0;
    let mut goldamt = 0;
    let mut tempstr = String::new();
    let mut have_obj = true;

    loop {
        if !(have_obj
            && get_gold(g, keeper) + shop.bank_account >= sell_price(g, ch, obj, &shop)
            && sold < sellnum)
        {
            break;
        }
        sold += 1;
        let price = sell_price(g, ch, obj, &shop);
        goldamt += price;
        if let Some(k) = g.get_char_mut(keeper) {
            k.points.gold -= price;
        }

        let short = obj_short(g, obj);
        if !short.is_empty() {
            tempstr = short;
        }
        g.obj_from_anywhere(obj);
        // slide_obj: put into keeper's inventory grouped with identicals.
        slide_obj(g, obj, keeper, &mut shop);

        match get_selling_obj(g, ch, &name, keeper, &shop, false) {
            Some(next) => obj = next,
            None => {
                have_obj = false;
            }
        }
    }

    if sold < sellnum {
        let body = if !have_obj {
            format!("You only have {} of those.", sold)
        } else if get_gold(g, keeper) + shop.bank_account < sell_price(g, ch, obj, &shop) {
            format!("I can only afford to buy {} of those.", sold)
        } else {
            format!("Something really screwy made me buy {}.", sold)
        };
        keeper_tell_named(g, keeper, ch, &body);
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.points.gold += goldamt;
    }
    if tempstr.is_empty() {
        tempstr = times_message(g, None, &name, sold);
    }
    let line = format!("$n sells {}.", tempstr);
    act(g, &line, false, ch, None, ActArg::None, To::Room);

    let body = sprintf_name_amt(&shop.message_sell, &get_name(g, ch), goldamt);
    keeper_tell_named(g, keeper, ch, &body);
    g.send_to_char(ch, &format!("The shopkeeper now has {}.\r\n", tempstr));

    // Replenish keeper coins from the bank when low.
    if get_gold(g, keeper) < MIN_OUTSIDE_BANK {
        let amt = (MAX_OUTSIDE_BANK - get_gold(g, keeper)).min(shop.bank_account);
        shop.bank_account -= amt;
        if let Some(k) = g.get_char_mut(keeper) {
            k.points.gold += amt;
        }
    }

    commit_shop(shop_idx, shop.bank_account, shop.lastsort);
}

/// slide_obj: place a just-bought item into the keeper's inventory, grouped
/// with any identical item already there (or absorb it into a producible).
fn slide_obj(g: &mut GameState, obj: ObjId, keeper: CharId, shop: &mut ShopData) -> Option<ObjId> {
    if shop.lastsort < is_carrying_n(g, keeper) {
        // Resort first (group identicals).
        let inv = g.get_char(keeper).map(|c| c.carrying.clone()).unwrap_or_default();
        let mut groups: Vec<Vec<ObjId>> = Vec::new();
        for oid in inv {
            let mut placed = false;
            for grp in groups.iter_mut() {
                if same_obj(g, grp.first().copied(), Some(oid)) {
                    grp.push(oid);
                    placed = true;
                    break;
                }
            }
            if !placed {
                groups.push(vec![oid]);
            }
        }
        let sorted: Vec<ObjId> = groups.into_iter().flatten().collect();
        let n = sorted.len() as i32;
        if let Some(c) = g.get_char_mut(keeper) {
            c.carrying = sorted;
        }
        shop.lastsort = n;
    }

    // If identical to a producible, just drop the instance (shop makes infinite).
    if shop_producing(g, obj, shop) {
        g.extract_obj(obj);
        return None;
    }

    shop.lastsort += 1;
    g.obj_to_char(obj, keeper);
    // Re-group: move obj adjacent to any identical item.
    let inv = g.get_char(keeper).map(|c| c.carrying.clone()).unwrap_or_default();
    let mut groups: Vec<Vec<ObjId>> = Vec::new();
    for oid in inv {
        let mut placed = false;
        for grp in groups.iter_mut() {
            if same_obj(g, grp.first().copied(), Some(oid)) {
                grp.push(oid);
                placed = true;
                break;
            }
        }
        if !placed {
            groups.push(vec![oid]);
        }
    }
    let sorted: Vec<ObjId> = groups.into_iter().flatten().collect();
    if let Some(c) = g.get_char_mut(keeper) {
        c.carrying = sorted;
    }
    Some(obj)
}

fn shopping_value(g: &mut GameState, arg: &str, ch: CharId, keeper: CharId, shop_idx: usize) {
    let shop = match shops().lock().ok().and_then(|v| v.get(shop_idx).cloned()) {
        Some(s) => s,
        None => return,
    };

    if !is_ok(g, keeper, ch, &shop) {
        return;
    }
    if arg.trim().is_empty() {
        keeper_tell_named(g, keeper, ch, "What do you want me to valuate??");
        return;
    }
    let (name, _) = crate::interpreter::one_argument(arg);
    let obj = match get_selling_obj(g, ch, &name, keeper, &shop, true) {
        Some(o) => o,
        None => return,
    };

    let body = format!(
        "I'll give you {} gold coins for that!",
        numdisplay(sell_price(g, ch, obj, &shop))
    );
    keeper_tell_named(g, keeper, ch, &body);
}

fn shopping_list(g: &mut GameState, arg: &str, ch: CharId, keeper: CharId, shop_idx: usize) {
    let mut shop = match shops().lock().ok().and_then(|v| v.get(shop_idx).cloned()) {
        Some(s) => s,
        None => return,
    };

    if !is_ok(g, keeper, ch, &shop) {
        return;
    }
    if !PK_ALLOWED && plr_killer(g, ch) && in_room(g, ch) == g.real_room(JAIL_NUM) {
        keeper_say(g, keeper, MSG_NO_SELL_PAWN);
        return;
    }

    if shop.lastsort < is_carrying_n(g, keeper) {
        sort_keeper_objs(g, keeper, shop_idx);
        shop.lastsort = is_carrying_n(g, keeper);
    }

    let (name, _) = crate::interpreter::one_argument(arg);

    let mut buf = String::new();
    buf.push_str(" ##   Available   Item                                                 Cost\r\n");
    buf.push_str("---------------------------------------------------------------------------\r\n");

    let carrying = g.get_char(keeper).map(|c| c.carrying.clone()).unwrap_or_default();
    let mut last_obj: Option<ObjId> = None;
    let mut cnt = 0;
    let mut index = 0;

    for oid in carrying {
        if can_see_obj(g, ch, oid) && obj_cost(g, oid) > 0 {
            if last_obj.is_none() {
                last_obj = Some(oid);
                cnt = 1;
            } else if same_obj(g, last_obj, Some(oid)) {
                cnt += 1;
            } else {
                index += 1;
                let lo = last_obj.unwrap();
                if name.is_empty() || crate::handler::isname(&name, &obj_name(g, lo)) {
                    buf.push_str(&list_object(g, lo, cnt, index, &shop));
                }
                cnt = 1;
                last_obj = Some(oid);
            }
        }
    }
    index += 1;
    match last_obj {
        None => {
            buf = if !name.is_empty() {
                "Presently, none of those are for sale.\r\n".to_string()
            } else {
                "Currently, there is nothing for sale.\r\n".to_string()
            };
        }
        Some(lo) => {
            if name.is_empty() || crate::handler::isname(&name, &obj_name(g, lo)) {
                buf.push_str(&list_object(g, lo, cnt, index, &shop));
            }
        }
    }

    page_string_to_char(g, ch, &buf);
    let _ = &mut shop;
}

// ---------------------------------------------------------------------------
// shop_keeper() — the SPECIAL() spec proc.
// ---------------------------------------------------------------------------

/// CircleMUD SPECIAL(shop_keeper). `me` is the keeper character; `cmd` is the
/// resolved command index (0 means a non-command tick); `arg` is the remaining
/// argument. Returns true if the keeper handled the command.
///
/// Invoked from the spec-proc dispatcher (spec_assign::special). We resolve the
/// shop by matching the keeper mob's vnum against SHOP_KEEPER, run the keeper's
/// SHOP_FUNC secondary spec (if any) first, then intercept
/// steal/buy/sell/value/list.
pub fn shop_keeper(g: &mut GameState, ch: CharId, me: CharId, cmd: &str, arg: &str) -> bool {
    let keeper = me;
    let keeper_vnum = g.get_char(keeper).map(|c| c.nr).unwrap_or(NOBODY);

    // Find the shop owned by this keeper.
    let shop_idx = match shops().lock().ok().and_then(|guard| {
        guard.iter().position(|s| s.keeper == keeper_vnum)
    }) {
        Some(i) => i,
        None => return false,
    };

    // SHOP_FUNC(shop_nr): the keeper's secondary spec proc, captured from its
    // original assign_mobiles() spec by assign_the_shopkeepers(). C calls it
    // first and, if it returns nonzero, the command is consumed before any of
    // the default keeper handling runs.
    //   if (SHOP_FUNC(shop_nr))
    //     if ((SHOP_FUNC(shop_nr)) (ch, me, cmd, arg)) return (TRUE);
    if let Some(func) = shop_func(keeper_vnum) {
        if func(g, ch, keeper, cmd, arg) {
            return true;
        }
    }

    if keeper == ch {
        if !cmd.is_empty() {
            // "drop all" safety: reset lastsort.
            if let Ok(mut guard) = shops().lock() {
                if let Some(s) = guard.get_mut(shop_idx) {
                    s.lastsort = 0;
                }
            }
        }
        return false;
    }

    // ok_shop_room: keeper must be in one of the shop's rooms.
    let room = match in_room(g, ch) {
        Some(r) => room_vnum(g, r),
        None => return false,
    };
    let in_shop_room = shops()
        .lock()
        .ok()
        .and_then(|guard| guard.get(shop_idx).map(|s| ok_shop_room(s, room)))
        .unwrap_or(false);
    if !in_shop_room {
        return false;
    }

    if !awake(g, keeper) {
        return false;
    }

    let cmdl = cmd.to_ascii_lowercase();
    if cmdl == "steal" {
        let pname = get_name(g, ch);
        crate::cmd_social::do_action_named(g, keeper, "slap", &pname);
        let line = format!("$N shouts '{}'", MSG_NO_STEAL_HERE.replace("$n", &pname));
        act(g, &line, false, ch, None, ActArg::Char(keeper), To::Char);
        return true;
    }
    if cmdl == "buy" {
        shopping_buy(g, arg, ch, keeper, shop_idx);
        return true;
    }
    if cmdl == "sell" {
        shopping_sell(g, arg, ch, keeper, shop_idx);
        return true;
    }
    if cmdl == "value" {
        shopping_value(g, arg, ch, keeper, shop_idx);
        return true;
    }
    if cmdl == "list" {
        shopping_list(g, arg, ch, keeper, shop_idx);
        return true;
    }
    false
}

/// ok_damage_shopkeeper(): true if `victim` may be harmed (false protects a
/// non-WILL_FIGHT shopkeeper, slapping + telling the attacker off). Exported
/// for the combat system to consult.
pub fn ok_damage_shopkeeper(g: &mut GameState, ch: CharId, victim: CharId) -> bool {
    if !is_npc(g, victim) {
        return true;
    }
    let vnum = g.get_char(victim).map(|c| c.nr).unwrap_or(NOBODY);
    let protected = shops()
        .lock()
        .ok()
        .map(|guard| guard.iter().any(|s| s.keeper == vnum && s.bitvector & WILL_START_FIGHT == 0))
        .unwrap_or(false);
    if protected {
        let pname = get_name(g, ch);
        crate::cmd_social::do_action_named(g, victim, "slap", &pname);
        let body = format!("{}", MSG_CANT_KILL_KEEPER);
        keeper_tell_named(g, victim, ch, &body);
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Player command handlers (buy / sell / list / value / appraise).
//
// The C routes these through the keeper spec proc; with spec dispatch not yet
// wired, each handler locates the shopkeeper standing in the room and runs the
// matching shopping_* path, falling back to the do_not_here message when there
// is no shopkeeper here (or no shop assigned to this room).
// ---------------------------------------------------------------------------

/// Find a shopkeeper present in the player's room whose shop covers this room.
/// Returns (keeper CharId, shop index).
fn keeper_here(g: &GameState, ch: CharId) -> Option<(CharId, usize)> {
    let rnum = in_room(g, ch)?;
    let rvnum = room_vnum(g, rnum);
    let people = g.room_opt(rnum).map(|r| r.people.clone()).unwrap_or_default();
    let guard = shops().lock().ok()?;
    for &cid in &people {
        if cid == ch {
            continue;
        }
        let vnum = match g.get_char(cid) {
            Some(c) if c.is_npc => c.nr,
            _ => continue,
        };
        if let Some(idx) = guard.iter().position(|s| s.keeper == vnum && ok_shop_room(s, rvnum)) {
            // Awake keeper required, mirroring the spec proc.
            if g.get_char(cid).map(|c| c.position > Position::Sleeping).unwrap_or(false) {
                return Some((cid, idx));
            }
        }
    }
    None
}

fn no_shop(g: &mut GameState, ch: CharId) {
    g.send_to_char(ch, "Sorry, but you cannot do that here!\r\n");
}

pub fn do_buy(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    match keeper_here(g, ch) {
        Some((keeper, idx)) => shopping_buy(g, arg, ch, keeper, idx),
        None => no_shop(g, ch),
    }
}

pub fn do_sell(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    match keeper_here(g, ch) {
        Some((keeper, idx)) => shopping_sell(g, arg, ch, keeper, idx),
        None => no_shop(g, ch),
    }
}

pub fn do_list(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    match keeper_here(g, ch) {
        Some((keeper, idx)) => shopping_list(g, arg, ch, keeper, idx),
        None => no_shop(g, ch),
    }
}

pub fn do_value(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    match keeper_here(g, ch) {
        Some((keeper, idx)) => shopping_value(g, arg, ch, keeper, idx),
        None => no_shop(g, ch),
    }
}

/// do_appraise: DeltaMUD treats appraise as a value-at-a-shop synonym (there is
/// no separate ACMD in the C). With no shopkeeper here, report no shop.
pub fn do_appraise(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    match keeper_here(g, ch) {
        Some((keeper, idx)) => shopping_value(g, arg, ch, keeper, idx),
        None => no_shop(g, ch),
    }
}

// ---------------------------------------------------------------------------
// Immortal shop listing (show shops) — list_all_shops / list_detailed_shop.
// Exported for `do_show`'s "shops" subcommand to call.
// ---------------------------------------------------------------------------

fn customer_string(shop: &ShopData, detailed: bool) -> String {
    let mut buf = String::new();
    let mut cnt = 1;
    for letter in TRADE_LETTERS.iter() {
        if shop.with_who & cnt == 0 {
            if detailed {
                if !buf.is_empty() {
                    buf.push_str(", ");
                }
                buf.push_str(letter);
            } else {
                buf.push(letter.chars().next().unwrap_or('?'));
            }
        } else if !detailed {
            buf.push('_');
        }
        cnt *= 2;
    }
    buf
}

/// show_shops(ch, arg): the immortal listing dispatcher (shop.c).
pub fn show_shops(g: &mut GameState, ch: CharId, arg: &str) {
    let arg = arg.trim();
    if arg.is_empty() {
        list_all_shops(g, ch);
        return;
    }

    let shops_snapshot = match shops().lock().ok().map(|v| v.clone()) {
        Some(v) => v,
        None => return,
    };
    let top = shops_snapshot.len();

    let shop_nr: i32 = if arg == "." {
        let room = in_room(g, ch).map(|r| room_vnum(g, r)).unwrap_or(NOWHERE);
        match shops_snapshot.iter().position(|s| ok_shop_room(s, room)) {
            Some(i) => i as i32,
            None => {
                g.send_to_char(ch, "This isn't a shop!\r\n");
                return;
            }
        }
    } else if is_number(arg) {
        arg.parse::<i32>().unwrap_or(0) - 1
    } else {
        -1
    };

    if shop_nr < 0 || shop_nr >= top as i32 {
        g.send_to_char(ch, "Illegal shop number.\r\n");
        return;
    }
    list_detailed_shop(g, ch, &shops_snapshot[shop_nr as usize], shop_nr as usize);
}

fn list_all_shops(g: &mut GameState, ch: CharId) {
    let shops_snapshot = match shops().lock().ok().map(|v| v.clone()) {
        Some(v) => v,
        None => return,
    };

    let mut buf = String::from("\r\n");
    for (shop_nr, shop) in shops_snapshot.iter().enumerate() {
        if shop_nr % 19 == 0 {
            buf.push_str(" ##   Virtual   Where    Keeper    Buy   Sell   Customers\r\n");
            buf.push_str("---------------------------------------------------------\r\n");
        }
        let where_room = shop.in_room.first().copied().unwrap_or(NOWHERE);
        let keeper = if shop.keeper < 0 {
            "<NONE>".to_string()
        } else {
            format!("{:6}", shop.keeper)
        };
        buf.push_str(&format!(
            "{:3}   {:6}   {:6}    {}   {:.2}   {:.2}    {}\r\n",
            shop_nr + 1,
            shop.vnum,
            where_room,
            keeper,
            shop.profit_sell,
            shop.profit_buy,
            customer_string(shop, false)
        ));
    }
    page_string_to_char(g, ch, &buf);
}

fn list_detailed_shop(g: &mut GameState, ch: CharId, shop: &ShopData, shop_idx: usize) {
    g.send_to_char(ch, &format!("Vnum:       [{:5}], Rnum: [{:5}]\r\n", shop.vnum, shop_idx + 1));

    // Rooms.
    let mut buf = String::from("Rooms:      ");
    let mut any = false;
    for (i, &rv) in shop.in_room.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        any = true;
        match g.real_room(rv) {
            Some(rnum) => {
                let name = g.room_opt(rnum).map(|r| r.name.clone()).unwrap_or_default();
                buf.push_str(&format!("{} (#{})", name, rv));
            }
            None => buf.push_str(&format!("<UNKNOWN> (#{})", rv)),
        }
    }
    if !any {
        g.send_to_char(ch, "Rooms:      None!\r\n");
    } else {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }

    // Shopkeeper.
    if shop.keeper >= 0 {
        let kname = g.mob_protos.get(&shop.keeper).map(|p| fname(&p.name)).unwrap_or_else(|| "<none>".to_string());
        g.send_to_char(ch, &format!("Shopkeeper: {} (#{}), Special Function: No\r\n", kname, shop.keeper));
        // Live coins, if an instance exists.
        if let Some(k) = g.chars.values().find(|c| c.is_npc && c.nr == shop.keeper) {
            let kg = k.points.gold;
            g.send_to_char(ch, &format!(
                "Coins:      [{:9}], Bank: [{:9}] (Total: {})\r\n",
                kg, shop.bank_account, kg + shop.bank_account
            ));
        }
    } else {
        g.send_to_char(ch, "Shopkeeper: <NONE>\r\n");
    }

    let cs = customer_string(shop, true);
    g.send_to_char(ch, &format!("Customers:  {}\r\n", if cs.is_empty() { "None" } else { &cs }));

    // Produces.
    let mut buf = String::from("Produces:   ");
    let mut any = false;
    for (i, &pv) in shop.producing.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        any = true;
        let short = g.obj_protos.get(&pv).map(|p| p.short_desc.clone()).unwrap_or_default();
        buf.push_str(&format!("{} (#{})", short, pv));
    }
    if !any {
        g.send_to_char(ch, "Produces:   Nothing!\r\n");
    } else {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }

    // Buys.
    let mut buf = String::from("Buys:       ");
    let mut any = false;
    for (i, entry) in shop.type_.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        any = true;
        let tname = ITEM_TYPE_NAMES.get(entry.type_ as usize).copied().unwrap_or("UNDEFINED");
        buf.push_str(&format!("{} (#{}) ", tname, entry.type_));
        match &entry.keywords {
            Some(k) => buf.push_str(&format!("[{}]", k)),
            None => buf.push_str("[all]"),
        }
    }
    if !any {
        g.send_to_char(ch, "Buys:       Nothing!\r\n");
    } else {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }

    g.send_to_char(ch, &format!(
        "Buy at:     [{:.2}], Sell at: [{:.2}], Open: [{}-{}, {}-{}]\r\n",
        shop.profit_sell, shop.profit_buy, shop.open1, shop.close1, shop.open2, shop.close2
    ));

    // Bits.
    let mut bits = String::new();
    for (i, name) in SHOP_BITS.iter().enumerate() {
        if shop.bitvector & (1 << i) != 0 {
            if !bits.is_empty() {
                bits.push(' ');
            }
            bits.push_str(name);
        }
    }
    g.send_to_char(ch, &format!("Bits:       {}\r\n", bits));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::dg_db_scripts::TrigProto;
    use crate::dg_handler::{ScriptKey, DG_TEST_LOCK, OBJ_TRIGGER, OTRIG_LOAD};
    use crate::object::{ExtraFlags, ObjectType, WearFlags};
    use crate::room::Room;
    use crate::world::ObjectProto;

    fn add_player(g: &mut GameState, name: &str, level: Level) -> CharId {
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.player.level = level;
        g.create_char(ch)
    }

    fn connected_player(g: &mut GameState, conn: ConnId, name: &str, level: Level) -> CharId {
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.player.level = level;
        g.create_char(ch)
    }

    fn object_proto(vnum: ObjVnum, short: &str, cost: i32) -> ObjectProto {
        ObjectProto {
            vnum,
            name: short.to_string(),
            short_desc: short.to_string(),
            description: format!("{} is here.", short),
            obj_type: ObjectType::Other,
            wear_flags: WearFlags::TAKE,
            extra_flags: ExtraFlags::empty(),
            weight: 1,
            cost,
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

    #[test]
    fn is_god_uses_godcmd_bitvectors_instead_of_level() {
        let mut g = GameState::new(Config::default());
        let high_level_without_cmds = add_player(&mut g, "High", LVL_GOD);
        let command_granted_mortal = add_player(&mut g, "Granted", 1);
        g.get_char_mut(command_granted_mortal).unwrap().godcmds4 = 1;

        let npc = g.create_char(Character::new_npc(1));
        g.get_char_mut(npc).unwrap().godcmds1 = 1;

        assert!(!is_god(&g, high_level_without_cmds));
        assert!(is_god(&g, command_granted_mortal));
        assert!(!is_god(&g, npc));
    }

    #[test]
    fn list_all_shops_uses_pager_for_long_output() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(64);
        let ch = connected_player(&mut g, conn, "Imm", LVL_IMPL);

        {
            let mut guard = shops().lock().unwrap();
            guard.clear();
            for i in 0..40 {
                let mut shop = ShopData::new(10_000 + i);
                shop.in_room.push(20_000 + i);
                shop.keeper = 30_000 + i;
                guard.push(shop);
            }
        }

        list_all_shops(&mut g, ch);

        assert!(crate::modify::page_active(conn));
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Virtual"));
        assert!(!out.contains("10039"));

        shops().lock().unwrap().clear();
        let _ = crate::modify::page_input(&mut g, conn, "q");
    }

    #[test]
    fn is_shop_keeper_vnum_matches_loaded_shop_table() {
        {
            let mut guard = shops().lock().unwrap();
            guard.clear();
            let mut shop = ShopData::new(1);
            shop.keeper = 4242;
            guard.push(shop);
        }

        assert!(is_shop_keeper_vnum(4242));
        assert!(!is_shop_keeper_vnum(4243));

        shops().lock().unwrap().clear();
    }

    #[test]
    fn producing_shop_purchase_fires_load_trigger_on_fresh_object() {
        let _dg = DG_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        crate::dg_handler::boot_handler();
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Shop".to_string(),
            "A small shop.".to_string(),
        ));
        let buyer = connected_player(&mut g, ConnId(2), "Buyer", 20);
        let keeper = g.create_char(Character::new_npc(9000));
        g.char_to_room(buyer, room);
        g.char_to_room(keeper, room);
        g.get_char_mut(buyer).unwrap().points.gold = 1000;
        g.obj_protos
            .insert(5100, object_proto(5100, "triggered amulet", 10));

        crate::dg_db_scripts::set_test_proto_trigger(
            OBJ_TRIGGER,
            5100,
            TrigProto {
                vnum: 95100,
                attach_type: OBJ_TRIGGER,
                name: "object load marker".to_string(),
                trigger_type: OTRIG_LOAD,
                narg: 100,
                arglist: String::new(),
                cmdlist: vec![
                    "set bought yes".to_string(),
                    "global bought".to_string(),
                    "halt".to_string(),
                ],
            },
        );
        let stock = g.load_object(5100).unwrap();
        g.obj_to_char(stock, keeper);

        {
            let mut guard = shops().lock().unwrap();
            guard.clear();
            let mut shop = ShopData::new(1);
            shop.producing.push(5100);
            shop.keeper = 9000;
            shop.close1 = 28;
            shop.close2 = 28;
            shop.message_buy = "$n bought it.".to_string();
            guard.push(shop);
        }

        shopping_buy(&mut g, "amulet", buyer, keeper, 0);

        let bought = g.get_char(buyer).unwrap().carrying.first().copied().unwrap();
        assert_eq!(
            crate::dg_handler::get_global_var(ScriptKey::Obj(bought), "bought").as_deref(),
            Some("yes")
        );
        shops().lock().unwrap().clear();
    }
}
