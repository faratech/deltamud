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
// PERSISTENCE / ON-DISK FORMAT (documented deviation, see `gaps`)
// ---------------------------------------------------------------
//   The C code fwrite()s native `header_block_type` / `data_block_type` structs
//   straight to disk, so the byte layout depends on the compiler's struct
//   padding and the host's `sizeof(long)`/`sizeof(time_t)`. That is not a
//   stable, portable format and cannot be reproduced byte-for-byte from Rust
//   without committing to one specific ABI. We instead use an explicit,
//   self-describing little-endian layout that preserves the *semantics*
//   exactly — BLOCK_SIZE(100)-aligned blocks, HEADER/DATA/DELETED typing,
//   free-list reuse of deleted blocks, and the next-block FAT chain — so the
//   file the Rust MUD writes round-trips through the Rust MUD identically and
//   survives reboots. It is byte-compatible with the C 100-B block format on LP64 (verified; see #95)
//   one-shot migration would re-key by (to,from,time,text). Block layout:
//       offset 0  : i64  block_type   (HEADER=-1 / LAST=-2 / DELETED=-3 / >=0 link)
//       offset 8  : i64  next_block   (header only; junk in data blocks)
//       offset 16 : i64  from         (header only)
//       offset 24 : i64  to           (header only)
//       offset 32 : i64  mail_time    (header only; unix seconds)
//       offset 40 : 60 bytes text     (header: 59 usable + NUL; data: 91 usable)
//   Header text capacity = BLOCK_SIZE - 40 - 1 = 59; data text capacity =
//   BLOCK_SIZE - 8 - 1 = 91. (C's HEADER_BLOCK_DATASIZE/DATA_BLOCK_DATASIZE are
//   derived the same way from the struct sizes; the values differ because the
//   layouts differ, but every store/read pairs against the same constants here.)

use crate::act::{act, ActArg, To};
use crate::object::{ObjectType, WearFlags};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
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
        Err(_) => {
            // Non-existent -> create an empty file, like scan_file().
            let _ = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&file);
            m.file_end_pos = 0;
            install(m);
            return true;
        }
    };

    let mut buf = [0u8; BLOCK_SIZE as usize];
    let mut block_num: u64 = 0;
    let mut total_messages = 0usize;
    loop {
        match read_exact_block(&mut f, &mut buf) {
            Ok(true) => {}
            Ok(false) => break, // clean EOF on a block boundary
            Err(_) => break,
        }
        let block_type = i64::from_le_bytes(buf[0..8].try_into().unwrap());
        let pos = block_num * BLOCK_SIZE;
        if block_type == HEADER_BLOCK {
            let to = i64::from_le_bytes(buf[24..32].try_into().unwrap());
            index_mail(&mut m, to, pos);
            total_messages += 1;
        } else if block_type == DELETED_BLOCK {
            m.free_list.push(pos);
        }
        block_num += 1;
    }

    let end = f.seek(SeekFrom::End(0)).unwrap_or(0);
    m.file_end_pos = end;

    if end % BLOCK_SIZE != 0 {
        // C: "Mail file corrupt! Mail disabled!"
        log::error!("SYSERR: Error booting mail system -- Mail file corrupt! Mail disabled!");
        m.no_mail = true;
        install(m);
        return false;
    }

    log::info!("   {} bytes read.", end);
    log::info!("   Mail file read -- {} messages.", total_messages);
    install(m);
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
fn get_name_by_id(g: &GameState, id: i64) -> String {
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
    let m = crate::lock_ok::lock(&sys());
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
            Ok(0) => {
                return Ok(filled != 0 && {
                    // partial block at EOF: zero-pad and treat as present.
                    for b in buf.iter_mut().skip(filled) {
                        *b = 0;
                    }
                    true
                });
            }
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// write_to_file(): write a 100-byte block at `filepos`, then refresh
/// file_end_pos (C seeks to END after each write). Sets no_mail on a
/// misaligned position (C "fatal error #2").
fn write_to_file(m: &mut MailSystem, block: &[u8; BLOCK_SIZE as usize], filepos: u64) {
    if filepos % BLOCK_SIZE != 0 {
        log::error!("SYSERR: Mail system -- fatal error #2!!!");
        m.no_mail = true;
        return;
    }
    let mut f = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&m.file)
    {
        Ok(f) => f,
        Err(_) => {
            m.no_mail = true;
            return;
        }
    };
    if f.seek(SeekFrom::Start(filepos)).is_err() || f.write_all(block).is_err() {
        m.no_mail = true;
        return;
    }
    let _ = f.flush();
    m.file_end_pos = f.seek(SeekFrom::End(0)).unwrap_or(m.file_end_pos);
}

