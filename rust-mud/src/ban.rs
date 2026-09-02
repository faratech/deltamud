// ban.rs — full port of C `src/ban.c` (the site-ban + invalid-name subsystem).
//
// Two cooperating facilities, both consulted from the login path (game.rs's
// `nanny` / `enter_game`):
//   * Site bans: `lib/etc/badsites`, a flat list of {type, sitemask, date,
//     banner}.  `isbanned(host)` wildmatches a connecting host against the list
//     and returns the strongest matching `BanType`. `do_ban`/`do_unban` are the
//     immortal verbs that mutate + persist the list.
//   * Invalid names: `lib/misc/xnames`, a list of forbidden substrings.
//     `valid_name(name)` rejects a chosen PC name whose lowercased form contains
//     any forbidden substring, or that collides with a loaded mob's keyword
//     list.
//
// House style (see cmd_informative.rs / cmd_wizard.rs): read scalars into
// locals, look entities up by id, never hold a borrow across a mutation, emit
// `&`-color codes literally. The authoritative ban list and invalid-name list
// live on GameState (`social.ban`); a published Arc snapshot (social.ban_handle)
// lets the pre-Game accept path check bans without touching the world thread.
//
// Logging: C's `mudlog` writes the on-disk syslog file *and* echoes the line to
// online immortals; we call the shared `crate::syslog::mudlog`, which does both
// (the file lives at `<lib>/syslog`, matching C's LOGFILE/DFLT_DIR). The command
// logic and every output string are reproduced 1:1.

use crate::interpreter::{any_one_arg, one_argument};
use crate::state::GameState;
use crate::syslog::{NRM, mudlog};
use crate::types::*;
use std::net::IpAddr;

// ---------------------------------------------------------------------------
// db.h constants.
// ---------------------------------------------------------------------------
const BANNED_SITE_LENGTH: usize = 50;
const MAX_NAME_LENGTH: usize = 20;
const BAN_FILE_REL: &str = "etc/badsites";
const XNAME_FILE_REL: &str = "misc/xnames";

// MAX_INVALID_NAMES (ban.c) — guard rail mirrored for parity logging only.
const MAX_INVALID_NAMES: usize = 200;

// ---------------------------------------------------------------------------
// BanType — the four ban severities (db.h BAN_NOT/BAN_NEW/BAN_SELECT/BAN_ALL),
// ordered so the numeric value is the C `type` int and `Ord` mirrors C's
// `MAX(i, banned_node->type)` "strongest ban wins" merge.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum BanType {
    None = 0,   // BAN_NOT  — not banned
    New = 1,    // BAN_NEW  — no new (unregistered) chars from this site
    Select = 2, // BAN_SELECT — only PLR_SITEOK chars may connect
    All = 3,    // BAN_ALL  — no connection at all
}

