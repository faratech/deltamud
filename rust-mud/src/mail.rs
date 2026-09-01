// mail.rs — the in-game mud-mail system + postmaster special proc, a full
// port of CircleMUD/DeltaMUD `src/mail.c` (+ `mail.h`).
//
// WHAT THIS PORTS
// ---------------
//   * The on-disk mail file (`<lib>/etc/plrmail`) holding fixed-size 100-byte
//     blocks, with a FAT-style next-block chain for messages longer than one
//     block. HEADER / DATA / DELETED block typing and the in-memory recipient
//     index + free-block list, all 1:1 with mail.c.
//   * store_mail() / read_delete() / has_mail() — the three public routines
//     every consumer of the mail system calls.
//   * scan_file() — boot-time indexing (exposed as boot_mail()).
//   * The postmaster() special proc and its three helpers
//     (postmaster_check_mail / postmaster_receive_mail / postmaster_send_mail).
//   * do_mail — the player-facing entry point. In C the words "mail", "check"
//     and "receive" reach the postmaster purely through the mob special-proc
//     dispatch; the Rust command table currently routes them to do_not_here.
//     do_mail reproduces that dispatch by locating a postmaster mob in the
//     room and running the same logic, so the commands work the moment the
//     command table points "mail"/"check"/"receive" here (see manifest).
//
// PERSISTENCE / ON-DISK FORMAT
// ----------------------------
//   The C code fwrite()s native `header_block_type` / `data_block_type` structs
//   straight to disk, so the byte layout depends on the compiler's struct
//   padding and the host's `sizeof(long)`/`sizeof(time_t)`. DeltaMUD's deployed
//   C ABI is little-endian LP64 (`long` and `time_t` are both eight bytes).
//   This module commits to that migration ABI explicitly, making its 100-byte
//   blocks byte-compatible with the C server while avoiding native Rust struct
//   layout. Exact generated C-layout fixtures cover both import and rewrite
//   (#95). Block layout:
//       offset 0  : i64  block_type   (HEADER=-1 / LAST=-2 / DELETED=-3 / >=0 link)
//       offset 8  : i64  next_block   (header only; junk in data blocks)
//       offset 16 : i64  from         (header only)
//       offset 24 : i64  to           (header only)
//       offset 32 : i64  mail_time    (header only; unix seconds)
//       offset 40 : 60 bytes text     (header: 59 usable + NUL; data: 91 usable)
//   Header text capacity = BLOCK_SIZE - 40 - 1 = 59; data text capacity =
//   BLOCK_SIZE - 8 - 1 = 91, exactly C's HEADER_BLOCK_DATASIZE and
//   DATA_BLOCK_DATASIZE on that ABI.

use crate::act::{ActArg, To, act};
use crate::object::{ObjectType, WearFlags};
use crate::state::GameState;
use crate::types::*;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Tunables (mail.h).
// ---------------------------------------------------------------------------

/// Minimum level a player must be to send mail (mail.h MIN_MAIL_LEVEL).
const MIN_MAIL_LEVEL: Level = 2;
/// Gold coins required to send mail (mail.h STAMP_PRICE).
const STAMP_PRICE: i32 = 150;
/// Maximum size of a mail body in bytes (mail.h MAX_MAIL_SIZE).
const MAX_MAIL_SIZE: usize = 4096;
/// Bound durable mail growth even if free postmasters or automated accounts
/// bypass the stamp's economic throttle.
const MAX_MAIL_STORE_BYTES: u64 = 64 * 1024 * 1024;
/// Prevent one recipient from pinning an unbounded number of live headers.
const MAX_MAIL_PER_RECIPIENT: usize = 100;
/// Disk allocation block size (mail.h BLOCK_SIZE).
const BLOCK_SIZE: u64 = 100;

// Block-type sentinels (mail.h).
const HEADER_BLOCK: i64 = -1;
const LAST_BLOCK: i64 = -2;
const DELETED_BLOCK: i64 = -3;

// Text payload capacities for our explicit layout (see file header comment).
const HEADER_TEXT_CAP: usize = (BLOCK_SIZE as usize) - 40 - 1; // 59
const DATA_TEXT_CAP: usize = (BLOCK_SIZE as usize) - 8 - 1; // 91

// Postmaster mob vnums (spec_assign.c: ASSIGNMOB 199 / 1201).
const POSTMASTER_VNUMS: [MobVnum; 2] = [199, 1201];

// Item type used for received mail (structs.h ITEM_NOTE handled via ObjectType).
// Mail object cost/rent/weight from postmaster_receive_mail.
const MAIL_OBJ_WEIGHT: i32 = 1;
const MAIL_OBJ_COST: i32 = 30;
const MAIL_OBJ_RENT: i32 = 10;

// ITEM_WEAR_TAKE | ITEM_WEAR_HOLD (structs.h) for the received mail object.
fn mail_wear_flags() -> WearFlags {
    WearFlags::TAKE | WearFlags::HOLD
}

// ---------------------------------------------------------------------------
// In-memory state.
// ---------------------------------------------------------------------------

/// A single recipient's index entry: the file positions (byte offsets) of all
/// HEADER blocks addressed to them, newest first (mirrors mail_index_type with
/// its position_list_type list_start).
#[derive(Default, Clone)]
struct MailIndexEntry {
    /// File byte offsets of HEADER blocks for this recipient (front = newest).
    positions: Vec<u64>,
}

struct MailSystem {
    /// Absolute path to `<lib>/etc/plrmail`.
    file: PathBuf,
    /// recipient idnum -> their list of header positions.
    index: HashMap<i64, MailIndexEntry>,
    /// Free (DELETED) block byte offsets available for reuse (LIFO, like C).
    free_list: Vec<u64>,
    /// Length of the file (byte offset just past the last block).
    file_end_pos: u64,
    /// Mail disabled flag (C `no_mail`): set on fatal corruption.
    no_mail: bool,
    #[cfg(test)]
    fail_write_on_call: Option<usize>,
    #[cfg(test)]
    write_calls: usize,
    /// Fail one copy-on-write store replacement immediately before the named
    /// block would be written to its unpublished sibling temp file.
    #[cfg(test)]
    fail_replace_on_block: Option<usize>,
    /// Name<->idnum table. There is no global player_table in the Rust port
    /// yet (handler/db expose only live chars + players_by_name), so the mail
    /// system keeps its own so get_name_by_id works for offline senders too.
    /// idnum -> lowercase name.
    id_to_name: HashMap<i64, String>,
    name_to_id: HashMap<String, i64>,
}

impl MailSystem {
    fn new(file: PathBuf) -> Self {
        MailSystem {
            file,
            index: HashMap::new(),
            free_list: Vec::new(),
            file_end_pos: 0,
            no_mail: false,
            #[cfg(test)]
            fail_write_on_call: None,
            #[cfg(test)]
            write_calls: 0,
            #[cfg(test)]
            fail_replace_on_block: None,
            id_to_name: HashMap::new(),
            name_to_id: HashMap::new(),
        }
    }
}

static MAIL: OnceLock<Mutex<MailSystem>> = OnceLock::new();

fn sys() -> &'static Mutex<MailSystem> {
    // If boot_mail() was never called (e.g. unit tests), default to "./lib".
    MAIL.get_or_init(|| {
        Mutex::new(MailSystem::new(
            PathBuf::from("./lib").join("etc").join("plrmail"),
        ))
    })
}

// ---------------------------------------------------------------------------
// Pending-compose table: ConnId -> recipient idnum currently being written to.
// In C this lives on the descriptor (`d->mail_to`, with PLR_MAILING set). The
// Rust Descriptor has no mail_to field and the integrator owns editor I/O, so
// the mapping is parked here and consumed by finish_mail()/abort_mail().
// ---------------------------------------------------------------------------

static PENDING: OnceLock<Mutex<HashMap<ConnId, PendingMail>>> = OnceLock::new();

#[derive(Clone)]
struct PendingMail {
    to: i64,
    from: i64,
}

fn pending() -> &'static Mutex<HashMap<ConnId, PendingMail>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Boot.
// ---------------------------------------------------------------------------

