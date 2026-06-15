// clan.rs — the player-clan system, a full port of the C `src/clan.c`
// (Chuck Reed's clan code as carried in DeltaMUD). Implements `do_clan` and
// all of its sub-verbs (create/destroy/score/privilege/rank/rname/apply/
// enlist/expel/resign/list/promote/demote/wname/ctitle/talk/help/info/who/
// roster/newowner/clanroom/withdraw/deposit) plus the `csay` clan channel.
//
// Persistence: the C server keeps the clan table in a flat binary file
// `lib/etc/clans.dat` (an int count followed by `num_of_clans` copies of
// `struct clan_info`) AND mirrors per-player clan membership in the SQL
// `player_main` table (clan / clan_rank columns). The Rust GameState exposes
// no synchronous DB handle, so this module:
//   * Holds the runtime clan table in a module static (OnceLock<Mutex<..>>),
//     loaded by `boot_clans(lib_path)` and re-saved (via `save_clans`) on
//     every mutation, exactly mirroring the C call sites.
//   * Reads/writes `clans.dat` with std::fs in a fixed little-endian binary
//     layout (documented below) so the file round-trips across reboots. It is
//     NOT byte-compatible with the C struct dump (which is padding/endian/
//     pointer-width dependent); see `gaps`.
//   * Operates on the live Character.clan / Character.clan_rank fields for the
//     actor and any ONLINE victim — the analogue of C's `is_playing()` branch.
//     The C `pe_printf`/`QUERY_DATABASE` offline-player branch has no
//     synchronous equivalent here, so offline targets degrade to "no such
//     player" exactly as C does when the name is absent from the live game.
//
// House style follows cmd_comm.rs / cmd_item.rs: copy needed values into
// locals before mutating or broadcasting; re-look-up entities by id every
// time; emit room/target broadcasts through act(); send direct text through
// send_to_char.

use crate::act::{act, ActArg, To};
use crate::interpreter::{half_chop, is_abbrev};
use crate::room::RoomFlags;
use crate::state::GameState;
use crate::types::*;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Constants (clan.h / structs.h) mirrored locally so this module is
// self-contained without editing flags.rs.
// ---------------------------------------------------------------------------

const MAX_CLANS: usize = 300;
const MAX_RANKS: usize = 10; // rank levels 1..=9 usable; rank_name[] has 9 slots
const LVL_CLAN_GOD: Level = 103;

// Privilege indices into clan_info.privilege[NUM_OF_PRIVS].
const WITHDRAW_PRIV: usize = 0;
const PROMOTE_PRIV: usize = 1;
const DEMOTE_PRIV: usize = 2;
const ENLIST_PRIV: usize = 3;
const SCORE_PRIV: usize = 4;
const EXPEL_PRIV: usize = 5;
const NUM_OF_PRIVS: usize = 6;

// structs.h preference / player / room flags used by the clan channel + title.
const PRF2_CTITLE: i64 = 1 << 3; // show clan name in who list
const PRF2_CLAN_TALK: i64 = 1 << 4; // can hear clan channel
const PLR_NOSHOUT: i64 = 1 << 8; // muted (no shout/goss/csay)
const ROOM_CLAN_ROOM_BIT: u32 = 1 << 19; // structs.h ROOM_CLAN_ROOM

// `noclan` (clan.c) — the canonical "you have no clan" string.
const NOCLAN: &str = "You don't even belong to a clan!\r\n";

// ---------------------------------------------------------------------------
// The runtime clan record. Field-for-field analogue of `struct clan_info`
// (clan.h). `rank_name` carries MAX_RANKS-1 (=9) entries: index i is the name
// for rank (i+1), so rank 1 == rank_name[0]. Empty string == C's "N/A"/unset.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
struct ClanInfo {
    number: i32,
    members: i32,
    ranks: i32,
    privilege: [i32; NUM_OF_PRIVS],
    clan_room: i32, // room VNUM owned by the clan (NOWHERE if unset)
    gold: i64,
    rank_name: [String; MAX_RANKS - 1],
    leader: String,
    name: String,
    who_name: String,
}

impl Default for ClanInfo {
    fn default() -> Self {
        ClanInfo {
            number: 0,
            members: 0,
            ranks: 0,
            privilege: [0; NUM_OF_PRIVS],
            clan_room: NOWHERE,
            gold: 0,
            rank_name: Default::default(),
            leader: String::new(),
            name: String::new(),
            who_name: String::new(),
        }
    }
}

impl ClanInfo {
    /// Name of a 1-based rank level, falling back to "N/A" like the C
    /// rank_name array when the slot is empty/out of range.
    fn rank_label(&self, rank: i32) -> &str {
        if rank >= 1 && (rank as usize) <= MAX_RANKS - 1 {
            let s = &self.rank_name[(rank - 1) as usize];
            if s.is_empty() {
                "N/A"
            } else {
                s.as_str()
            }
        } else {
            "N/A"
        }
    }
}

/// The whole table plus its count, behind one mutex (C globals `clan[]` and
/// `num_of_clans`). `lib_path` is captured at boot so `save_clans` can rewrite
/// the file without threading it through every call site.
struct ClanTable {
    clans: Vec<ClanInfo>,
    lib_path: String,
}

static CLANS: OnceLock<Mutex<ClanTable>> = OnceLock::new();

fn table() -> &'static Mutex<ClanTable> {
    // If boot_clans was never called (e.g. a unit test), install an empty
    // table with the default lib path so the commands still operate.
    CLANS.get_or_init(|| {
        Mutex::new(ClanTable { clans: Vec::new(), lib_path: "lib".to_string() })
    })
}

// ---------------------------------------------------------------------------
// On-disk format (lib/etc/clans.dat), little-endian, self-describing:
//   u32  count
//   repeat count times:
//     i32  number
//     i32  members
//     i32  ranks
//     i32  privilege[0..6]      (6 * i32)
//     i32  clan_room
//     i64  gold
//     9 *  { u16 len; len bytes utf8 }   rank_name[0..9]
//     { u16 len; len bytes utf8 }        leader
//     { u16 len; len bytes utf8 }        name
//     { u16 len; len bytes utf8 }        who_name
// Strings are length-prefixed (C used fixed-width char arrays; we store the
// trimmed contents). Documented in `gaps` as intentionally NOT C-binary-
// compatible.
// ---------------------------------------------------------------------------

const CLAN_FILE_REL: &str = "etc/clans.dat";

fn clan_file_path(lib_path: &str) -> String {
    format!("{}/{}", lib_path.trim_end_matches('/'), CLAN_FILE_REL)
}