impl BanType {
    fn from_i32(v: i32) -> BanType {
        match v {
            1 => BanType::New,
            2 => BanType::Select,
            3 => BanType::All,
            _ => BanType::None,
        }
    }
    /// ban_types[] string used in the file format and `do_ban` output.
    fn as_str(self) -> &'static str {
        match self {
            BanType::None => "no",
            BanType::New => "new",
            BanType::Select => "select",
            BanType::All => "all",
        }
    }
    /// Parse the flag keyword a god types ("new"/"select"/"all") for `do_ban`.
    fn parse_flag(s: &str) -> Option<BanType> {
        if str_cmp(s, "new") {
            Some(BanType::New)
        } else if str_cmp(s, "select") {
            Some(BanType::Select)
        } else if str_cmp(s, "all") {
            Some(BanType::All)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// ban_list_element (db.h). `date` is a unix timestamp (0 == "Unknown").
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct BanNode {
    site: String, // already lowercased, truncated to BANNED_SITE_LENGTH
    name: String, // banner's name, truncated to MAX_NAME_LENGTH
    date: i64,
    ban_type: BanType,
}

/// Process-global tables (C `ban_list` + `invalid_list` file statics). `lib_path`
/// is captured at boot so `do_ban`/`do_unban` can rewrite badsites without
/// threading it through the ACMD signature.
#[derive(Clone, Default)]
pub struct BanData {
    pub ban_list: Vec<crate::ban::BanNode>,
    pub invalid: Vec<String>,
    pub lib_path: String,
}

/// Shared read-side handle for the pre-Game accept path. The Game owns the
/// authoritative `BanData` on GameState and publishes a fresh snapshot here
/// after every mutation; the async accept loop reads one Arc clone per probe
/// and never touches the world thread.
#[derive(Clone)]
pub struct BanHandle(std::sync::Arc<std::sync::RwLock<std::sync::Arc<BanData>>>);

impl Default for BanHandle {
    fn default() -> Self {
        BanHandle(std::sync::Arc::new(std::sync::RwLock::new(
            std::sync::Arc::new(BanData::default()),
        )))
    }
}

impl BanHandle {
    pub fn snapshot(&self) -> std::sync::Arc<BanData> {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The authoritative in-memory ban tables live on GameState (`social.ban`).
fn data(g: &GameState) -> &BanData {
    &g.social.ban
}

fn data_mut(g: &mut GameState) -> &mut BanData {
    &mut g.social.ban
}

/// Publish the authoritative state to the shared accept-path snapshot. Call
/// after every mutation of `g.social.ban`.
fn publish(g: &GameState) {
    *g.social
        .ban_handle
        .0
        .write()
        .unwrap_or_else(|p| p.into_inner()) = std::sync::Arc::new(g.social.ban.clone());
}

// ---------------------------------------------------------------------------
// Small pure helpers (no GameState).
// ---------------------------------------------------------------------------

/// CircleMUD str_cmp(): case-insensitive whole-string equality.
fn str_cmp(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// CircleMUD ban.c wildmatch() by Storm: glob match of `mask` against `string`,
/// where `*` matches a run and `?` matches any single character. Ported as a
/// faithful translation of the original index walk (kept verbatim so quirky
/// builder masks behave identically — see gaps note).
fn wildmatch(mask: &str, string: &str) -> bool {
    let m: Vec<u8> = mask.bytes().collect();
    let s: Vec<u8> = string.bytes().collect();
    let mlen = m.len();
    let slen = s.len();
    let mut mpos = 0usize;
    let mut spos = 0usize;

    // The C code indexes mask[mpos]/string[spos] and compares strlen-pos-1
    // against 0; out-of-range reads in the original land on the NUL terminator.
    // We treat any read at/after the slice end as 0 ('\0').
    let mc = |i: usize| -> u8 { if i < mlen { m[i] } else { 0 } };
    let sc = |i: usize| -> u8 { if i < slen { s[i] } else { 0 } };

    loop {
        // strlen(string)-spos-1==0  <=>  spos == slen-1 (last char), and the
        // empty-string case (slen==0) makes (0usize.wrapping_sub) huge; mirror
        // the C by testing "one char left" via slen.checked_sub.
        let s_last = slen >= 1 && spos == slen - 1;
        let m_last = mlen >= 1 && mpos == mlen - 1;
        if s_last || m_last || slen == 0 || mlen == 0 {
            // (mask[mpos]=='*' && last-mask) || (last-mask && last-string)
            if (mc(mpos) == b'*' && m_last) || (m_last && s_last) {
                return true;
            } else {
                return false;
            }
        }
        match mc(mpos) {
            b'?' => {
                spos += 1;
                mpos += 1;
            }
            b'*' => {
                while mc(mpos + 1) == b'*' {
                    mpos += 1;
                }
                if mc(mpos + 1) == sc(spos) || mc(mpos + 1) == b'?' {
                    mpos += 2;
                }
                spos += 1;
            }
            other => {
                if other != sc(spos) {
                    return false;
                }
                mpos += 1;
                spos += 1;
            }
        }
    }
}

fn ban_file_path(lib_path: &str) -> String {
    format!("{}/{}", lib_path.trim_end_matches('/'), BAN_FILE_REL)
}

fn xname_file_path(lib_path: &str) -> String {
    format!("{}/{}", lib_path.trim_end_matches('/'), XNAME_FILE_REL)
}

/// CircleMUD is_name(): true if `arg` is a whole keyword of `namelist`
/// (case-insensitive). Local copy (handler::isname is identical but private
/// reuse keeps this module self-contained for the mob-name collision check).
fn is_name(arg: &str, namelist: &str) -> bool {
    if arg.is_empty() {
        return false;
    }
    namelist
        .split_whitespace()
        .any(|w| w.eq_ignore_ascii_case(arg))
}

/// Canonicalize an IP mask against `SocketAddr::ip().to_string()`. Exact
/// IPv4/IPv6 values are parsed through `IpAddr`; globs may contain only IP
/// punctuation, hexadecimal digits, `*`, and `?`. Wildcard IPv4 masks retain
/// their spelling because the C server compared them with its `%03u` numeric
/// host, while live matching checks both canonical and C-padded peer forms.
/// Returning `None` means the token may still be a hostname mask rather than
/// that it is invalid.
fn canonical_ip_mask(raw: &str) -> Option<String> {
    let raw = raw.trim().to_ascii_lowercase();
    if raw.is_empty() || raw.len() > BANNED_SITE_LENGTH {
        return None;
    }
    let has_wildcard = raw.contains('*') || raw.contains('?');
    if !has_wildcard {
        if let Ok(ip) = raw.parse::<IpAddr>() {
            return Some(ip.to_string());
        }
    } else if raw == "*" {
        return Some(raw);
    }
    if raw.contains(':') {
        return raw
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '*' | '?'))
            .then_some(raw);
    }
    if !raw.contains('.') {
        return None;
    }
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.is_empty()
        || parts.len() > 4
        || (!has_wildcard && parts.len() != 4)
        || parts.iter().any(|part| part.is_empty())
    {
        return None;
    }
    let mut validated = Vec::with_capacity(parts.len());
    for part in parts {
        if !part
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '*' | '?'))
        {
            return None;
        }
        if part.contains('*') || part.contains('?') {
            if part.len() > 3 {
                return None;
            }
            validated.push(part.to_string());
        } else {
            part.parse::<u8>().ok()?;
            validated.push(part.to_string());
        }
    }
    if has_wildcard {
        Some(validated.join("."))
    } else {
        let canonical = validated
            .iter()
            .map(|part| part.parse::<u8>().ok().map(|octet| octet.to_string()))
            .collect::<Option<Vec<_>>>()?;
        Some(canonical.join("."))
    }
}

/// C accepts any whitespace-delimited site token up to BANNED_SITE_LENGTH and
/// lowercases it. Keep that broad hostname/glob compatibility while rejecting
/// control/non-ASCII text that cannot be emitted safely in logs or returned by
/// the system DNS resolver. IP-shaped masks take the stricter canonicalization
/// path above so legacy `%03u.%03u.%03u.%03u` records still match Rust's
/// canonical peer address.
fn canonical_ban_mask(raw: &str) -> Option<String> {
    let raw = raw.trim().to_ascii_lowercase();
    if raw.is_empty() || raw.len() > BANNED_SITE_LENGTH || !raw.is_ascii() {
        return None;
    }
    if let Some(ip_mask) = canonical_ip_mask(&raw) {
        return Some(ip_mask);
    }
    raw.bytes()
        .all(|byte| byte.is_ascii_graphic())
        .then_some(raw)
}

// ===========================================================================
// Boot / load
// ===========================================================================

/// CircleMUD boot_ban(): load_banned() + Read_Invalid_List(). The integrator
/// calls this once at startup with the configured lib path, e.g.
/// `ban::boot_ban(&config.lib_path)` (matching the other boot_* hooks).
pub fn boot_ban(g: &mut GameState, lib_path: &str) {
    let ban_list = load_banned(lib_path);
    let invalid = read_invalid_list(lib_path);
    g.social.ban = BanData {
        ban_list,
        invalid,
        lib_path: lib_path.to_string(),
    };
    publish(g);
}

/// CircleMUD load_banned(): parse `lib/etc/badsites`. Each record is four
/// whitespace-delimited fields: `type site date banner`. C prepends each node
/// (newest first); we mirror that by inserting at the front.
fn load_banned(lib_path: &str) -> Vec<BanNode> {
    let path = ban_file_path(lib_path);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            // C: perror("SYSERR: Unable to open banfile") then returns.
            eprintln!("SYSERR: Unable to open banfile '{}'", path);
            return Vec::new();
        }
    };

    // C reads with `fscanf(fl, " %s %s %d %s ", ...)` which is whitespace-
    // delimited across the *whole* stream, not strictly line-oriented. Tokenise
    // the entire file and consume 4-tuples, ignoring any trailing partial.
    let mut tokens = contents.split_whitespace();
    let mut list: Vec<BanNode> = Vec::new();
    loop {
        let ban_type = match tokens.next() {
            Some(t) => t,
            None => break,
        };
        let site = match tokens.next() {
            Some(t) => t,
            None => break,
        };
        let date = match tokens.next() {
            Some(t) => t,
            None => break,
        };
        let name = match tokens.next() {
            Some(t) => t,
            None => break,
        };

        // ban_types[] match: BAN_NOT..=BAN_ALL by exact string.
        let bt = if str_cmp(ban_type, "no") {
            BanType::None
        } else if str_cmp(ban_type, "new") {
            BanType::New
        } else if str_cmp(ban_type, "select") {
            BanType::Select
        } else if str_cmp(ban_type, "all") {
            BanType::All
        } else {
            // C leaves type at the previous CREATE() zero-init (BAN_NOT) when no
            // ban_types[] entry matches; preserve that.
            BanType::None
        };

        let mut site_s = site.to_string();
        crate::text::truncate_utf8_bytes(&mut site_s, BANNED_SITE_LENGTH);
        let site_s = match canonical_ban_mask(&site_s) {
            Some(canonical) => canonical,
            None => {
                eprintln!(
                    "SYSERR: invalid site ban mask '{}' in '{}'; record ignored",
                    site_s, path
                );
                continue;
            }
        };
        let mut name_s = name.to_string();
        crate::text::truncate_utf8_bytes(&mut name_s, MAX_NAME_LENGTH);

        // prepend (ban_list = next_node).
        list.insert(
            0,
            BanNode {
                site: site_s,
                name: name_s,
                date: match crate::text::parse_i64_strict(date) {
                    Ok(value) => value,
                    Err(crate::text::ParseIntError::Overflow) => {
                        let clamped = if date.starts_with('-') {
                            i64::MIN
                        } else {
                            i64::MAX
                        };
                        log::warn!(
                            "SYSERR: ban timestamp overflow in {}; clamped to {}",
                            path,
                            clamped
                        );
                        clamped
                    }
                    Err(_) => 0,
                },
                ban_type: bt,
            },
        );
    }
    list
}