/// Boot the mail system (C scan_file()): open/create `<lib>/etc/plrmail`, scan
/// every block, index live HEADER blocks by recipient, and reclaim DELETED
/// blocks into the free list. The integrator calls this once at boot, after
/// the world is loaded (so player ids can be registered) and before any
/// command runs. Returns true on success (C returns 1), false if the file is
/// corrupt (mail then disabled, matching `no_mail = 1`).
pub fn boot_mail(lib_path: &str) -> bool {
    let file = PathBuf::from(lib_path).join("etc").join("plrmail");
    let mut m = MailSystem::new(file.clone());

    // Ensure the etc directory and file exist (C: "creating new file").
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut f = match OpenOptions::new().read(true).open(&file) {
        Ok(f) => f,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Non-existent -> create an empty file, like scan_file().
            if let Err(error) = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&file)
            {
                log::error!(
                    "SYSERR: cannot create mail store {}: {error}",
                    file.display()
                );
                m.no_mail = true;
                install(m);
                return false;
            }
            m.file_end_pos = 0;
            install(m);
            return true;
        }
        Err(error) => {
            log::error!("SYSERR: cannot open mail store {}: {error}", file.display());
            m.no_mail = true;
            install(m);
            return false;
        }
    };

    let mut buf = [0u8; BLOCK_SIZE as usize];
    let mut block_num: u64 = 0;
    let mut blocks = Vec::new();
    loop {
        match read_exact_block(&mut f, &mut buf) {
            Ok(true) => {}
            Ok(false) => break, // clean EOF on a block boundary
            Err(error) => {
                log::error!(
                    "SYSERR: cannot read mail store {} at block {block_num}: {error}",
                    file.display()
                );
                m.no_mail = true;
                install(m);
                return false;
            }
        }
        blocks.push(buf);
        block_num += 1;
    }

    let end = match f.seek(SeekFrom::End(0)) {
        Ok(end) => end,
        Err(error) => {
            log::error!(
                "SYSERR: cannot determine mail store {} length: {error}",
                file.display()
            );
            m.no_mail = true;
            install(m);
            return false;
        }
    };
    m.file_end_pos = end;

    if end % BLOCK_SIZE != 0 {
        // C: "Mail file corrupt! Mail disabled!"
        log::error!("SYSERR: Error booting mail system -- Mail file corrupt! Mail disabled!");
        m.no_mail = true;
        install(m);
        return false;
    }

    if !validate_mail_blocks(&mut m, &blocks) {
        install(m);
        return false;
    }

    log::info!("   {} bytes read.", end);
    log::info!(
        "   Mail file read -- {} messages.",
        m.index
            .values()
            .map(|entry| entry.positions.len())
            .sum::<usize>()
    );
    install(m);
    true
}

/// Validate the whole FAT graph before exposing any boot-time index. Every
/// live header must own one acyclic, in-range chain; blocks cannot be shared,
/// and non-deleted blocks cannot be orphaned. This turns crash/corruption into
/// an explicit disabled-mail state rather than deferred destructive failure at
/// the first unlucky recipient.
fn validate_mail_blocks(m: &mut MailSystem, blocks: &[[u8; BLOCK_SIZE as usize]]) -> bool {
    let mut headers = Vec::new();
    let mut deleted = HashSet::new();
    let mut owners: HashMap<u64, u64> = HashMap::new();

    for (index, block) in blocks.iter().enumerate() {
        let address = index as u64 * BLOCK_SIZE;
        let block_type = i64::from_le_bytes(block[0..8].try_into().unwrap());
        match block_type {
            HEADER_BLOCK => {
                let from = i64::from_le_bytes(block[16..24].try_into().unwrap());
                let to = i64::from_le_bytes(block[24..32].try_into().unwrap());
                if from < 0 || to < 0 {
                    log::error!(
                        "SYSERR: mail header at {address} has invalid sender/recipient {from}/{to}"
                    );
                    m.no_mail = true;
                    return false;
                }
                owners.insert(address, address);
                headers.push((address, to));
            }
            DELETED_BLOCK => {
                deleted.insert(address);
            }
            _ => {}
        }
    }

    for &(header_address, _) in &headers {
        let header = &blocks[(header_address / BLOCK_SIZE) as usize];
        let mut body_len = read_text_bytes(&header[40..], HEADER_TEXT_CAP).len();
        let mut following = i64::from_le_bytes(header[8..16].try_into().unwrap());
        let mut visited = HashSet::from([header_address]);

        while following != LAST_BLOCK {
            if following < 0 {
                log::error!(
                    "SYSERR: mail chain from {header_address} contains invalid link {following}"
                );
                m.no_mail = true;
                return false;
            }
            let address = following as u64;
            if address % BLOCK_SIZE != 0
                || address
                    .checked_add(BLOCK_SIZE)
                    .is_none_or(|end| end > m.file_end_pos)
            {
                log::error!(
                    "SYSERR: mail chain from {header_address} points outside the store at {address}"
                );
                m.no_mail = true;
                return false;
            }
            if !visited.insert(address) {
                log::error!(
                    "SYSERR: mail chain from {header_address} contains a cycle at {address}"
                );
                m.no_mail = true;
                return false;
            }
            if let Some(other_header) = owners.get(&address) {
                log::error!(
                    "SYSERR: mail block {address} is shared by headers {other_header} and {header_address}"
                );
                m.no_mail = true;
                return false;
            }
            if deleted.contains(&address) {
                log::error!(
                    "SYSERR: mail chain from {header_address} points to deleted block {address}"
                );
                m.no_mail = true;
                return false;
            }

            let data = &blocks[(address / BLOCK_SIZE) as usize];
            let next = i64::from_le_bytes(data[0..8].try_into().unwrap());
            if next < 0 && next != LAST_BLOCK {
                log::error!("SYSERR: mail data block {address} has invalid next marker {next}");
                m.no_mail = true;
                return false;
            }
            body_len = body_len.saturating_add(read_text_bytes(&data[8..], DATA_TEXT_CAP).len());
            if body_len > MAX_MAIL_SIZE {
                log::error!(
                    "SYSERR: mail chain from {header_address} exceeds the {MAX_MAIL_SIZE}-byte limit"
                );
                m.no_mail = true;
                return false;
            }
            owners.insert(address, header_address);
            following = next;
        }
    }

    for index in 0..blocks.len() {
        let address = index as u64 * BLOCK_SIZE;
        if !owners.contains_key(&address) && !deleted.contains(&address) {
            log::error!("SYSERR: mail store contains orphan block {address}");
            m.no_mail = true;
            return false;
        }
    }

    for (address, recipient) in headers {
        index_mail(m, recipient, address);
    }
    for index in 0..blocks.len() {
        let address = index as u64 * BLOCK_SIZE;
        if deleted.contains(&address) {
            m.free_list.push(address);
        }
    }
    true
}

fn install(m: MailSystem) {
    match MAIL.get() {
        Some(lock) => {
            *crate::lock_ok::lock(&lock) = m;
        }
        None => {
            let _ = MAIL.set(Mutex::new(m));
        }
    }
}

/// Register a player's idnum<->name so the mail system can address offline
/// recipients and render "To:/From:" headers (replaces the C global
/// player_table that the Rust port does not yet expose). The integrator should
/// call this for every player as the player index loads at boot, and again on
/// new-character creation. Name is matched case-insensitively (lowercased).
pub fn mail_register_player(idnum: i64, name: &str) {
    if idnum < 0 || name.is_empty() {
        return;
    }
    let lname = name.to_lowercase();
    let mut m = crate::lock_ok::lock(&sys());
    // Re-registration is also the rename path.  Drop every stale name which
    // still points at this identity before publishing the new one; otherwise
    // an old renamed name could continue resolving through mail's fallback
    // registry after GameState's authoritative index stopped recognizing it.
    m.name_to_id.retain(|_, stored_id| *stored_id != idnum);
    m.id_to_name.insert(idnum, lname.clone());
    m.name_to_id.insert(lname, idnum);
}

// ---------------------------------------------------------------------------
// Name <-> id resolution (C get_id_by_name / get_name_by_id).
// ---------------------------------------------------------------------------

/// get_id_by_name(): prefer a live player, then the registered table. Returns
/// a negative value when unknown (C returns -1).
fn get_id_by_name(g: &GameState, name: &str) -> i64 {
    let lname = name.to_lowercase();
    if let Some(cid) = g.find_player_by_name(&lname) {
        if let Some(c) = g.get_char(cid) {
            if c.idnum >= 0 {
                return c.idnum;
            }
        }
    }
    // The shared GameState player_table (C player_table) is the authoritative
    // offline name<->id index; consult it before the mail-local mirror.
    if let Some(id) = g.get_id_by_name(&lname) {
        return id;
    }
    let m = crate::lock_ok::lock(&sys());
    *m.name_to_id.get(&lname).unwrap_or(&-1)
}