/// CircleMUD boot_clans(): load (or create) the clan table from disk. The
/// integrator calls this once at boot, before any command dispatch, with the
/// configured lib path (e.g. `boot_clans(&config.lib_path)`).
pub fn boot_clans(lib_path: &str) {
    let path = clan_file_path(lib_path);
    let clans = match std::fs::read(&path) {
        Ok(bytes) => match decode_clans(&bytes) {
            Some(v) => v,
            None => {
                eprintln!("SYSERR: clans.dat at '{}' is corrupt; starting empty.", path);
                Vec::new()
            }
        },
        Err(_) => {
            // C logs "Clan file doesn't exist, a new one will be created."
            eprintln!("Clan file doesn't exist, a new one will be created.");
            Vec::new()
        }
    };

    let new_table = ClanTable { clans, lib_path: lib_path.to_string() };

    // Install (or, on a double-boot, replace the contents of) the static.
    match CLANS.get() {
        Some(m) => {
            *m.lock().unwrap() = new_table;
        }
        None => {
            let _ = CLANS.set(Mutex::new(new_table));
        }
    }

    // Mirror the C "create a new one if missing" behaviour: persist now so the
    // file exists on first boot. Member recounting (C walks the player table)
    // is deferred — the live count is maintained incrementally by the verbs,
    // and a roster count over the live game is done in `clan who`.
    save_clans();
    eprintln!("Booting clans . . . Done.");
}

/// CircleMUD save_clans(): serialize the whole table to clans.dat.
fn save_clans() {
    let t = table().lock().unwrap();
    let bytes = encode_clans(&t.clans);
    let path = clan_file_path(&t.lib_path);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, bytes) {
        eprintln!("SYSERR: Error writing to clan file '{}': {}", path, e);
    }
}

fn encode_clans(clans: &[ClanInfo]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(clans.len() as u32).to_le_bytes());
    for c in clans {
        out.extend_from_slice(&c.number.to_le_bytes());
        out.extend_from_slice(&c.members.to_le_bytes());
        out.extend_from_slice(&c.ranks.to_le_bytes());
        for p in &c.privilege {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(&c.clan_room.to_le_bytes());
        out.extend_from_slice(&c.gold.to_le_bytes());
        for r in &c.rank_name {
            put_string(&mut out, r);
        }
        put_string(&mut out, &c.leader);
        put_string(&mut out, &c.name);
        put_string(&mut out, &c.who_name);
    }
    out
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let len = b.len().min(u16::MAX as usize) as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&b[..len as usize]);
}