/// CircleMUD Read_Invalid_List(): load forbidden substrings from
/// `lib/misc/xnames`, one per line, skipping blank lines and `*` comments
/// (get_line semantics).
fn read_invalid_list(lib_path: &str) -> Vec<String> {
    let path = xname_file_path(lib_path);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("SYSERR: Unable to open invalid name file '{}'", path);
            return Vec::new();
        }
    };
    let mut out: Vec<String> = Vec::new();
    for raw in contents.lines() {
        // get_line: trim the trailing newline (already gone via .lines()), skip
        // empty lines and lines beginning with '*'.
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('*') {
            continue;
        }
        if out.len() >= MAX_INVALID_NAMES {
            // C: fprintf(stderr, "...Too many invalid names...") then exit(1).
            // We log and stop loading rather than abort the whole process.
            eprintln!(
                "SYSERR: Too many invalid names (>{}); ignoring the rest of '{}'.",
                MAX_INVALID_NAMES, path
            );
            break;
        }
        out.push(line.to_string());
    }
    out
}

/// CircleMUD write_ban_list(): rewrite `lib/etc/badsites`. C writes the list
/// recursively from tail to head (`_write_one_node`), i.e. *oldest first*; since
/// our Vec is stored newest-first (front == most recent), we emit in reverse to
/// reproduce that on-disk order exactly.
fn write_ban_list(d: &BanData) {
    let path = ban_file_path(&d.lib_path);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut out = String::new();
    for node in d.ban_list.iter().rev() {
        out.push_str(&format!(
            "{} {} {} {}\n",
            node.ban_type.as_str(),
            node.site,
            node.date,
            node.name
        ));
    }
    if let Err(e) = std::fs::write(&path, out) {
        eprintln!("SYSERR: write_ban_list '{}': {}", path, e);
    }
}