/// get_name_by_id(): prefer a live player's display name, then the table.
fn get_name_by_id(g: &GameState, m: &MailSystem, id: i64) -> String {
    // Live player first (preserves original casing).
    for &cid in g.players_by_name.values() {
        if let Some(c) = g.get_char(cid) {
            if c.idnum == id {
                return c.player.name.clone();
            }
        }
    }
    // Shared GameState index (canonical-cased name) next.
    if let Some(n) = g.get_name_by_id(id) {
        return n;
    }
    m.id_to_name
        .get(&id)
        .map(|s| cap_first(s))
        .unwrap_or_else(|| "(null)".to_string())
}

// ---------------------------------------------------------------------------
// Low-level block I/O.
// ---------------------------------------------------------------------------

/// Read exactly one BLOCK_SIZE block. Returns Ok(true) if a full block was
/// read, Ok(false) on a clean EOF at a block boundary.
fn read_exact_block(
    f: &mut std::fs::File,
    buf: &mut [u8; BLOCK_SIZE as usize],
) -> std::io::Result<bool> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "partial mail block",
                ));
            }
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// write_to_file(): durably write one 100-byte block at `filepos`.
///
/// `file_end_pos` advances from the address we just committed rather than from
/// a fallible post-commit seek. Once `write_all` + `flush` + `sync_data` have
/// succeeded, there must be no bookkeeping error which reports failure after a
/// header is already live on disk.
fn write_to_file(m: &mut MailSystem, block: &[u8; BLOCK_SIZE as usize], filepos: u64) -> bool {
    #[cfg(test)]
    {
        let call = m.write_calls;
        m.write_calls = m.write_calls.saturating_add(1);
        if m.fail_write_on_call == Some(call) {
            m.fail_write_on_call = None;
            log::error!("SYSERR: injected mail write failure on call {call}");
            m.no_mail = true;
            return false;
        }
    }
    if filepos % BLOCK_SIZE != 0 {
        log::error!("SYSERR: Mail system -- fatal error #2!!!");
        m.no_mail = true;
        return false;
    }
    let Some(committed_end) = filepos.checked_add(BLOCK_SIZE) else {
        log::error!("SYSERR: mail store address space is exhausted");
        m.no_mail = true;
        return false;
    };
    let mut f = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Mail blocks are updated in place at filepos; truncating here would
        // destroy every later block in the store.
        .truncate(false)
        .open(&m.file)
    {
        Ok(f) => f,
        Err(error) => {
            log::error!(
                "SYSERR: cannot open mail store {} for writing at {filepos}: {error}",
                m.file.display()
            );
            m.no_mail = true;
            return false;
        }
    };
    if let Err(error) = f
        .seek(SeekFrom::Start(filepos))
        .and_then(|_| f.write_all(block))
        .and_then(|_| f.flush())
        .and_then(|_| f.sync_data())
    {
        log::error!(
            "SYSERR: cannot write mail store {} at {filepos}: {error}",
            m.file.display()
        );
        m.no_mail = true;
        return false;
    }
    m.file_end_pos = m.file_end_pos.max(committed_end);
    true
}

/// read_from_file(): read a 100-byte block from `filepos`. Sets no_mail on a
/// misaligned position (C "fatal error #3"). Returns None on error.
fn read_from_file(m: &mut MailSystem, filepos: u64) -> Option<[u8; BLOCK_SIZE as usize]> {
    if filepos % BLOCK_SIZE != 0 {
        log::error!("SYSERR: Mail system -- fatal error #3!!!");
        m.no_mail = true;
        return None;
    }
    if filepos
        .checked_add(BLOCK_SIZE)
        .is_none_or(|end| end > m.file_end_pos)
    {
        log::error!(
            "SYSERR: mail block {filepos} is outside the indexed store length {}",
            m.file_end_pos
        );
        m.no_mail = true;
        return None;
    }
    let mut f = match OpenOptions::new().read(true).open(&m.file) {
        Ok(file) => file,
        Err(error) => {
            log::error!(
                "SYSERR: cannot open mail store {} for reading at {filepos}: {error}",
                m.file.display()
            );
            m.no_mail = true;
            return None;
        }
    };
    if let Err(error) = f.seek(SeekFrom::Start(filepos)) {
        log::error!(
            "SYSERR: cannot seek mail store {} to {filepos}: {error}",
            m.file.display()
        );
        m.no_mail = true;
        return None;
    }
    let mut buf = [0u8; BLOCK_SIZE as usize];
    if let Err(error) = f.read_exact(&mut buf) {
        log::error!(
            "SYSERR: cannot read complete mail block {filepos} from {}: {error}",
            m.file.display()
        );
        m.no_mail = true;
        return None;
    }
    Some(buf)
}

// ---------------------------------------------------------------------------
// Free-list / index management (C push_free_list / pop_free_list / index_mail).
// ---------------------------------------------------------------------------

fn pop_free_list(m: &mut MailSystem) -> u64 {
    match m.free_list.pop() {
        Some(p) => p,
        None => m.file_end_pos,
    }
}

fn push_free_list(m: &mut MailSystem, pos: u64) {
    m.free_list.push(pos);
}

/// Best-effort cleanup after a multi-block write fails. Mail remains disabled
/// after the originating failure; this only makes a subsequent clean boot able
/// to reclaim the complete attempted chain instead of finding orphan data.
fn best_effort_delete_blocks(m: &mut MailSystem, addresses: &[u64]) {
    let mut deleted = [0u8; BLOCK_SIZE as usize];
    deleted[0..8].copy_from_slice(&DELETED_BLOCK.to_le_bytes());
    for &address in addresses {
        let _ = write_to_file(m, &deleted, address);
    }
}

/// Read the complete currently indexed store through a bounded handle. A
/// delete transaction rewrites this snapshot rather than touching live blocks
/// in place, so any error before its final rename leaves the original message
/// and every other mailbox byte-for-byte intact.
fn read_complete_store(m: &mut MailSystem) -> Option<Vec<u8>> {
    if m.file_end_pos > MAX_MAIL_STORE_BYTES || m.file_end_pos % BLOCK_SIZE != 0 {
        log::error!(
            "SYSERR: mail store has invalid indexed length {}",
            m.file_end_pos
        );
        m.no_mail = true;
        return None;
    }

    let mut file = match File::open(&m.file) {
        Ok(file) => file,
        Err(error) => {
            log::error!(
                "SYSERR: cannot open mail store {} for transactional read: {error}",
                m.file.display()
            );
            m.no_mail = true;
            return None;
        }
    };
    let mut bytes = Vec::new();
    if let Err(error) = (&mut file)
        .take(MAX_MAIL_STORE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        log::error!(
            "SYSERR: cannot read mail store {} for transactional replacement: {error}",
            m.file.display()
        );
        m.no_mail = true;
        return None;
    }
    if bytes.len() as u64 != m.file_end_pos {
        log::error!(
            "SYSERR: mail store {} changed length during transactional read (indexed {}, read {})",
            m.file.display(),
            m.file_end_pos,
            bytes.len()
        );
        m.no_mail = true;
        return None;
    }
    Some(bytes)
}

static NEXT_MAIL_REWRITE: AtomicU64 = AtomicU64::new(0);