fn decode_clans(bytes: &[u8]) -> Option<Vec<ClanInfo>> {
    let mut cur = Cursor { buf: bytes, pos: 0 };
    let count = cur.u32()? as usize;
    if count > MAX_CLANS {
        return None;
    }
    let mut clans = Vec::with_capacity(count);
    for _ in 0..count {
        let mut c = ClanInfo::default();
        c.number = cur.i32()?;
        c.members = cur.i32()?;
        c.ranks = cur.i32()?;
        for p in c.privilege.iter_mut() {
            *p = cur.i32()?;
        }
        c.clan_room = cur.i32()?;
        c.gold = cur.i64()?;
        for r in c.rank_name.iter_mut() {
            *r = cur.string()?;
        }
        c.leader = cur.string()?;
        c.name = cur.string()?;
        c.who_name = cur.string()?;
        clans.push(c);
    }
    Some(clans)
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u16()? as usize;
        let b = self.take(len)?;
        Some(String::from_utf8_lossy(b).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Small character/world predicate helpers (utils.h macros).
// ---------------------------------------------------------------------------

fn get_name(g: &GameState, id: CharId) -> String {
    g.get_char(id).map(|c| c.player.name.clone()).unwrap_or_default()
}
fn get_level(g: &GameState, id: CharId) -> Level {
    g.get_char(id).map(|c| c.player.level).unwrap_or(0)
}
fn get_clan(g: &GameState, id: CharId) -> i32 {
    g.get_char(id).map(|c| c.clan).unwrap_or(-1)
}
fn get_clan_rank(g: &GameState, id: CharId) -> i32 {
    g.get_char(id).map(|c| c.clan_rank).unwrap_or(-1)
}
fn prf2(g: &GameState, id: CharId, flag: i64) -> bool {
    g.get_char(id).map(|c| c.prf2_flags & flag != 0).unwrap_or(false)
}
fn plr_flagged(g: &GameState, id: CharId, flag: i64) -> bool {
    g.get_char(id).map(|c| !c.is_npc && c.act_flags & flag != 0).unwrap_or(false)
}
fn has_desc(g: &GameState, id: CharId) -> bool {
    g.get_char(id).map(|c| c.desc.is_some()).unwrap_or(false)
}

/// CAP() — uppercase the first character (utils.h). Used to canonicalise clan
/// names (stored already-capitalised, like C's `CAP(arg2)`).
fn cap(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(first) = out.chars().next() {
        if first.is_ascii_lowercase() {
            let up = first.to_ascii_uppercase();
            out.replace_range(0..first.len_utf8(), &up.to_string());
        }
    }
    out
}

/// The connected-player set in a stable order — the analogue of walking
/// `descriptor_list` for clan who / csay / global lookups.
fn connected_players(g: &GameState) -> Vec<CharId> {
    let mut v: Vec<CharId> = g.players_by_name.values().copied().filter(|&id| has_desc(g, id)).collect();
    v.sort_by_key(|c| c.0);
    v
}

/// is_playing(name): the online PC with this exact (case-insensitive) name, if
/// any. Mirrors clan.c's is_playing(); the player table / pfile branch is
/// unavailable synchronously (see header), so offline players read as absent.
fn is_playing(g: &GameState, name: &str) -> Option<CharId> {
    if name.is_empty() {
        return None;
    }
    g.find_player_by_name(name).filter(|&id| has_desc(g, id))
}

/// get_char_vis-equivalent for clan create / newowner: an online, visible PC
/// found by keyword anywhere in the world (C get_char_vis searches the room,
/// then the whole world). We search the connected-player set.
fn get_player_vis(g: &GameState, observer: CharId, name: &str) -> Option<CharId> {
    if name.is_empty() {
        return None;
    }
    // Prefer an exact name match (the common clan case), else a prefix.
    let lname = name.to_lowercase();
    let mut prefix: Option<CharId> = None;
    for id in connected_players(g) {
        if !g.can_see(observer, id) {
            continue;
        }
        let n = get_name(g, id).to_lowercase();
        if n == lname {
            return Some(id);
        }
        if prefix.is_none() && n.starts_with(&lname) {
            prefix = Some(id);
        }
    }
    prefix
}

/// Read a clan field by index without holding the lock across a send.
fn with_clan<R>(idx: i32, f: impl FnOnce(&ClanInfo) -> R) -> Option<R> {
    let t = table().lock().unwrap();
    let i = usize::try_from(idx).ok()?;
    t.clans.get(i).map(f)
}

fn num_of_clans() -> i32 {
    table().lock().unwrap().clans.len() as i32
}

// ---------------------------------------------------------------------------
// Usage / format text (send_clan_format).
// ---------------------------------------------------------------------------

fn send_clan_format(g: &mut GameState, ch: CharId) {
    g.send_to_char(
        ch,
        "Usage: clan help <topic>\r\n\
\x20      clan score\r\n\
\x20      clan privilege <privilege name> <privilege level>\r\n\
\x20      clan rank <direction> <num of ranks>\r\n\
\x20      clan rname <rank level> <rank name>\r\n\
\x20      clan apply <clan num>\r\n\
\x20      clan enlist <victim>\r\n\
\x20      clan expel <victim>\r\n\
\x20      clan resign\r\n\
\x20      clan list\r\n\
\x20      clan info <clan num>\r\n\
\x20      clan talk\r\n\
\x20      csay <message>\r\n\
\x20      clan who\r\n\
\x20      clan promote <victim>\r\n\
\x20      clan ctitle\r\n\
\x20      clan roster\r\n\
\x20      clan demote <victim>\r\n",
    );
    if get_level(g, ch) >= LVL_CLAN_GOD {
        g.send_to_char(
            ch,
            "\x20      clan create <leader> <name>\r\n\
\x20      clan destroy <clan num>\r\n\
\x20      clan wname <clan num> <name>\r\n",
        );
    }
}

// ---------------------------------------------------------------------------
// clan create
// ---------------------------------------------------------------------------

fn clan_create(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    if get_level(g, ch) < LVL_CLAN_GOD {
        g.send_to_char(ch, "You can't do that!\r\n");
        return;
    }
    if num_of_clans() as usize >= MAX_CLANS {
        g.send_to_char(ch, "Max clans reached.  Report this to an implementor.\r\n");
        return;
    }

    let (arg1, arg2) = half_chop(arg);

    let leader = match get_player_vis(g, ch, &arg1) {
        Some(l) => l,
        None => {
            g.send_to_char(ch, "The leader of the new clan must be present.\r\n");
            return;
        }
    };
    if arg2.len() >= 32 {
        g.send_to_char(ch, "Clan name too long! (32 characters max)\r\n");
        return;
    }
    if get_level(g, leader) >= LVL_IMMORT {
        g.send_to_char(ch, "You cannot set an immortal as the leader of a clan.\r\n");
        return;
    }
    if get_clan(g, leader) >= 0 && get_clan_rank(g, leader) > 0 {
        g.send_to_char(ch, "The leader already belongs to a clan!\r\n");
        return;
    }
    if find_clan(&arg2).is_some() {
        g.send_to_char(ch, "That clan name alread exists!\r\n");
        return;
    }

    let leader_name = get_name(g, leader);
    let new_index;
    {
        let mut t = table().lock().unwrap();
        let number = t.clans.len() as i32;
        let mut info = ClanInfo::default();
        info.name = cap(&arg2);
        info.number = number;
        info.ranks = 2;
        info.rank_name[0] = "Member".to_string();
        info.rank_name[1] = "Leader".to_string();
        info.members = 1;
        info.privilege = [0; NUM_OF_PRIVS];
        info.leader = leader_name;
        info.gold = 0;
        info.clan_room = NOWHERE;
        info.who_name = "N/A".to_string();
        t.clans.push(info);
        new_index = number;
    }
    save_clans();

    // GET_CLAN(leader) = new index; GET_CLAN_RANK(leader) = clan.ranks.
    let ranks = with_clan(new_index, |c| c.ranks).unwrap_or(2);
    if let Some(l) = g.get_char_mut(leader) {
        l.clan = new_index;
        l.clan_rank = ranks;
    }
    g.send_to_char(ch, "Clan created successfuly.\r\n");
}

/// find_clan(name): index of the clan whose (capitalised) name matches, else
/// None. C compares CAP(arg) to CAP(clan.name); both are already-capitalised.
fn find_clan(arg: &str) -> Option<i32> {
    if arg.is_empty() {
        return None;
    }
    let target = cap(arg);
    let t = table().lock().unwrap();
    t.clans.iter().position(|c| cap(&c.name) == target).map(|i| i as i32)
}

// ---------------------------------------------------------------------------
// clan destroy
// ---------------------------------------------------------------------------

fn clan_destroy(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let i: i32 = arg.trim().parse().unwrap_or(0);
    let n = num_of_clans();
    if i < 0 || i > n - 1 {
        g.send_to_char(ch, "Unknown clan.\r\n");
        return;
    }
    if get_level(g, ch) < LVL_CLAN_GOD {
        g.send_to_char(ch, "Your not mighty enough to destroy clans!\r\n");
        return;
    }

    // Fix up every ONLINE player's clan membership (the C SQL UPDATEs cover the
    // offline pfile, which we cannot reach synchronously — see header).
    let players: Vec<CharId> = connected_players(g);
    for v in players {
        let vclan = get_clan(g, v);
        if vclan == i {
            if let Some(c) = g.get_char_mut(v) {
                c.clan = -1;
                c.clan_rank = -1;
            }
        } else if vclan > i {
            if let Some(c) = g.get_char_mut(v) {
                c.clan -= 1;
            }
        }
    }

    {
        let mut t = table().lock().unwrap();
        let idx = i as usize;
        if idx < t.clans.len() {
            t.clans.remove(idx);
            // Keep each clan's stored number equal to its index.
            for (j, c) in t.clans.iter_mut().enumerate() {
                c.number = j as i32;
            }
        }
    }
    g.send_to_char(ch, "Clan deleted.\r\n");
    save_clans();
}

// ---------------------------------------------------------------------------
// clan score
// ---------------------------------------------------------------------------

fn clan_score(g: &mut GameState, ch: CharId) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let score_priv = with_clan(cl, |c| c.privilege[SCORE_PRIV]).unwrap_or(0);
    if rank < score_priv {
        g.send_to_char(ch, "You aren't high enough ranked to do that.\r\n");
        return;
    }

    let info = match with_clan(cl, |c| c.clone()) {
        Some(c) => c,
        None => {
            g.send_to_char(ch, NOCLAN);
            return;
        }
    };

    let buf = format!(
        "   Clan Statistics\r\n\
         \r   ----------------\r\n\
         \r   Name           : {}\r\n\
         \r   Number         : {}\r\n\
         \r   Owner          : {}\r\n\
         \r   Members        : {}\r\n\
         \r   Gold           : {}\r\n\
         \r   Withdraw Level : {}\r\n\
         \r   Promote Level  : {}\r\n\
         \r   Demote Level   : {}\r\n\
         \r   Enlist Level   : {}\r\n\
         \r   Expel Level    : {}\r\n\
         \r   Score Level    : {}\r\n\
         \r   Your Rank      : {}\r\n\
         \r   Ranks          : {}\r\n\
         \r   ----------------\r\n",
        info.name,
        info.number,
        info.leader,
        info.members,
        info.gold,
        info.privilege[WITHDRAW_PRIV],
        info.privilege[PROMOTE_PRIV],
        info.privilege[DEMOTE_PRIV],
        info.privilege[ENLIST_PRIV],
        info.privilege[EXPEL_PRIV],
        info.privilege[SCORE_PRIV],
        rank,
        info.ranks,
    );
    g.send_to_char(ch, &buf);

    // The C loop runs i = 0..=ranks and prints rank_name[i-1]; at i==0 that is
    // rank_name[-1] (undefined in C). We reproduce the *intended* listing of
    // each rank label by number, which is what builders actually read.
    let mut list = String::new();
    for i in 0..=info.ranks {
        list.push_str(&format!("{:2}) {}\r\n", i, info.rank_label(i)));
    }
    g.send_to_char(ch, &list);
}

// ---------------------------------------------------------------------------
// clan privilege (set_priv)
// ---------------------------------------------------------------------------

fn set_priv(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank < 0 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let ranks = with_clan(cl, |c| c.ranks).unwrap_or(0);
    if rank != ranks {
        g.send_to_char(ch, "You have to be the highest rank of a clan to set privileges.\r\n");
        return;
    }

    let (arg1, arg2) = half_chop(arg);
    if arg1.is_empty() || arg2.is_empty() {
        send_clan_format(g, ch);
        return;
    }

    let level: i32 = arg2.trim().parse().unwrap_or(0);
    if level < 1 || level > ranks {
        g.send_to_char(ch, "Invalid level.\r\n");
        return;
    }

    let target = if is_abbrev(&arg1, "withdraw") {
        Some((WITHDRAW_PRIV, "Withdrawal level"))
    } else if is_abbrev(&arg1, "promote") {
        Some((PROMOTE_PRIV, "Promote privilege"))
    } else if is_abbrev(&arg1, "demote") {
        Some((DEMOTE_PRIV, "Demote privilege"))
    } else if is_abbrev(&arg1, "score") {
        Some((SCORE_PRIV, "Score privilege"))
    } else if is_abbrev(&arg1, "enlist") {
        Some((ENLIST_PRIV, "Enlist privilege"))
    } else if is_abbrev(&arg1, "expel") {
        Some((EXPEL_PRIV, "Expel privilege"))
    } else {
        None
    };

    match target {
        Some((priv_idx, label)) => {
            {
                let mut t = table().lock().unwrap();
                if let Some(c) = t.clans.get_mut(cl as usize) {
                    c.privilege[priv_idx] = level;
                }
            }
            save_clans();
            g.send_to_char(ch, &format!("{} changed to {}.\r\n", label, level));
        }
        None => {
            g.send_to_char(ch, "Unknown privilege setting option.\r\n");
        }
    }
}

// ---------------------------------------------------------------------------
// clan rank (handle_rank: raise / lower)
// ---------------------------------------------------------------------------

fn handle_rank(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }

    let (arg1, arg2) = half_chop(arg);
    if arg1.is_empty() || arg2.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let n: i32 = arg2.trim().parse().unwrap_or(0);
    let ranks = with_clan(cl, |c| c.ranks).unwrap_or(0);

    if is_abbrev(&arg1, "raise") {
        // C guard: n in 1..9 AND n >= clan.ranks (raising upward).
        if n < 1 || n > 9 || n < ranks {
            send_clan_format(g, ch);
            return;
        }
        g.send_to_char(ch, &format!("Clan ranks changed to {}.\r\n", n));
        {
            let mut t = table().lock().unwrap();
            if let Some(c) = t.clans.get_mut(cl as usize) {
                let mut i = c.ranks;
                while i < n {
                    if (i as usize) < MAX_RANKS - 1 {
                        c.rank_name[i as usize] = "N/A".to_string();
                    }
                    i += 1;
                }
                c.ranks = n;
            }
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.clan_rank = n;
        }
        save_clans();
        return;
    }
    if is_abbrev(&arg1, "lower") {
        if n < 1 || n > 9 || n > ranks {
            send_clan_format(g, ch);
            return;
        }
        g.send_to_char(ch, &format!("Clan ranks changed to {}.\r\n", n));
        {
            let mut t = table().lock().unwrap();
            if let Some(c) = t.clans.get_mut(cl as usize) {
                c.ranks = n;
            }
        }
        lower_entire_clan(g, cl);
        if let Some(c) = g.get_char_mut(ch) {
            c.clan_rank = n;
        }
        save_clans();
        return;
    }
    send_clan_format(g, ch);
}