// ===========================================================================
// Public query API (called from the login path / nanny)
// ===========================================================================

/// CircleMUD isbanned(): lowercase the host, wildmatch every ban mask against
/// it, and return the strongest matching BanType (MAX over matches). An empty
/// host is never banned.
pub fn isbanned(g: &GameState, host: &str) -> BanType {
    isbanned_connection(g, host, None)
}

/// Match every connection identity that is safe to trust. `peer_ip` is always
/// the canonical address captured from the socket and is checked even when a
/// forward-confirmed reverse-DNS hostname is available. This prevents adding
/// hostname support from weakening existing exact/glob IP bans.
pub fn isbanned_connection(
    g: &GameState,
    peer_ip: &str,
    verified_hostname: Option<&str>,
) -> BanType {
    let d = g.social.ban_handle.snapshot();
    isbanned_snapshot(&d, peer_ip, verified_hostname)
}

/// Pure matcher over one snapshot: shared by the GameState path and the
/// pre-Game accept path (which holds a `BanHandle`).
pub fn isbanned_snapshot(d: &BanData, peer_ip: &str, verified_hostname: Option<&str>) -> BanType {
    let parsed_ip = peer_ip.parse::<IpAddr>().ok();
    let canonical_ip = parsed_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| peer_ip.to_ascii_lowercase());
    let c_padded_ipv4 = match parsed_ip {
        Some(IpAddr::V4(ip)) => {
            let [a, b, c, d] = ip.octets();
            Some(format!("{a:03}.{b:03}.{c:03}.{d:03}"))
        }
        _ => None,
    };
    let hostname = verified_hostname
        .filter(|host| !host.is_empty())
        .map(str::to_ascii_lowercase);
    if canonical_ip.is_empty() && hostname.is_none() {
        return BanType::None;
    }

    let mut worst = BanType::None;
    for node in &d.ban_list {
        let matches_ip = (!canonical_ip.is_empty() && wildmatch(&node.site, &canonical_ip))
            || c_padded_ipv4
                .as_deref()
                .is_some_and(|ip| wildmatch(&node.site, ip));
        let matches_hostname = hostname
            .as_deref()
            .is_some_and(|host| wildmatch(&node.site, host));
        if matches_ip || matches_hostname {
            if node.ban_type > worst {
                worst = node.ban_type;
            }
        }
    }
    worst
}