/// Publish one complete replacement of `plrmail` from a unique sibling temp.
/// The old inode remains the live store until the final same-directory rename.
/// All fallible content work and a pre-publication directory sync happen first.
///
/// A pre-publication temp/rename failure is recoverable because the verified
/// original remains live; mail stays enabled so the caller can retry. A
/// directory-sync error after rename is logged but cannot turn the already
/// published replacement into a reported failure. On crash the filesystem may
/// retain either the old or new directory entry; both are complete valid mail
/// stores, so consumption is at-least-once rather than message-losing.
fn replace_store_atomically(m: &mut MailSystem, replacement: &[u8]) -> bool {
    if replacement.len() as u64 > MAX_MAIL_STORE_BYTES || replacement.len() as u64 % BLOCK_SIZE != 0
    {
        log::error!(
            "SYSERR: refusing invalid {}-byte mail-store replacement",
            replacement.len()
        );
        m.no_mail = true;
        return false;
    }
    let Some(parent) = m.file.parent() else {
        log::error!(
            "SYSERR: mail store {} has no parent directory",
            m.file.display()
        );
        m.no_mail = true;
        return false;
    };
    let parent_handle = match File::open(parent) {
        Ok(file) => file,
        Err(error) => {
            log::error!(
                "SYSERR: cannot open mail-store directory {}: {error}",
                parent.display()
            );
            return false;
        }
    };
    let permissions = match std::fs::metadata(&m.file) {
        Ok(metadata) => metadata.permissions(),
        Err(error) => {
            log::error!(
                "SYSERR: cannot inspect mail store {} before replacement: {error}",
                m.file.display()
            );
            m.no_mail = true;
            return false;
        }
    };

    let basename = m
        .file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "plrmail".to_string());
    let mut opened = None;
    for _ in 0..32 {
        let sequence = NEXT_MAIL_REWRITE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{basename}.rewrite-{}-{sequence}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                opened = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                log::error!(
                    "SYSERR: cannot create sibling mail-store replacement in {}: {error}",
                    parent.display()
                );
                return false;
            }
        }
    }
    let Some((temp_path, mut temp)) = opened else {
        log::error!(
            "SYSERR: cannot reserve a unique sibling replacement for {}",
            m.file.display()
        );
        return false;
    };

    let prepare_result = (|| -> std::io::Result<()> {
        temp.set_permissions(permissions)?;
        #[cfg(test)]
        let mut block_index = 0usize;
        for block in replacement.chunks(BLOCK_SIZE as usize) {
            #[cfg(test)]
            {
                if m.fail_replace_on_block == Some(block_index) {
                    m.fail_replace_on_block = None;
                    return Err(std::io::Error::other(format!(
                        "injected mail-store replacement failure before block {block_index}"
                    )));
                }
                block_index += 1;
            }
            temp.write_all(block)?;
        }
        temp.flush()?;
        temp.sync_all()?;
        // Persist the complete temp entry before the atomic namespace switch.
        parent_handle.sync_all()?;
        Ok(())
    })();
    if let Err(error) = prepare_result {
        log::error!(
            "SYSERR: cannot prepare replacement for mail store {}: {error}",
            m.file.display()
        );
        drop(temp);
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }
    drop(temp);

    if let Err(error) = std::fs::rename(&temp_path, &m.file) {
        log::error!(
            "SYSERR: cannot atomically publish mail store {}: {error}",
            m.file.display()
        );
        let _ = std::fs::remove_file(&temp_path);
        return false;
    }

    // Rename is the publication point. No subsequent bookkeeping failure may
    // make the caller report that the still-live old message was retained.
    m.file_end_pos = replacement.len() as u64;
    if let Err(error) = parent_handle.sync_all() {
        log::error!(
            "SYSERR: mail-store replacement was published but directory sync failed for {}: {error}",
            parent.display()
        );
    }
    true
}

/// index_mail(): record that a HEADER block for `recipient` lives at `pos`,
/// prepending to that recipient's position list (newest first).
fn index_mail(m: &mut MailSystem, recipient: i64, pos: u64) {
    if recipient < 0 {
        log::error!("SYSERR: Mail system -- non-fatal error #4.");
        return;
    }
    let entry = m.index.entry(recipient).or_default();
    entry.positions.insert(0, pos);
}

// ---------------------------------------------------------------------------
// Block builders.
// ---------------------------------------------------------------------------

/// Build a HEADER block (block_type/next/from/to/time + up to HEADER_TEXT_CAP
/// bytes of NUL-terminated text).
fn make_header_block(
    next_block: i64,
    from: i64,
    to: i64,
    mail_time: i64,
    txt: &str,
) -> [u8; BLOCK_SIZE as usize] {
    let mut b = [0u8; BLOCK_SIZE as usize];
    b[0..8].copy_from_slice(&HEADER_BLOCK.to_le_bytes());
    b[8..16].copy_from_slice(&next_block.to_le_bytes());
    b[16..24].copy_from_slice(&from.to_le_bytes());
    b[24..32].copy_from_slice(&to.to_le_bytes());
    b[32..40].copy_from_slice(&mail_time.to_le_bytes());
    write_text(&mut b[40..], txt, HEADER_TEXT_CAP);
    b
}

/// Build a DATA block (block_type link + up to DATA_TEXT_CAP bytes of text).
fn make_data_block(block_type: i64, txt: &str) -> [u8; BLOCK_SIZE as usize] {
    let mut b = [0u8; BLOCK_SIZE as usize];
    b[0..8].copy_from_slice(&block_type.to_le_bytes());
    write_text(&mut b[8..], txt, DATA_TEXT_CAP);
    b
}

/// Copy at most `cap` bytes of `txt` into `dst`, NUL-terminating (C strncpy +
/// explicit terminator). `dst` is assumed already zeroed.
fn write_text(dst: &mut [u8], txt: &str, cap: usize) {
    let bytes = txt.as_bytes();
    let n = bytes.len().min(cap);
    dst[..n].copy_from_slice(&bytes[..n]);
    // dst[n] is the NUL (already zero from the zeroed block).
}

/// Borrow the NUL-terminated payload bytes from one C mail block.
///
/// Decoding must happen only after every block in the chain is concatenated:
/// the C writer splits raw bytes at its fixed 59/91-byte capacities, including
/// in the middle of a UTF-8 scalar (#95/#395).
fn read_text_bytes(src: &[u8], cap: usize) -> &[u8] {
    let region = &src[..cap.min(src.len())];
    let end = region.iter().position(|&c| c == 0).unwrap_or(region.len());
    &region[..end]
}

// ===========================================================================
// PUBLIC API: has_mail / store_mail / read_delete
// ===========================================================================

/// has_mail(): does this recipient have any mail waiting? (mail.c has_mail)
pub fn has_mail(recipient: i64) -> bool {
    let m = crate::lock_ok::lock(&sys());
    !m.no_mail
        && m.index
            .get(&recipient)
            .map(|e| !e.positions.is_empty())
            .unwrap_or(false)
}

/// store_mail(): store a message addressed to `to` from `from`. Splits the body
/// across a HEADER block plus a FAT-chained run of DATA blocks, reusing free
/// (deleted) blocks where possible. The data chain is written first and the
/// header/index are the publication point, so an I/O failure cannot publish a
/// dangling or incomplete message. (mail.c store_mail)
pub fn store_mail(to: i64, from: i64, message: &str) -> bool {
    let mut m = crate::lock_ok::lock(&sys());
    if m.no_mail {
        return false;
    }
    if from < 0 || to < 0 || message.is_empty() {
        log::error!("SYSERR: Mail system -- non-fatal error #5.");
        return false;
    }
    if message.len() > MAX_MAIL_SIZE {
        log::warn!("mail from {from} to {to} exceeds the {MAX_MAIL_SIZE}-byte message limit");
        return false;
    }
    if m.index
        .get(&to)
        .is_some_and(|entry| entry.positions.len() >= MAX_MAIL_PER_RECIPIENT)
    {
        log::warn!("mailbox {to} already contains the {MAX_MAIL_PER_RECIPIENT}-message limit");
        return false;
    }

    let mail_time = now_secs();
    let header_txt = take_prefix(message, HEADER_TEXT_CAP);
    let mut bytes_written = header_txt.len();
    let mut data_text = Vec::new();
    while bytes_written < message.len() {
        let chunk = take_window(message, bytes_written, DATA_TEXT_CAP);
        if chunk.is_empty() {
            log::error!("SYSERR: mail UTF-8 chunker made no progress");
            m.no_mail = true;
            return false;
        }
        bytes_written += chunk.len();
        data_text.push(chunk);
    }

    // Reserve distinct addresses without advancing the durable end before a
    // write succeeds. Popped free blocks remain unavailable if a later write
    // fails, but mail is then disabled; a clean reboot reconstructs the lists
    // from disk without ever indexing the unpublished header.
    let blocks_needed = data_text.len() + 1;
    let append_blocks = blocks_needed.saturating_sub(m.free_list.len());
    let Ok(append_blocks) = u64::try_from(append_blocks) else {
        log::error!("SYSERR: mail store block count exceeds the address range");
        m.no_mail = true;
        return false;
    };
    let Some(projected_end) = append_blocks
        .checked_mul(BLOCK_SIZE)
        .and_then(|growth| m.file_end_pos.checked_add(growth))
    else {
        log::error!("SYSERR: mail store address space is exhausted");
        m.no_mail = true;
        return false;
    };
    if projected_end > MAX_MAIL_STORE_BYTES {
        log::warn!("mail store cannot grow past its {MAX_MAIL_STORE_BYTES}-byte safety limit");
        return false;
    }

    let mut next_append = m.file_end_pos;
    let mut addresses = Vec::with_capacity(blocks_needed);
    for _ in 0..blocks_needed {
        let address = if let Some(address) = m.free_list.pop() {
            address
        } else {
            let address = next_append;
            let Some(next) = next_append.checked_add(BLOCK_SIZE) else {
                log::error!("SYSERR: mail store address space is exhausted");
                m.no_mail = true;
                return false;
            };
            next_append = next;
            address
        };
        if i64::try_from(address).is_err() {
            log::error!("SYSERR: mail store exceeds its signed on-disk link range");
            m.no_mail = true;
            return false;
        }
        addresses.push(address);
    }

    // Materialize every reservation as DELETED before writing link-bearing
    // data. These writes are not publication; they ensure unwritten holes are
    // never mistaken for valid data after a failed attempt.
    let mut deleted = [0u8; BLOCK_SIZE as usize];
    deleted[0..8].copy_from_slice(&DELETED_BLOCK.to_le_bytes());
    for &address in &addresses {
        if !write_to_file(&mut m, &deleted, address) {
            best_effort_delete_blocks(&mut m, &addresses);
            return false;
        }
    }

    // Publish every data block before the header that makes the chain visible.
    for (index, text) in data_text.iter().enumerate() {
        let next = addresses.get(index + 2).map_or(LAST_BLOCK, |address| {
            i64::try_from(*address).expect("reserved mail address was range-checked")
        });
        let data = make_data_block(next, text);
        if !write_to_file(&mut m, &data, addresses[index + 1]) {
            best_effort_delete_blocks(&mut m, &addresses);
            return false;
        }
    }
    let first_data = addresses.get(1).map_or(LAST_BLOCK, |address| {
        i64::try_from(*address).expect("reserved mail address was range-checked")
    });
    let header = make_header_block(first_data, from, to, mail_time, header_txt);
    if !write_to_file(&mut m, &header, addresses[0]) {
        best_effort_delete_blocks(&mut m, &addresses);
        return false;
    }
    index_mail(&mut m, to, addresses[0]);
    true
}