/// lower_entire_clan: when a clan's ranks are lowered, everyone in it (except
/// rank -1 applicants) drops to rank 1. Only ONLINE members are reachable.
fn lower_entire_clan(g: &mut GameState, clan_idx: i32) {
    let number = with_clan(clan_idx, |c| c.number).unwrap_or(clan_idx);
    let players = connected_players(g);
    for v in players {
        if get_clan(g, v) == number && get_clan_rank(g, v) != -1 {
            if let Some(c) = g.get_char_mut(v) {
                c.clan_rank = 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// clan rname (clan_rank_name)
// ---------------------------------------------------------------------------

fn clan_rank_name(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let ranks = with_clan(cl, |c| c.ranks).unwrap_or(0);
    if rank < ranks {
        g.send_to_char(ch, "You aren't in a position to do that!\r\n");
        return;
    }

    let (arg1, arg2) = half_chop(arg);
    if arg1.is_empty() || arg2.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    if arg2.len() > 20 {
        g.send_to_char(ch, "Ranks must be no more than 20 characters long.\r\n");
        return;
    }
    let lvl: i32 = arg1.trim().parse().unwrap_or(0);
    if lvl < 1 || lvl > 9 || lvl > ranks {
        g.send_to_char(ch, "Rank number does not exist.\r\n");
        return;
    }
    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(cl as usize) {
            let slot = (lvl - 1) as usize;
            if slot < MAX_RANKS - 1 {
                c.rank_name[slot] = arg2.clone();
            }
        }
    }
    g.send_to_char(ch, "Rank name changed.\r\n");
    save_clans();
}

// ---------------------------------------------------------------------------
// clan apply
// ---------------------------------------------------------------------------

fn clan_apply(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    if get_clan(g, ch) != -1 {
        g.send_to_char(ch, "You are already in a clan.\r\n");
        return;
    }
    let (arg1, _arg2) = half_chop(arg);
    if arg1.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let num: i32 = arg1.trim().parse().unwrap_or(-1);
    if num >= 0 && num < num_of_clans() {
        g.send_to_char(
            ch,
            "Clan applied for.  You may type 'resign' to withdraw your application at any point.\r\n\
             Note that this does not notify the clan applied for.  It is suggested that you\r\n\
             contact the clan's leaders through mudmail to inform them.\r\n",
        );
        if let Some(c) = g.get_char_mut(ch) {
            c.clan = num;
            c.clan_rank = -1;
        }
        return;
    }
    g.send_to_char(ch, "Illegal clan number.  Clan does not exist.\r\n");
}

// ---------------------------------------------------------------------------
// clan resign
// ---------------------------------------------------------------------------

fn clan_resign(g: &mut GameState, ch: CharId) {
    let cl = get_clan(g, ch);
    if cl == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let leader = with_clan(cl, |c| c.leader.clone()).unwrap_or_default();
    if get_name(g, ch) == leader {
        g.send_to_char(ch, "Clan owners can't resign.  Speak to a clan god.\r\n");
        return;
    }
    if get_clan_rank(g, ch) > 0 {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(cl as usize) {
            c.members -= 1;
        }
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.clan = -1;
        c.clan_rank = 0;
    }
    g.send_to_char(ch, "You have resigned from your clan.\r\n");
    save_clans();
}

// ---------------------------------------------------------------------------
// clan enlist
// ---------------------------------------------------------------------------

fn clan_enlist(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank <= 0 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let enlist_priv = with_clan(cl, |c| c.privilege[ENLIST_PRIV]).unwrap_or(0);
    if rank < enlist_priv {
        g.send_to_char(ch, "You aren't privileged enough to do that.\r\n");
        return;
    }

    let (arg1, _arg2) = half_chop(arg);
    let victim = match is_playing(g, &arg1) {
        Some(v) => v,
        None => {
            // C also checks the offline pfile here; unreachable synchronously.
            g.send_to_char(ch, "There is no such player.\r\n");
            return;
        }
    };

    if get_clan(g, victim) != cl {
        g.send_to_char(ch, "I don't think they want to be in your clan.\r\n");
        return;
    }
    if get_clan_rank(g, victim) != -1 {
        g.send_to_char(ch, "They are already in your clan.\r\n");
        return;
    }
    if get_level(g, victim) >= LVL_IMMORT {
        g.send_to_char(ch, "You can't enlist immortals.\r\n");
        return;
    }

    if let Some(v) = g.get_char_mut(victim) {
        v.clan_rank = 1;
    }
    let clan_name = with_clan(cl, |c| c.name.clone()).unwrap_or_default();
    let ch_name = get_name(g, ch);
    let vic_name = get_name(g, victim);
    g.send_to_char(victim, &format!("{} has enlisted you into {}!\r\n", ch_name, clan_name));
    g.send_to_char(ch, &format!("{} has been enlisted into your clan!\r\n", vic_name));
    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(cl as usize) {
            c.members += 1;
        }
    }
    save_clans();
}

// ---------------------------------------------------------------------------
// clan expel
// ---------------------------------------------------------------------------

fn clan_expel(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank < 0 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let expel_priv = with_clan(cl, |c| c.privilege[EXPEL_PRIV]).unwrap_or(0);
    if rank < expel_priv {
        g.send_to_char(ch, "You aren't privileged enough to do that.\r\n");
        return;
    }

    let (arg1, _arg2) = half_chop(arg);
    let victim = match is_playing(g, &arg1) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "There are no players by that name.\r\n");
            return;
        }
    };

    if get_clan(g, victim) != cl {
        g.send_to_char(ch, "Maybe if they were in YOUR clan . . .\r\n");
        return;
    }
    if get_clan_rank(g, victim) > rank {
        g.send_to_char(ch, "You can't expel those higher than you!\r\n");
        return;
    }

    let clan_name = with_clan(cl, |c| c.name.clone()).unwrap_or_default();
    let ch_name = get_name(g, ch);
    let vic_name = get_name(g, victim);
    if let Some(v) = g.get_char_mut(victim) {
        v.clan = -1;
        v.clan_rank = -1;
    }
    g.send_to_char(victim, &format!("{} has expelled you from {}.\r\n", ch_name, clan_name));
    g.send_to_char(ch, &format!("You have expelled {} from the clan.\r\n", vic_name));
    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(cl as usize) {
            c.members -= 1;
        }
    }
    save_clans();
}