/// CircleMUD Valid_Name(): reject a chosen PC name that (a) contains any
/// forbidden xname substring (case-insensitive), or (b) collides with a loaded
/// mob's keyword list. Returns true if the name is acceptable.
///
/// Note: this is the *xname* check only — the basic length/charset validation
/// (C `_parse_name`) stays in game.rs. The login path should pass a name that
/// already cleared `_parse_name`. The mob-collision half needs the world, so it
/// is exposed as `valid_name_in` taking &GameState; this no-world entry point
/// performs only the substring check (still useful where the world isn't
/// threaded, and matching C's behaviour when `mob_proto` is empty).
pub fn valid_name(g: &GameState, newname: &str) -> bool {
    let d = g.social.ban_handle.snapshot();
    if d.invalid.is_empty() {
        return true;
    }
    let tempname = newname.to_ascii_lowercase();
    for bad in &d.invalid {
        if tempname.contains(&bad.to_ascii_lowercase()) {
            return false;
        }
    }
    true
}

/// Full CircleMUD Valid_Name() including the mob-keyword collision scan, for
/// the login path that has the world in hand. Mirrors ban.c exactly: substring
/// check first, then `is_name(tempname, mob_proto[i].player.name)` over every
/// loaded mob prototype.
pub fn valid_name_in(g: &GameState, newname: &str) -> bool {
    {
        let d = data(g);
        if d.invalid.is_empty() {
            // C returns valid if the list doesn't exist; the mob check still
            // runs in C, but with no invalid list the substring loop is the only
            // skipped part — keep going to the mob scan below for fidelity.
        } else {
            let tempname = newname.to_ascii_lowercase();
            for bad in &d.invalid {
                if tempname.contains(&bad.to_ascii_lowercase()) {
                    return false;
                }
            }
        }
    }
    // Is the desired name already a keyword of a loaded mob?
    let tempname = newname.to_ascii_lowercase();
    for proto in g.mob_protos.values() {
        if is_name(&tempname, &proto.name) {
            return false;
        }
    }
    true
}