/// read_delete(): pop the *oldest* message for `recipient` (C walks to the
/// tail of the position list), render its formatted "Deltanian Postal Service"
/// header + body, mark every block of that message DELETED and free it, and
/// return the full text. Returns None when there is no mail / on error.
/// (mail.c read_delete)
pub fn read_delete(g: &GameState, recipient: i64) -> Option<String> {
    let mut m = crate::lock_ok::lock(&sys());
    if m.no_mail {
        return None;
    }
    if recipient < 0 {
        log::error!("SYSERR: Mail system -- non-fatal error #6.");
        return None;
    }
    // Find the recipient's oldest message, but do not mutate its index until
    // every referenced block has been validated and every delete write has
    // succeeded. A corrupt/truncated chain must never become partial delivered
    // mail or silently consume the only durable pointer to the message.
    let mail_address = match m.index.get(&recipient) {
        Some(entry) => match entry.positions.last() {
            Some(address) => *address,
            None => {
                log::error!("SYSERR: Mail system -- non-fatal error #8.");
                return None;
            }
        },
        None => {
            log::error!("SYSERR: Mail system -- post office spec_proc error?  Error #7.");
            return None;
        }
    };

    // Preflight the complete FAT chain before changing the index, free list, or
    // any bytes on disk. The fixed maximum body size is also a hard bound on a
    // corrupt chain's memory/CPU cost.
    let header = read_from_file(&mut m, mail_address)?;
    let block_type = i64::from_le_bytes(header[0..8].try_into().unwrap());
    if block_type != HEADER_BLOCK {
        log::error!("SYSERR: Oh dear.");
        m.no_mail = true;
        log::error!("SYSERR: Mail system disabled!  -- Error #9.");
        return None;
    }
    let stored_recipient = i64::from_le_bytes(header[24..32].try_into().unwrap());
    if stored_recipient != recipient {
        log::error!(
            "SYSERR: indexed mail recipient {recipient} does not match header recipient {stored_recipient}"
        );
        m.no_mail = true;
        return None;
    }
    let next_block = i64::from_le_bytes(header[8..16].try_into().unwrap());
    let from = i64::from_le_bytes(header[16..24].try_into().unwrap());
    if from < 0 {
        log::error!("SYSERR: mail header at {mail_address} has invalid sender {from}");
        m.no_mail = true;
        return None;
    }
    let mail_time = i64::from_le_bytes(header[32..40].try_into().unwrap());
    let mut body = read_text_bytes(&header[40..], HEADER_TEXT_CAP).to_vec();
    let mut blocks = vec![(mail_address, header)];
    let mut visited = HashSet::from([mail_address]);
    let mut following = next_block;
    while following != LAST_BLOCK {
        if following < 0 {
            log::error!("SYSERR: mail chain from {mail_address} contains invalid link {following}");
            m.no_mail = true;
            return None;
        }
        let addr = following as u64;
        if !visited.insert(addr) {
            log::error!("SYSERR: mail chain from {mail_address} contains a cycle at {addr}");
            m.no_mail = true;
            return None;
        }
        let data = read_from_file(&mut m, addr)?;
        let next = i64::from_le_bytes(data[0..8].try_into().unwrap());
        if next < 0 && next != LAST_BLOCK {
            log::error!("SYSERR: mail data block {addr} contains invalid next-block marker {next}");
            m.no_mail = true;
            return None;
        }
        body.extend_from_slice(read_text_bytes(&data[8..], DATA_TEXT_CAP));
        if body.len() > MAX_MAIL_SIZE {
            log::error!(
                "SYSERR: mail chain from {mail_address} exceeds the {MAX_MAIL_SIZE}-byte limit"
            );
            m.no_mail = true;
            return None;
        }
        blocks.push((addr, data));
        following = next;
    }

    // Compose the formatted header (matches mail.c sprintf, colour codes and
    // all). asctime() with the trailing newline stripped.
    let tmstr = fmt_asctime(mail_time);
    // Reuse the already-held mail-system guard. Re-locking the same
    // non-reentrant Mutex here deadlocked every successful receive.
    let to_name = get_name_by_id(g, &m, recipient);
    let from_name = get_name_by_id(g, &m, from);
    let mut message = format!(
        " &b-&c=&y Deltanian Postal Service&c =&b-&n\r\n\
         &GD&gate&c:&n {}\r\n\
         \x20\x20&GT&go&c:&n {}\r\n\
         &GF&grom&c:&n {}\r\n\r\n",
        tmstr, to_name, from_name
    );

    // The mutex prevents an in-process race, but assert the exact publication
    // pointer again before the destructive phase so a future refactor cannot
    // accidentally delete a different message.
    if m.index
        .get(&recipient)
        .and_then(|entry| entry.positions.last())
        .copied()
        != Some(mail_address)
    {
        log::error!("SYSERR: mail index changed during read of {mail_address}");
        m.no_mail = true;
        return None;
    }

    // Consume the message through a whole-store copy-on-write replacement.
    // Verify that the snapshot still contains the exact chain we preflighted,
    // then tombstone only the private copy. Until atomic rename succeeds, the
    // live header and every data block remain byte-for-byte readable.
    let mut replacement = read_complete_store(&mut m)?;
    for (addr, block) in &blocks {
        let Ok(start) = usize::try_from(*addr) else {
            log::error!("SYSERR: mail block address {addr} exceeds memory range");
            m.no_mail = true;
            return None;
        };
        let Some(end) = start.checked_add(BLOCK_SIZE as usize) else {
            log::error!("SYSERR: mail block address {addr} exceeds memory range");
            m.no_mail = true;
            return None;
        };
        if replacement.get(start..end) != Some(block.as_slice()) {
            log::error!("SYSERR: mail store changed at block {addr} during transactional delete");
            m.no_mail = true;
            return None;
        }
        replacement[start..start + 8].copy_from_slice(&DELETED_BLOCK.to_le_bytes());
    }
    if !replace_store_atomically(&mut m, &replacement) {
        return None;
    }

    for (addr, _) in &blocks {
        push_free_list(&mut m, *addr);
    }

    let remove_recipient = {
        let entry = m.index.get_mut(&recipient).expect("preflighted mail index");
        let removed = entry.positions.pop();
        debug_assert_eq!(removed, Some(mail_address));
        entry.positions.is_empty()
    };
    if remove_recipient {
        m.index.remove(&recipient);
    }

    message.push_str(&String::from_utf8_lossy(&body));

    Some(message)
}

// ===========================================================================
// POSTMASTER special proc + helpers
// ===========================================================================