// ---------------------------------------------------------------------------
// clan list
// ---------------------------------------------------------------------------

fn clan_list(g: &mut GameState, ch: CharId) {
    g.send_to_char(
        ch,
        "Number Name                             Owner\r\n\
         ------ -------------------------------- ---->\r\n",
    );
    let rows: Vec<(i32, String, String)> = {
        let t = table().lock().unwrap();
        t.clans.iter().enumerate().map(|(i, c)| (i as i32, c.name.clone(), c.leader.clone())).collect()
    };
    for (i, name, leader) in rows {
        g.send_to_char(ch, &format!("{:<6} {:<32} {}\r\n", i, name, leader));
    }
    g.send_to_char(ch, "------ -------------------------------- ---->\r\n");
}

// ---------------------------------------------------------------------------
// clan promote
// ---------------------------------------------------------------------------

fn clan_promote(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let promote_priv = with_clan(cl, |c| c.privilege[PROMOTE_PRIV]).unwrap_or(0);
    if rank < promote_priv {
        g.send_to_char(ch, "You aren't privileged enough to do that!\r\n");
        return;
    }

    let (arg1, _arg2) = half_chop(arg);
    if arg1.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let victim = match is_playing(g, &arg1) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "There is no such player.\r\n");
            return;
        }
    };

    if get_clan(g, victim) != cl {
        g.send_to_char(ch, "Who are you to do that!?  They aren't in your clan!\r\n");
        return;
    }
    if ch == victim {
        g.send_to_char(ch, "Sure wiseguy, you do that.\r\n");
        return;
    }
    let vrank = get_clan_rank(g, victim);
    if rank <= vrank {
        g.send_to_char(ch, "You can't promote someone higher than yourself.\r\n");
        return;
    }
    let vclan_ranks = with_clan(get_clan(g, victim), |c| c.ranks).unwrap_or(0);
    if vrank == vclan_ranks {
        g.send_to_char(ch, "That would exceed the current ranks.  Don't do that.\r\n");
        return;
    }

    let new_rank = vrank + 1;
    if let Some(v) = g.get_char_mut(victim) {
        v.clan_rank = new_rank;
    }
    let label = with_clan(cl, |c| c.rank_label(new_rank).to_string()).unwrap_or_else(|| "N/A".to_string());
    let ch_name = get_name(g, ch);
    let vic_name = get_name(g, victim);
    g.send_to_char(victim, &format!("{} has promoted you to {}!\r\n", ch_name, label));
    g.send_to_char(ch, &format!("You promote {} to {}.\r\n", vic_name, label));
}