// ===========================================================================
// Immortal verbs
// ===========================================================================

/// GET_INVIS_LEV(ch): the actor's invisibility level (Character::invis_level).
fn invis_lev(g: &GameState, ch: CharId) -> u8 {
    g.get_char(ch)
        .map(|c| c.invis_level.clamp(0, u8::MAX as i32) as u8)
        .unwrap_or(0)
}

fn name_of(g: &GameState, ch: CharId) -> String {
    g.get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default()
}

/// Format a unix timestamp the way C's `asctime(localtime())[..10]` does for the
/// ban list — the "Www Mmm dd" prefix (e.g. "Mon Jun 15"). A zero date is
/// rendered "Unknown" by the caller.
fn ban_date_str(date: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(date, 0).single() {
        Some(dt) => dt.format("%a %b %d").to_string(),
        None => "Unknown".to_string(),
    }
}

/// ACMD(do_ban): with no argument, list the bans; otherwise `ban {all|select|
/// new} site_name` adds a new ban (rejecting duplicate sites), logs it, and
/// rewrites badsites.
pub fn do_ban(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let argument = argument.trim();

    // ----- No argument: print the ban list. -----
    if argument.is_empty() {
        let rows: Vec<(String, &'static str, String, String)> = {
            let d = data(g);
            if d.ban_list.is_empty() {
                g.send_to_char(ch, "No sites are banned.\r\n");
                return;
            }
            d.ban_list
                .iter()
                .map(|n| {
                    let when = if n.date != 0 {
                        ban_date_str(n.date)
                    } else {
                        "Unknown".to_string()
                    };
                    (n.site.clone(), n.ban_type.as_str(), when, n.name.clone())
                })
                .collect()
        };

        // format: "%-25.25s  %-8.8s  %-10.10s  %-16.16s\r\n"
        let header = fmt_ban_row("Banned Site Name", "Ban Type", "Banned On", "Banned By");
        g.send_to_char(ch, &header);
        let rule = fmt_ban_row(
            "---------------------------------",
            "---------------------------------",
            "---------------------------------",
            "---------------------------------",
        );
        g.send_to_char(ch, &rule);
        for (site, bt, when, by) in rows {
            let line = fmt_ban_row(&site, bt, &when, &by);
            g.send_to_char(ch, &line);
        }
        return;
    }

    // ----- two_arguments(argument, flag, site). -----
    let (flag, rest) = any_one_arg(argument);
    let (site, _) = any_one_arg(rest);
    if site.is_empty() || flag.is_empty() {
        g.send_to_char(ch, "Usage: ban {all | select | new} site_name\r\n");
        return;
    }
    let ban_type = match BanType::parse_flag(&flag) {
        Some(b) => b,
        None => {
            g.send_to_char(ch, "Flag must be ALL, SELECT, or NEW.\r\n");
            return;
        }
    };

    let site_lc = match canonical_ban_mask(&site) {
        Some(mask) => mask,
        None => {
            g.send_to_char(
                ch,
                "Site must be a hostname, IP address, or wildcard mask.\r\n",
            );
            return;
        }
    };

    let banner = name_of(g, ch);
    let now = chrono::Utc::now().timestamp();

    // Duplicate check + insert under the lock, then persist a snapshot.
    let duplicate = data(g).ban_list.iter().any(|n| str_cmp(&n.site, &site_lc));
    if duplicate {
        g.send_to_char(
            ch,
            "That site has already been banned -- unban it to change the ban type.\r\n",
        );
        return;
    }

    {
        let mut banner_t = banner.clone();
        crate::text::truncate_utf8_bytes(&mut banner_t, MAX_NAME_LENGTH);
        data_mut(g).ban_list.insert(
            0,
            BanNode {
                site: site_lc.clone(),
                name: banner_t,
                date: now,
                ban_type,
            },
        );
        write_ban_list(data(g));
        publish(g);
    }

    let log = format!(
        "{} has banned {} for {} players.",
        banner,
        site_lc,
        ban_type.as_str()
    );
    let level = LVL_GOD.max(invis_lev(g, ch));
    mudlog(g, &log, NRM, level);
    g.send_to_char(ch, "Site banned.\r\n");
}