/// postmaster() special proc (mail.c SPECIAL(postmaster)). `ch` is the actor,
/// `mailman` is the postmaster mob running the proc, `cmd_name` is the command
/// word typed, `arg` its argument. Returns true if the command was consumed.
///
/// Wire this into the spec-proc dispatch keyed on POSTMASTER_VNUMS once a
/// spec-proc table exists; until then do_mail() drives the same helpers.
pub fn postmaster(
    g: &mut GameState,
    ch: CharId,
    mailman: CharId,
    cmd_name: &str,
    arg: &str,
) -> bool {
    // "so mobs don't get caught here" — actor must be a real player.
    let has_desc = g
        .get_char(ch)
        .map(|c| c.desc.is_some() && !c.is_npc)
        .unwrap_or(false);
    if !has_desc {
        return false;
    }

    let is_mail = cmd_is(cmd_name, "mail");
    let is_check = cmd_is(cmd_name, "check");
    let is_receive = cmd_is(cmd_name, "receive");
    if !is_mail && !is_check && !is_receive {
        return false;
    }

    if crate::lock_ok::lock(&sys()).no_mail {
        g.send_to_char(
            ch,
            "Sorry, the mail system is having technical difficulties.\r\n",
        );
        return false;
    }

    if is_mail {
        postmaster_send_mail(g, ch, mailman, arg);
        true
    } else if is_check {
        postmaster_check_mail(g, ch, mailman);
        true
    } else if is_receive {
        postmaster_receive_mail(g, ch, mailman);
        true
    } else {
        false
    }
}

fn postmaster_check_mail(g: &mut GameState, ch: CharId, mailman: CharId) {
    let idnum = char_idnum(g, ch);
    let msg = if has_mail(idnum) {
        "$n tells you, 'You have mail waiting.'"
    } else {
        "$n tells you, 'Sorry, you don't have any mail waiting.'"
    };
    act(g, msg, false, mailman, None, ActArg::Char(ch), To::Vict);
}