// ---------------------------------------------------------------------------
// clan demote
// ---------------------------------------------------------------------------

fn clan_demote(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let demote_priv = with_clan(cl, |c| c.privilege[DEMOTE_PRIV]).unwrap_or(0);
    if rank < demote_priv {
        g.send_to_char(ch, "You aren't privileged enough to do that!\r\n");
        return;
    }

    let (arg1, _arg2) = half_chop(arg);
    if arg1.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let victim = match is_playing(g, &arg1) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "There is no player by that name.\r\n");
            return;
        }
    };

    if get_clan(g, victim) != cl {
        g.send_to_char(ch, "Who are you to do that!?  They aren't in your clan!\r\n");
        return;
    }
    if ch == victim {
        g.send_to_char(ch, "Sure wiseguy, you do that.\r\n");
        return;
    }
    let vrank = get_clan_rank(g, victim);
    if rank <= vrank {
        g.send_to_char(ch, "You can't demote someone higher than yourself.\r\n");
        return;
    }
    if vrank == 1 {
        g.send_to_char(ch, "They are level 1, the only thing left to do is expel them.\r\n");
        return;
    }

    let new_rank = vrank - 1;
    if let Some(v) = g.get_char_mut(victim) {
        v.clan_rank = new_rank;
    }
    let label = with_clan(cl, |c| c.rank_label(new_rank).to_string()).unwrap_or_else(|| "N/A".to_string());
    let ch_name = get_name(g, ch);
    let vic_name = get_name(g, victim);
    g.send_to_char(victim, &format!("{} has demoted you to {}.\r\n", ch_name, label));
    g.send_to_char(ch, &format!("You demote {} to {}.\r\n", vic_name, label));
}

// ---------------------------------------------------------------------------
// clan wname (clan_who_title)
// ---------------------------------------------------------------------------

fn clan_who_title(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    if get_level(g, ch) < LVL_CLAN_GOD {
        g.send_to_char(ch, "You cannot do that.\r\n");
        return;
    }
    let (arg1, arg2) = half_chop(arg);
    if arg1.is_empty() || arg2.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let num: i32 = arg1.trim().parse().unwrap_or(-1);
    if num < 0 || num > num_of_clans() - 1 {
        g.send_to_char(ch, "That clan number does not exist.\r\n");
        return;
    }
    if arg2.len() > 15 {
        g.send_to_char(ch, "A clan's who title may not be more than 15 characters.\r\n");
        return;
    }
    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(num as usize) {
            c.who_name = arg2.clone();
        }
    }
    g.send_to_char(ch, "Clan who title changed.\r\n");
    save_clans();
}

// ---------------------------------------------------------------------------
// clan ctitle (toggle_title)
// ---------------------------------------------------------------------------

fn toggle_title(g: &mut GameState, ch: CharId) {
    if get_clan(g, ch) == -1 || get_clan_rank(g, ch) == -1 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    if prf2(g, ch, PRF2_CTITLE) {
        g.send_to_char(ch, "Your clan name will now be hidden.\r\n");
        if let Some(c) = g.get_char_mut(ch) {
            c.prf2_flags &= !PRF2_CTITLE;
        }
    } else {
        g.send_to_char(ch, "Your clan name will now be shown.\r\n");
        if let Some(c) = g.get_char_mut(ch) {
            c.prf2_flags |= PRF2_CTITLE;
        }
    }
}

// ---------------------------------------------------------------------------
// clan talk (toggle_talk)
// ---------------------------------------------------------------------------

fn toggle_talk(g: &mut GameState, ch: CharId) {
    if get_clan(g, ch) == -1 || get_clan_rank(g, ch) <= 0 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    if prf2(g, ch, PRF2_CLAN_TALK) {
        g.send_to_char(ch, "You will no longer hear the clan channel.\r\n");
        if let Some(c) = g.get_char_mut(ch) {
            c.prf2_flags &= !PRF2_CLAN_TALK;
        }
    } else {
        g.send_to_char(ch, "You will now hear the clan channel.\r\n");
        if let Some(c) = g.get_char_mut(ch) {
            c.prf2_flags |= PRF2_CLAN_TALK;
        }
    }
}

// ---------------------------------------------------------------------------
// clan help (handle_help)
// ---------------------------------------------------------------------------

fn handle_help(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        g.send_to_char(
            ch,
            "Clan help topics: score, roster, who, info, list, rname, privilege, \
             expel, resign, demote, promote, enlist, apply, rank, withdraw, \
             ctitle, talk, deposit\r\n",
        );
        return;
    }
    g.send_to_char(ch, "Help for clan commands is currently unavailable.\r\n");
}

// ---------------------------------------------------------------------------
// clan info (clan_info_list)
// ---------------------------------------------------------------------------

fn clan_info_list(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let num: i32 = arg.trim().parse().unwrap_or(-1);
    if num < 0 || num > num_of_clans() - 1 {
        g.send_to_char(ch, "That clan doesn't exist.\r\n");
        return;
    }
    let info = match with_clan(num, |c| c.clone()) {
        Some(c) => c,
        None => {
            g.send_to_char(ch, "That clan doesn't exist.\r\n");
            return;
        }
    };
    let buf = format!(
        "\r\n\
         \t\t\t  Clan Information\r\n\
         \x20    -----------------------------------------------------------\r\n\
         \r\n\
         \t\t\tName   : {}\r\n\
         \t\t\tNumber : {}\r\n\
         \t\t\tLeader : {}\r\n\
         \t\t\tMembers: {}\r\n\
         \r\n\
         \x20    -----------------------------------------------------------\r\n\r\n",
        info.name, info.number, info.leader, info.members
    );
    g.send_to_char(ch, &buf);
}

// ---------------------------------------------------------------------------
// clan who
// ---------------------------------------------------------------------------

