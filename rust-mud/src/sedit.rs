// sedit.rs — the OasisOLC shop editor (C sedit.c), ported to the id-indexed
// GameState and the shared olc.rs framework contract.
//
// What this does, mirroring sedit.c:
//   * do_sedit(g, ch, arg, subcmd)  — the `sedit <vnum>` / `sedit save <zone>`
//     command. Snapshots the matching shop (or a fresh default shop) into the
//     per-connection edit state, registers EditorKind::Sedit with olc, and
//     displays the main menu.
//   * sedit_parse(g, conn, line)    — the per-line input router olc calls.
//
// ON-DISK FORMAT (byte-faithful inverse of shop.rs boot_the_shops / C
// boot_the_shops + sedit_save_to_disk). One zone's shops live in
// world/shp/<zone>.shp:
//     CircleMUD v3.0 Shop File~\n
//     #<vnum>~\n
//     <producing obj vnums, one per line, then -1>\n
//     <profit_buy %.2f>\n  <profit_sell %.2f>\n
//     <trade types: "<type-int><keywords>" lines, then a "-1" line>\n
//     7 message strings (no_such_item1, no_such_item2, do_not_buy,
//        missing_cash1, missing_cash2, message_buy, message_sell), each
//        terminated by '~'
//     <temper1>\n <bitvector>\n <keeper mob vnum>\n <with_who>\n
//     <room vnums, one per line, then -1>\n
//     <open1>\n <close1>\n <open2>\n <close2>\n
//   ...repeats... then "$~\n".
//
// OWNERSHIP NOTE: sedit owns an editable copy of a zone's shops. On save it
// atomically replaces the zone file, then asks shop.rs to parse that persisted
// record and upsert it into the live table.

use crate::constants::ITEM_TYPES;
use crate::olc::{self, EditorKind};
use crate::state::GameState;
use crate::types::*;
use std::io::Result as IoResult;
use std::sync::{Mutex, OnceLock};

// olc.h.
const NUM_SHOP_FLAGS: usize = 2;
const NUM_TRADERS: usize = 7;
const NUM_ITEM_TYPES: usize = 29; // olc.h NUM_ITEM_TYPES (matches ITEM_TYPES len-1).

// shop bit names (shop_bits[]) and trade-with names (trade_letters[]).
const SHOP_BITS: [&str; NUM_SHOP_FLAGS] = ["WILL_FIGHT", "USES_BANK"];
const TRADE_LETTERS: [&str; NUM_TRADERS] = [
    "Good",
    "Evil",
    "Neutral",
    "Magic User",
    "Cleric",
    "Thief",
    "Warrior",
];

const SHP_REL_DIR: &str = "world/shp";

// ---------------------------------------------------------------------------
// shop_data / shop_buy_data (shop.h) — the editable mirror.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct ShopBuyData {
    /// Trade item type (raw ITEM_* integer) or -1 terminator.
    type_: i32,
    keywords: Option<String>,
}

#[derive(Clone, Debug)]
struct Shop {
    vnum: i32,
    /// Producing object vnums (C stores real numbers; we keep vnums, matching
    /// shop.rs and the on-disk obj_index[..].vnum the C writer emits).
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
    /// Keeper mob vnum (-1 / NOBODY for none).
    keeper: MobVnum,
    with_who: i32,
    in_room: Vec<RoomVnum>,
    open1: i32,
    close1: i32,
    open2: i32,
    close2: i32,
}