fn postmaster_receive_mail(g: &mut GameState, ch: CharId, mailman: CharId) {
    let idnum = char_idnum(g, ch);
    if !has_mail(idnum) {
        act(
            g,
            "$n tells you, 'Sorry, you don't have any mail waiting.'",
            false,
            mailman,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        return;
    }

    // Hand over every waiting message as a separate ITEM_NOTE object.
    while has_mail(idnum) {
        let Some(body) = read_delete(g, idnum) else {
            act(
                g,
                "$n tells you, 'Sorry, the mail system encountered an error while retrieving your mail.'",
                false,
                mailman,
                None,
                ActArg::Char(ch),
                To::Vict,
            );
            break;
        };

        let mut obj = crate::object::Object::new(
            NOTHING,
            "mail paper letter".to_string(),
            "a piece of mail".to_string(),
        );
        obj.description = "Someone has left a piece of mail here.".to_string();
        obj.obj_type = ObjectType::Note;
        obj.wear_flags = mail_wear_flags();
        obj.weight = MAIL_OBJ_WEIGHT;
        obj.cost = MAIL_OBJ_COST;
        obj.rent = MAIL_OBJ_RENT;
        obj.action_description = Some(body);
        let oid = g.create_obj(obj);
        g.obj_to_char(oid, ch);

        act(
            g,
            "$n gives you a piece of mail.",
            false,
            mailman,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        // C: act("$N gives $n a piece of mail.", ..., ch, 0, mailman, TO_ROOM)
        // i.e. broadcast in ch's room with $n=ch, $N=mailman.
        act(
            g,
            "$N gives $n a piece of mail.",
            false,
            ch,
            None,
            ActArg::Char(mailman),
            To::Room,
        );
    }
}

fn postmaster_send_mail(g: &mut GameState, ch: CharId, mailman: CharId, arg: &str) {
    let ch_level = char_level(g, ch);
    let mailman_level = char_level(g, mailman);

    // Level gate (free for the level-1 "newbie" postmaster).
    if ch_level < MIN_MAIL_LEVEL && mailman_level != 1 {
        let buf = format!(
            "$n tells you, 'Sorry, you have to be level {} to send mail!'",
            MIN_MAIL_LEVEL
        );
        act(g, &buf, false, mailman, None, ActArg::Char(ch), To::Vict);
        return;
    }

    let (recipient_name, _) = crate::interpreter::one_argument(arg);
    if recipient_name.is_empty() {
        act(
            g,
            "$n tells you, 'You need to specify an address!'",
            false,
            mailman,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        return;
    }

    let ch_gold = char_gold(g, ch);
    if ch_gold < STAMP_PRICE && ch_level < LVL_IMMORT && mailman_level != 1 {
        let buf = format!(
            "$n tells you, 'A stamp costs {} coins.'\r\n\
             $n tells you, '...which I see you can't afford.'",
            STAMP_PRICE
        );
        act(g, &buf, false, mailman, None, ActArg::Char(ch), To::Vict);
        return;
    }

    let recipient = get_id_by_name(g, &recipient_name);
    if recipient < 0 {
        act(
            g,
            "$n tells you, 'No one by that name is registered here!'",
            false,
            mailman,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        return;
    }

    act(
        g,
        "$n starts to write some mail.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    let buf = if mailman_level != 1 {
        format!(
            "$n tells you, 'I'll take {} coins for the stamp.'\r\n\
             $n tells you, 'Write your message, (/s saves /h for help)'",
            STAMP_PRICE
        )
    } else {
        "$n smiles and tells you, 'The stamp is free.'\r\n\
         $n tells you, 'Write your message, (/s saves /h for help)'"
            .to_string()
    };
    act(g, &buf, false, mailman, None, ActArg::Char(ch), To::Vict);

    // Charge the stamp (immortals exempt) and open the compose editor.
    if ch_level < LVL_IMMORT {
        if let Some(c) = g.get_char_mut(ch) {
            crate::gold::debit(c, crate::gold::Account::Carried, i64::from(STAMP_PRICE));
        }
    }

    let from = char_idnum(g, ch);
    open_compose_editor(g, ch, recipient, from);
}

// ===========================================================================
// do_mail — player-facing entry point that emulates the spec-proc dispatch.
// ===========================================================================

/// do_mail: routed from the "mail"/"check"/"receive" command words. Finds a
/// postmaster mob in the actor's room and runs the postmaster proc against it.
/// `subcmd` selects the command word the player typed (see SCMD_* below) so a
/// single handler covers all three, mirroring how the C special proc inspects
/// `cmd`. With no postmaster present this is the normal "can't do that here".
pub fn do_mail(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let cmd_name = match subcmd {
        SCMD_MAIL_CHECK => "check",
        SCMD_MAIL_RECEIVE => "receive",
        _ => "mail",
    };

    let mailman = match find_postmaster_in_room(g, ch) {
        Some(m) => m,
        None => {
            g.send_to_char(ch, "Sorry, but you cannot do that here!\r\n");
            return;
        }
    };

    // Mirror the spec-proc guard set: the actor needs a live descriptor.
    let has_desc = g
        .get_char(ch)
        .map(|c| c.desc.is_some() && !c.is_npc)
        .unwrap_or(false);
    if !has_desc {
        return;
    }
    if crate::lock_ok::lock(&sys()).no_mail {
        g.send_to_char(
            ch,
            "Sorry, the mail system is having technical difficulties.\r\n",
        );
        return;
    }

    match cmd_name {
        "check" => postmaster_check_mail(g, ch, mailman),
        "receive" => postmaster_receive_mail(g, ch, mailman),
        _ => postmaster_send_mail(g, ch, mailman, arg),
    }
}

// do_mail subcmds (interpreter.h-style SCMD_*). The command table points
// "mail"/"check"/"receive" at this handler with these values.
pub const SCMD_MAIL_SEND: i32 = 0;
pub const SCMD_MAIL_CHECK: i32 = 1;
pub const SCMD_MAIL_RECEIVE: i32 = 2;

/// Find a visible postmaster mob (vnum 199 or 1201) in the actor's room.
fn find_postmaster_in_room(g: &GameState, ch: CharId) -> Option<CharId> {
    let rnum = g.get_char(ch)?.in_room?;
    for &cid in &g.rooms.get(rnum)?.people {
        if let Some(c) = g.get_char(cid) {
            if c.is_npc && POSTMASTER_VNUMS.contains(&c.nr) && g.can_see(ch, cid) {
                return Some(cid);
            }
        }
    }
    None
}

// ===========================================================================
// Compose-editor plumbing (integrator-driven; see file header).
// ===========================================================================

/// Open the string editor that captures the mail body. Records the recipient
/// in the pending table keyed by the actor's connection, then pushes a
/// StringEdit context onto the descriptor so the integrator's editor loop
/// gathers the body and calls finish_mail() on save. (C set PLR_MAILING |
/// PLR_WRITING, allocated d->str and set d->max_str / d->mail_to.)
fn open_compose_editor(g: &mut GameState, ch: CharId, to: i64, from: i64) {
    let conn = match g.get_char(ch).and_then(|c| c.desc) {
        Some(c) => c,
        None => return,
    };
    pending()
        .lock()
        .unwrap()
        .insert(conn, PendingMail { to, from });
    crate::modify::start_mail_editing(g, conn, MAX_MAIL_SIZE);
}

/// finish_mail(): the integrator calls this when a mail-compose StringEdit
/// editor is saved (the player typed /s). `body` is the gathered text. Stores
/// the message and clears the pending entry. Returns true if a pending mail
/// was found for this connection (so the integrator knows it owned this save).
pub fn finish_mail(g: &GameState, conn_id: ConnId, body: &str) -> bool {
    let pm = match crate::lock_ok::lock(&pending()).remove(&conn_id) {
        Some(p) => p,
        None => return false,
    };
    let _ = g; // body resolution does not need the world; kept for symmetry.
    let text = if body.is_empty() {
        // C parse_action behaviour: an empty note saves a single space so the
        // !*message_pointer guard in store_mail still accepts it.
        " ".to_string()
    } else {
        body.to_string()
    };
    store_mail(pm.to, pm.from, &text)
}

/// abort_mail(): the integrator calls this if a mail-compose editor is aborted
/// (player disconnects / quits the editor without saving). Drops the pending
/// entry without storing anything. Returns true if one was pending.
pub fn abort_mail(conn_id: ConnId) -> bool {
    crate::lock_ok::lock(&pending()).remove(&conn_id).is_some()
}

/// has_pending_mail(): whether a connection is mid-compose (PLR_MAILING).
pub fn has_pending_mail(conn_id: ConnId) -> bool {
    crate::lock_ok::lock(&pending()).contains_key(&conn_id)
}

#[cfg(test)]
pub(crate) fn seed_pending_mail_for_test(conn_id: ConnId) {
    crate::lock_ok::lock(&pending()).insert(conn_id, PendingMail { to: 2, from: 1 });
}

// ===========================================================================
// Small helpers.
// ===========================================================================

/// CMD_IS(): case-insensitive whole-word match of the typed command.
fn cmd_is(typed: &str, name: &str) -> bool {
    typed.eq_ignore_ascii_case(name)
}

fn char_idnum(g: &GameState, ch: CharId) -> i64 {
    g.get_char(ch).map(|c| c.idnum).unwrap_or(-1)
}
fn char_level(g: &GameState, ch: CharId) -> Level {
    g.get_char(ch).map(|c| c.player.level).unwrap_or(0)
}
fn char_gold(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.points.gold).unwrap_or(0)
}

/// Capitalise the first character of a (lowercase) name for display.
fn cap_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Current unix time in seconds.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The first `cap` bytes of `s` as a &str (on a UTF-8 boundary, never past
/// the end). Used to slice the header payload.
fn take_prefix(s: &str, cap: usize) -> &str {
    crate::text::utf8_prefix(s, cap)
}

/// `cap` bytes of `s` starting at byte offset `start`, clamped to UTF-8
/// boundaries and the string end.
fn take_window(s: &str, start: usize, cap: usize) -> String {
    if start >= s.len() {
        return String::new();
    }
    // Cursor advancement in store_mail always preserves a boundary. For a
    // defensive caller which supplies a byte in the middle of a scalar, move
    // forward to the next boundary; moving backward would duplicate data from
    // the preceding block.
    let mut begin = start.min(s.len());
    while begin < s.len() && !s.is_char_boundary(begin) {
        begin += 1;
    }
    let mut end = (begin + cap).min(s.len());
    while end > begin && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[begin..end].to_string()
}

/// asctime(localtime(t)) with the trailing newline stripped (C read_delete).
/// chrono is already a dependency; render the classic "Wed Jun 15 13:04:22
/// 2026" form in UTC (the port has no TZ wiring; documented in gaps).
fn fmt_asctime(secs: i64) -> String {
    use chrono::{TimeZone, Utc};
    match Utc.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format("%a %b %e %H:%M:%S %Y").to_string(),
        None => "Wed Dec 31 00:00:00 1969".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn mail_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        crate::lock_ok::lock(LOCK.get_or_init(|| Mutex::new(())))
    }

    #[test]
    fn utf8_crossing_header_and_data_seams_round_trips_without_loss() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-utf8-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        assert!(boot_mail(root.to_str().unwrap()));
        mail_register_player(700_001, "Sender");
        mail_register_player(700_002, "Recipient");

        // The first scalar straddles the 59-byte header seam. The crab near a
        // later 91-byte data seam exercises chained block cursor advancement.
        let body = format!("{}é{}🦀{}", "a".repeat(58), "b".repeat(89), "c".repeat(200));
        assert!(store_mail(700_002, 700_001, &body));

        let g = GameState::new(Config::default());
        let rendered = read_delete(&g, 700_002).expect("stored mail is readable");
        assert!(
            rendered.ends_with(&body),
            "mail body changed across block seams"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn imported_c_blocks_decode_utf8_only_after_reassembling_the_raw_chain() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-c-split-utf8-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");

        let from = 700_021i64;
        let to = 700_022i64;
        let body = format!("{}é from C", "a".repeat(58));
        let body_bytes = body.as_bytes();
        assert_eq!(body_bytes[58], 0xc3);
        assert_eq!(body_bytes[59], 0xa9);

        // Reproduce C mail.c's raw fixed-capacity split: the leading byte of
        // `é` is the last header byte and its continuation begins data block 1.
        let mut header = [0u8; BLOCK_SIZE as usize];
        header[0..8].copy_from_slice(&HEADER_BLOCK.to_le_bytes());
        header[8..16].copy_from_slice(&(BLOCK_SIZE as i64).to_le_bytes());
        header[16..24].copy_from_slice(&from.to_le_bytes());
        header[24..32].copy_from_slice(&to.to_le_bytes());
        header[32..40].copy_from_slice(&1_700_000_000i64.to_le_bytes());
        header[40..40 + HEADER_TEXT_CAP].copy_from_slice(&body_bytes[..HEADER_TEXT_CAP]);

        let mut data = [0u8; BLOCK_SIZE as usize];
        data[0..8].copy_from_slice(&LAST_BLOCK.to_le_bytes());
        data[8..8 + body_bytes.len() - HEADER_TEXT_CAP]
            .copy_from_slice(&body_bytes[HEADER_TEXT_CAP..]);

        let mut fixture = Vec::from(header);
        fixture.extend_from_slice(&data);
        std::fs::write(&path, fixture).unwrap();

        assert!(boot_mail(root.to_str().unwrap()));
        mail_register_player(from, "Sender");
        mail_register_player(to, "Recipient");
        let g = GameState::new(Config::default());
        let rendered = read_delete(&g, to).expect("C split-scalar fixture is readable");
        assert!(rendered.ends_with(&body));
        assert!(!rendered.contains('\u{fffd}'));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn truncated_chain_disables_mail_without_delivering_or_consuming_the_index() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-truncated-chain-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");
        let from = 700_031i64;
        let to = 700_032i64;

        let header = make_header_block(BLOCK_SIZE as i64, from, to, 1_700_000_000, "prefix");
        let data = make_data_block(LAST_BLOCK, "suffix");
        let mut fixture = Vec::from(header);
        fixture.extend_from_slice(&data);
        std::fs::write(&path, fixture).unwrap();

        assert!(boot_mail(root.to_str().unwrap()));
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(BLOCK_SIZE)
            .unwrap();

        let g = GameState::new(Config::default());
        assert!(read_delete(&g, to).is_none());
        let state = crate::lock_ok::lock(&sys());
        assert!(state.no_mail, "a short chain read must disable mail");
        assert_eq!(state.index.get(&to).unwrap().positions, vec![0]);
        drop(state);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            i64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            HEADER_BLOCK,
            "preflight failure must not delete the header"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cyclic_chain_disables_mail_before_any_destructive_write() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-cycle-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");
        let from = 700_041i64;
        let to = 700_042i64;

        let header = make_header_block(BLOCK_SIZE as i64, from, to, 1_700_000_000, "prefix");
        let data = make_data_block(LAST_BLOCK, "loop");
        let mut fixture = Vec::from(header);
        fixture.extend_from_slice(&data);
        std::fs::write(&path, &fixture).unwrap();

        assert!(boot_mail(root.to_str().unwrap()));
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(BLOCK_SIZE)).unwrap();
        file.write_all(&(BLOCK_SIZE as i64).to_le_bytes()).unwrap();
        file.sync_data().unwrap();
        fixture[BLOCK_SIZE as usize..BLOCK_SIZE as usize + 8]
            .copy_from_slice(&(BLOCK_SIZE as i64).to_le_bytes());
        let g = GameState::new(Config::default());
        assert!(read_delete(&g, to).is_none());
        let state = crate::lock_ok::lock(&sys());
        assert!(state.no_mail, "a cyclic chain must disable mail");
        assert_eq!(state.index.get(&to).unwrap().positions, vec![0]);
        drop(state);
        assert_eq!(std::fs::read(&path).unwrap(), fixture);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn boot_rejects_shared_and_orphaned_mail_blocks_before_indexing() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-boot-graph-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");

        let first = make_header_block(2 * BLOCK_SIZE as i64, 10, 20, 1_700_000_000, "one");
        let second = make_header_block(2 * BLOCK_SIZE as i64, 11, 21, 1_700_000_001, "two");
        let shared = make_data_block(LAST_BLOCK, "shared");
        let mut shared_fixture = Vec::from(first);
        shared_fixture.extend_from_slice(&second);
        shared_fixture.extend_from_slice(&shared);
        std::fs::write(&path, shared_fixture).unwrap();
        assert!(!boot_mail(root.to_str().unwrap()));
        {
            let state = crate::lock_ok::lock(&sys());
            assert!(state.no_mail);
            assert!(state.index.is_empty());
        }

        let orphan = make_data_block(LAST_BLOCK, "orphan");
        std::fs::write(&path, orphan).unwrap();
        assert!(!boot_mail(root.to_str().unwrap()));
        {
            let state = crate::lock_ok::lock(&sys());
            assert!(state.no_mail);
            assert!(state.index.is_empty());
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_header_publication_is_not_indexed_or_acknowledged() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-publish-failure-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        assert!(boot_mail(root.to_str().unwrap()));

        let conn = ConnId(98_765);
        seed_pending_mail_for_test(conn);
        {
            let mut state = crate::lock_ok::lock(&sys());
            // Two DELETED reservations, one data write, then the header
            // publication write. Fail only that final publication attempt;
            // best-effort cleanup calls are allowed to succeed.
            state.fail_write_on_call = Some(3);
        }
        let g = GameState::new(Config::default());
        assert!(!finish_mail(&g, conn, &"x".repeat(100)));
        {
            let state = crate::lock_ok::lock(&sys());
            assert!(state.no_mail);
            assert!(!state.index.contains_key(&2));
        }

        // Cleanup rewrites every unpublished reservation as DELETED, so a
        // clean restart can reclaim them instead of discovering orphan data.
        assert!(boot_mail(root.to_str().unwrap()));
        let state = crate::lock_ok::lock(&sys());
        assert!(!state.no_mail);
        assert!(!state.index.contains_key(&2));
        assert_eq!(state.free_list.len(), 2);
        drop(state);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn committed_block_write_advances_file_end_deterministically() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-committed-end-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");
        assert!(boot_mail(root.to_str().unwrap()));

        let header = make_header_block(LAST_BLOCK, 1, 2, 1_700_000_000, "first");
        let deleted = {
            let mut block = [0u8; BLOCK_SIZE as usize];
            block[0..8].copy_from_slice(&DELETED_BLOCK.to_le_bytes());
            block
        };
        {
            let mut state = crate::lock_ok::lock(&sys());
            assert!(write_to_file(&mut state, &header, 0));
            assert_eq!(state.file_end_pos, BLOCK_SIZE);
            assert!(write_to_file(&mut state, &header, 0));
            assert_eq!(state.file_end_pos, BLOCK_SIZE);
            assert!(write_to_file(&mut state, &deleted, BLOCK_SIZE));
            assert_eq!(state.file_end_pos, 2 * BLOCK_SIZE);
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * BLOCK_SIZE);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_multiblock_delete_keeps_original_message_readable() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-delete-failure-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");
        assert!(boot_mail(root.to_str().unwrap()));
        let body = "x".repeat(250);
        assert!(store_mail(2, 1, &body));
        let original = std::fs::read(&path).unwrap();
        assert_eq!(original.len(), 4 * BLOCK_SIZE as usize);
        {
            let mut state = crate::lock_ok::lock(&sys());
            // Write one private replacement block, then fail before its second
            // block. The live multi-block message must remain byte-for-byte.
            state.fail_replace_on_block = Some(1);
        }

        let g = GameState::new(Config::default());
        assert!(read_delete(&g, 2).is_none());
        {
            let state = crate::lock_ok::lock(&sys());
            assert!(
                !state.no_mail,
                "an unpublished temp-file failure leaves the live store usable"
            );
            assert_eq!(state.index.get(&2).unwrap().positions, vec![0]);
        }
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(has_mail(2));

        let delivered = read_delete(&g, 2).expect("original mail remains immediately consumable");
        assert!(delivered.ends_with(&body));

        assert!(boot_mail(root.to_str().unwrap()));
        let rebooted = crate::lock_ok::lock(&sys());
        assert!(!rebooted.no_mail);
        assert!(!rebooted.index.contains_key(&2));
        assert_eq!(rebooted.free_list.len(), 4);
        drop(rebooted);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn store_limits_message_mailbox_and_global_growth_without_disabling_mail() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-limits-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");
        assert!(boot_mail(root.to_str().unwrap()));

        assert!(!store_mail(2, 1, &"x".repeat(MAX_MAIL_SIZE + 1)));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

        {
            let mut state = crate::lock_ok::lock(&sys());
            state.index.insert(
                2,
                MailIndexEntry {
                    positions: vec![0; MAX_MAIL_PER_RECIPIENT],
                },
            );
        }
        assert!(!store_mail(2, 1, "bounded mailbox"));

        {
            let mut state = crate::lock_ok::lock(&sys());
            state.file_end_pos = MAX_MAIL_STORE_BYTES;
        }
        assert!(!store_mail(3, 1, "bounded store"));
        let state = crate::lock_ok::lock(&sys());
        assert!(!state.no_mail, "capacity rejection is not disk corruption");
        drop(state);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn exact_c_lp64_header_fixture_imports_and_rewrites_byte_for_byte() {
        let _guard = mail_test_lock();
        let root = std::env::temp_dir().join(format!(
            "deltamud-mail-c-fixture-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(root.join("etc")).unwrap();
        let path = root.join("etc/plrmail");

        let from = 700_011i64;
        let to = 700_012i64;
        let timestamp = 1_700_000_000i64;
        let body = "C ABI fixture";
        let mut fixture = [0u8; BLOCK_SIZE as usize];
        fixture[0..8].copy_from_slice(&HEADER_BLOCK.to_le_bytes());
        fixture[8..16].copy_from_slice(&LAST_BLOCK.to_le_bytes());
        fixture[16..24].copy_from_slice(&from.to_le_bytes());
        fixture[24..32].copy_from_slice(&to.to_le_bytes());
        fixture[32..40].copy_from_slice(&timestamp.to_le_bytes());
        fixture[40..40 + body.len()].copy_from_slice(body.as_bytes());
        assert_eq!(
            make_header_block(LAST_BLOCK, from, to, timestamp, body),
            fixture,
            "Rust's fixed-time writer must match the independent C ABI fixture"
        );
        std::fs::write(&path, fixture).unwrap();

        assert!(boot_mail(root.to_str().unwrap()));
        mail_register_player(from, "Sender");
        mail_register_player(to, "Recipient");
        let g = GameState::new(Config::default());
        assert!(has_mail(to));
        let rendered = read_delete(&g, to).expect("C fixture is readable");
        assert!(rendered.ends_with(body));

        // The deleted block is reused at the same offset. Apart from the new
        // current timestamp, the public Rust rewrite must reproduce the
        // independently assembled C bytes.
        assert!(store_mail(to, from, body));
        let rewritten = std::fs::read(&path).unwrap();
        assert_eq!(rewritten.len(), BLOCK_SIZE as usize);
        let mut expected = fixture;
        expected[32..40].copy_from_slice(&rewritten[32..40]);
        assert_eq!(rewritten, expected);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn re_registering_an_id_for_rename_removes_the_stale_old_name() {
        let _guard = mail_test_lock();
        let idnum = 9_413_777;
        mail_register_player(idnum, "Oldmailname");
        mail_register_player(idnum, "Newmailname");

        let registry = crate::lock_ok::lock(&sys());
        assert_eq!(
            registry.id_to_name.get(&idnum).map(String::as_str),
            Some("newmailname")
        );
        assert_eq!(registry.name_to_id.get("newmailname"), Some(&idnum));
        assert!(!registry.name_to_id.contains_key("oldmailname"));
    }
}