/// ACMD(do_unban): `unban site_name` removes the matching ban node, logs it,
/// and rewrites badsites.
pub fn do_unban(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (site, _) = one_argument(argument);
    if site.is_empty() {
        g.send_to_char(ch, "A site to unban might help.\r\n");
        return;
    }

    // Find + remove the node, capturing its type/site for the log message.
    let lookup = canonical_ban_mask(&site).unwrap_or_else(|| site.to_ascii_lowercase());
    let removed = {
        let d = data_mut(g);
        match d.ban_list.iter().position(|n| str_cmp(&n.site, &lookup)) {
            Some(idx) => {
                let node = d.ban_list.remove(idx);
                write_ban_list(d);
                Some(node)
            }
            None => None,
        }
    };
    publish(g);

    let node = match removed {
        Some(n) => n,
        None => {
            g.send_to_char(ch, "That site is not currently banned.\r\n");
            return;
        }
    };

    g.send_to_char(ch, "Site unbanned.\r\n");
    let log = format!(
        "{} removed the {}-player ban on {}.",
        name_of(g, ch),
        node.ban_type.as_str(),
        node.site
    );
    let level = LVL_GOD.max(invis_lev(g, ch));
    mudlog(g, &log, NRM, level);
}

// ---------------------------------------------------------------------------
// Output formatting: the C ban-list uses
//   "%-25.25s  %-8.8s  %-10.10s  %-16.16s\r\n"
// i.e. each column is left-justified, padded to a width AND truncated to that
// same width, separated by two spaces.
// ---------------------------------------------------------------------------
fn pad_trunc(s: &str, width: usize) -> String {
    let truncated: String = s.chars().take(width).collect();
    format!("{:<width$}", truncated, width = width)
}

fn fmt_ban_row(site: &str, bantype: &str, when: &str, by: &str) -> String {
    format!(
        "{}  {}  {}  {}\r\n",
        pad_trunc(site, 25),
        pad_trunc(bantype, 8),
        pad_trunc(when, 10),
        pad_trunc(by, 16),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_exact_and_zero_padded_ip_masks() {
        assert_eq!(
            canonical_ip_mask("010.000.000.001").as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(
            canonical_ip_mask("2001:0db8::1").as_deref(),
            Some("2001:db8::1")
        );
    }

    #[test]
    fn accepts_ip_and_hostname_globs() {
        assert_eq!(canonical_ip_mask("010.000.*").as_deref(), Some("010.000.*"));
        assert_eq!(canonical_ip_mask("01?.000.*").as_deref(), Some("01?.000.*"));
        assert_eq!(
            canonical_ip_mask("2001:db8::*").as_deref(),
            Some("2001:db8::*")
        );
        assert_eq!(canonical_ip_mask("*.example.com"), None);
        assert_eq!(
            canonical_ban_mask("*.Example.COM").as_deref(),
            Some("*.example.com")
        );
        assert_eq!(
            canonical_ban_mask("example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(canonical_ban_mask("bad\nmask"), None);
    }

    #[test]
    fn badsites_loader_preserves_utf8_boundaries_at_fixed_byte_caps() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("deltamud-ban-utf8-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let mut contents = String::new();
        for scalar in ['é', '€', '🦀'] {
            contents.push_str(&format!(
                "all {}{scalar} 0 {}{scalar}\n",
                "1".repeat(BANNED_SITE_LENGTH - 1),
                "a".repeat(MAX_NAME_LENGTH - 1),
            ));
        }
        std::fs::write(root.join(BAN_FILE_REL), contents).unwrap();

        let loaded = load_banned(root.to_str().unwrap());
        assert_eq!(loaded.len(), 3);
        for node in loaded {
            assert!(node.site.len() <= BANNED_SITE_LENGTH);
            assert!(node.site.is_char_boundary(node.site.len()));
            assert!(node.name.len() <= MAX_NAME_LENGTH);
            assert!(node.name.is_char_boundary(node.name.len()));
            assert!(
                ['é', '€', '🦀']
                    .into_iter()
                    .all(|scalar| !node.name.contains(scalar))
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