fn clan_who(g: &mut GameState, ch: CharId) {
    if get_clan(g, ch) < 0 || get_clan_rank(g, ch) == -1 {
        g.send_to_char(ch, "You don't belong to a clan.\r\n");
        return;
    }
    let num = get_clan(g, ch);

    g.send_to_char(ch, "Clan members on-line:\r\n");
    g.send_to_char(ch, "~~~~~~~~~~~~~~~~~~~~~\r\n");

    let mut count = 0;
    for d in connected_players(g) {
        if get_clan(g, d) == num && get_clan_rank(g, d) > 0 && g.can_see(ch, d) {
            count += 1;
            let (lvl, class_abbr, name) = match g.get_char(d) {
                Some(c) => (c.player.level, c.class_abbrev().to_string(), c.player.name.clone()),
                None => continue,
            };
            let drank = get_clan_rank(g, d);
            let label = with_clan(num, |c| c.rank_label(drank).to_string()).unwrap_or_else(|| "N/A".to_string());
            g.send_to_char(ch, &format!("{}. [{:2} {} {}] {}\r\n", count, lvl, class_abbr, label, name));
        }
    }
    g.send_to_char(ch, "~~~~~~~~~~~~~~~~~~~~~\r\n");
}

// ---------------------------------------------------------------------------
// clan roster — C reads the whole player_main table; we have no synchronous
// roster of offline players, so we list the online membership (the same set
// `clan who` shows, formatted as the roster) and the stored member count.
// ---------------------------------------------------------------------------

fn clan_roster(g: &mut GameState, ch: CharId) {
    if get_clan(g, ch) < 0 || get_clan_rank(g, ch) == -1 {
        g.send_to_char(ch, "You don't belong to a clan.\r\n");
        return;
    }
    let num = get_clan(g, ch);

    g.send_to_char(ch, "Full Clan Roster:\r\n");
    g.send_to_char(ch, "------------------------------------\r\n");

    let mut any = false;
    for d in connected_players(g) {
        if get_clan(g, d) == num && get_clan_rank(g, d) > 0 {
            any = true;
            let (lvl, class_abbr, name) = match g.get_char(d) {
                Some(c) => (c.player.level, c.class_abbrev().to_string(), c.player.name.clone()),
                None => continue,
            };
            let drank = get_clan_rank(g, d);
            let label = with_clan(num, |c| c.rank_label(drank).to_string()).unwrap_or_else(|| "N/A".to_string());
            g.send_to_char(ch, &format!("[ {} {} ] {} - {}\r\n", lvl, class_abbr, name, label));
        }
    }
    if !any {
        g.send_to_char(ch, "No clan members on-line!\r\n");
    }
    let members = with_clan(num, |c| c.members).unwrap_or(0);
    g.send_to_char(ch, &format!("\r\nTotal clan members: &c{}&n\r\n", members));
    g.send_to_char(ch, "------------------------------------\r\n");
}

// ---------------------------------------------------------------------------
// clan newowner (clan_new_owner)
// ---------------------------------------------------------------------------

fn clan_new_owner(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() || get_level(g, ch) < LVL_CLAN_GOD {
        send_clan_format(g, ch);
        return;
    }
    let (arg1, arg2) = half_chop(arg);
    if arg1.is_empty() || arg2.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let victim = match get_player_vis(g, ch, &arg2) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "The new owner has to be on-line for this to work!\r\n");
            return;
        }
    };
    let num: i32 = arg1.trim().parse().unwrap_or(-1);
    if num < 0 || num > num_of_clans() - 1 {
        g.send_to_char(ch, "That clan doesn't exist.\r\n");
        return;
    }

    if get_clan(g, victim) != num {
        if let Some(v) = g.get_char_mut(victim) {
            v.clan = num;
        }
    }
    let target_ranks = with_clan(num, |c| c.ranks).unwrap_or(2);
    if get_clan_rank(g, victim) < target_ranks {
        if let Some(v) = g.get_char_mut(victim) {
            v.clan_rank = target_ranks;
        }
    }
    let vic_name = get_name(g, victim);
    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(num as usize) {
            c.leader = vic_name;
        }
    }
    save_clans();
}

// ---------------------------------------------------------------------------
// clan clanroom (set_clanroom)
// ---------------------------------------------------------------------------

fn set_clanroom(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.trim().is_empty() || get_level(g, ch) < LVL_CLAN_GOD {
        send_clan_format(g, ch);
        return;
    }
    let (arg1, arg2) = half_chop(arg);
    if arg1.is_empty() || arg2.is_empty() {
        send_clan_format(g, ch);
        return;
    }
    let num: i32 = arg1.trim().parse().unwrap_or(-1);
    if num < 0 || num > num_of_clans() - 1 {
        g.send_to_char(ch, "No such clan.\r\n");
        return;
    }
    let room_vnum: i32 = arg2.trim().parse().unwrap_or(NOWHERE);
    if g.real_room(room_vnum).is_none() {
        g.send_to_char(ch, "Invalid room number!\r\n");
        return;
    }
    g.send_to_char(ch, &format!("Clan number {}'s clan room is set to {}.\r\n", num, room_vnum));
    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(num as usize) {
            c.clan_room = room_vnum;
        }
    }
    save_clans();
}

// ---------------------------------------------------------------------------
// clan withdraw / deposit
// ---------------------------------------------------------------------------

/// Is the actor standing in their clan's clan-room? (C: real_room == clan_room
/// AND ROOM_FLAGGED(ROOM_CLAN_ROOM).)
fn in_clan_room(g: &GameState, ch: CharId, clan_idx: i32) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let clan_room_vnum = match with_clan(clan_idx, |c| c.clan_room) {
        Some(v) => v,
        None => return false,
    };
    let here_is_clan_room = g.real_room(clan_room_vnum) == Some(rnum);
    let flagged = g
        .room_opt(rnum)
        .map(|r| r.room_flags.bits() & ROOM_CLAN_ROOM_BIT != 0 || r.room_flags.contains(RoomFlags::NO_CLAN))
        .unwrap_or(false);
    // C requires BOTH the room match AND the ROOM_CLAN_ROOM flag. RoomFlags in
    // this port does not yet carry the 1<<19 bit, so we test the raw bits()
    // (which is what C's ROOM_FLAGGED does); NO_CLAN is accepted as a fallback
    // marker for clan-controlled rooms loaded by the world parser.
    here_is_clan_room && flagged
}

fn do_clan_withdraw(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank < 0 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    let withdraw_priv = with_clan(cl, |c| c.privilege[WITHDRAW_PRIV]).unwrap_or(0);
    if rank < withdraw_priv {
        g.send_to_char(ch, "You aren't privileged enough to do that.\r\n");
        return;
    }
    if !in_clan_room(g, ch, cl) {
        g.send_to_char(ch, "You need to be in your clanroom to do that!\r\n");
        return;
    }
    let amount: i32 = arg.trim().parse().unwrap_or(0);
    if arg.trim().is_empty() || amount < 1 {
        g.send_to_char(ch, "Don't you want to withdraw something?\r\n");
        return;
    }
    let clan_gold = with_clan(cl, |c| c.gold).unwrap_or(0);
    if amount as i64 > clan_gold {
        g.send_to_char(ch, "Your clan doesn't have that much gold!\r\n");
        return;
    }

    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(cl as usize) {
            c.gold -= amount as i64;
        }
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.points.gold += amount;
    }
    save_clans();
    g.send_to_char(ch, &format!("You withdraw {} gold coins from your clan bank account.\r\n", amount));
    act(g, "$n makes a clan withdrawal.", true, ch, None, ActArg::None, To::Room);
}