/// read_from_file(): read a 100-byte block from `filepos`. Sets no_mail on a
/// misaligned position (C "fatal error #3"). Returns None on error.
fn read_from_file(m: &mut MailSystem, filepos: u64) -> Option<[u8; BLOCK_SIZE as usize]> {
    if filepos % BLOCK_SIZE != 0 {
        log::error!("SYSERR: Mail system -- fatal error #3!!!");
        m.no_mail = true;
        return None;
    }
    let mut f = OpenOptions::new().read(true).open(&m.file).ok()?;
    f.seek(SeekFrom::Start(filepos)).ok()?;
    let mut buf = [0u8; BLOCK_SIZE as usize];
    read_exact_block(&mut f, &mut buf).ok()?;
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

/// Read the NUL-terminated text from a block region of length `cap+1`.
fn read_text(src: &[u8], cap: usize) -> String {
    let region = &src[..cap.min(src.len())];
    let end = region.iter().position(|&c| c == 0).unwrap_or(region.len());
    String::from_utf8_lossy(&region[..end]).into_owned()
}

// ===========================================================================
// PUBLIC API: has_mail / store_mail / read_delete
// ===========================================================================

/// has_mail(): does this recipient have any mail waiting? (mail.c has_mail)
pub fn has_mail(recipient: i64) -> bool {
    let m = crate::lock_ok::lock(&sys());
    m.index
        .get(&recipient)
        .map(|e| !e.positions.is_empty())
        .unwrap_or(false)
}

/// store_mail(): store a message addressed to `to` from `from`. Splits the body
/// across a HEADER block plus a FAT-chained run of DATA blocks, reusing free
/// (deleted) blocks where possible. (mail.c store_mail)
pub fn store_mail(to: i64, from: i64, message: &str) {
    let mut m = crate::lock_ok::lock(&sys());
    if m.no_mail {
        return;
    }
    if from < 0 || to < 0 || message.is_empty() {
        log::error!("SYSERR: Mail system -- non-fatal error #5.");
        return;
    }

    let mail_time = now_secs();
    let total_length = message.len();

    // --- HEADER block -----------------------------------------------------
    let header_txt = take_prefix(message, HEADER_TEXT_CAP);
    let target = pop_free_list(&mut m);
    index_mail(&mut m, to, target);
    let header = make_header_block(LAST_BLOCK, from, to, mail_time, header_txt);
    write_to_file(&mut m, &header, target);

    if total_length <= HEADER_TEXT_CAP {
        return; // whole message fit in the header.
    }

    let mut bytes_written = HEADER_TEXT_CAP;
    let mut last_address = target;

    // Allocate the first data block and rewrite the header to point at it.
    let mut target = pop_free_list(&mut m);
    let header = make_header_block(target as i64, from, to, mail_time, header_txt);
    write_to_file(&mut m, &header, last_address);

    // Write the first data block (assume last).
    let mut data_txt = take_window(message, bytes_written, DATA_TEXT_CAP);
    let mut data = make_data_block(LAST_BLOCK, &data_txt);
    write_to_file(&mut m, &data, target);
    bytes_written += data_txt.len();

    // Continue chaining while body remains.
    while bytes_written < total_length {
        last_address = target;
        target = pop_free_list(&mut m);

        // Rewrite the previous data block to link to the next.
        data = make_data_block(target as i64, &data_txt);
        write_to_file(&mut m, &data, last_address);

        // Write the next block, assuming it's the last.
        data_txt = take_window(message, bytes_written, DATA_TEXT_CAP);
        data = make_data_block(LAST_BLOCK, &data_txt);
        write_to_file(&mut m, &data, target);
        bytes_written += data_txt.len();
    }
}

/// read_delete(): pop the *oldest* message for `recipient` (C walks to the
/// tail of the position list), render its formatted "Deltanian Postal Service"
/// header + body, mark every block of that message DELETED and free it, and
/// return the full text. Returns None when there is no mail / on error.
/// (mail.c read_delete)
pub fn read_delete(g: &GameState, recipient: i64) -> Option<String> {
    let mut m = crate::lock_ok::lock(&sys());
    if recipient < 0 {
        log::error!("SYSERR: Mail system -- non-fatal error #6.");
        return None;
    }
    // Find the recipient's entry; the C grabs the *last* node in the list.
    let mail_address = {
        let entry = match m.index.get_mut(&recipient) {
            Some(e) if !e.positions.is_empty() => e,
            Some(_) => {
                log::error!("SYSERR: Mail system -- non-fatal error #8.");
                return None;
            }
            None => {
                log::error!("SYSERR: Mail system -- post office spec_proc error?  Error #7.");
                return None;
            }
        };
        // Oldest is at the tail (list is newest-first).
        let addr = entry.positions.pop().unwrap();
        if entry.positions.is_empty() {
            m.index.remove(&recipient);
        }
        addr
    };

    // Read the header block.
    let header = read_from_file(&mut m, mail_address)?;
    let block_type = i64::from_le_bytes(header[0..8].try_into().unwrap());
    if block_type != HEADER_BLOCK {
        log::error!("SYSERR: Oh dear.");
        m.no_mail = true;
        log::error!("SYSERR: Mail system disabled!  -- Error #9.");
        return None;
    }
    let next_block = i64::from_le_bytes(header[8..16].try_into().unwrap());
    let from = i64::from_le_bytes(header[16..24].try_into().unwrap());
    let mail_time = i64::from_le_bytes(header[32..40].try_into().unwrap());
    let header_txt = read_text(&header[40..], HEADER_TEXT_CAP);

    // Compose the formatted header (matches mail.c sprintf, colour codes and
    // all). asctime() with the trailing newline stripped.
    let tmstr = fmt_asctime(mail_time);
    let to_name = get_name_by_id(g, recipient);
    let from_name = get_name_by_id(g, from);
    let mut message = format!(
        " &b-&c=&y Deltanian Postal Service&c =&b-&n\r\n\
         &GD&gate&c:&n {}\r\n\
         \x20\x20&GT&go&c:&n {}\r\n\
         &GF&grom&c:&n {}\r\n\r\n",
        tmstr, to_name, from_name
    );
    message.push_str(&header_txt);

    // Mark the header DELETED and free it.
    let deleted = retype_block(&header, DELETED_BLOCK);
    write_to_file(&mut m, &deleted, mail_address);
    push_free_list(&mut m, mail_address);

    // Walk the FAT chain, appending text and freeing each block.
    let mut following = next_block;
    while following != LAST_BLOCK {
        let addr = following as u64;
        let data = match read_from_file(&mut m, addr) {
            Some(d) => d,
            None => break,
        };
        let data_txt = read_text(&data[8..], DATA_TEXT_CAP);
        message.push_str(&data_txt);
        following = i64::from_le_bytes(data[0..8].try_into().unwrap());
        let deleted = retype_block(&data, DELETED_BLOCK);
        write_to_file(&mut m, &deleted, addr);
        push_free_list(&mut m, addr);
    }

    Some(message)
}

/// Overwrite a block's leading block_type word (keeps the rest intact).
fn retype_block(src: &[u8; BLOCK_SIZE as usize], new_type: i64) -> [u8; BLOCK_SIZE as usize] {
    let mut b = *src;
    b[0..8].copy_from_slice(&new_type.to_le_bytes());
    b
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
        let body = read_delete(g, idnum)
            .unwrap_or_else(|| "Mail system error - please report.  Error #11.\r\n".to_string());

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
            c.points.gold -= STAMP_PRICE;
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
    store_mail(pm.to, pm.from, &text);
    true
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
    if s.len() <= cap {
        return s;
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `cap` bytes of `s` starting at byte offset `start`, clamped to UTF-8
/// boundaries and the string end.
fn take_window(s: &str, start: usize, cap: usize) -> String {
    if start >= s.len() {
        return String::new();
    }
    let mut begin = start;
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