impl Shop {
    fn new(vnum: i32) -> Self {
        // sedit_setup_new defaults.
        Shop {
            vnum,
            producing: Vec::new(),
            profit_buy: 1.0,
            profit_sell: 1.0,
            type_: Vec::new(),
            no_such_item1: "%s Sorry, I don't stock that item.".to_string(),
            no_such_item2: "%s You don't seem to have that.".to_string(),
            do_not_buy: "%s I don't trade in such items.".to_string(),
            missing_cash1: "%s I can't afford that!".to_string(),
            missing_cash2: "%s You are too poor!".to_string(),
            message_buy: "%s That'll be %d coins, thanks.".to_string(),
            message_sell: "%s I'll give you %d coins for that.".to_string(),
            temper1: 0,
            bitvector: 0,
            keeper: NOBODY,
            with_who: 0,
            in_room: Vec::new(),
            open1: 0,
            close1: 28,
            open2: 0,
            close2: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-connection editor state.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct SeditState {
    ch: CharId,
    /// The shop vnum being edited (C OLC_NUM).
    vnum: i32,
    /// Zone number for this shop (vnum / 100), used by the save path.
    zone: i32,
    /// The scratch shop (C OLC_SHOP).
    shop: Shop,
    /// "Anything changed?" flag (C OLC_VAL).
    changed: bool,
    /// Current sub-menu (C OLC_MODE).
    mode: SeditMode,
    /// Scratch holder for the type-menu -> namelist two-step (C OLC_VAL reuse).
    pending_type: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeditMode {
    MainMenu,
    ConfirmSave,
    Keeper,
    Open1,
    Close1,
    Open2,
    Close2,
    BuyProfit,
    SellProfit,
    NoItem1,
    NoItem2,
    NoCash1,
    NoCash2,
    NoBuy,
    Buy,
    Sell,
    NamelistMenu,
    ProductsMenu,
    RoomsMenu,
    ShopFlags,
    NoTrade,
    TypeMenu,
    Namelist,
    DeleteType,
    NewProduct,
    DeleteProduct,
    NewRoom,
    DeleteRoom,
}

/// Modes strictly greater than this require a numerical first character (C:
/// `OLC_MODE(d) > SEDIT_NUMERICAL_RESPONSE`). The numeric-response group are the
/// keeper / open / close / profit / type / list-edit modes below.
fn is_numerical_mode(m: SeditMode) -> bool {
    matches!(
        m,
        SeditMode::Keeper
            | SeditMode::Open1
            | SeditMode::Close1
            | SeditMode::Open2
            | SeditMode::Close2
            | SeditMode::BuyProfit
            | SeditMode::SellProfit
            | SeditMode::TypeMenu
            | SeditMode::DeleteType
            | SeditMode::NewProduct
            | SeditMode::DeleteProduct
            | SeditMode::NewRoom
            | SeditMode::DeleteRoom
            | SeditMode::ShopFlags
            | SeditMode::NoTrade
    )
}

static STATES: OnceLock<Mutex<std::collections::HashMap<ConnId, SeditState>>> = OnceLock::new();

fn states() -> &'static Mutex<std::collections::HashMap<ConnId, SeditState>> {
    STATES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn with_state<R>(conn: ConnId, f: impl FnOnce(&mut SeditState) -> R) -> Option<R> {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.get_mut(&conn).map(f)
}

fn take_state(conn: ConnId) -> Option<SeditState> {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.remove(&conn)
}

/// abort: drop this conn's editor state without saving (player disconnected
/// mid-edit). `olc::abort_editor` calls `olc::clear_active`.
pub fn abort(conn: ConnId) {
    take_state(conn);
}

fn set_state(conn: ConnId, st: SeditState) {
    let mut guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.insert(conn, st);
}

/// True if another connection is already editing this shop vnum.
fn shop_busy(vnum: i32, exclude: ConnId) -> bool {
    let guard = match states().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    guard.iter().any(|(&c, st)| c != exclude && st.vnum == vnum)
}

// ---------------------------------------------------------------------------
// Disk I/O: load a zone's shops, and write them back (byte-faithful .shp).
// ---------------------------------------------------------------------------

fn shp_path(lib_path: &str, zone: i32) -> String {
    format!(
        "{}/{}/{}.shp",
        lib_path.trim_end_matches('/'),
        SHP_REL_DIR,
        zone
    )
}

/// Load every shop in `<lib>/world/shp/<zone>.shp` into editable structs.
fn load_zone_shops(lib_path: &str, zone: i32) -> Vec<Shop> {
    let path = shp_path(lib_path, zone);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    parse_shop_file(&raw)
}

/// Find a specific shop by vnum across the whole shp directory's index. Used so
/// `sedit <vnum>` finds the right zone file even when vnum/100 != file name.
/// (DeltaMUD shop files are named by zone == vnum/100, so the direct lookup is
/// the common path; the index scan is the fallback.)
fn find_shop_zone(lib_path: &str, vnum: i32) -> Option<i32> {
    // Primary: the conventional zone == vnum/100 file.
    let zone = vnum / 100;
    if load_zone_shops(lib_path, zone)
        .iter()
        .any(|s| s.vnum == vnum)
    {
        return Some(zone);
    }
    // Fallback: scan the shp index for any file that contains this shop.
    let dir = format!("{}/{}", lib_path.trim_end_matches('/'), SHP_REL_DIR);
    let index = std::fs::read_to_string(format!("{}/index", dir)).ok()?;
    for line in index.lines() {
        let fname = line.trim();
        if fname.is_empty() || fname == "$" {
            break;
        }
        if let Some(stem) = fname.strip_suffix(".shp") {
            if let Ok(z) = stem.parse::<i32>() {
                if load_zone_shops(lib_path, z).iter().any(|s| s.vnum == vnum) {
                    return Some(z);
                }
            }
        }
    }
    None
}

// --- the inverse of the writer: a faithful re-parser (matches shop.rs) -------

struct Reader<'a> {
    lines: Vec<&'a str>,
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(s: &'a str) -> Self {
        Reader {
            lines: s.lines().collect(),
            pos: 0,
        }
    }
    fn get_line(&mut self) -> Option<&'a str> {
        while self.pos < self.lines.len() {
            let l = self.lines[self.pos];
            self.pos += 1;
            let t = l.trim();
            if t.is_empty() || t.starts_with('*') {
                continue;
            }
            return Some(l);
        }
        None
    }
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

fn parse_int_lead(t: &str) -> Option<i32> {
    let t = t.trim();
    let bytes = t.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let mut end = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        end = i;
    }
    if end == 0 {
        return None;
    }
    match t[..end].parse::<i32>() {
        Ok(value) => Some(value),
        Err(_) => {
            let clamped = if t.starts_with('-') {
                i32::MIN
            } else {
                i32::MAX
            };
            log::warn!(
                "SYSERR: shop numeric field overflow; clamped to {clamped}: {}",
                &t[..end]
            );
            Some(clamped)
        }
    }
}

fn r_int(r: &mut Reader) -> i32 {
    r.get_line()
        .and_then(|l| l.trim().split_whitespace().next().and_then(parse_int_lead))
        .unwrap_or(0)
}
fn r_float(r: &mut Reader) -> f32 {
    r.get_line()
        .and_then(|l| {
            l.trim()
                .split_whitespace()
                .next()
                .and_then(|t| t.parse::<f32>().ok())
        })
        .unwrap_or(0.0)
}
fn r_int_list(r: &mut Reader) -> Vec<i32> {
    let mut out = Vec::new();
    loop {
        let v = r_int(r);
        if v < 0 {
            break;
        }
        out.push(v);
    }
    out
}
fn r_type_list(r: &mut Reader) -> Vec<ShopBuyData> {
    let mut out = Vec::new();
    loop {
        let line = match r.get_line() {
            Some(l) => l,
            None => break,
        };
        let body = match line.find(';') {
            Some(i) => &line[..i],
            None => line,
        };
        let body = body.trim_end_matches('\r');
        let trimmed = body.trim_start();

        let mut num: i32 = -1;
        let mut rest = body;
        for (idx, name) in ITEM_TYPES.iter().enumerate() {
            if *name == "\n" {
                continue;
            }
            if !name.is_empty()
                && trimmed
                    .get(..name.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
            {
                num = idx as i32;
                rest = trimmed.get(name.len()..).unwrap_or_default();
                break;
            }
        }
        if num == -1 {
            if let Some(n) = parse_int_lead(trimmed) {
                num = n;
                let bytes = trimmed.as_bytes();
                let mut i = 0;
                while i < bytes.len() && !bytes[i].is_ascii_digit() && bytes[i] != b'-' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'-' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                rest = &trimmed[i.min(trimmed.len())..];
            } else {
                break;
            }
        }
        let kw = rest.trim();
        let kw = if kw.is_empty() {
            None
        } else {
            Some(kw.to_string())
        };
        if num < 0 {
            break;
        }
        out.push(ShopBuyData {
            type_: num,
            keywords: kw,
        });
    }
    out
}

fn parse_shop_file(contents: &str) -> Vec<Shop> {
    let mut r = Reader::new(contents);
    let _opening = r.fread_string(); // "CircleMUD v3.0 Shop File~"
    let mut table = Vec::new();

    loop {
        let nxt = match r.peek_significant() {
            Some(l) => l,
            None => break,
        };
        let nt = nxt.trim();
        if nt.starts_with('$') {
            break;
        }
        if !nt.starts_with('#') {
            let _ = r.fread_string();
            continue;
        }
        let header = r.fread_string();
        let vnum = parse_int_lead(header.trim_start_matches('#')).unwrap_or(0);
        let mut shop = Shop::new(vnum);
        shop.producing = r_int_list(&mut r);
        shop.profit_buy = r_float(&mut r);
        shop.profit_sell = r_float(&mut r);
        shop.type_ = r_type_list(&mut r);
        shop.no_such_item1 = r.fread_string();
        shop.no_such_item2 = r.fread_string();
        shop.do_not_buy = r.fread_string();
        shop.missing_cash1 = r.fread_string();
        shop.missing_cash2 = r.fread_string();
        shop.message_buy = r.fread_string();
        shop.message_sell = r.fread_string();
        shop.temper1 = r_int(&mut r);
        shop.bitvector = r_int(&mut r);
        shop.keeper = r_int(&mut r);
        shop.with_who = r_int(&mut r);
        shop.in_room = r_int_list(&mut r);
        shop.open1 = r_int(&mut r);
        shop.close1 = r_int(&mut r);
        shop.open2 = r_int(&mut r);
        shop.close2 = r_int(&mut r);
        table.push(shop);
    }
    table
}

/// Write a zone's shop table back to <zone>.shp, byte-faithful with C
/// sedit_save_to_disk: shops are emitted in ascending vnum order over the
/// zone's vnum range.
fn sedit_save_to_disk(lib_path: &str, zone: i32, shops: &[Shop]) -> IoResult<()> {
    let dir = format!("{}/{}", lib_path.trim_end_matches('/'), SHP_REL_DIR);
    let tmp = format!("{}/{}.new", dir, zone);
    let final_path = shp_path(lib_path, zone);

    let mut out = String::new();
    out.push_str("CircleMUD v3.0 Shop File~\n");

    // C iterates i from zone*100..=zone_top and emits real_shop(i); we emit the
    // zone's shops sorted by vnum to reproduce that ascending order.
    let mut ordered: Vec<&Shop> = shops.iter().collect();
    ordered.sort_by_key(|s| s.vnum);

    for shop in ordered {
        out.push_str(&format!("#{}~\n", shop.vnum));

        // Products: each obj vnum, then -1.
        for &p in &shop.producing {
            out.push_str(&format!("{}\n", p));
        }
        // Rates: "-1\n%.2f\n%.2f\n".
        out.push_str(&format!(
            "-1\n{:.2}\n{:.2}\n",
            shop.profit_buy, shop.profit_sell
        ));

        // Trade types + namelists. C: do { j++; print "%d%s\n" } while != -1,
        // i.e. each entry then the -1 terminator with no keyword.
        for t in &shop.type_ {
            out.push_str(&format!(
                "{}{}\n",
                t.type_,
                t.keywords.as_deref().unwrap_or("")
            ));
        }
        out.push_str("-1\n");

        // 7 messages, each '~'-terminated. C emits in this order with "Ke?!"
        // fallbacks for empties.
        let msg = |s: &str, fallback: &str| -> String {
            if s.is_empty() {
                fallback.to_string()
            } else {
                s.to_string()
            }
        };
        out.push_str(&format!("{}~\n", msg(&shop.no_such_item1, "%s Ke?!")));
        out.push_str(&format!("{}~\n", msg(&shop.no_such_item2, "%s Ke?!")));
        out.push_str(&format!("{}~\n", msg(&shop.do_not_buy, "%s Ke?!")));
        out.push_str(&format!("{}~\n", msg(&shop.missing_cash1, "%s Ke?!")));
        out.push_str(&format!("{}~\n", msg(&shop.missing_cash2, "%s Ke?!")));
        out.push_str(&format!("{}~\n", msg(&shop.message_buy, "%s Ke?! %d?")));
        out.push_str(&format!("{}~\n", msg(&shop.message_sell, "%s Ke?! %d?")));

        // temper1, bitvector, keeper mob vnum, with_who.
        out.push_str(&format!(
            "{}\n{}\n{}\n{}\n",
            shop.temper1, shop.bitvector, shop.keeper, shop.with_who
        ));

        // Rooms, then -1.
        for &room in &shop.in_room {
            out.push_str(&format!("{}\n", room));
        }
        out.push_str("-1\n");

        // open1, close1, open2, close2.
        out.push_str(&format!(
            "{}\n{}\n{}\n{}\n",
            shop.open1, shop.close1, shop.open2, shop.close2
        ));
    }

    out.push_str("$~\n");

    std::fs::create_dir_all(&dir)?;
    std::fs::write(&tmp, &out)?;
    // rename replaces the existing file atomically on this Unix host. Do not
    // unlink first: that creates a missing-file window if rename fails.
    std::fs::rename(&tmp, &final_path)?;
    Ok(())
}

/// Central OLC save dispatcher entry. The live shop table is private to
/// shop.rs, so this path preserves the current zone shop file by reading it and
/// rewriting it through the canonical C-shaped renderer.
pub fn sedit_save_zone_to_disk(g: &mut GameState, zone_rnum: usize) {
    if let Some(z) = g.zones.get(zone_rnum) {
        // C sedit.c:532 (#274).
        crate::olc::olc_add_to_save_list(z.number, crate::olc::OLC_SAVE_SHOP);
    }
    let zone = match g.zones.get(zone_rnum) {
        Some(z) => z.number,
        None => return,
    };
    let lib_path = g.config.lib_path.clone();
    let shops = load_zone_shops(&lib_path, zone);
    match sedit_save_to_disk(&lib_path, zone, &shops) {
        Ok(()) => crate::olc::olc_remove_from_save_list(zone, crate::olc::OLC_SAVE_SHOP),
        Err(err) => log::warn!("SYSERR: OLC: Cannot save shop zone {}: {}", zone, err),
    }
}

// ---------------------------------------------------------------------------
// Command entry: do_sedit (the sedit.c-relevant slice of olc.c do_olc).
// ---------------------------------------------------------------------------

pub fn do_sedit(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        return;
    }
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    let lib_path = g.config.lib_path.clone();

    // two_arguments(argument, buf1, buf2).
    let (buf1, rest) = crate::interpreter::one_argument(arg);
    let (buf2, _) = crate::interpreter::one_argument(rest);

    if buf1.is_empty() {
        send(g, ch, "Specify a shop VNUM to edit.\r\n");
        return;
    }

    // "save <zone>" — write that zone's .shp file (do_olc SEDIT save path).
    if !buf1
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        if "save".starts_with(&buf1.to_lowercase()) {
            if buf2.is_empty() {
                send(g, ch, "Save which zone?\r\n");
                return;
            }
            let Some(zone) = olc::parse_i32_input(g, conn, &buf2, -1) else {
                return;
            };
            if zone < 0 {
                send(g, ch, "Save which zone?\r\n");
                return;
            }
            send(g, ch, &format!("Saving all shops in zone {}.\r\n", zone));
            let name = g
                .get_char(ch)
                .map(|c| c.player.name.clone())
                .unwrap_or_default();
            log::info!("OLC: {} saves shop info for zone {}.", name, zone);
            let shops = load_zone_shops(&lib_path, zone);
            if let Err(err) = sedit_save_to_disk(&lib_path, zone, &shops) {
                log::warn!("SYSERR: OLC: Cannot save shop zone {}: {}", zone, err);
                send(g, ch, "Could not save that shop zone.\r\n");
            }
            return;
        }
        send(g, ch, "Yikes!  Stop that, someone will get hurt!\r\n");
        return;
    }

    let Some(vnum) = olc::parse_i32_input(g, conn, &buf1, -1) else {
        return;
    };
    if vnum < 0 {
        send(g, ch, "Yikes!  Stop that, someone will get hurt!\r\n");
        return;
    }

    if shop_busy(vnum, conn) {
        g.send_to_char(
            ch,
            "That shop is currently being edited by someone else.\r\n",
        );
        return;
    }

    // Determine the zone and whether the shop exists.
    let (zone, existing) = match find_shop_zone(&lib_path, vnum) {
        Some(z) => {
            let shop = load_zone_shops(&lib_path, z)
                .into_iter()
                .find(|s| s.vnum == vnum);
            (z, shop)
        }
        None => (vnum / 100, None),
    };

    let shop = existing.unwrap_or_else(|| Shop::new(vnum));

    set_state(
        conn,
        SeditState {
            ch,
            vnum,
            zone,
            shop,
            changed: false,
            mode: SeditMode::MainMenu,
            pending_type: 0,
        },
    );
    olc::set_active(conn, EditorKind::Sedit);

    crate::act::act(
        g,
        "$n starts using OLC.",
        true,
        ch,
        None,
        crate::act::ActArg::None,
        crate::act::To::Room,
    );

    sedit_disp_menu(g, conn);
}

// ---------------------------------------------------------------------------
// Lookups against GameState (real_mobile / real_object / real_room).
// ---------------------------------------------------------------------------

fn mob_exists(g: &GameState, vnum: MobVnum) -> bool {
    g.mob_protos.contains_key(&vnum)
}
fn obj_exists(g: &GameState, vnum: ObjVnum) -> bool {
    g.obj_protos.contains_key(&vnum)
}
fn keeper_short(g: &GameState, vnum: MobVnum) -> String {
    g.mob_protos
        .get(&vnum)
        .map(|m| m.short_desc.clone())
        .unwrap_or_else(|| "None".to_string())
}
fn product_short(g: &GameState, vnum: ObjVnum) -> String {
    g.obj_protos
        .get(&vnum)
        .map(|o| o.short_desc.clone())
        .unwrap_or_else(|| "<UNKNOWN>".to_string())
}
fn room_name(g: &GameState, vnum: RoomVnum) -> String {
    match g.real_room(vnum) {
        Some(rnum) => g.room(rnum).name.clone(),
        None => "<INVALID>".to_string(),
    }
}

// ---------------------------------------------------------------------------
// sprintbit helper (utils.c).
// ---------------------------------------------------------------------------

fn sprintbit(bits: i64, names: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        if bits & (1 << i) != 0 {
            out.push_str(name);
            out.push(' ');
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

// ---------------------------------------------------------------------------
// Menu rendering.
// ---------------------------------------------------------------------------

fn sedit_disp_menu(g: &mut GameState, conn: ConnId) {
    let st = match with_state(conn, |st| {
        st.mode = SeditMode::MainMenu;
        st.clone()
    }) {
        Some(s) => s,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let shop = &st.shop;

    let notrade = sprintbit(shop.with_who as i64, &TRADE_LETTERS);
    let bitvec = sprintbit(shop.bitvector as i64, &SHOP_BITS);

    let keeper_disp = if shop.keeper == NOBODY {
        "None".to_string()
    } else {
        keeper_short(g, shop.keeper)
    };

    let menu = format!(
        "\
-- Shop Number : [&c{}&n]\r\n\
&g0&n) Keeper      : [&c{}&n] &y{}\r\n\
&g1&n) Open 1      : &c{:4}&n          &g2&n) Close 1     : &c{:4}\r\n\
&g3&n) Open 2      : &c{:4}&n          &g4&n) Close 2     : &c{:4}\r\n\
&g5&n) Sell rate   : &c{:.2}&n          &g6&n) Buy rate    : &c{:.2}\r\n\
&g7&n) Keeper no item : &y{}\r\n\
&g8&n) Player no item : &y{}\r\n\
&g9&n) Keeper no cash : &y{}\r\n\
&gA&n) Player no cash : &y{}\r\n\
&gB&n) Keeper no buy  : &y{}\r\n\
&gC&n) Buy sucess     : &y{}\r\n\
&gD&n) Sell sucess    : &y{}\r\n\
&gE&n) No Trade With  : &c{}\r\n\
&gF&n) Shop flags     : &c{}\r\n\
&gR&n) Rooms Menu\r\n\
&gP&n) Products Menu\r\n\
&gT&n) Accept Types Menu\r\n\
&gQ&n) Quit\r\n\
Enter Choice : ",
        st.vnum,
        if shop.keeper == NOBODY {
            -1
        } else {
            shop.keeper
        },
        keeper_disp,
        shop.open1,
        shop.close1,
        shop.open2,
        shop.close2,
        shop.profit_buy,
        shop.profit_sell,
        shop.no_such_item1,
        shop.no_such_item2,
        shop.missing_cash1,
        shop.missing_cash2,
        shop.do_not_buy,
        shop.message_buy,
        shop.message_sell,
        notrade,
        bitvec,
    );
    send(g, ch, &menu);
}

fn sedit_products_menu(g: &mut GameState, conn: ConnId) {
    let prods = match with_state(conn, |st| {
        st.mode = SeditMode::ProductsMenu;
        st.shop.producing.clone()
    }) {
        Some(p) => p,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("##     VNUM     Product\r\n");
    for (i, &p) in prods.iter().enumerate() {
        out.push_str(&format!(
            "{:2} - [&c{:5}&n] - &y{}&n\r\n",
            i,
            p,
            product_short(g, p)
        ));
    }
    out.push_str(
        "\r\n&gA&n) Add a new product.\r\n&gD&n) Delete a product.\r\n&gQ&n) Quit\r\nEnter choice : ",
    );
    send(g, ch, &out);
}

fn sedit_rooms_menu(g: &mut GameState, conn: ConnId) {
    let rooms = match with_state(conn, |st| {
        st.mode = SeditMode::RoomsMenu;
        st.shop.in_room.clone()
    }) {
        Some(r) => r,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("##     VNUM     Room\r\n\r\n");
    for (i, &room) in rooms.iter().enumerate() {
        out.push_str(&format!(
            "{:2} - [&c{:5}&n] - &y{}&n\r\n",
            i,
            room,
            room_name(g, room)
        ));
    }
    out.push_str(
        "\r\n&gA&n) Add a new room.\r\n&gD&n) Delete a room.\r\n&gC&n) Compact Display.\r\n\
&gQ&n) Quit\r\nEnter choice : ",
    );
    send(g, ch, &out);
}

fn sedit_compact_rooms_menu(g: &mut GameState, conn: ConnId) {
    let rooms = match with_state(conn, |st| {
        st.mode = SeditMode::RoomsMenu;
        st.shop.in_room.clone()
    }) {
        Some(r) => r,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("");
    for (i, &room) in rooms.iter().enumerate() {
        out.push_str(&format!("{:2} - [&c{:5}&n]  | ", i, room));
        if (i + 1) % 5 == 0 {
            out.push_str("\r\n");
        }
    }
    out.push_str(
        "\r\n&gA&n) Add a new room.\r\n&gD&n) Delete a room.\r\n&gL&n) Long display.\r\n\
&gQ&n) Quit\r\nEnter choice : ",
    );
    send(g, ch, &out);
}

fn sedit_namelist_menu(g: &mut GameState, conn: ConnId) {
    let types = match with_state(conn, |st| {
        st.mode = SeditMode::NamelistMenu;
        st.shop.type_.clone()
    }) {
        Some(t) => t,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("##              Type   Namelist\r\n\r\n");
    for (i, t) in types.iter().enumerate() {
        let tname = type_name(t.type_);
        out.push_str(&format!(
            "{:2} - &c{:>15}&n - &y{}&n\r\n",
            i,
            tname,
            t.keywords.as_deref().unwrap_or("<None>")
        ));
    }
    out.push_str(
        "\r\n&gA&n) Add a new entry.\r\n&gD&n) Delete an entry.\r\n&gQ&n) Quit\r\nEnter choice : ",
    );
    send(g, ch, &out);
}

fn sedit_types_menu(g: &mut GameState, conn: ConnId) {
    with_state(conn, |st| st.mode = SeditMode::TypeMenu);
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("");
    let mut count = 0;
    for i in 0..NUM_ITEM_TYPES {
        out.push_str(&format!("&g{:2}&n) &c{:<20}&n  ", i, type_name(i as i32)));
        count += 1;
        if count % 3 == 0 {
            out.push_str("\r\n");
        }
    }
    out.push_str("&nEnter choice : ");
    send(g, ch, &out);
}

fn sedit_shop_flags_menu(g: &mut GameState, conn: ConnId) {
    let bits = match with_state(conn, |st| {
        st.mode = SeditMode::ShopFlags;
        st.shop.bitvector
    }) {
        Some(b) => b,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("");
    let mut count = 0;
    for (i, name) in SHOP_BITS.iter().enumerate() {
        out.push_str(&format!("&g{:2}&n) {:<20.20}   ", i + 1, name));
        count += 1;
        if count % 2 == 0 {
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!(
        "\r\nCurrent Shop Flags : &c{}&n\r\nEnter choice : ",
        sprintbit(bits as i64, &SHOP_BITS)
    ));
    send(g, ch, &out);
}

fn sedit_no_trade_menu(g: &mut GameState, conn: ConnId) {
    let bits = match with_state(conn, |st| {
        st.mode = SeditMode::NoTrade;
        st.shop.with_who
    }) {
        Some(b) => b,
        None => return,
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => return,
    };
    let mut out = String::from("");
    let mut count = 0;
    for (i, name) in TRADE_LETTERS.iter().enumerate() {
        out.push_str(&format!("&g{:2}&n) {:<20.20}   ", i + 1, name));
        count += 1;
        if count % 2 == 0 {
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!(
        "\r\nCurrently won't trade with: &c{}&n\r\nEnter choice : ",
        sprintbit(bits as i64, &TRADE_LETTERS)
    ));
    send(g, ch, &out);
}

/// item_types[] name for a trade-type int (0..NUM_ITEM_TYPES); out-of-range
/// renders the raw integer.
fn type_name(t: i32) -> String {
    if t >= 0 && (t as usize) < ITEM_TYPES.len() && ITEM_TYPES[t as usize] != "\n" {
        ITEM_TYPES[t as usize].to_string()
    } else {
        t.to_string()
    }
}

// ---------------------------------------------------------------------------
// The GARGANTUAN event handler (sedit_parse).
// ---------------------------------------------------------------------------

pub fn sedit_parse(g: &mut GameState, conn: ConnId, line: &str) {
    let mode = match with_state(conn, |st| st.mode) {
        Some(m) => m,
        None => {
            olc::clear_active(conn);
            return;
        }
    };
    let ch = match conn_char(g, conn) {
        Some(c) => c,
        None => {
            cleanup(conn);
            return;
        }
    };
    let arg = line; // keep full line for string fields
    let trimmed = line.trim();

    // Numeric-field guard (C: OLC_MODE > SEDIT_NUMERICAL_RESPONSE requires a
    // digit or "-<digit>" first character).
    if is_numerical_mode(mode) {
        let first = trimmed.as_bytes().first().copied();
        let is_num = match first {
            Some(b) if b.is_ascii_digit() => true,
            Some(b'-') => trimmed
                .as_bytes()
                .get(1)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false),
            _ => false,
        };
        if !is_num {
            send(g, ch, "Field must be numerical, try again : ");
            return;
        }
    }

    match mode {
        SeditMode::ConfirmSave => {
            match trimmed.chars().next() {
                Some('y') | Some('Y') => match sedit_save_internally(g, conn) {
                    Ok(()) => {
                        send(g, ch, "Saving shop to memory.\r\n");
                        let (name, vnum) = (
                            g.get_char(ch)
                                .map(|c| c.player.name.clone())
                                .unwrap_or_default(),
                            with_state(conn, |st| st.vnum).unwrap_or(0),
                        );
                        log::info!("OLC: {} edits shop {}", name, vnum);
                        cleanup(conn);
                    }
                    Err(err) => {
                        log::warn!("SYSERR: OLC: Cannot save edited shop: {}", err);
                        send(
                            g,
                            ch,
                            "Could not save the shop; your editor remains open. Try again? (y/n) : ",
                        );
                    }
                },
                Some('n') | Some('N') => cleanup(conn),
                _ => send(g, ch, "Invalid choice!\r\nDo you wish to save the shop? : "),
            }
            return;
        }

        SeditMode::MainMenu => {
            // Returns true when the choice opened a field/submenu (already
            // prompted); false when we should fall through to disp_menu.
            match trimmed.chars().next() {
                Some('q') | Some('Q') => {
                    let changed = with_state(conn, |st| st.changed).unwrap_or(false);
                    if changed {
                        send(
                            g,
                            ch,
                            "Do you wish to save the changes to the shop? (y/n) : ",
                        );
                        with_state(conn, |st| st.mode = SeditMode::ConfirmSave);
                    } else {
                        cleanup(conn);
                    }
                    return;
                }
                Some('0') => {
                    with_state(conn, |st| st.mode = SeditMode::Keeper);
                    send(g, ch, "Enter virtual number of shop keeper : ");
                    return;
                }
                Some('1') => {
                    with_state(conn, |st| st.mode = SeditMode::Open1);
                    send(g, ch, "\r\nEnter new value : ");
                    return;
                }
                Some('2') => {
                    with_state(conn, |st| st.mode = SeditMode::Close1);
                    send(g, ch, "\r\nEnter new value : ");
                    return;
                }
                Some('3') => {
                    with_state(conn, |st| st.mode = SeditMode::Open2);
                    send(g, ch, "\r\nEnter new value : ");
                    return;
                }
                Some('4') => {
                    with_state(conn, |st| st.mode = SeditMode::Close2);
                    send(g, ch, "\r\nEnter new value : ");
                    return;
                }
                Some('5') => {
                    with_state(conn, |st| st.mode = SeditMode::BuyProfit);
                    send(g, ch, "\r\nEnter new value : ");
                    return;
                }
                Some('6') => {
                    with_state(conn, |st| st.mode = SeditMode::SellProfit);
                    send(g, ch, "\r\nEnter new value : ");
                    return;
                }
                Some('7') => {
                    with_state(conn, |st| st.mode = SeditMode::NoItem1);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('8') => {
                    with_state(conn, |st| st.mode = SeditMode::NoItem2);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('9') => {
                    with_state(conn, |st| st.mode = SeditMode::NoCash1);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('a') | Some('A') => {
                    with_state(conn, |st| st.mode = SeditMode::NoCash2);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('b') | Some('B') => {
                    with_state(conn, |st| st.mode = SeditMode::NoBuy);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('c') | Some('C') => {
                    with_state(conn, |st| st.mode = SeditMode::Buy);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('d') | Some('D') => {
                    with_state(conn, |st| st.mode = SeditMode::Sell);
                    send(g, ch, "\r\nEnter new text :\r\n] ");
                    return;
                }
                Some('e') | Some('E') => {
                    sedit_no_trade_menu(g, conn);
                    return;
                }
                Some('f') | Some('F') => {
                    sedit_shop_flags_menu(g, conn);
                    return;
                }
                Some('r') | Some('R') => {
                    sedit_rooms_menu(g, conn);
                    return;
                }
                Some('p') | Some('P') => {
                    sedit_products_menu(g, conn);
                    return;
                }
                Some('t') | Some('T') => {
                    sedit_namelist_menu(g, conn);
                    return;
                }
                _ => {
                    sedit_disp_menu(g, conn);
                    return;
                }
            }
        }

        SeditMode::NamelistMenu => {
            match trimmed.chars().next() {
                Some('a') | Some('A') => {
                    sedit_types_menu(g, conn);
                    return;
                }
                Some('d') | Some('D') => {
                    send(g, ch, "\r\nDelete which entry? : ");
                    with_state(conn, |st| st.mode = SeditMode::DeleteType);
                    return;
                }
                Some('q') | Some('Q') => {} // fall to disp_menu
                _ => {}
            }
        }

        SeditMode::ProductsMenu => match trimmed.chars().next() {
            Some('a') | Some('A') => {
                send(g, ch, "\r\nEnter new product virtual number : ");
                with_state(conn, |st| st.mode = SeditMode::NewProduct);
                return;
            }
            Some('d') | Some('D') => {
                send(g, ch, "\r\nDelete which product? : ");
                with_state(conn, |st| st.mode = SeditMode::DeleteProduct);
                return;
            }
            Some('q') | Some('Q') => {}
            _ => {}
        },

        SeditMode::RoomsMenu => match trimmed.chars().next() {
            Some('a') | Some('A') => {
                send(g, ch, "\r\nEnter new room virtual number : ");
                with_state(conn, |st| st.mode = SeditMode::NewRoom);
                return;
            }
            Some('c') | Some('C') => {
                sedit_compact_rooms_menu(g, conn);
                return;
            }
            Some('l') | Some('L') => {
                sedit_rooms_menu(g, conn);
                return;
            }
            Some('d') | Some('D') => {
                send(g, ch, "\r\nDelete which room? : ");
                with_state(conn, |st| st.mode = SeditMode::DeleteRoom);
                return;
            }
            Some('q') | Some('Q') => {}
            _ => {}
        },

        // ---- String edits -------------------------------------------------
        SeditMode::NoItem1 => {
            modify_string(conn, arg, |s, v| s.no_such_item1 = v);
        }
        SeditMode::NoItem2 => {
            modify_string(conn, arg, |s, v| s.no_such_item2 = v);
        }
        SeditMode::NoCash1 => {
            modify_string(conn, arg, |s, v| s.missing_cash1 = v);
        }
        SeditMode::NoCash2 => {
            modify_string(conn, arg, |s, v| s.missing_cash2 = v);
        }
        SeditMode::NoBuy => {
            modify_string(conn, arg, |s, v| s.do_not_buy = v);
        }
        SeditMode::Buy => {
            modify_string(conn, arg, |s, v| s.message_buy = v);
        }
        SeditMode::Sell => {
            modify_string(conn, arg, |s, v| s.message_sell = v);
        }
        SeditMode::Namelist => {
            // The keyword line for a previously chosen trade type.
            let kw = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            with_state(conn, |st| {
                let entry = ShopBuyData {
                    type_: st.pending_type,
                    keywords: kw,
                };
                st.shop.type_.push(entry);
            });
            sedit_namelist_menu(g, conn);
            return;
        }

        // ---- Numerical responses -----------------------------------------
        SeditMode::Keeper => {
            let Some(i) = olc::parse_i32_input(g, conn, trimmed, -1) else {
                return;
            };
            if i == -1 {
                with_state(conn, |st| st.shop.keeper = NOBODY);
            } else if !mob_exists(g, i) {
                send(g, ch, "That mobile does not exist, try again : ");
                return;
            } else {
                // C sedit.c:1130-1134: the keeper mob must be in a zone the
                // builder can edit (< LVL_IMMORT) (#267).
                let level = conn_char(g, conn)
                    .and_then(|c| g.get_char(c))
                    .map(|c| c.player.level)
                    .unwrap_or(LVL_IMPL);
                if level < LVL_IMMORT && !crate::olc::can_edit_zone(g, ch, zone_rnum_of(g, i)) {
                    send(
                        g,
                        ch,
                        "You don't have permissions to that zone, try again : ",
                    );
                    return;
                }
                with_state(conn, |st| st.shop.keeper = i);
            }
        }
        SeditMode::Open1 => {
            let Some(v) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let v = v.clamp(0, 28);
            with_state(conn, |st| st.shop.open1 = v);
        }
        SeditMode::Open2 => {
            let Some(v) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let v = v.clamp(0, 28);
            with_state(conn, |st| st.shop.open2 = v);
        }
        SeditMode::Close1 => {
            let Some(v) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let v = v.clamp(0, 28);
            with_state(conn, |st| st.shop.close1 = v);
        }
        SeditMode::Close2 => {
            let Some(v) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let v = v.clamp(0, 28);
            with_state(conn, |st| st.shop.close2 = v);
        }
        SeditMode::BuyProfit => {
            // C sscanf("%f") leaves the field untouched on parse failure — a
            // bad rate must NOT become 0.00 ("everything free") (#295).
            if let Ok(v) = trimmed.parse::<f32>() {
                with_state(conn, |st| st.shop.profit_buy = v);
            }
        }
        SeditMode::SellProfit => {
            if let Ok(v) = trimmed.parse::<f32>() {
                with_state(conn, |st| st.shop.profit_sell = v);
            }
        }
        SeditMode::TypeMenu => {
            let Some(v) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let v = v.clamp(0, NUM_ITEM_TYPES as i32 - 1);
            with_state(conn, |st| {
                st.pending_type = v;
                st.mode = SeditMode::Namelist;
            });
            send(g, ch, "Enter namelist (return for none) :-\r\n] ");
            return;
        }
        SeditMode::DeleteType => {
            let Some(n) = olc::parse_i32_input(g, conn, trimmed, -1) else {
                return;
            };
            with_state(conn, |st| {
                if n >= 0 && (n as usize) < st.shop.type_.len() {
                    st.shop.type_.remove(n as usize);
                }
            });
            sedit_namelist_menu(g, conn);
            return;
        }
        SeditMode::NewProduct => {
            let Some(i) = olc::parse_i32_input(g, conn, trimmed, -1) else {
                return;
            };
            if i == -1 {
                sedit_products_menu(g, conn);
                return;
            }
            if !obj_exists(g, i) {
                send(g, ch, "That object does not exist, try again : ");
                return;
            }
            // C sedit.c:1181-1188: a builder below LVL_GRGOD may only stock
            // objects from zones they own (#267).
            let level = conn_char(g, conn)
                .and_then(|c| g.get_char(c))
                .map(|c| c.player.level)
                .unwrap_or(LVL_IMPL);
            if level < LVL_GRGOD && !crate::olc::obj_proto_in_owned_zone(g, ch, i) {
                send(
                    g,
                    ch,
                    "You don't have permissions to that zone, try again : ",
                );
                return;
            }
            with_state(conn, |st| st.shop.producing.push(i));
            sedit_products_menu(g, conn);
            return;
        }
        SeditMode::DeleteProduct => {
            let Some(n) = olc::parse_i32_input(g, conn, trimmed, -1) else {
                return;
            };
            with_state(conn, |st| {
                if n >= 0 && (n as usize) < st.shop.producing.len() {
                    st.shop.producing.remove(n as usize);
                }
            });
            sedit_products_menu(g, conn);
            return;
        }
        SeditMode::NewRoom => {
            let Some(i) = olc::parse_i32_input(g, conn, trimmed, -1) else {
                return;
            };
            if i == -1 {
                sedit_rooms_menu(g, conn);
                return;
            }
            if g.real_room(i).is_none() {
                send(g, ch, "That room does not exist, try again : ");
                return;
            }
            // C sedit.c:1199-1206: the shop room's zone must be owned
            // (< LVL_IMMORT) (#267).
            let level = conn_char(g, conn)
                .and_then(|c| g.get_char(c))
                .map(|c| c.player.level)
                .unwrap_or(LVL_IMPL);
            let owned = g
                .real_room(i)
                .and_then(|r| g.room_opt(r))
                .and_then(|room| crate::olc::real_zone(g, room.number))
                .map(|zr| crate::olc::can_edit_zone(g, ch, zr))
                .unwrap_or(false);
            if level < LVL_IMMORT && !owned {
                send(
                    g,
                    ch,
                    "You don't have permissions to that zone, try again : ",
                );
                return;
            }
            with_state(conn, |st| st.shop.in_room.push(i));
            sedit_rooms_menu(g, conn);
            return;
        }
        SeditMode::DeleteRoom => {
            let Some(n) = olc::parse_i32_input(g, conn, trimmed, -1) else {
                return;
            };
            with_state(conn, |st| {
                if n >= 0 && (n as usize) < st.shop.in_room.len() {
                    st.shop.in_room.remove(n as usize);
                }
            });
            sedit_rooms_menu(g, conn);
            return;
        }
        SeditMode::ShopFlags => {
            let Some(i) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let i = i.clamp(0, NUM_SHOP_FLAGS as i32);
            if i > 0 {
                with_state(conn, |st| st.shop.bitvector ^= 1 << (i - 1));
                sedit_shop_flags_menu(g, conn);
                return;
            }
        }
        SeditMode::NoTrade => {
            let Some(i) = olc::parse_i32_input(g, conn, trimmed, 0) else {
                return;
            };
            let i = i.clamp(0, NUM_TRADERS as i32);
            if i > 0 {
                with_state(conn, |st| st.shop.with_who ^= 1 << (i - 1));
                sedit_no_trade_menu(g, conn);
                return;
            }
        }
    }

    // END OF CASE: something changed; return to the main menu.
    with_state(conn, |st| st.changed = true);
    sedit_disp_menu(g, conn);
}

/// sedit_modify_string: ensure a leading "%s " when the text doesn't start with
/// '%', then store. Mirrors C sedit_modify_string exactly.
fn modify_string(conn: ConnId, new: &str, apply: impl FnOnce(&mut Shop, String)) {
    let value = if !new.starts_with('%') {
        format!("%s {}", new)
    } else {
        new.to_string()
    };
    with_state(conn, |st| apply(&mut st.shop, value));
}

// ---------------------------------------------------------------------------
// Internal + disk save (sedit_save_internally writes back into the zone file).
// ---------------------------------------------------------------------------

fn sedit_save_internally(g: &mut GameState, conn: ConnId) -> IoResult<()> {
    let (shop, vnum, zone) = match with_state(conn, |st| (st.shop.clone(), st.vnum, st.zone)) {
        Some(v) => v,
        None => return Ok(()),
    };
    let lib_path = g.config.lib_path.clone();

    // Load the zone's existing shops, replace/insert this one by vnum, write.
    let mut shops = load_zone_shops(&lib_path, zone);
    let mut this = shop;
    this.vnum = vnum;
    if let Some(slot) = shops.iter_mut().find(|s| s.vnum == vnum) {
        *slot = this;
    } else {
        shops.push(this);
    }
    sedit_save_to_disk(&lib_path, zone, &shops)?;
    crate::shop::upsert_shop_from_zone_file(&lib_path, zone, vnum)
}

// ---------------------------------------------------------------------------
// Cleanup.
// ---------------------------------------------------------------------------

fn cleanup(conn: ConnId) {
    take_state(conn);
    olc::clear_active(conn);
}

/// zone rnum for a mob vnum (C real_zone).
fn zone_rnum_of(g: &GameState, mob_vnum: i32) -> usize {
    crate::olc::real_zone(g, mob_vnum).unwrap_or(usize::MAX)
}

/// OLC output with C get_char_cols semantics: the &-codes in these menus are
/// stripped for builders whose colour level is below C_NRM (#306).
fn send(g: &mut GameState, ch: CharId, msg: &str) {
    if crate::olc::olc_colour_on(g, ch) {
        g.send_to_char(ch, msg);
    } else {
        g.send_to_char(ch, &crate::connection::strip_color(msg));
    }
}

fn conn_char(g: &GameState, conn: ConnId) -> Option<CharId> {
    g.descriptors.get(&conn).and_then(|d| d.character)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::object::Object;
    use crate::room::Room;
    use crate::world::Zone;

    fn temp_shop_lib(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "deltamud-sedit-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn internal_save_atomically_replaces_disk_then_refreshes_live_shop() {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!(
            "deltamud-sedit-live-{}-{}",
            std::process::id(),
            unique
        ));

        let zone = 9876;
        let vnum = 987600;
        let old_keeper = 987601;
        let new_keeper = 987602;
        let mut original = Shop::new(vnum);
        original.keeper = old_keeper;
        original.profit_buy = 1.10;
        sedit_save_to_disk(dir.to_str().unwrap(), zone, &[original]).unwrap();
        crate::shop::upsert_shop_from_zone_file(dir.to_str().unwrap(), zone, vnum).unwrap();

        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let ch = g.create_char(Character::new_player(
            "Root".into(),
            Class::Cleric,
            Race::Human,
        ));
        let conn = ConnId(9876);
        let mut edited = Shop::new(vnum);
        edited.keeper = new_keeper;
        edited.profit_buy = 1.75;
        set_state(
            conn,
            SeditState {
                ch,
                vnum,
                zone,
                shop: edited,
                changed: true,
                mode: SeditMode::ConfirmSave,
                pending_type: 0,
            },
        );

        sedit_save_internally(&mut g, conn).unwrap();

        let persisted = load_zone_shops(dir.to_str().unwrap(), zone);
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].keeper, new_keeper);
        assert_eq!(persisted[0].profit_buy, 1.75);
        assert!(!dir.join("world/shp/9876.new").exists());
        let live = crate::shop::test_shop_definition(vnum).unwrap();
        assert_eq!(live.0, new_keeper);
        assert_eq!(live.1, 1.75);
        assert!(!crate::shop::is_shop_keeper_vnum(old_keeper));
        assert!(crate::shop::is_shop_keeper_vnum(new_keeper));

        take_state(conn);
        crate::shop::test_remove_shop(vnum);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn new_shop_is_immediately_live_for_room_hours_list_buy_and_messages() {
        let dir = temp_shop_lib("new-live");
        let zone = 9877;
        let vnum = 987_700;
        let keeper_vnum = 987_701;
        let old_room_vnum = 987_710;
        let new_room_vnum = 987_711;
        let conn = ConnId(9877);

        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let old_room = g.add_room(Room::new(
            old_room_vnum,
            zone,
            "Old room".into(),
            String::new(),
        ));
        let new_room = g.add_room(Room::new(
            new_room_vnum,
            zone,
            "New shop".into(),
            String::new(),
        ));
        let mut buyer = Character::new_player("Buyer".into(), Class::Warrior, Race::Human);
        buyer.desc = Some(conn);
        let buyer = g.create_char(buyer);
        let mut keeper_character = Character::new_npc(keeper_vnum);
        keeper_character.points.hit = 100;
        keeper_character.points.max_hit = 100;
        keeper_character.position = Position::Standing;
        let keeper = g.create_char(keeper_character);
        let mut descriptor = Descriptor::new(conn, "example.test".into());
        descriptor.character = Some(buyer);
        g.descriptors.insert(conn, descriptor);
        g.char_to_room(buyer, old_room);
        g.char_to_room(keeper, old_room);

        let mut closed = Shop::new(vnum);
        closed.keeper = keeper_vnum;
        closed.in_room = vec![new_room_vnum];
        closed.open1 = 24;
        closed.close1 = 24;
        closed.open2 = 24;
        closed.close2 = 24;
        set_state(
            conn,
            SeditState {
                ch: buyer,
                vnum,
                zone,
                shop: closed,
                changed: true,
                mode: SeditMode::ConfirmSave,
                pending_type: 0,
            },
        );
        sedit_save_internally(&mut g, conn).unwrap();

        // The newly created live definition binds only the edited room.
        assert!(!crate::shop::shop_keeper(&mut g, buyer, keeper, "list", ""));
        g.char_from_room(buyer);
        g.char_from_room(keeper);
        g.char_to_room(buyer, new_room);
        g.char_to_room(keeper, new_room);
        assert_eq!(
            crate::shop::test_shop_definition(vnum).unwrap().0,
            keeper_vnum
        );
        assert_eq!(g.get_char(keeper).unwrap().nr, keeper_vnum);
        assert_eq!(g.get_char(keeper).unwrap().position, Position::Standing);
        assert_eq!(g.get_char(buyer).unwrap().in_room, Some(new_room));
        assert_eq!(g.room(new_room).number, new_room_vnum);
        assert!(crate::shop::shop_keeper(&mut g, buyer, keeper, "list", ""));
        assert!(g.descriptors[&conn].outbuf.contains("Come back later!"));

        let mut open = Shop::new(vnum);
        open.keeper = keeper_vnum;
        open.in_room = vec![new_room_vnum];
        open.open1 = 0;
        open.close1 = 28;
        open.open2 = 0;
        open.close2 = 28;
        open.no_such_item1 = "%s LIVE NO STOCK.".into();
        open.message_buy = "%s LIVE PURCHASE %d.".into();
        set_state(
            conn,
            SeditState {
                ch: buyer,
                vnum,
                zone,
                shop: open,
                changed: true,
                mode: SeditMode::ConfirmSave,
                pending_type: 0,
            },
        );
        sedit_save_internally(&mut g, conn).unwrap();

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        assert!(crate::shop::shop_keeper(
            &mut g, buyer, keeper, "buy", "missing"
        ));
        assert!(g.descriptors[&conn].outbuf.contains("LIVE NO STOCK."));

        let mut widget = Object::new(987_799, "fresh widget".into(), "a fresh widget".into());
        widget.cost = 50;
        let widget = g.create_obj(widget);
        g.obj_to_char(widget, keeper);
        crate::gold::set(
            g.get_char_mut(buyer).unwrap(),
            crate::gold::Account::Carried,
            500,
        );

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        assert!(crate::shop::shop_keeper(&mut g, buyer, keeper, "list", ""));
        assert!(
            g.descriptors[&conn]
                .outbuf
                .to_ascii_lowercase()
                .contains("a fresh widget"),
            "live list output: {:?}",
            g.descriptors[&conn].outbuf
        );
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        assert!(crate::shop::shop_keeper(
            &mut g, buyer, keeper, "buy", "widget"
        ));
        assert!(g.get_char(buyer).unwrap().carrying.contains(&widget));
        assert!(g.descriptors[&conn].outbuf.contains("LIVE PURCHASE 50."));

        take_state(conn);
        crate::shop::test_remove_shop(vnum);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_shop_disk_write_does_not_publish_or_report_a_live_edit() {
        let dir = temp_shop_lib("disk-failure");
        let zone = 9878;
        let vnum = 987_800;
        let conn = ConnId(9878);
        let mut original = Shop::new(vnum);
        original.keeper = 987_801;
        original.profit_buy = 1.25;
        sedit_save_to_disk(dir.to_str().unwrap(), zone, &[original]).unwrap();
        crate::shop::upsert_shop_from_zone_file(dir.to_str().unwrap(), zone, vnum).unwrap();

        // Force the atomic temporary-file write itself to fail while leaving
        // the prior durable zone file intact.
        std::fs::create_dir(dir.join(format!("world/shp/{zone}.new"))).unwrap();
        let mut config = Config::default();
        config.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(config);
        let ch = g.create_char(Character::new_player(
            "Root".into(),
            Class::Cleric,
            Race::Human,
        ));
        let mut edited = Shop::new(vnum);
        edited.keeper = 987_802;
        edited.profit_buy = 9.75;
        set_state(
            conn,
            SeditState {
                ch,
                vnum,
                zone,
                shop: edited,
                changed: true,
                mode: SeditMode::ConfirmSave,
                pending_type: 0,
            },
        );

        assert!(sedit_save_internally(&mut g, conn).is_err());
        let persisted = load_zone_shops(dir.to_str().unwrap(), zone);
        assert_eq!(persisted[0].keeper, 987_801);
        assert_eq!(persisted[0].profit_buy, 1.25);
        let live = crate::shop::test_shop_definition(vnum).unwrap();
        assert_eq!(live.0, 987_801);
        assert_eq!(live.1, 1.25);

        take_state(conn);
        crate::shop::test_remove_shop(vnum);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn profit_parse_keeps_previous_value_on_garbage() {
        let mut g = GameState::new(Config::default());
        g.zones.push(Zone {
            number: 1,
            name: "Zone 1".to_string(),
            builders: "Root".to_string(),
            lifespan: 30,
            age: 0,
            top: 199,
            reset_mode: 2,
            min_level: 0,
            max_level: 60,
            status_mode: 0,
            map_x: None,
            map_y: None,
            reset_commands: Vec::new(),
        });
        let mut ch = Character::new_player("Root".into(), Class::Cleric, Race::Human);
        ch.player.level = LVL_IMPL;
        let ch = g.create_char(ch);
        let conn = ConnId(81);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        do_sedit(&mut g, ch, "150", 0);
        sedit_parse(&mut g, conn, "6"); // sell rate
        sedit_parse(&mut g, conn, "1.25");
        assert_eq!(
            with_state(conn, |st| st.shop.profit_sell).unwrap(),
            1.25,
            "a valid rate is stored"
        );
        // C sscanf("%f") leaves the field untouched on failure (#295).
        sedit_parse(&mut g, conn, "garbage");
        assert_eq!(
            with_state(conn, |st| st.shop.profit_sell).unwrap(),
            1.25,
            "garbage must not zero the rate"
        );
    }
}