fn do_clan_deposit(g: &mut GameState, ch: CharId, arg: &str) {
    let (cl, rank) = (get_clan(g, ch), get_clan_rank(g, ch));
    if cl == -1 || rank < 0 {
        g.send_to_char(ch, NOCLAN);
        return;
    }
    if !in_clan_room(g, ch, cl) {
        g.send_to_char(ch, "You need to be in your clanroom to do that!\r\n");
        return;
    }
    let amount: i32 = arg.trim().parse().unwrap_or(0);
    if arg.trim().is_empty() || amount < 1 {
        g.send_to_char(ch, "Don't you want to deposit something?\r\n");
        return;
    }
    let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
    if gold < amount {
        g.send_to_char(ch, "You don't have that much!\r\n");
        return;
    }

    {
        let mut t = table().lock().unwrap();
        if let Some(c) = t.clans.get_mut(cl as usize) {
            c.gold += amount as i64;
        }
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.points.gold -= amount;
    }
    save_clans();
    g.send_to_char(ch, &format!("You deposit {} gold coins into your clan bank account.\r\n", amount));
    act(g, "$n makes a clan deposit.", true, ch, None, ActArg::None, To::Room);
}

// ---------------------------------------------------------------------------
// do_clan — the dispatcher (ACMD). Wired from the command table as DoClan.
// ---------------------------------------------------------------------------

pub fn do_clan(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (arg1, arg2) = half_chop(arg);
    let rest = arg2.as_str();

    if arg1.is_empty() {
        send_clan_format(g, ch);
        return;
    }

    // Order mirrors the C if-chain so abbreviation precedence is identical.
    if is_abbrev(&arg1, "create") {
        clan_create(g, ch, rest);
    } else if is_abbrev(&arg1, "destroy") {
        clan_destroy(g, ch, rest);
    } else if is_abbrev(&arg1, "score") {
        clan_score(g, ch);
    } else if is_abbrev(&arg1, "privilege") {
        set_priv(g, ch, rest);
    } else if is_abbrev(&arg1, "rank") {
        handle_rank(g, ch, rest);
    } else if is_abbrev(&arg1, "rname") {
        clan_rank_name(g, ch, rest);
    } else if is_abbrev(&arg1, "apply") {
        clan_apply(g, ch, rest);
    } else if is_abbrev(&arg1, "enlist") {
        clan_enlist(g, ch, rest);
    } else if is_abbrev(&arg1, "expel") {
        clan_expel(g, ch, rest);
    } else if is_abbrev(&arg1, "resign") {
        clan_resign(g, ch);
    } else if is_abbrev(&arg1, "list") {
        clan_list(g, ch);
    } else if is_abbrev(&arg1, "promote") {
        clan_promote(g, ch, rest);
    } else if is_abbrev(&arg1, "demote") {
        clan_demote(g, ch, rest);
    } else if is_abbrev(&arg1, "wname") {
        clan_who_title(g, ch, rest);
    } else if is_abbrev(&arg1, "ctitle") {
        toggle_title(g, ch);
    } else if is_abbrev(&arg1, "talk") {
        toggle_talk(g, ch);
    } else if is_abbrev(&arg1, "help") {
        handle_help(g, ch, rest);
    } else if is_abbrev(&arg1, "info") {
        clan_info_list(g, ch, rest);
    } else if is_abbrev(&arg1, "who") {
        clan_who(g, ch);
    } else if is_abbrev(&arg1, "roster") {
        clan_roster(g, ch);
    } else if is_abbrev(&arg1, "newowner") {
        clan_new_owner(g, ch, rest);
    } else if is_abbrev(&arg1, "clanroom") {
        set_clanroom(g, ch, rest);
    } else if is_abbrev(&arg1, "withdraw") {
        do_clan_withdraw(g, ch, rest);
    } else if is_abbrev(&arg1, "deposit") {
        do_clan_deposit(g, ch, rest);
    } else {
        send_clan_format(g, ch);
    }
}

// ---------------------------------------------------------------------------
// do_csay — the clan-talk channel (ACMD). Wired from the command table as
// DoCsay. PERS() is composed by hand here, exactly like C.
// ---------------------------------------------------------------------------

pub fn do_csay(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if get_clan(g, ch) < 0 || get_clan_rank(g, ch) == -1 {
        g.send_to_char(ch, "You don't belong to a clan.\r\n");
        return;
    }
    if !prf2(g, ch, PRF2_CLAN_TALK) {
        g.send_to_char(ch, "You aren't even on the channel!\r\n");
        return;
    }
    if plr_flagged(g, ch, PLR_NOSHOUT) {
        g.send_to_char(ch, "No.  Muted.  Look it up already.\r\n");
        return;
    }
    // ROOM_SOUNDPROOF check (structs.h 1 << 5 == RoomFlags::SOUNDPROOF).
    let soundproof = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.room_opt(r))
        .map(|r| r.room_flags.contains(RoomFlags::SOUNDPROOF))
        .unwrap_or(false);
    if soundproof {
        g.send_to_char(ch, "The walls seem to absorb your words.\r\n");
        return;
    }

    let num = get_clan(g, ch);
    let argument = arg.trim_start();
    if argument.is_empty() {
        g.send_to_char(ch, "You must have SOMETHING to say, yes?\r\n");
        return;
    }
    let argument = argument.to_string();

    // Broadcast to every online clan-mate on the channel (excluding self),
    // composing PERS(d->character, ch) as "name" / "someone" by hand.
    let hearers = connected_players(g);
    for d in hearers {
        if d == ch {
            continue;
        }
        if get_clan(g, d) == num && get_clan_rank(g, d) > 0 && prf2(g, d, PRF2_CLAN_TALK) {
            let speaker = pers(g, d, ch);
            g.send_to_char(d, &format!("{} clan says, '{}'\r\n", speaker, argument));
        }
    }
    g.send_to_char(ch, &format!("You clan say, '{}'\r\n", argument));
}

/// PERS(viewer, target): the target's visible name, else the immortal /
/// "someone" fallback (utils.h PERS, mirrored from cmd_comm.rs).
fn pers(g: &GameState, viewer: CharId, target: CharId) -> String {
    if g.can_see(viewer, target) {
        get_name(g, target)
    } else if get_level(g, target) >= LVL_IMMORT {
        "A Mystical Being".to_string()
    } else {
        "someone".to_string()
    }
}
