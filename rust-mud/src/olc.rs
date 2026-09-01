// olc.rs — the OasisOLC shared framework (CircleMUD olc.c), ported to the
// id-indexed single-owner GameState.
//
// This module OWNS the cross-editor plumbing every OLC sub-editor plugs into:
//
//   * `EditorKind` — which editor a connection is in.
//   * `set_active` / `clear_active` / `in_olc` / `active_editor` — the
//     per-connection "am I in OLC?" registry (game.rs routes input here when
//     `in_olc(conn)` is true).
//   * `olc_input` — the master per-line router: it looks up the active editor
//     and forwards the line to that editor's `<kind>_parse(g, conn, line)`.
//   * the OLC save-list (`olc_add_to_save_list` / `olc_remove_from_save_list`
//     / `olc_saveinfo`) and the on-disk save dispatcher (`olc_save_to_disk`).
//   * `do_olc` — the immortal command (`olc` / `redit` / `oedit` / ... ) that
//     starts an editor or saves a zone, gated on subcmd (SCMD_OLC_*).
//   * shared menu helpers (`sprintbit` / `sprinttype` / `strip_cr`) and the
//     OLC color constants every editor renders with.
//
// The C code stashed the editor and its working data in `d->olc` /
// `STATE(d)`; here neither Descriptor nor GameState may carry an OLC field, so
// the active-editor map lives in a module-static keyed by ConnId, and each
// sub-editor (redit/oedit/...) keeps its own per-connection edit state the same
// way. `olc_input` is the only place that knows the full editor set, so the
// router stays the single source of truth for dispatch.
//
// Public-type contract shared with the sibling editors: the save-kind tags are
// plain `i32`, `real_zone` returns the loaded-zone *index* (rnum), and the
// color constants are raw ANSI (connection.rs forwards them untouched).

use crate::state::AuthenticatedCommandRequest;
use crate::state::GameState;
use crate::types::*;
use crate::world::zone_vnum_bounds;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// SCMD_OLC_* — must match command_table.rs's private constants exactly.
// (redit=0 oedit=1 zedit=2 medit=3 sedit=4 trigedit=5 hedit=6 aedit=7 info=8)
// ---------------------------------------------------------------------------
const LVL_BUILDER_LEVEL: u8 = 100; // LVL_BUILDER

pub const SCMD_OLC_REDIT: i32 = 0;
pub const SCMD_OLC_OEDIT: i32 = 1;
pub const SCMD_OLC_ZEDIT: i32 = 2;
pub const SCMD_OLC_MEDIT: i32 = 3;
pub const SCMD_OLC_SEDIT: i32 = 4;
pub const SCMD_OLC_TRIGEDIT: i32 = 5;
pub const SCMD_OLC_HEDIT: i32 = 6;
pub const SCMD_OLC_AEDIT: i32 = 7;
pub const SCMD_OLC_SAVEINFO: i32 = 8;

// ---------------------------------------------------------------------------
// OLC_SAVE_* — save-list component tags (olc.h). `SAVE_INFO_MSG[]` is indexed
// by these. Plain i32 to match the shared editor contract.
// ---------------------------------------------------------------------------
pub const OLC_SAVE_ROOM: i32 = 0;
pub const OLC_SAVE_OBJ: i32 = 1;
pub const OLC_SAVE_ZONE: i32 = 2;
pub const OLC_SAVE_MOB: i32 = 3;
pub const OLC_SAVE_SHOP: i32 = 4;
pub const OLC_SAVE_HELP: i32 = 5;
pub const OLC_SAVE_ACTION: i32 = 6;

/// `save_info_msg[]` (olc.c) — human label per OLC_SAVE_* tag.
pub const SAVE_INFO_MSG: [&str; 7] = [
    "Rooms",
    "Objects",
    "Zone info",
    "Mobiles",
    "Shops",
    "Help",
    "Actions",
];

// A process-local suffix keeps concurrent OLC saves from sharing a temporary
// file. `create_new` remains the authority in case a stale file already has
// the generated name.
static ATOMIC_REPLACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Every in-process OLC file publication participates in one critical
/// section. This lets compare-and-replace callers validate their exact source
/// bytes without another OLC writer changing the target before rename.
fn atomic_publication_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Marker carried inside an [`io::Error`] when the final rename succeeded but
/// syncing the parent directory did not.  At that point callers must not claim
/// that the old file is still live: the replacement is already visible.  OLC
/// editors use this distinction to reconcile their in-memory view while
/// retaining the dirty marker so a later save can confirm crash durability.
#[derive(Debug)]
struct PublishedButDurabilityUnconfirmed {
    source: io::Error,
}

#[derive(Debug)]
struct PublishedButIncomplete {
    context: String,
    source: io::Error,
}

impl std::fmt::Display for PublishedButIncomplete {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for PublishedButIncomplete {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub(crate) fn published_but_incomplete(context: impl Into<String>, source: io::Error) -> io::Error {
    let kind = source.kind();
    io::Error::new(
        kind,
        PublishedButIncomplete {
            context: context.into(),
            source,
        },
    )
}

impl std::fmt::Display for PublishedButDurabilityUnconfirmed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "replacement was published, but parent-directory sync failed; crash durability is unconfirmed: {}",
            self.source
        )
    }
}

impl std::error::Error for PublishedButDurabilityUnconfirmed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// True only for the post-rename failure state described above.  Ordinary
/// errors mean publication never happened and the old durable bytes remain.
pub(crate) fn replacement_was_published(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|inner| {
        inner
            .downcast_ref::<PublishedButDurabilityUnconfirmed>()
            .is_some()
            || inner.downcast_ref::<PublishedButIncomplete>().is_some()
    })
}

/// Durably replace `path` with `bytes` without first unlinking the live file.
///
/// The temporary file is a uniquely named sibling, so the final rename stays
/// on one filesystem. All writes, the file flush, and the file sync are
/// checked before rename; on Unix the containing directory is synced after
/// rename so the directory entry itself is durable. A failure before rename
/// leaves the old file untouched and removes the temporary file best-effort.
/// A parent-directory sync failure happens after publication: the replacement
/// may already be visible, but crash durability is unconfirmed and the caller
/// must retain any pending-save marker.
pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_replace_with(path, bytes, |_| Ok(()))
}

/// Replace `path` only when it still contains the exact bytes read during
/// preflight. All OLC atomic writers share the same lock, so this closes the
/// in-process read/rename race instead of silently discarding another save.
pub(crate) fn atomic_replace_if_unchanged(
    path: &Path,
    expected: &[u8],
    replacement: &[u8],
) -> io::Result<()> {
    let _publication_guard = crate::lock_ok::lock(atomic_publication_lock());
    validate_exact_contents(path, expected)?;
    atomic_replace_with_hooks_unlocked(path, replacement, |_| Ok(()), sync_parent_directory)
}

/// Revalidate and durably confirm an already-visible idempotent publication.
/// This is deliberately in the same critical section as replacement.
pub(crate) fn confirm_publication_unchanged(path: &Path, expected: &[u8]) -> io::Result<()> {
    let _publication_guard = crate::lock_ok::lock(atomic_publication_lock());
    validate_exact_contents(path, expected)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "published path has no parent directory",
        )
    })?;
    File::open(path)?.sync_all()?;
    sync_parent_directory(parent)
}

fn validate_exact_contents(path: &Path, expected: &[u8]) -> io::Result<()> {
    let actual = std::fs::read(path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "atomic replacement source {} changed after preflight",
                path.display()
            ),
        ))
    }
}

/// Durably create `path` from a fully-synced unique sibling without replacing
/// an existing target. Linking the sibling into place is atomic and fails with
/// `AlreadyExists` if another writer won the name after the caller's preflight.
/// This is the no-clobber counterpart to [`atomic_replace`], used for new-zone
/// component files whose pre-existing contents must never be overwritten.
pub(crate) fn atomic_create(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _publication_guard = crate::lock_ok::lock(atomic_publication_lock());
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic creation target has no parent directory",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic creation target has no file name",
        )
    })?;

    let mut temp: Option<(PathBuf, File)> = None;
    for _ in 0..100 {
        let sequence = ATOMIC_REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => {
                temp = Some((temp_path, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (temp_path, mut temp_file) = temp.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique atomic-creation temporary file",
        )
    })?;

    let result = (|| {
        temp_file.write_all(bytes)?;
        temp_file.flush()?;
        temp_file.sync_all()?;
        drop(temp_file);

        // hard_link is an atomic no-replace publication on the same filesystem:
        // unlike rename, it cannot clobber a target created after preflight.
        std::fs::hard_link(&temp_path, path)?;
        std::fs::remove_file(&temp_path).map_err(|error| {
            published_but_incomplete(
                "new file was published but its sibling temporary link could not be removed",
                error,
            )
        })?;
        sync_parent_directory(parent).map_err(|error| {
            let kind = error.kind();
            io::Error::new(kind, PublishedButDurabilityUnconfirmed { source: error })
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn atomic_replace_with<F>(path: &Path, bytes: &[u8], before_rename: F) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let _publication_guard = crate::lock_ok::lock(atomic_publication_lock());
    atomic_replace_with_hooks_unlocked(path, bytes, before_rename, sync_parent_directory)
}

pub(crate) fn atomic_replace_with_hooks<F, S>(
    path: &Path,
    bytes: &[u8],
    before_rename: F,
    sync_parent: S,
) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
    S: FnOnce(&Path) -> io::Result<()>,
{
    let _publication_guard = crate::lock_ok::lock(atomic_publication_lock());
    atomic_replace_with_hooks_unlocked(path, bytes, before_rename, sync_parent)
}

fn atomic_replace_with_hooks_unlocked<F, S>(
    path: &Path,
    bytes: &[u8],
    before_rename: F,
    sync_parent: S,
) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
    S: FnOnce(&Path) -> io::Result<()>,
{
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic replacement target has no file name",
        )
    })?;
    let existing_permissions = match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };

    let mut temp: Option<(PathBuf, File)> = None;
    for _ in 0..100 {
        let sequence = ATOMIC_REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(
            ".{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence
        );
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => {
                temp = Some((temp_path, file));
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    let (temp_path, mut temp_file) = temp.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique atomic-replacement temporary file",
        )
    })?;

    let result = (|| {
        if let Some(permissions) = existing_permissions {
            temp_file.set_permissions(permissions)?;
        }
        temp_file.write_all(bytes)?;
        temp_file.flush()?;
        temp_file.sync_all()?;
        drop(temp_file);
        before_rename(&temp_path)?;
        std::fs::rename(&temp_path, path)?;
        sync_parent(parent).map_err(|error| {
            let kind = error.kind();
            io::Error::new(kind, PublishedButDurabilityUnconfirmed { source: error })
        })?;

        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

const NEW_ZONE_TRANSACTION_DIRECTORY: &str = ".new-zone-transactions";
const NEW_ZONE_TRANSACTION_VERSION: &str = "deltamud-new-zone-v1";

fn new_zone_transaction_directory(lib_path: &str) -> PathBuf {
    Path::new(lib_path)
        .join("world")
        .join(NEW_ZONE_TRANSACTION_DIRECTORY)
}

fn new_zone_transaction_marker(lib_path: &str, zone_number: i32) -> PathBuf {
    new_zone_transaction_directory(lib_path).join(format!("{zone_number}.pending"))
}

fn new_zone_transaction_bytes(zone_number: i32) -> Vec<u8> {
    format!("{NEW_ZONE_TRANSACTION_VERSION}\nzone={zone_number}\n").into_bytes()
}

pub(crate) fn new_zone_unresolved_key(zone_number: i32) -> String {
    format!("new-zone:{zone_number}")
}

/// Return every new-zone publication which boot must hide until all six
/// legacy index files have been committed. A malformed journal is an error,
/// never an invitation to load possibly-partial world data.
pub(crate) fn pending_new_zone_publications(lib_path: &str) -> io::Result<HashSet<i32>> {
    let directory = new_zone_transaction_directory(lib_path);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => return Err(error),
    };

    let mut pending = HashSet::new();
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A crash before atomic_create's hard-link publication can leave only
        // a hidden sibling temp. It is not a transaction marker and no world
        // component could have been published after it.
        if name.starts_with('.') {
            continue;
        }
        if !file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unexpected new-zone transaction entry {}",
                    entry.path().display()
                ),
            ));
        }
        let Some(number) = name.strip_suffix(".pending") else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "malformed new-zone transaction marker {}",
                    entry.path().display()
                ),
            ));
        };
        let zone_number = number.parse::<i32>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid zone number in transaction marker {}: {error}",
                    entry.path().display()
                ),
            )
        })?;
        if zone_vnum_bounds(zone_number).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "out-of-range zone number in transaction marker {}",
                    entry.path().display()
                ),
            ));
        }
        let expected = new_zone_transaction_bytes(zone_number);
        let actual = std::fs::read(entry.path())?;
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid contents in new-zone transaction marker {}",
                    entry.path().display()
                ),
            ));
        }
        pending.insert(zone_number);
    }
    Ok(pending)
}

pub(crate) fn new_zone_index_entry_is_pending(
    pending_new_zones: &HashSet<i32>,
    entry: &str,
) -> bool {
    Path::new(entry.trim())
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse::<i32>().ok())
        .is_some_and(|zone_number| pending_new_zones.contains(&zone_number))
}

/// Publish the durable boot gate before creating any new-zone component. The
/// marker is idempotent for an exact retry and is itself fsynced before this
/// function returns.
pub(crate) fn begin_new_zone_publication(lib_path: &str, zone_number: i32) -> io::Result<()> {
    begin_new_zone_publication_with_sync(lib_path, zone_number, sync_parent_directory)
}

fn begin_new_zone_publication_with_sync<S>(
    lib_path: &str,
    zone_number: i32,
    mut sync_directory: S,
) -> io::Result<()>
where
    S: FnMut(&Path) -> io::Result<()>,
{
    if zone_vnum_bounds(zone_number).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "new-zone transaction number is outside the supported range",
        ));
    }
    let directory = new_zone_transaction_directory(lib_path);
    let world_directory = directory.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "new-zone transaction directory has no parent",
        )
    })?;
    match std::fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !std::fs::metadata(&directory)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "new-zone transaction path {} is not a directory",
                        directory.display()
                    ),
                ));
            }
        }
        Err(error) => return Err(error),
    }
    // Confirm the transaction directory's parent entry even on an idempotent
    // retry. The preceding attempt may have created the directory but returned
    // when this exact sync failed; syncing only the child cannot make its name
    // in `world/` crash-durable.
    sync_directory(world_directory)?;
    sync_directory(&directory)?;

    let marker = new_zone_transaction_marker(lib_path, zone_number);
    let expected = new_zone_transaction_bytes(zone_number);
    match std::fs::read(&marker) {
        Ok(actual) if actual == expected => confirm_publication_unchanged(&marker, &expected),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "new-zone transaction marker {} has conflicting contents",
                marker.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => atomic_create(&marker, &expected),
        Err(error) => Err(error),
    }
}

/// Remove the durable boot gate only after every component and index row is
/// confirmed. Exact-byte validation under the publication lock prevents a
/// stale completion from deleting a different transaction marker.
pub(crate) fn complete_new_zone_publication(lib_path: &str, zone_number: i32) -> io::Result<()> {
    let marker = new_zone_transaction_marker(lib_path, zone_number);
    let expected = new_zone_transaction_bytes(zone_number);
    let _publication_guard = crate::lock_ok::lock(atomic_publication_lock());
    validate_exact_contents(&marker, &expected)?;
    std::fs::remove_file(&marker)?;
    let directory = marker.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "new-zone transaction marker has no parent directory",
        )
    })?;
    sync_parent_directory(directory).map_err(|error| {
        let kind = error.kind();
        io::Error::new(kind, PublishedButDurabilityUnconfirmed { source: error })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DgScriptEditMode {
    Main,
    New,
    Delete,
}

// ---------------------------------------------------------------------------
// OLC color cols. C's get_char_cols() (olc.c:482-488) fills its globals with
// the screen.h KNRM/KGRN/KCYN/KYEL `&`-codes when the builder's colour level
// >= C_NRM (PRF_COLOR_2), else KNUL (""). We keep the same `&`-codes as the
// menu constants; the builder-facing send helpers strip them for builders
// whose colour level is below C_NRM (#306).
// ---------------------------------------------------------------------------
pub const NRM: &str = "&n";
pub const GRN: &str = "&G";
pub const CYN: &str = "&C";
pub const YEL: &str = "&Y";

/// screen.h C_NRM: the colour level OLC menus require (PRF_COLOR_2 set).
const C_NRM_LEVEL: i32 = 2;

/// screen.h _clrlevel: 0-3 from PRF_COLOR_1/2.
pub fn colour_level(g: &GameState, ch: CharId) -> i32 {
    use crate::flags::{PRF_COLOR_1, PRF_COLOR_2};
    let (p1, p2) = g
        .get_char(ch)
        .map(|c| {
            (
                c.prf_flags & PRF_COLOR_1 != 0,
                c.prf_flags & PRF_COLOR_2 != 0,
            )
        })
        .unwrap_or((false, false));
    (p1 as i32) + ((p2 as i32) * 2)
}

/// True when the builder sees OLC menu colours (clr(ch, C_NRM)).
pub fn olc_colour_on(g: &GameState, ch: CharId) -> bool {
    colour_level(g, ch) >= C_NRM_LEVEL
}

/// Send OLC text to a connection, stripping the `&`-codes when the builder's
/// colour level is below C_NRM (C get_char_cols handing back KNUL) (#306).
pub fn olc_send(g: &mut GameState, conn: ConnId, msg: &str) {
    let ch = g.descriptors.get(&conn).and_then(|d| d.character);
    let keep = ch.map(|c| olc_colour_on(g, c)).unwrap_or(false);
    if keep {
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.write(msg);
        }
    } else if let Some(d) = g.descriptors.get_mut(&conn) {
        d.write(&crate::connection::strip_color(msg));
    }
}

/// Parse an OLC numeric response while preserving each legacy mode's fallback
/// for syntactically invalid input. A syntactically numeric value outside the
/// `i32` range is different: report it and keep the current editor mode so it
/// cannot silently become the fallback (usually zero or -1).
pub fn parse_i32_input(
    g: &mut GameState,
    conn: ConnId,
    input: &str,
    invalid_fallback: i32,
) -> Option<i32> {
    match crate::text::parse_i32_strict(input) {
        Ok(value) => Some(value),
        Err(crate::text::ParseIntError::Empty | crate::text::ParseIntError::Invalid) => {
            Some(invalid_fallback)
        }
        Err(crate::text::ParseIntError::Overflow) => {
            olc_send(
                g,
                conn,
                "That number is outside the supported 32-bit range.\r\n",
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// EditorKind — which OLC editor a connection is currently driving.
// ---------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKind {
    Redit,
    Oedit,
    Medit,
    Zedit,
    Sedit,
    Aedit,
    Hedit,
    Trigedit,
    Tedit,
}

// ---------------------------------------------------------------------------
// Active-editor registry (replaces STATE(d) == CON_*EDIT). Keyed by ConnId.
// ---------------------------------------------------------------------------
fn active() -> &'static Mutex<HashMap<ConnId, EditorKind>> {
    static ACTIVE: OnceLock<Mutex<HashMap<ConnId, EditorKind>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mark `conn` as actively editing in `kind`. Called by each `do_X` on entry.
pub fn set_active(conn: ConnId, kind: EditorKind) {
    crate::lock_ok::lock(&active()).insert(conn, kind);
}

/// Clear `conn`'s OLC editor (called on save/quit by each editor's parser).
pub fn clear_active(conn: ConnId) {
    crate::lock_ok::lock(&active()).remove(&conn);
}

/// Abort whatever OLC editor `conn` is in WITHOUT saving, then clear active.
/// Called when a descriptor goes away mid-edit (Game::disconnect): the C MUD's
/// `free_olc` / connection teardown drops the editor's working copy so the
/// edited vnum's lock is released and the per-conn state doesn't leak until the
/// next reboot. Dispatches to the owning editor's `abort(conn)`, which removes
/// the conn's working copy (and any text buffer) from that editor's per-conn
/// map. No-op if the conn isn't editing.
pub fn abort_editor(conn: ConnId) {
    if let Some(kind) = active_editor(conn) {
        match kind {
            EditorKind::Redit => crate::redit::abort(conn),
            EditorKind::Oedit => crate::oedit::abort(conn),
            EditorKind::Medit => crate::medit::abort(conn),
            EditorKind::Zedit => crate::zedit::abort(conn),
            EditorKind::Sedit => crate::sedit::abort(conn),
            EditorKind::Aedit => crate::aedit::abort(conn),
            EditorKind::Hedit => crate::hedit::abort(conn),
            EditorKind::Trigedit => crate::trigedit::abort(conn),
            EditorKind::Tedit => {}
        }
    }
    clear_active(conn);
}

/// True if `conn` is currently inside any OLC editor. game.rs consults this to
/// route raw input into `olc_input` instead of the command interpreter.
pub fn in_olc(conn: ConnId) -> bool {
    crate::lock_ok::lock(&active()).contains_key(&conn)
}

/// The currently-active editor kind for `conn`, if any.
pub fn active_editor(conn: ConnId) -> Option<EditorKind> {
    crate::lock_ok::lock(&active()).get(&conn).copied()
}

/// Master input router (CircleMUD: the `case CON_*EDIT:` block of nanny()).
/// Forwards one input line to the active editor's `<kind>_parse`. Does nothing
/// if the connection is not in OLC (defensive — game.rs should gate on
/// `in_olc` first).
pub fn olc_input(g: &mut GameState, conn: ConnId, line: &str) {
    let kind = match active_editor(conn) {
        Some(k) => k,
        None => return,
    };
    match kind {
        EditorKind::Redit => crate::redit::redit_parse(g, conn, line),
        EditorKind::Oedit => crate::oedit::oedit_parse(g, conn, line),
        EditorKind::Medit => crate::medit::medit_parse(g, conn, line),
        EditorKind::Zedit => crate::zedit::zedit_parse(g, conn, line),
        EditorKind::Sedit => crate::sedit::sedit_parse(g, conn, line),
        EditorKind::Aedit => crate::aedit::aedit_parse(g, conn, line),
        EditorKind::Hedit => crate::hedit::hedit_parse(g, conn, line),
        EditorKind::Trigedit => crate::trigedit::trigedit_parse(g, conn, line),
        EditorKind::Tedit => {}
    }
}

// ===========================================================================
// OLC save list (olc.c olc_save_list). A global list of (zone, component) pairs
// that have been edited in memory but not yet written to disk.
// ===========================================================================
#[cfg(not(test))]
fn save_list() -> &'static Mutex<Vec<(i32, i32)>> {
    static SAVE_LIST: OnceLock<Mutex<Vec<(i32, i32)>>> = OnceLock::new();
    SAVE_LIST.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
thread_local! {
    static TEST_SAVE_LIST: std::cell::RefCell<Vec<(i32, i32)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[cfg(not(test))]
fn with_save_list<R>(operation: impl FnOnce(&mut Vec<(i32, i32)>) -> R) -> R {
    let mut list = crate::lock_ok::lock(&save_list());
    operation(&mut list)
}

#[cfg(test)]
fn with_save_list<R>(operation: impl FnOnce(&mut Vec<(i32, i32)>) -> R) -> R {
    TEST_SAVE_LIST.with(|list| operation(&mut list.borrow_mut()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UnresolvedSaveKey {
    Number(i32),
    Name(String),
}

impl std::fmt::Display for UnresolvedSaveKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(key) => write!(formatter, "{key}"),
            Self::Name(key) => formatter.write_str(key),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnresolvedSave {
    kind: EditorKind,
    key: UnresolvedSaveKey,
    published: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnresolvedDiscardOutcome {
    Missing,
    Discarded,
    Published,
}

#[cfg(not(test))]
fn unresolved_publications() -> &'static Mutex<Vec<UnresolvedSave>> {
    static UNRESOLVED: OnceLock<Mutex<Vec<UnresolvedSave>>> = OnceLock::new();
    UNRESOLVED.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
thread_local! {
    static TEST_UNRESOLVED_PUBLICATIONS: std::cell::RefCell<Vec<UnresolvedSave>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[cfg(not(test))]
fn with_unresolved_publications<R>(operation: impl FnOnce(&mut Vec<UnresolvedSave>) -> R) -> R {
    let mut unresolved = crate::lock_ok::lock(&unresolved_publications());
    operation(&mut unresolved)
}

#[cfg(test)]
fn with_unresolved_publications<R>(operation: impl FnOnce(&mut Vec<UnresolvedSave>) -> R) -> R {
    TEST_UNRESOLVED_PUBLICATIONS.with(|unresolved| operation(&mut unresolved.borrow_mut()))
}

fn mark_unresolved_save(kind: EditorKind, key: UnresolvedSaveKey, published: bool) {
    with_unresolved_publications(|unresolved| {
        if let Some(entry) = unresolved
            .iter_mut()
            .find(|entry| entry.kind == kind && entry.key == key)
        {
            // Once rename exposed a candidate, a later pre-publication retry
            // cannot make an explicit discard safe again.
            entry.published |= published;
        } else {
            unresolved.push(UnresolvedSave {
                kind,
                key,
                published,
            });
        }
    });
}

/// Restore crash-surviving new-zone transactions into the process-local exit
/// blocker registry. The prior process's exact publication phase is unknowable,
/// so recovered markers are conservatively non-discardable until an exact retry
/// confirms all files and indexes and removes the durable gate.
pub(crate) fn register_pending_new_zone_publication_blockers(pending: &HashSet<i32>) {
    for &zone_number in pending {
        mark_unresolved_save(
            EditorKind::Zedit,
            UnresolvedSaveKey::Name(new_zone_unresolved_key(zone_number)),
            true,
        );
    }
}

/// Record every failed editor save, including failures before publication.
pub(crate) fn mark_unresolved_save_failure(kind: EditorKind, key: i32, error: &io::Error) {
    mark_unresolved_save(
        kind,
        UnresolvedSaveKey::Number(key),
        replacement_was_published(error),
    );
}

pub(crate) fn mark_unresolved_named_save_failure(kind: EditorKind, key: &str, error: &io::Error) {
    mark_unresolved_save(
        kind,
        UnresolvedSaveKey::Name(key.to_string()),
        replacement_was_published(error),
    );
}

pub(crate) fn clear_unresolved_publication(kind: EditorKind, key: i32) {
    let key = UnresolvedSaveKey::Number(key);
    with_unresolved_publications(|unresolved| {
        unresolved.retain(|entry| entry.kind != kind || entry.key != key)
    });
}

pub(crate) fn clear_unresolved_named_save(kind: EditorKind, key: &str) {
    let key = UnresolvedSaveKey::Name(key.to_string());
    with_unresolved_publications(|unresolved| {
        unresolved.retain(|entry| entry.kind != kind || entry.key != key)
    });
}

/// Dropping a scratch edit resolves a failed pre-publication attempt because
/// neither durable nor live state changed. A post-publication marker is kept:
/// abandoning the editor cannot confirm the rename's crash durability.
pub(crate) fn discard_unresolved_save(kind: EditorKind, key: i32) {
    let key = UnresolvedSaveKey::Number(key);
    with_unresolved_publications(|unresolved| {
        unresolved.retain(|entry| entry.kind != kind || entry.key != key || entry.published)
    });
}

pub(crate) fn discard_unresolved_named_save(
    kind: EditorKind,
    key: &str,
) -> UnresolvedDiscardOutcome {
    let key = UnresolvedSaveKey::Name(key.to_string());
    with_unresolved_publications(|unresolved| {
        let Some(index) = unresolved
            .iter()
            .position(|entry| entry.kind == kind && entry.key == key)
        else {
            return UnresolvedDiscardOutcome::Missing;
        };
        if unresolved[index].published {
            UnresolvedDiscardOutcome::Published
        } else {
            unresolved.remove(index);
            UnresolvedDiscardOutcome::Discarded
        }
    })
}

/// A successful whole-component rewrite confirms every prior published
/// candidate of that editor kind. Pre-publication failures remain tied to the
/// still-open scratch editor and must not be cleared by an unrelated save.
pub(crate) fn clear_published_unresolved_kind(kind: EditorKind) {
    with_unresolved_publications(|unresolved| {
        unresolved.retain(|entry| entry.kind != kind || !entry.published)
    });
}

pub(crate) fn clear_published_unresolved_numeric_range(kind: EditorKind, first: i32, last: i32) {
    with_unresolved_publications(|unresolved| {
        unresolved.retain(|entry| {
            if entry.kind != kind || !entry.published {
                return true;
            }
            !matches!(&entry.key, UnresolvedSaveKey::Number(key) if *key >= first && *key <= last)
        })
    });
}

#[cfg(test)]
pub(crate) fn test_unresolved_publication(kind: EditorKind, key: i32) -> bool {
    let key = UnresolvedSaveKey::Number(key);
    with_unresolved_publications(|unresolved| {
        unresolved
            .iter()
            .any(|entry| entry.kind == kind && entry.key == key)
    })
}

#[cfg(test)]
pub(crate) fn test_unresolved_named_save(kind: EditorKind, key: &str) -> bool {
    let key = UnresolvedSaveKey::Name(key.to_string());
    with_unresolved_publications(|unresolved| {
        unresolved
            .iter()
            .any(|entry| entry.kind == kind && entry.key == key)
    })
}

/// Serialize tests which manipulate the process-global OLC save list. Tests in
/// sibling modules use the same guard so parallel `cargo test` execution cannot
/// make one test flush or remove another test's dirty entries.
#[cfg(test)]
pub(crate) struct TestSaveListGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestSaveListGuard {
    fn drop(&mut self) {
        with_save_list(Vec::clear);
        with_unresolved_publications(Vec::clear);
    }
}

#[cfg(test)]
pub(crate) fn test_save_list_guard() -> TestSaveListGuard {
    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = crate::lock_ok::lock(TEST_LOCK.get_or_init(|| Mutex::new(())));
    with_save_list(Vec::clear);
    with_unresolved_publications(Vec::clear);
    TestSaveListGuard { _guard: guard }
}

/// olc_add_to_save_list: record that `zone` (the builder zone *number*, not
/// rnum) has unsaved `kind` changes. No-op if already present.
pub fn olc_add_to_save_list(zone: i32, kind: i32) {
    with_save_list(|list| {
        if !list.iter().any(|&(z, t)| z == zone && t == kind) {
            // C prepends; order only matters for olc_saveinfo display, where we
            // iterate the whole list, so prepend to mirror C exactly.
            list.insert(0, (zone, kind));
        }
    });
}

/// olc_remove_from_save_list: drop the (zone, kind) entry once it is on disk.
pub fn olc_remove_from_save_list(zone: i32, kind: i32) {
    with_save_list(|list| list.retain(|&(z, t)| !(z == zone && t == kind)));
}

#[cfg(test)]
pub(crate) fn test_pending_save(zone: i32, kind: i32) -> bool {
    with_save_list(|list| list.contains(&(zone, kind)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OlcSaveTarget {
    pub zone: i32,
    pub kind: i32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OlcFlushReport {
    pub attempted: usize,
    pub saved: Vec<OlcSaveTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlcFlushFailure {
    pub target: OlcSaveTarget,
    pub error_kind: io::ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlcFlushError {
    pub report: OlcFlushReport,
    pub failures: Vec<OlcFlushFailure>,
}

impl std::fmt::Display for OlcFlushError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} of {} pending OLC save(s) failed",
            self.failures.len(),
            self.report.attempted
        )?;
        if let Some(failure) = self.failures.first() {
            write!(
                formatter,
                ": zone {} component {}: {}",
                failure.target.zone, failure.target.kind, failure.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for OlcFlushError {}

/// C act.wizard.c:1927-1990 / comm.c:458-510: before copyover or shutdown,
/// every entry on the save list is attempted. Successfully durable entries
/// are removed while every failed entry remains dirty. Callers must treat an
/// error as an exit/exec blocker; the embedded report records partial success
/// without discarding the independent failures (#262).
pub fn flush_save_list_to_disk(
    g: &mut GameState,
) -> std::result::Result<OlcFlushReport, OlcFlushError> {
    let entries: Vec<(i32, i32)> = with_save_list(|list| list.clone());
    let mut report = OlcFlushReport {
        attempted: entries.len(),
        saved: Vec::new(),
    };
    let mut failures: Vec<OlcFlushFailure> = Vec::new();

    for (zone, kind) in entries {
        let target = OlcSaveTarget { zone, kind };
        let result = match kind {
            OLC_SAVE_HELP => crate::hedit::save_all_help(g),
            OLC_SAVE_ACTION => crate::aedit::save_all_actions(g),
            OLC_SAVE_ROOM | OLC_SAVE_OBJ | OLC_SAVE_ZONE | OLC_SAVE_MOB | OLC_SAVE_SHOP => {
                match zone_rnum_for_number(g, zone) {
                    Some(zone_rnum) => try_olc_save_to_disk(g, zone_rnum, kind),
                    None => Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("zone {zone} is not loaded"),
                    )),
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported OLC save component",
            )),
        };

        match result {
            Ok(()) => {
                // Zone writers already remove their own entry after durable
                // publication. This explicit removal also covers the global
                // help/action writers and is intentionally success-only.
                olc_remove_from_save_list(zone, kind);
                report.saved.push(target);
                log::info!("OLC: Reboot saved zone {} component {}.", zone, kind);
            }
            Err(error) => {
                log::warn!(
                    "SYSERR: OLC: Reboot could not save zone {} component {}: {}",
                    zone,
                    kind,
                    error
                );
                failures.push(OlcFlushFailure {
                    target,
                    error_kind: error.kind(),
                    message: error.to_string(),
                });
            }
        }
    }

    // Re-read after attempting the ordinary dirty list: a successful whole
    // component retry may have resolved a post-publication marker during this
    // same flush. Pre-publication failures have no publishable live candidate
    // and therefore remain blockers until retry or explicit discard.
    let unresolved = with_unresolved_publications(|unresolved| unresolved.clone());
    report.attempted += unresolved.len();
    failures.extend(unresolved.into_iter().map(|entry| {
        let report_key = match &entry.key {
            UnresolvedSaveKey::Number(key) => *key,
            UnresolvedSaveKey::Name(_) => -1,
        };
        let phase = if entry.published {
            "was published but needs a durability-confirming retry"
        } else {
            "failed before publication and needs a retry or explicit discard"
        };
        OlcFlushFailure {
            target: OlcSaveTarget {
                zone: report_key,
                kind: -1,
            },
            error_kind: io::ErrorKind::Other,
            message: format!("{:?} entry {} {phase}", entry.kind, entry.key),
        }
    }));

    if failures.is_empty() {
        Ok(report)
    } else {
        Err(OlcFlushError { report, failures })
    }
}

/// olc_saveinfo: tell the immortal which OLC components still need saving.
pub fn olc_saveinfo(g: &mut GameState, ch: CharId) {
    let entries: Vec<(i32, i32)> = with_save_list(|list| list.clone());
    let unresolved = with_unresolved_publications(|unresolved| unresolved.clone());
    if entries.is_empty() && unresolved.is_empty() {
        g.send_to_char(ch, "The database is up to date.\r\n");
        return;
    }
    // C olc.c:393-408: Help/Actions lines need >= LVL_IMMORT; zone lines
    // need can_edit_zone on the listed zone (#278).
    let authority = validated_olc_trust(g, ch).unwrap_or(-1);
    let mut out = String::from("The following OLC components need saving:\r\n");
    let mut any = false;
    for (zone, kind) in entries {
        if kind != OLC_SAVE_HELP && kind != OLC_SAVE_ACTION {
            let owned = zone_rnum_for_number(g, zone)
                .map(|zr| can_edit_zone(g, ch, zr))
                .unwrap_or(false);
            if !owned {
                continue;
            }
        } else if authority < i32::from(LVL_IMMORT) {
            continue;
        }
        let line = match kind {
            OLC_SAVE_HELP => " - Help Entries.\r\n".to_string(),
            OLC_SAVE_ACTION => " - Actions.\r\n".to_string(),
            t if (t as usize) < SAVE_INFO_MSG.len() => {
                format!(" - {} for zone {}.\r\n", SAVE_INFO_MSG[t as usize], zone)
            }
            _ => continue,
        };
        out.push_str(&line);
        any = true;
    }
    for entry in unresolved {
        let may_view = match (&entry.kind, &entry.key) {
            (EditorKind::Aedit | EditorKind::Hedit, _) => authority >= i32::from(LVL_IMMORT),
            (EditorKind::Tedit, _) => authority >= i32::from(LVL_GRGOD),
            (EditorKind::Zedit, UnresolvedSaveKey::Number(zone)) => {
                zone_rnum_for_number(g, *zone).is_some_and(|zr| can_edit_zone(g, ch, zr))
            }
            (
                EditorKind::Medit | EditorKind::Sedit | EditorKind::Trigedit,
                UnresolvedSaveKey::Number(vnum),
            ) => real_zone(g, *vnum).is_some_and(|zr| can_edit_zone(g, ch, zr)),
            _ => has_implementor_olc_authority(g, ch),
        };
        if !may_view {
            continue;
        }
        let phase = if entry.published {
            "has an unconfirmed published save"
        } else {
            "has a failed unpublished save awaiting retry or discard"
        };
        out.push_str(&format!(
            " - {:?} entry {} {phase}.\r\n",
            entry.kind, entry.key
        ));
        any = true;
    }
    if any {
        g.send_to_char(ch, &out);
    } else {
        g.send_to_char(ch, "The database is up to date.\r\n");
    }
}

// ===========================================================================
// real_zone: find the loaded-zone *index* (rnum) owning a vnum (olc.c).
// DeltaMUD zones own the vnum band [number*100 .. top]. Returns None if no zone
// owns it.
// ===========================================================================
pub fn real_zone(g: &GameState, vnum: i32) -> Option<usize> {
    if vnum < 0 {
        return None;
    }
    g.zones.iter().position(|z| z.contains_vnum(vnum))
}

/// Resolve a persisted zone number without repeating unchecked `* 100`
/// arithmetic at command/save-list call sites.
fn zone_rnum_for_number(g: &GameState, zone_number: i32) -> Option<usize> {
    let (first, _) = zone_vnum_bounds(zone_number)?;
    real_zone(g, first)
}

/// True when `ch` may edit the loaded zone at `zone_rnum`.
/// True when object vnum `obj_vnum` lives in a zone the builder can edit
/// (sedit.c:1181-1188 product gate; #267).
pub fn obj_proto_in_owned_zone(g: &GameState, ch: CharId, obj_vnum: i32) -> bool {
    match g.obj_protos.get(&obj_vnum) {
        Some(p) => real_zone(g, p.vnum)
            .map(|zr| can_edit_zone(g, ch, zr))
            .unwrap_or(false),
        None => false,
    }
}

/// Persisted command trust is the OLC authority source. Durable player rows
/// are validated at startup, but this runtime check keeps direct handler calls
/// and later in-memory mutations fail-closed as well.
pub(crate) fn validated_olc_trust(g: &GameState, ch: CharId) -> Option<i32> {
    g.principal_authority(ch)
        .filter(|principal| principal.is_authenticated_player())
        .map(|principal| principal.authority)
}

pub(crate) fn has_implementor_olc_authority(g: &GameState, ch: CharId) -> bool {
    validated_olc_trust(g, ch) == Some(i32::from(LVL_IMPL))
}

/// Exact authenticated session identity retained by a long-lived OLC editor.
/// The editor input router is keyed only by `ConnId`, so every publication
/// must compare this tuple with the live descriptor instead of trusting that
/// the connection still belongs to the player who opened the editor.
pub(crate) type OlcAuthorization = AuthenticatedCommandRequest;

/// Capture the player/session tuple when an editor opens. Production editor
/// entry always comes through the authenticated command dispatcher. Unit tests
/// also exercise the individual editor entry points directly, so their build
/// may reconstruct the same tuple from an otherwise valid descriptor.
pub(crate) fn capture_olc_authorization(g: &GameState, ch: CharId) -> Option<OlcAuthorization> {
    if let Some(request) = crate::interpreter::authenticated_command_request(g, ch) {
        return Some(request);
    }

    #[cfg(test)]
    {
        let authority = g
            .principal_authority(ch)
            .filter(|authority| authority.is_authenticated_player())?;
        let descriptor = authority.descriptor?;
        let principal = g.get_char(authority.principal)?;
        return Some(AuthenticatedCommandRequest {
            requester_body: ch,
            requester_principal: authority.principal,
            descriptor,
            idnum: principal.idnum,
        });
    }

    #[cfg(not(test))]
    None
}

/// Revalidate a retained OLC session at the live-memory or disk publication
/// boundary. This repeats the command-table trust/grant checks after any
/// authority updates which happened while the scratch editor was open, then
/// optionally re-resolves and checks the current zone ACL. Exact Implementor
/// trust continues to override the builder list through `can_edit_zone`.
pub(crate) fn revalidate_olc_authorization(
    g: &GameState,
    authorization: OlcAuthorization,
    implementor_editor: bool,
    zone_rnum: Option<usize>,
) -> io::Result<CharId> {
    let (godcmd_set, godcmd) = if implementor_editor {
        (3, crate::gcmd::GCMD3_IMPOLC)
    } else {
        (2, crate::gcmd::GCMD2_OLC)
    };
    if !g.authenticated_command_request_is_current(
        authorization,
        i32::from(LVL_IMMORT),
        godcmd_set,
        godcmd,
    ) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OLC editor authorization is no longer current",
        ));
    }
    if zone_rnum.is_some_and(|zone_rnum| !can_edit_zone(g, authorization.requester_body, zone_rnum))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OLC editor zone ownership is no longer current",
        ));
    }
    Ok(authorization.requester_body)
}

pub fn can_edit_zone(g: &GameState, ch: CharId, zone_rnum: usize) -> bool {
    let Some(principal) = g
        .principal_authority(ch)
        .filter(|principal| principal.is_authenticated_player())
    else {
        return false;
    };
    if principal.authority == i32::from(LVL_IMPL) {
        return true;
    }
    let Some(principal_character) = g.get_char(principal.principal) else {
        return false;
    };
    let Some(zone) = g.zones.get(zone_rnum) else {
        return false;
    };
    zone_builder_token_matches(&zone.builders, &principal_character.player.name)
}

/// Builder ACLs are whitespace-separated character names. A legacy file may
/// punctuate a token with a trailing comma (for example `Michael Fara,
/// Claude`), but abbreviations are never identities: `Far` must not inherit
/// `Fara`'s zone authority.
pub(crate) fn zone_builder_token_matches(builders: &str, player_name: &str) -> bool {
    builders.split_whitespace().any(|token| {
        token
            .trim_end_matches(',')
            .eq_ignore_ascii_case(player_name)
    })
}

/// New accounts may not claim a name currently delegated by any loaded zone.
/// Existing accounts are checked in the database first and remain able to log
/// in, so this closes deleted-name reuse without locking out real builders.
pub(crate) fn name_reserved_by_zone_acl(g: &GameState, player_name: &str) -> bool {
    g.zones
        .iter()
        .any(|zone| zone_builder_token_matches(&zone.builders, player_name))
}

/// Shared DG script-list menu used by redit/oedit/medit. This is the Rust
/// analogue of dg_olc.c `dg_script_menu`: edit a prototype entity's attached
/// trigger vnum list.
pub fn dg_script_menu(g: &mut GameState, conn: ConnId, kind: i32, entity_vnum: i32) {
    let mut out = String::from("     Script Editor\r\n\r\n     Trigger List:\r\n");
    let triggers = crate::dg_db_scripts::proto_trigger_vnums(kind, entity_vnum);
    if triggers.is_empty() {
        out.push_str("     <none>\r\n");
    } else {
        for (idx, trig_vnum) in triggers.iter().enumerate() {
            let (name, mismatch) = {
                let rnum = crate::dg_db_scripts::real_trigger(*trig_vnum);
                if rnum < 0 {
                    ("unknown trigger".to_string(), true)
                } else {
                    match crate::dg_db_scripts::trig_proto(rnum as usize) {
                        Some(proto) => (proto.name, proto.attach_type != kind),
                        None => ("unknown trigger".to_string(), true),
                    }
                }
            };
            out.push_str(&format!(
                "     {:2}) [{}{}{}] {}{}{}",
                idx + 1,
                CYN,
                trig_vnum,
                NRM,
                CYN,
                name,
                NRM
            ));
            if mismatch {
                out.push_str(&format!(
                    "   {}** Mis-matched Trigger Type **{}\r\n",
                    GRN, NRM
                ));
            } else {
                out.push_str("\r\n");
            }
        }
    }
    out.push_str(&format!(
        "\r\n {}N{})  New trigger for this script\r\n\
         {}D{})  Delete a trigger in this script\r\n\
         {}X{})  Exit Script Editor\r\n\r\n\
             Enter choice :",
        GRN, NRM, GRN, NRM, GRN, NRM
    ));
    send_to_conn(g, conn, &out);
}

/// Parse one line of the shared DG script-list editor. Returns false when the
/// user exits back to the owning editor's main menu.
pub fn dg_script_edit_parse(
    g: &mut GameState,
    conn: ConnId,
    kind: i32,
    entity_vnum: i32,
    mode: &mut DgScriptEditMode,
    line: &str,
) -> bool {
    match *mode {
        DgScriptEditMode::Main => {
            match line.trim().chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('x') => return false,
                Some('n') => {
                    send_to_conn(g, conn, "\r\nPlease enter position, vnum   (ex: 1, 200):");
                    *mode = DgScriptEditMode::New;
                }
                Some('d') => {
                    send_to_conn(g, conn, "     Which entry should be deleted?  0 to abort :");
                    *mode = DgScriptEditMode::Delete;
                }
                _ => dg_script_menu(g, conn, kind, entity_vnum),
            }
        }
        DgScriptEditMode::New => {
            let (pos, trig_vnum) = match parse_script_position_vnum(line) {
                Ok(Some(parsed)) => parsed,
                Err(crate::text::ParseIntError::Overflow) => {
                    send_to_conn(
                        g,
                        conn,
                        "That position or trigger VNUM is outside the supported 32-bit range.\r\nPlease enter position, vnum   (ex: 1, 200):",
                    );
                    return true;
                }
                Ok(None)
                | Err(crate::text::ParseIntError::Empty | crate::text::ParseIntError::Invalid) => {
                    // C dg_olc.c:766-783: an unparseable line leaves vnum at -1 →
                    // real_trigger() < 0 → "Invalid Trigger VNUM!" re-prompt (#304).
                    send_to_conn(
                        g,
                        conn,
                        "Invalid Trigger VNUM!\r\nPlease enter position, vnum   (ex: 1, 200):",
                    );
                    return true;
                }
            };
            if pos == 0 || trig_vnum == 0 {
                dg_script_menu(g, conn, kind, entity_vnum);
                *mode = DgScriptEditMode::Main;
                return true;
            }
            if crate::dg_db_scripts::real_trigger(trig_vnum) < 0 {
                send_to_conn(
                    g,
                    conn,
                    "Invalid Trigger VNUM!\r\nPlease enter position, vnum   (ex: 1, 200):",
                );
                return true;
            }
            if !can_edit_trigger_zone(g, conn, trig_vnum) {
                send_to_conn(
                    g,
                    conn,
                    "You do not have permissions to that zone.\r\nPlease enter position, vnum   (ex: 1, 200):",
                );
                return true;
            }
            if crate::dg_db_scripts::insert_proto_trigger(kind, entity_vnum, trig_vnum, pos) {
                mark_dg_script_dirty(g, kind, entity_vnum);
            }
            *mode = DgScriptEditMode::Main;
            dg_script_menu(g, conn, kind, entity_vnum);
        }
        DgScriptEditMode::Delete => {
            let pos = match crate::text::parse_i32_strict(line) {
                Ok(value) if value > 0 => value as usize,
                Ok(_)
                | Err(crate::text::ParseIntError::Empty | crate::text::ParseIntError::Invalid) => 0,
                Err(crate::text::ParseIntError::Overflow) => {
                    send_to_conn(
                        g,
                        conn,
                        "That entry number is outside the supported 32-bit range.\r\n",
                    );
                    return true;
                }
            };
            if pos != 0 && crate::dg_db_scripts::remove_proto_trigger_at(kind, entity_vnum, pos) {
                mark_dg_script_dirty(g, kind, entity_vnum);
            }
            *mode = DgScriptEditMode::Main;
            dg_script_menu(g, conn, kind, entity_vnum);
        }
    }
    true
}

fn parse_script_position_vnum(
    line: &str,
) -> Result<Option<(usize, i32)>, crate::text::ParseIntError> {
    let tokens: Vec<&str> = line
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    let nums: Vec<i32> = tokens
        .iter()
        .map(|token| crate::text::parse_i32_strict(token))
        .collect::<Result<_, _>>()?;
    Ok(match nums.as_slice() {
        [vnum] => Some((999, *vnum)),
        [pos, vnum, ..] => Some(((*pos).max(0) as usize, *vnum)),
        _ => None,
    })
}

fn can_edit_trigger_zone(g: &GameState, conn: ConnId, trig_vnum: i32) -> bool {
    let Some(ch) = g.descriptors.get(&conn).and_then(|d| d.character) else {
        return false;
    };
    real_zone(g, trig_vnum)
        .map(|zr| can_edit_zone(g, ch, zr))
        .unwrap_or(false)
}

fn mark_dg_script_dirty(g: &mut GameState, kind: i32, entity_vnum: i32) {
    let Some(zr) = real_zone(g, entity_vnum) else {
        return;
    };
    let Some(zone) = g.zones.get(zr) else {
        return;
    };
    let save_kind = match kind {
        crate::dg_handler::MOB_TRIGGER => OLC_SAVE_MOB,
        crate::dg_handler::OBJ_TRIGGER => OLC_SAVE_OBJ,
        crate::dg_handler::WLD_TRIGGER => OLC_SAVE_ROOM,
        _ => return,
    };
    olc_add_to_save_list(zone.number, save_kind);
}

fn send_to_conn(g: &mut GameState, conn: ConnId, msg: &str) {
    if let Some(ch) = g.descriptors.get(&conn).and_then(|d| d.character) {
        g.send_to_char(ch, msg);
    } else if let Some(d) = g.descriptors.get_mut(&conn) {
        d.write(msg);
    }
}

// ===========================================================================
// olc_save_to_disk: the per-component save dispatcher (olc.c do_olc save arm).
// Writes a single zone's component to its CircleMUD world file. Each writer
// removes its save-list entry only after the durable replacement succeeds.
// ===========================================================================
fn try_olc_save_to_disk(g: &mut GameState, zone_rnum: usize, kind: i32) -> io::Result<()> {
    match kind {
        OLC_SAVE_ROOM => crate::redit::redit_save_to_disk(g, zone_rnum),
        OLC_SAVE_OBJ => crate::oedit::oedit_save_to_disk(g, zone_rnum),
        OLC_SAVE_ZONE => crate::zedit::zedit_save_to_disk(g, zone_rnum),
        OLC_SAVE_MOB => crate::medit::medit_save_to_disk(g, zone_rnum),
        OLC_SAVE_SHOP => crate::sedit::sedit_save_zone_to_disk(g, zone_rnum),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported OLC save component",
        )),
    }
}

pub fn olc_save_to_disk(g: &mut GameState, zone_rnum: usize, kind: i32) {
    if let Err(err) = try_olc_save_to_disk(g, zone_rnum, kind) {
        let zone = g.zones.get(zone_rnum).map(|z| z.number).unwrap_or(-1);
        log::warn!(
            "SYSERR: OLC: could not save zone {} component {}: {}",
            zone,
            kind,
            err
        );
    }
}

// ===========================================================================
// Shared menu-rendering helpers used by every editor (utils.c sprintbit /
// sprinttype). Local copies because the per-command versions are private.
// ===========================================================================

/// sprintbit: name every set bit of `bits` using `table` (a "\n"-terminated
/// name list). Stops at the sentinel; unnamed bits past the table are skipped.
pub fn sprintbit(bits: i64, table: &[&str]) -> String {
    let mut out = String::new();
    for (i, n) in table.iter().enumerate() {
        if *n == "\n" {
            break;
        }
        if bits & (1 << i) != 0 {
            out.push_str(n);
            out.push(' ');
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

/// sprinttype: ordinal lookup into a "\n"-terminated name table.
pub fn sprinttype(t: i32, table: &[&str]) -> String {
    if t >= 0 && (t as usize) < table.len() && table[t as usize] != "\n" {
        table[t as usize].to_string()
    } else {
        "UNDEFINED".to_string()
    }
}

/// strip_string (olc.c): drop '\r' so a "\r\n"-bearing buffer writes Unix-style
/// to the world file (the loader re-adds CRLF semantics on read).
pub fn strip_cr(s: &str) -> String {
    s.chars().filter(|&c| c != '\r').collect()
}

// ===========================================================================
// do_copy / do_rlink (C olc.c:735 / :880) — complete in the C source but
// never registered in cmd_info, so builders could never reach them. Ported
// and registered as the "finish the game" activations (registered in
// COMPATIBILITY.md).
// ===========================================================================

const COPY_FORMAT: &str = "Usage:  copy { room | obj } <source> <target>\r\n";
const RLINK_FORMAT: &str = "Usage:  rlink <dir> <connect|disconnect> <1|2> [target]\r\n";

/// C olc.c:646 zone_number(): the builder NUMBER of the zone owning this
/// entity. Rooms resolve through real_zone; objects/mobs use vnum/100 (the
/// author's own truncation formula).
fn zone_number_of_room(g: &GameState, rnum: usize) -> i32 {
    g.rooms
        .get(rnum)
        .and_then(|r| real_zone(g, r.number))
        .and_then(|zr| g.zones.get(zr).map(|z| z.number))
        .unwrap_or(0)
}

/// C olc.c:702 copy_room: name/description/sector/flags only — the author
/// deliberately skipped extra descriptions ("I think it will stay that way.").
fn copy_room_fields(g: &mut GameState, src: usize, targ: usize) {
    let (name, description, sector_type, room_flags) = {
        let r = g.room(src);
        (
            r.name.clone(),
            r.description.clone(),
            r.sector_type,
            r.room_flags,
        )
    };
    let t = g.room_mut(targ);
    t.name = name;
    t.description = description;
    t.sector_type = sector_type;
    t.room_flags = room_flags;
}

/// C olc.c:722 copy_object: the description/flag fields of one prototype onto
/// another (worn_on copied in C is an instance artifact and meaningless on a
/// proto — skipped).
fn copy_object_fields(g: &mut GameState, src_vnum: i32, targ_vnum: i32) {
    let src = match g.obj_protos.get(&src_vnum) {
        Some(p) => p.clone(),
        None => return,
    };
    if let Some(t) = g.obj_protos.get_mut(&targ_vnum) {
        t.name = src.name;
        t.description = src.description;
        t.short_desc = src.short_desc;
        t.action_description = src.action_description;
        t.ex_descriptions = src.ex_descriptions;
        t.obj_type = src.obj_type;
        t.extra_flags = src.extra_flags;
        t.wear_flags = src.wear_flags;
        t.weight = src.weight;
        t.cost = src.cost;
        t.rent = src.rent;
        t.values = src.values;
        t.curr_slots = src.curr_slots;
        t.total_slots = src.total_slots;
        t.bitvector = src.bitvector;
        t.obj_class = src.obj_class;
        t.min_level = src.min_level;
    }
}

/// C olc.c:735 do_copy.
pub fn do_copy(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (ty, rest) = crate::interpreter::one_argument(arg);
    let (src_num, rest2) = crate::interpreter::one_argument(&rest);
    let (targ_num, _) = crate::interpreter::one_argument(rest2);

    if ty.is_empty() || src_num.is_empty() {
        g.send_to_char(ch, COPY_FORMAT);
        return;
    }
    // C olc.c:748 tests `room_or_obj == OBJECT` BEFORE the type is parsed, so
    // this guard can never fire there; the parse-aware placement here is the
    // evident intent (registered).
    let is_obj = crate::interpreter::is_abbrev(&ty, "obj");
    if targ_num.is_empty() && is_obj {
        g.send_to_char(ch, "You must specify a target when copying objects.\r\n");
        return;
    }

    let is_room = crate::interpreter::is_abbrev(&ty, "room");
    let numeric = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_digit())
            && crate::text::parse_i32_strict(s).is_ok()
    };

    let (room_or_obj, vnum_src, rnum_src, vnum_targ, rnum_targ, save_zone) =
        if is_room && numeric(&src_num) {
            let vnum_src = crate::text::parse_i32_strict(&src_num).unwrap_or(-1);
            let rnum_src = g.real_room(vnum_src);
            let (vnum_targ, rnum_targ) = if targ_num.is_empty() {
                match g.get_char(ch).and_then(|c| c.in_room) {
                    Some(r) => (g.rooms[r].number, Some(r)),
                    None => return,
                }
            } else if numeric(&targ_num) {
                let v = crate::text::parse_i32_strict(&targ_num).unwrap_or(-1);
                (v, g.real_room(v))
            } else {
                g.send_to_char(ch, COPY_FORMAT);
                return;
            };
            let save_zone = rnum_targ.map(|r| zone_number_of_room(g, r)).unwrap_or(0);
            (
                0,
                vnum_src,
                rnum_src.is_some(),
                vnum_targ,
                rnum_targ.is_some(),
                save_zone,
            )
        } else if is_obj && !targ_num.is_empty() && numeric(&src_num) && numeric(&targ_num) {
            let vnum_src = crate::text::parse_i32_strict(&src_num).unwrap_or(-1);
            let vnum_targ = crate::text::parse_i32_strict(&targ_num).unwrap_or(-1);
            let rnum_src = g.obj_protos.contains_key(&vnum_src);
            let rnum_targ = g.obj_protos.contains_key(&vnum_targ);
            (
                1,
                vnum_src,
                rnum_src,
                vnum_targ,
                rnum_targ,
                vnum_targ / 100, // C zone_number OBJECT formula
            )
        } else {
            g.send_to_char(ch, COPY_FORMAT);
            return;
        };

    let (src_ok, targ_ok) = (rnum_src, rnum_targ);
    if !src_ok || !targ_ok {
        g.send_to_char(
            ch,
            &format!(
                "The source and target {}s must both currently exist.\r\n",
                if room_or_obj == 1 { "object" } else { "room" }
            ),
        );
        return;
    }
    let save_zone_rnum = match zone_rnum_for_number(g, save_zone) {
        Some(zone_rnum) => zone_rnum,
        None => {
            g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
            return;
        }
    };
    if !can_edit_zone(g, ch, save_zone_rnum) {
        g.send_to_char(ch, "You cannot edit that zone.\r\n");
        return;
    }

    if room_or_obj == 0 {
        let s = g.real_room(vnum_src).unwrap();
        let t = g.real_room(vnum_targ).unwrap();
        copy_room_fields(g, s, t);
    } else {
        copy_object_fields(g, vnum_src, vnum_targ);
    }

    g.send_to_char(
        ch,
        &format!(
            "You copy {} {} to {}.\r\n",
            if room_or_obj == 0 { "room" } else { "object" },
            vnum_src,
            vnum_targ
        ),
    );
    olc_add_to_save_list(save_zone, room_or_obj); // C: ROOM==OLC_SAVE_ROOM, OBJECT==OLC_SAVE_OBJ
}

/// C olc.c:767 create_dir: an empty exit in `dir` ("No target yet").
fn create_dir(g: &mut GameState, rnum: usize, dir: usize) -> bool {
    let Some(room) = g.rooms.get_mut(rnum) else {
        return false;
    };
    if room.exits[dir].is_some() {
        return false;
    }
    room.exits[dir] = Some(crate::room::Exit {
        description: Some("You see nothing special.\r\n".to_string()),
        keyword: None,
        exit_info: 0,
        key: -1,
        to_room: NOWHERE,
    });
    true
}

/// C olc.c:785 free_dir: remove the exit entirely.
fn free_dir(g: &mut GameState, rnum: usize, dir: usize) -> bool {
    g.rooms
        .get_mut(rnum)
        .map(|room| room.exits[dir].take().is_some())
        .unwrap_or(false)
}

/// C olc.c:880 do_rlink ("The big baby").
pub fn do_rlink(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (direction, rest) = crate::interpreter::one_argument(arg);
    let (command, rest2) = crate::interpreter::one_argument(&rest);
    let (ty, rest3) = crate::interpreter::one_argument(rest2);
    let (target, _) = crate::interpreter::one_argument(rest3);

    if direction.is_empty() || command.is_empty() || ty.is_empty() {
        g.send_to_char(ch, RLINK_FORMAT);
        return;
    }
    let type_int: i32 = match ty.parse() {
        Ok(v) if v == 1 || v == 2 => v,
        _ => {
            g.send_to_char(ch, RLINK_FORMAT);
            return;
        }
    };

    let base_rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let vnum_base = g.rooms[base_rnum].number;

    let disconnect = crate::interpreter::is_abbrev(&command, "disconnect");
    let connect = crate::interpreter::is_abbrev(&command, "connect");
    let mut create_new_room = false;
    let mut vnum_targ: i32 = 0;
    let mut rnum_targ: Option<usize> = None;
    if target.is_empty() && !disconnect {
        create_new_room = true;
    } else if !target.is_empty()
        && target.chars().all(|c| c.is_ascii_digit())
        && crate::text::parse_i32_strict(&target).is_ok()
    {
        vnum_targ = crate::text::parse_i32_strict(&target).unwrap_or(-1);
        rnum_targ = g.real_room(vnum_targ);
    } else {
        g.send_to_char(ch, RLINK_FORMAT);
        return;
    }
    // C checks `rnum_targ < 0` here; a given-but-missing target is a format
    // error, matching the C flow (real_room returning < 0).
    if !create_new_room && target.is_empty() {
        g.send_to_char(ch, RLINK_FORMAT);
        return;
    }

    let save_zone_1 = zone_number_of_room(g, base_rnum);
    let Some(base_zone_rnum) = zone_rnum_for_number(g, save_zone_1) else {
        g.send_to_char(ch, "You cannot create exits in this zone.\r\n");
        return;
    };
    if !can_edit_zone(g, ch, base_zone_rnum) {
        g.send_to_char(ch, "You cannot create exits in this zone.\r\n");
        return;
    }

    let mut save_zone_2;
    if create_new_room {
        // C olc.c:950-970: first free vnum in the builder's zone becomes a new
        // "An unfinished room" (the redit internal path). C's unreachable
        // "no space" guard is repaired here: if no free vnum exists we say so
        // instead of falling through with target 0 (registered).
        let Some(zr) = real_zone(g, vnum_base) else {
            return;
        };
        let (zone_start, top_room) = match g.zones.get(zr) {
            Some(z) => match z.vnum_start() {
                Some(start) => (start, z.top),
                None => return,
            },
            None => return,
        };
        let mut created: Option<i32> = None;
        for k in zone_start..=top_room {
            if g.real_room(k).is_none() {
                created = Some(k);
                break;
            }
        }
        let Some(k) = created else {
            g.send_to_char(ch, "Cannot create a new room in this zone!\r\n");
            return;
        };
        let room = crate::room::Room::new(
            k,
            zr as i32,
            "An unfinished room".to_string(),
            "You are in an unfinished room.\r\n".to_string(),
        );
        g.add_room(room);
        vnum_targ = k;
        rnum_targ = g.real_room(k);
        save_zone_2 = save_zone_1;
        g.send_to_char(ch, &format!("You have created new room #{}.\r\n", k));
    } else {
        let Some(rt) = rnum_targ else {
            g.send_to_char(ch, RLINK_FORMAT);
            return;
        };
        save_zone_2 = zone_number_of_room(g, rt);
    }

    if type_int == 2
        && !zone_rnum_for_number(g, save_zone_2).is_some_and(|zr| can_edit_zone(g, ch, zr))
    {
        g.send_to_char(ch, "You cannot create exits in the target zone.\r\n");
        return;
    }

    let dir = match direction.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('n') => NORTH,
        Some('e') => EAST,
        Some('s') => SOUTH,
        Some('w') => WEST,
        Some('u') => UP,
        Some('d') => DOWN,
        _ => {
            g.send_to_char(ch, "No such direction!\r\n");
            return;
        }
    };

    if connect {
        if g.rooms[base_rnum].exits[dir].is_none() {
            create_dir(g, base_rnum, dir);
        }
        if let Some(room) = g.rooms.get_mut(base_rnum) {
            if let Some(e) = room.exits[dir].as_mut() {
                e.to_room = vnum_targ;
            }
        }
        if type_int == 2 {
            if let Some(rt) = rnum_targ {
                let rdir = REV_DIR[dir];
                if g.rooms[rt].exits[rdir].is_none() {
                    create_dir(g, rt, rdir);
                }
                if let Some(room) = g.rooms.get_mut(rt) {
                    if let Some(e) = room.exits[rdir].as_mut() {
                        e.to_room = vnum_base;
                    }
                }
            }
            if save_zone_2 == 0 {
                save_zone_2 = rnum_targ.map(|rt| zone_number_of_room(g, rt)).unwrap_or(0);
            }
        }
    } else if disconnect {
        // C dereferences the exit without a NULL check here (crash on a
        // missing own exit); the guard is the registered repair.
        let own_to = g.rooms[base_rnum].exits[dir].as_ref().map(|e| e.to_room);
        if type_int == 2 {
            match own_to {
                Some(to) if to > 0 => {
                    let Some(to_rnum) = g.real_room(to) else {
                        g.send_to_char(
                            ch,
                            "The exit destination does not exist; no exits changed.\r\n",
                        );
                        return;
                    };
                    if rnum_targ != Some(to_rnum) {
                        g.send_to_char(
                            ch,
                            "The target does not match this exit; no exits changed.\r\n",
                        );
                        return;
                    }
                    let actual_zone = zone_number_of_room(g, to_rnum);
                    if !zone_rnum_for_number(g, actual_zone)
                        .is_some_and(|zr| can_edit_zone(g, ch, zr))
                    {
                        g.send_to_char(ch, "You cannot remove exits in the target zone.\r\n");
                        return;
                    }
                    let reverse = REV_DIR[dir];
                    let reciprocal_to = g.rooms[to_rnum].exits[reverse]
                        .as_ref()
                        .map(|exit| exit.to_room);
                    if reciprocal_to != Some(vnum_base) {
                        g.send_to_char(
                            ch,
                            "There is no matching reciprocal exit; no exits changed.\r\n",
                        );
                        return;
                    }
                    // Both sides have been validated before either mutation.
                    free_dir(g, to_rnum, reverse);
                    if !free_dir(g, base_rnum, dir) {
                        g.send_to_char(ch, "No such exit!\r\n");
                        return;
                    }
                    save_zone_2 = actual_zone;
                }
                _ => {
                    g.send_to_char(ch, "No such exit!\r\n");
                    return;
                }
            }
        } else {
            match own_to {
                Some(to) if to > 0 => {
                    free_dir(g, base_rnum, dir);
                }
                _ => {
                    g.send_to_char(ch, "No such exit!\r\n");
                    return;
                }
            }
        }
    } else {
        g.send_to_char(
            ch,
            "Invalid command type.  Valid choices are connect and disconnect.\r\n",
        );
        return;
    }

    if connect {
        g.send_to_char(
            ch,
            &format!(
                "You make an exit {} to room {}.\r\n",
                crate::constants::DIRS[dir],
                vnum_targ
            ),
        );
    } else {
        g.send_to_char(ch, "Exit deleted.\r\n");
    }

    olc_add_to_save_list(save_zone_1, OLC_SAVE_ROOM);
    if save_zone_2 != 0 {
        olc_add_to_save_list(save_zone_2, OLC_SAVE_ROOM);
    }
}

// ===========================================================================
// do_olc — the OLC command interface (olc.c do_olc). Generic parsing, then a
// hand-off to the right sub-editor's `do_X`, or a save.
// ===========================================================================
pub fn do_olc(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    // No screwing around as a mobile.
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        return;
    }

    if subcmd == SCMD_OLC_SAVEINFO {
        olc_saveinfo(g, ch);
        return;
    }

    let Some(command_authorization) = capture_olc_authorization(g, ch) else {
        g.send_to_char(ch, "You do not have permission to use OLC.\r\n");
        return;
    };
    let implementor_editor = matches!(subcmd, SCMD_OLC_HEDIT | SCMD_OLC_AEDIT);
    if revalidate_olc_authorization(g, command_authorization, implementor_editor, None).is_err() {
        g.send_to_char(ch, "You do not have permission to use OLC.\r\n");
        return;
    }

    // Two-argument parse: buf1 = first word, buf2 = second word.
    let (buf1, rest) = crate::interpreter::half_chop(arg);
    let (buf2, _) = crate::interpreter::half_chop(&rest);

    let mut number: i32 = -1;
    let mut save = false;

    let Some(authority) = validated_olc_trust(g, ch) else {
        g.send_to_char(ch, "You do not have permission to use OLC.\r\n");
        return;
    };
    if authority < i32::from(LVL_IMMORT) {
        g.send_to_char(ch, "You do not have permission to use OLC.\r\n");
        return;
    }
    let in_room_vnum = g.char_room_vnum(ch).unwrap_or(NOWHERE);

    if buf1.is_empty() {
        // No argument given.
        match subcmd {
            SCMD_OLC_ZEDIT | SCMD_OLC_REDIT => {
                number = in_room_vnum;
            }
            SCMD_OLC_TRIGEDIT | SCMD_OLC_OEDIT | SCMD_OLC_MEDIT | SCMD_OLC_SEDIT => {
                let t = olc_type_word(subcmd);
                g.send_to_char(ch, &format!("Specify a {} VNUM to edit.\r\n", t));
                return;
            }
            SCMD_OLC_HEDIT => {
                g.send_to_char(ch, "Specify a help entry to edit.\r\n");
                return;
            }
            SCMD_OLC_AEDIT => {
                g.send_to_char(ch, "Specify an action to edit.\r\n");
                return;
            }
            _ => {}
        }
    } else if !buf1
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        // First arg is not a number. C: strn_cmp("save", buf1, 4) == 0 — the
        // first 4 chars of buf1 are "save" (i.e. buf1 begins with "save").
        if buf1.starts_with("save") {
            if subcmd == SCMD_OLC_HEDIT || subcmd == SCMD_OLC_AEDIT {
                save = true;
                number = 0;
            } else if buf2.is_empty() {
                g.send_to_char(ch, "Save which zone?\r\n");
                return;
            } else {
                save = true;
                let zone = match crate::text::parse_i32_strict(&buf2) {
                    Ok(zone) => zone,
                    Err(crate::text::ParseIntError::Overflow) => {
                        g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
                        return;
                    }
                    Err(_) => 0,
                };
                number = match zone_vnum_bounds(zone) {
                    Some((number, _)) => number,
                    None => {
                        g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
                        return;
                    }
                };
            }
        } else if subcmd == SCMD_OLC_HEDIT || subcmd == SCMD_OLC_AEDIT {
            number = 0;
        } else if subcmd == SCMD_OLC_ZEDIT && authority == i32::from(LVL_IMPL) {
            if buf1.len() >= 3 && buf1.starts_with("new") && !buf2.is_empty() {
                // C zedit.c:153-330: 'olc zedit new <zone>' CREATES the zone
                // (six stub files + index append + zone-table insert) and
                // then exits - it does not enter the editor (also fixes the
                // strn_cmp prefix inversion, #263).
                let zone_num = match crate::text::parse_i32_strict(&buf2) {
                    Ok(zone) => zone,
                    Err(crate::text::ParseIntError::Overflow) => {
                        g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
                        return;
                    }
                    Err(_) => -1,
                };
                crate::zedit::zedit_new_zone(g, ch, zone_num);
            } else if buf1.eq_ignore_ascii_case("discard") && !buf2.is_empty() {
                let zone_num = match crate::text::parse_i32_strict(&buf2) {
                    Ok(zone) => zone,
                    Err(crate::text::ParseIntError::Overflow) => {
                        g.send_to_char(ch, "That zone number is outside the supported range.\r\n");
                        return;
                    }
                    Err(_) => -1,
                };
                crate::zedit::zedit_discard_new_zone_failure(g, ch, zone_num);
            } else {
                g.send_to_char(
                    ch,
                    "Specify 'zedit new <zone>' or 'zedit discard <zone>'.\r\n",
                );
            }
            return;
        } else {
            g.send_to_char(ch, "Yikes!  Stop that, someone will get hurt!\r\n");
            return;
        }
    }

    // If a numeric argument was given, parse it.
    if number == -1 && subcmd != SCMD_OLC_AEDIT && subcmd != SCMD_OLC_HEDIT {
        number = match crate::text::parse_i32_strict(&buf1) {
            Ok(number) => number,
            Err(crate::text::ParseIntError::Overflow) => {
                g.send_to_char(ch, "That VNUM is outside the supported range.\r\n");
                return;
            }
            Err(_) => -1,
        };
    }

    // Resolve the zone rnum (skip for AEDIT and un-saved HEDIT, which are
    // action-/keyword-keyed rather than zone-keyed).
    let znum_rnum = if subcmd != SCMD_OLC_AEDIT {
        if subcmd == SCMD_OLC_HEDIT && !save {
            None
        } else {
            match real_zone(g, number) {
                Some(z) => Some(z),
                None => {
                    g.send_to_char(ch, "Sorry, there is no zone for that number!\r\n");
                    return;
                }
            }
        }
    } else {
        None
    };

    if let Some(zr) = znum_rnum {
        if !can_edit_zone(g, ch, zr) && subcmd != SCMD_OLC_HEDIT {
            g.send_to_char(ch, "You do not have permission to edit this zone.\r\n");
            return;
        }
    }

    if save {
        match subcmd {
            SCMD_OLC_TRIGEDIT => {
                g.send_to_char(
                    ch,
                    "Triggers are autosaved to disk when edited, there's no need.\r\n",
                );
                return;
            }
            SCMD_OLC_HEDIT => {
                let name = g
                    .get_char(ch)
                    .map(|c| c.get_name().to_string())
                    .unwrap_or_default();
                match crate::hedit::save_all_help(g) {
                    Ok(()) => {
                        crate::syslog::mudlog(
                            g,
                            &format!("OLC: {} saves help entries.", name),
                            crate::syslog::NRM,
                            LVL_GOD,
                        );
                        g.send_to_char(ch, "Help entries saved.\r\n");
                    }
                    Err(err) => {
                        log::warn!("SYSERR: OLC: cannot save help entries: {}", err);
                        g.send_to_char(ch, "Could not save the help file.\r\n");
                    }
                }
                return;
            }
            SCMD_OLC_AEDIT => {
                let name = g
                    .get_char(ch)
                    .map(|c| c.get_name().to_string())
                    .unwrap_or_default();
                match crate::aedit::save_all_actions(g) {
                    Ok(()) => {
                        crate::syslog::mudlog(
                            g,
                            &format!("OLC: {} saves all actions.", name),
                            crate::syslog::NRM,
                            LVL_GOD,
                        );
                        g.send_to_char(ch, "Actions saved.\r\n");
                    }
                    Err(err) => {
                        log::warn!("SYSERR: OLC: cannot save actions: {}", err);
                        g.send_to_char(ch, "Could not save the actions file.\r\n");
                    }
                }
                return;
            }
            _ => {}
        }
        let zr = match znum_rnum {
            Some(z) => z,
            None => {
                g.send_to_char(ch, "Oops, I forgot what you wanted to save.\r\n");
                return;
            }
        };
        let kind = match subcmd {
            SCMD_OLC_REDIT => OLC_SAVE_ROOM,
            SCMD_OLC_ZEDIT => OLC_SAVE_ZONE,
            SCMD_OLC_OEDIT => OLC_SAVE_OBJ,
            SCMD_OLC_MEDIT => OLC_SAVE_MOB,
            SCMD_OLC_SEDIT => OLC_SAVE_SHOP,
            _ => {
                g.send_to_char(ch, "Oops, I forgot what you wanted to save.\r\n");
                return;
            }
        };
        let znumber = g.zones.get(zr).map(|z| z.number).unwrap_or(-1);
        match try_olc_save_to_disk(g, zr, kind) {
            Ok(()) => {
                g.send_to_char(
                    ch,
                    &format!(
                        "Saved all {}s in zone {}.\r\n",
                        olc_type_word(subcmd),
                        znumber
                    ),
                );
                // C olc.c:283: mudlog 'OLC: %s saves %s info for zone %d.'
                // Publication is logged only after the checked writer returns.
                let name = g
                    .get_char(ch)
                    .map(|c| c.get_name().to_string())
                    .unwrap_or_default();
                let level = g
                    .get_char(ch)
                    .map(|c| c.player.level)
                    .unwrap_or(LVL_BUILDER_LEVEL);
                crate::syslog::mudlog(
                    g,
                    &format!(
                        "OLC: {} saves {} info for zone {}.",
                        name,
                        olc_type_word(subcmd),
                        znumber
                    ),
                    crate::syslog::CMP,
                    LVL_BUILDER_LEVEL.max(level),
                );
            }
            Err(error) => {
                log::warn!(
                    "SYSERR: OLC: could not save zone {} component {}: {}",
                    znumber,
                    kind,
                    error
                );
                g.send_to_char(
                    ch,
                    &format!(
                        "Could not save {}s in zone {}; changes remain pending.\r\n",
                        olc_type_word(subcmd),
                        znumber
                    ),
                );
            }
        }
        return;
    }

    // Not a save: hand off to the right editor's do_X.
    match subcmd {
        SCMD_OLC_REDIT => crate::redit::do_redit(g, ch, &number.to_string(), 0),
        SCMD_OLC_OEDIT => crate::oedit::do_oedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_ZEDIT => crate::zedit::do_zedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_MEDIT => crate::medit::do_medit(g, ch, &number.to_string(), 0),
        SCMD_OLC_SEDIT => crate::sedit::do_sedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_TRIGEDIT => crate::trigedit::do_trigedit(g, ch, &number.to_string(), 0),
        SCMD_OLC_HEDIT => crate::hedit::do_hedit(g, ch, &buf1, 0),
        SCMD_OLC_AEDIT => crate::aedit::do_aedit(g, ch, &buf1, 0),
        _ => {}
    }
}

/// The descriptive word for each editor (olc_scmd_info[].text).
fn olc_type_word(subcmd: i32) -> &'static str {
    match subcmd {
        SCMD_OLC_REDIT => "room",
        SCMD_OLC_OEDIT => "object",
        SCMD_OLC_ZEDIT => "room",
        SCMD_OLC_MEDIT => "mobile",
        SCMD_OLC_SEDIT => "shop",
        SCMD_OLC_TRIGEDIT => "trigger",
        SCMD_OLC_HEDIT => "help",
        SCMD_OLC_AEDIT => "action",
        _ => "thing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::dg_db_scripts::TrigProto;
    use crate::world::{MAX_ZONE_NUMBER, Zone, zone_vnum_bounds};
    use std::path::PathBuf;

    fn zone(number: i32, builders: &str) -> Zone {
        let (_, top) = zone_vnum_bounds(number).expect("valid test zone number");
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

    fn player(g: &mut GameState, name: &str, level: Level) -> CharId {
        let mut ch = Character::new_player(name.into(), Class::Cleric, Race::Human);
        ch.player.level = level;
        ch.trust = i32::from(level);
        ch.godcmds2 |= crate::gcmd::GCMD2_OLC;
        g.create_char(ch)
    }

    fn connect_player(g: &mut GameState, ch: CharId, conn: ConnId) {
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(ch);
        g.descriptors.insert(conn, descriptor);
    }

    fn temp_lib(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("deltamud-olc-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("world/zon")).unwrap();
        std::fs::create_dir_all(path.join("world/mob")).unwrap();
        std::fs::create_dir_all(path.join("world/shp")).unwrap();
        path
    }

    fn olc_test_lock() -> TestSaveListGuard {
        test_save_list_guard()
    }

    #[test]
    fn atomic_replace_is_durable_and_preserves_old_file_before_rename_failure() {
        let _guard = olc_test_lock();
        let dir = temp_lib("atomic-replace");
        let path = dir.join("durable.txt");
        std::fs::write(&path, b"old contents").unwrap();

        atomic_replace(&path, b"new contents").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");

        let err = atomic_replace_with(&path, b"must not publish", |_| {
            Err(io::Error::other("injected pre-rename failure"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
        let temp_prefix = ".durable.txt.tmp-";
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(temp_prefix)
        }));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_replace_reports_post_rename_directory_sync_failure_as_published() {
        let _guard = olc_test_lock();
        let dir = temp_lib("atomic-replace-directory-sync");
        let path = dir.join("durable.txt");
        std::fs::write(&path, b"old contents").unwrap();

        let error = atomic_replace_with_hooks(
            &path,
            b"new contents",
            |_| Ok(()),
            |_| Err(io::Error::other("injected directory sync failure")),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(replacement_was_published(&error));
        assert!(error.to_string().contains("replacement was published"));
        assert!(error.to_string().contains("durability is unconfirmed"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"new contents",
            "rename has already published the replacement before directory fsync"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_create_never_clobbers_an_existing_target() {
        let _guard = olc_test_lock();
        let dir = temp_lib("atomic-create-no-clobber");
        let path = dir.join("new-zone-component.wld");

        atomic_create(&path, b"first contents").unwrap();
        let error = atomic_create(&path, b"must not replace").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"first contents");
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".new-zone-component.wld.tmp-")
        }));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn new_zone_transaction_marker_is_durable_idempotent_and_exact() {
        let _guard = olc_test_lock();
        let lib = temp_lib("new-zone-transaction");
        let lib_text = lib.to_string_lossy();
        let zone_number = 40417;

        begin_new_zone_publication(&lib_text, zone_number).unwrap();
        begin_new_zone_publication(&lib_text, zone_number).unwrap();
        assert_eq!(
            pending_new_zone_publications(&lib_text).unwrap(),
            HashSet::from([zone_number])
        );
        let pending = pending_new_zone_publications(&lib_text).unwrap();
        for extension in ["zon", "wld", "mob", "obj", "shp", "trg"] {
            assert!(new_zone_index_entry_is_pending(
                &pending,
                &format!("{zone_number}.{extension}")
            ));
        }
        assert!(!new_zone_index_entry_is_pending(&pending, "20000.trg"));

        complete_new_zone_publication(&lib_text, zone_number).unwrap();
        assert!(pending_new_zone_publications(&lib_text).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn new_zone_transaction_retry_reconfirms_directory_parent_durability() {
        let _guard = olc_test_lock();
        let lib = temp_lib("new-zone-transaction-parent-sync-retry");
        let lib_text = lib.to_string_lossy();
        let world_directory = lib.join("world");
        let transaction_directory = world_directory.join(NEW_ZONE_TRANSACTION_DIRECTORY);
        let marker = transaction_directory.join("40419.pending");
        let mut fail_world_sync_once = true;
        let mut sync_calls = Vec::new();
        {
            let mut injected_sync = |directory: &Path| {
                sync_calls.push(directory.to_path_buf());
                if directory == world_directory && fail_world_sync_once {
                    fail_world_sync_once = false;
                    Err(io::Error::other("injected world-directory sync failure"))
                } else {
                    sync_parent_directory(directory)
                }
            };

            let error = begin_new_zone_publication_with_sync(&lib_text, 40419, &mut injected_sync)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert!(transaction_directory.is_dir());
            assert!(
                !marker.exists(),
                "the marker must not publish before its directory entry is durable"
            );

            begin_new_zone_publication_with_sync(&lib_text, 40419, &mut injected_sync).unwrap();
        }
        assert_eq!(
            sync_calls
                .iter()
                .filter(|directory| *directory == &world_directory)
                .count(),
            2,
            "an AlreadyExists retry must re-sync the transaction directory's parent"
        );
        assert_eq!(
            pending_new_zone_publications(&lib_text).unwrap(),
            HashSet::from([40419])
        );

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn malformed_new_zone_transaction_state_fails_closed() {
        let _guard = olc_test_lock();
        let lib = temp_lib("malformed-new-zone-transaction");
        let directory = lib.join("world").join(NEW_ZONE_TRANSACTION_DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("40418.pending"), b"not a transaction\n").unwrap();

        let error = pending_new_zone_publications(&lib.to_string_lossy()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn failed_flush_reports_target_and_retains_pending_save_entry() {
        let _guard = olc_test_lock();
        let lib = temp_lib("failed-zone-save");
        std::fs::remove_dir_all(lib.join("world/zon")).unwrap();
        std::fs::write(lib.join("world/zon"), b"not a directory").unwrap();

        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        g.zones.push(zone(47, "Root"));
        olc_add_to_save_list(47, OLC_SAVE_ZONE);

        let error = flush_save_list_to_disk(&mut g).unwrap_err();
        assert_eq!(error.report.attempted, 1);
        assert!(error.report.saved.is_empty());
        assert_eq!(error.failures.len(), 1);
        assert_eq!(
            error.failures[0].target,
            OlcSaveTarget {
                zone: 47,
                kind: OLC_SAVE_ZONE
            }
        );
        assert!(with_save_list(|list| list
            .iter()
            .any(|&(zone, kind)| zone == 47 && kind == OLC_SAVE_ZONE)));

        olc_remove_from_save_list(47, OLC_SAVE_ZONE);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn flush_removes_successes_but_retains_independent_failures() {
        let _guard = olc_test_lock();
        let lib = temp_lib("partial-flush");
        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        g.zones.push(zone(43, "Root"));
        olc_add_to_save_list(43, OLC_SAVE_ZONE);
        olc_add_to_save_list(99, OLC_SAVE_ZONE);

        let error = flush_save_list_to_disk(&mut g).unwrap_err();
        assert_eq!(error.report.attempted, 2);
        assert_eq!(
            error.report.saved,
            vec![OlcSaveTarget {
                zone: 43,
                kind: OLC_SAVE_ZONE
            }]
        );
        assert_eq!(error.failures.len(), 1);
        assert_eq!(error.failures[0].target.zone, 99);
        assert_eq!(error.failures[0].error_kind, io::ErrorKind::NotFound);
        let dirty = with_save_list(|list| list.clone());
        assert!(!dirty.contains(&(43, OLC_SAVE_ZONE)));
        assert!(dirty.contains(&(99, OLC_SAVE_ZONE)));
        assert!(lib.join("world/zon/43.zon").exists());

        olc_remove_from_save_list(99, OLC_SAVE_ZONE);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn manual_olc_save_reports_failure_without_a_success_message() {
        let _guard = olc_test_lock();
        let lib = temp_lib("manual-save-failure");
        std::fs::remove_dir_all(lib.join("world/zon")).unwrap();
        std::fs::write(lib.join("world/zon"), b"not a directory").unwrap();

        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        g.zones.push(zone(47, "Root"));
        olc_add_to_save_list(47, OLC_SAVE_ZONE);

        let conn = ConnId(109);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(ch);
        g.descriptors.insert(conn, descriptor);

        do_olc(&mut g, ch, "save 47", SCMD_OLC_ZEDIT);

        let output = &g.descriptors[&conn].outbuf;
        assert!(output.contains("Could not save"));
        assert!(output.contains("changes remain pending"));
        assert!(!output.contains("Saved all"));
        assert!(!output.contains("Saving all"));
        assert!(with_save_list(|list| list.contains(&(47, OLC_SAVE_ZONE))));

        olc_remove_from_save_list(47, OLC_SAVE_ZONE);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn can_edit_zone_uses_builder_list_below_impl() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Alice Bob"));
        let alice = player(&mut g, "Alice", LVL_IMMORT);
        let charlie = player(&mut g, "Charlie", LVL_IMMORT);
        let imp = player(&mut g, "Root", LVL_IMPL);
        connect_player(&mut g, alice, ConnId(1_087));
        connect_player(&mut g, charlie, ConnId(1_088));
        connect_player(&mut g, imp, ConnId(1_089));

        assert!(can_edit_zone(&g, alice, 0));
        assert!(!can_edit_zone(&g, charlie, 0));
        assert!(can_edit_zone(&g, imp, 0));
    }

    #[test]
    fn builder_acl_uses_exact_case_insensitive_tokens_with_legacy_commas() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Michael Fara, Claude"));
        let exact = player(&mut g, "Fara", LVL_IMMORT);
        let case_variant = player(&mut g, "claude", LVL_IMMORT);
        let prefix = player(&mut g, "Far", LVL_IMMORT);
        connect_player(&mut g, exact, ConnId(1_181));
        connect_player(&mut g, case_variant, ConnId(1_182));
        connect_player(&mut g, prefix, ConnId(1_183));

        assert!(can_edit_zone(&g, exact, 0));
        assert!(can_edit_zone(&g, case_variant, 0));
        assert!(!can_edit_zone(&g, prefix, 0));
        assert!(name_reserved_by_zone_acl(&g, "Michael"));
        assert!(name_reserved_by_zone_acl(&g, "fara"));
        assert!(!name_reserved_by_zone_acl(&g, "Far"));
    }

    #[test]
    fn descriptorless_players_have_no_olc_authority() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Root"));
        let imp = player(&mut g, "Root", LVL_IMPL);

        assert_eq!(validated_olc_trust(&g, imp), None);
        assert!(!can_edit_zone(&g, imp, 0));
    }

    #[test]
    fn can_edit_zone_uses_the_player_principal_name_while_switched() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Principal"));
        let conn = ConnId(1_090);
        let principal = player(&mut g, "Principal", LVL_IMMORT);
        g.get_char_mut(principal).unwrap().desc = None;

        let mut body = Character::new_npc(7_001);
        body.player.name = "Vessel".to_string();
        body.desc = Some(conn);
        let body = g.create_char(body);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.character = Some(body);
        descriptor.original = Some(principal);
        g.descriptors.insert(conn, descriptor);

        assert!(
            can_edit_zone(&g, body, 0),
            "the delegated player name must retain its legitimate zone access"
        );
        g.zones[0].builders = "Vessel".to_string();
        assert!(
            !can_edit_zone(&g, body, 0),
            "an NPC body name must not confer a player's zone delegation"
        );
    }

    #[test]
    fn retained_olc_authorization_accepts_a_switched_principal_but_not_a_principal_handoff() {
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(1, "Principal Interloper"));
        let conn = ConnId(1_184);
        let principal = player(&mut g, "Principal", LVL_IMMORT);
        g.get_char_mut(principal).unwrap().desc = None;

        let mut body = Character::new_npc(7_002);
        body.player.name = "Vessel".to_string();
        body.desc = Some(conn);
        let body = g.create_char(body);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(body);
        descriptor.original = Some(principal);
        g.descriptors.insert(conn, descriptor);

        let authorization = capture_olc_authorization(&g, body).unwrap();
        assert_eq!(authorization.requester_body, body);
        assert_eq!(authorization.requester_principal, principal);
        assert!(revalidate_olc_authorization(&g, authorization, false, Some(0)).is_ok());

        let interloper = player(&mut g, "Interloper", LVL_IMMORT);
        g.get_char_mut(interloper).unwrap().desc = None;
        g.descriptors.get_mut(&conn).unwrap().original = Some(interloper);

        assert_eq!(
            revalidate_olc_authorization(&g, authorization, false, Some(0))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied,
            "a still-authorized replacement principal must not inherit the retained editor"
        );
    }

    #[test]
    fn persisted_trust_controls_unowned_zone_override_not_character_level() {
        let _guard = olc_test_lock();
        let lib = temp_lib("trust-authority");
        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().into_owned();
        let mut g = GameState::new(cfg);
        g.zones.push(zone(1, "Owner"));

        let demoted_conn = ConnId(1_091);
        let demoted = player(&mut g, "Demoted", LVL_IMPL);
        {
            let character = g.get_char_mut(demoted).unwrap();
            character.trust = 1;
            character.godcmds2 = crate::gcmd::GCMD2_OLC;
            character.desc = Some(demoted_conn);
        }
        let mut descriptor = Descriptor::new(demoted_conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(demoted);
        g.descriptors.insert(demoted_conn, descriptor);

        // The dispatcher rejects the retained OLC bit because command trust
        // was demoted, and the handler itself independently rejects a direct
        // call against an unowned zone.
        crate::interpreter::command_interpreter_authenticated(&mut g, demoted, "zedit save 1");
        assert_eq!(g.descriptors[&demoted_conn].outbuf, "Huh?!?\r\n");
        g.descriptors.get_mut(&demoted_conn).unwrap().outbuf.clear();
        do_olc(&mut g, demoted, "save 1", SCMD_OLC_ZEDIT);
        assert!(
            g.descriptors[&demoted_conn]
                .outbuf
                .contains("do not have permission")
        );
        assert!(!lib.join("world/zon/1.zon").exists());

        let trusted_conn = ConnId(1_092);
        let trusted = player(&mut g, "Trusted", 1);
        {
            let character = g.get_char_mut(trusted).unwrap();
            character.trust = i32::from(LVL_IMPL);
            character.godcmds2 = crate::gcmd::GCMD2_OLC;
            character.desc = Some(trusted_conn);
        }
        let mut descriptor = Descriptor::new(trusted_conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(trusted);
        g.descriptors.insert(trusted_conn, descriptor);

        crate::interpreter::command_interpreter_authenticated(&mut g, trusted, "zedit save 1");
        assert!(
            g.descriptors[&trusted_conn]
                .outbuf
                .contains("Saved all rooms in zone 1")
        );
        assert!(lib.join("world/zon/1.zon").exists());

        olc_remove_from_save_list(1, OLC_SAVE_ZONE);
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn real_olc_new_zone_checks_boundary_and_overflow_before_writing() {
        let _guard = olc_test_lock();
        let lib = temp_lib("zone-number-boundary");
        for extension in ["zon", "wld", "mob", "obj", "shp", "trg"] {
            let directory = lib.join(format!("world/{extension}"));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("index"), "$\n").unwrap();
        }
        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);

        let conn = ConnId(106);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(ch);
        g.descriptors.insert(conn, descriptor);

        do_olc(
            &mut g,
            ch,
            &format!("new {MAX_ZONE_NUMBER}"),
            SCMD_OLC_ZEDIT,
        );
        let (first, top) = zone_vnum_bounds(MAX_ZONE_NUMBER).unwrap();
        assert_eq!(g.zones.len(), 1);
        assert_eq!(g.zones[0].number, MAX_ZONE_NUMBER);
        assert_eq!(g.zones[0].top, top);
        assert_eq!(
            std::fs::read_to_string(lib.join(format!("world/zon/{MAX_ZONE_NUMBER}.zon"))).unwrap(),
            format!("#{MAX_ZONE_NUMBER}\nNew Zone~\n~\n{top} 30 2\n0 0 0\nS\n$\n")
        );
        assert!(
            std::fs::read_to_string(lib.join(format!("world/wld/{MAX_ZONE_NUMBER}.wld")))
                .unwrap()
                .starts_with(&format!("#{first}\n"))
        );

        for rejected in [
            (MAX_ZONE_NUMBER + 1).to_string(),
            "21474836".to_string(),
            "2147483648".to_string(),
        ] {
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
            do_olc(&mut g, ch, &format!("new {rejected}"), SCMD_OLC_ZEDIT);
            assert_eq!(g.zones.len(), 1, "zone {rejected} must not be inserted");
            assert!(
                !lib.join(format!("world/zon/{rejected}.zon")).exists(),
                "zone {rejected} must be rejected before filesystem mutation"
            );
            assert!(
                g.descriptors[&conn]
                    .outbuf
                    .contains("outside the supported range")
                    || g.descriptors[&conn]
                        .outbuf
                        .contains("higher then highest zone allowed"),
                "zone {rejected} should receive an explicit range error"
            );
        }

        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn dg_script_editor_adds_deletes_and_marks_room_dirty() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(42, "Root"));
        olc_remove_from_save_list(42, OLC_SAVE_ROOM);

        let conn = ConnId(99);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        crate::dg_db_scripts::set_test_proto_trigger(
            crate::dg_handler::WLD_TRIGGER,
            999_001,
            TrigProto {
                vnum: 4205,
                attach_type: crate::dg_handler::WLD_TRIGGER,
                name: "first room trigger".to_string(),
                trigger_type: 1,
                narg: 0,
                arglist: String::new(),
                cmdlist: vec!["say first".to_string()],
            },
        );
        crate::dg_db_scripts::set_test_proto_trigger(
            crate::dg_handler::WLD_TRIGGER,
            999_002,
            TrigProto {
                vnum: 4206,
                attach_type: crate::dg_handler::WLD_TRIGGER,
                name: "second room trigger".to_string(),
                trigger_type: 1,
                narg: 0,
                arglist: String::new(),
                cmdlist: vec!["say second".to_string()],
            },
        );

        let mut mode = DgScriptEditMode::Main;
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "n",
        ));
        assert_eq!(mode, DgScriptEditMode::New);
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "1, 4205",
        ));
        assert_eq!(
            crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::WLD_TRIGGER, 4201),
            vec![4205]
        );

        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "n",
        ));
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "4206",
        ));
        assert_eq!(
            crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::WLD_TRIGGER, 4201),
            vec![4205, 4206]
        );

        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "d",
        ));
        assert_eq!(mode, DgScriptEditMode::Delete);
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::WLD_TRIGGER,
            4201,
            &mut mode,
            "1",
        ));
        assert_eq!(
            crate::dg_db_scripts::proto_trigger_vnums(crate::dg_handler::WLD_TRIGGER, 4201),
            vec![4206]
        );

        olc_saveinfo(&mut g, ch);
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Rooms for zone 42"));
        olc_remove_from_save_list(42, OLC_SAVE_ROOM);
        crate::dg_db_scripts::clear_proto_triggers(crate::dg_handler::WLD_TRIGGER, 4201);
    }

    #[test]
    fn dg_script_editor_reprompts_on_unparseable_line() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(44, "Root"));
        let conn = ConnId(103);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        let mut mode = DgScriptEditMode::New;
        // C dg_olc.c:766-783: garbage stays in the sub-editor with the
        // "Invalid Trigger VNUM!" re-prompt (#304).
        assert!(dg_script_edit_parse(
            &mut g,
            conn,
            crate::dg_handler::MOB_TRIGGER,
            4401,
            &mut mode,
            "not a vnum",
        ));
        assert_eq!(mode, DgScriptEditMode::New);
        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Invalid Trigger VNUM!"));
    }

    #[test]
    fn menu_colours_follow_the_builder_colour_level() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        let colour_on = player(&mut g, "Colour", LVL_IMPL);
        let colour_off = player(&mut g, "Plain", LVL_IMPL);
        g.get_char_mut(colour_off).unwrap().prf_flags = 0;
        // Colour level from PRF_COLOR_1/2 (screen.h _clrlevel).
        assert_eq!(colour_level(&g, colour_off), 0);
        assert!(!olc_colour_on(&g, colour_off));
        g.get_char_mut(colour_on).unwrap().prf_flags =
            crate::flags::PRF_COLOR_1 | crate::flags::PRF_COLOR_2;
        assert!(olc_colour_on(&g, colour_on));

        // olc_send strips the &-codes for a colour-off builder (#306).
        let conn = ConnId(105);
        g.get_char_mut(colour_off).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(colour_off);
        g.descriptors.insert(conn, d);
        olc_send(&mut g, conn, "-- Menu [&C42&n]\r\n");
        assert_eq!(g.descriptors.get(&conn).unwrap().outbuf, "-- Menu [42]\r\n");
    }

    #[test]
    fn saveinfo_hides_zones_the_builder_cannot_edit() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(45, "Alice"));
        g.zones.push(zone(46, "Bob"));
        olc_add_to_save_list(45, OLC_SAVE_ROOM);
        olc_add_to_save_list(46, OLC_SAVE_ROOM);

        let conn = ConnId(104);
        let alice = player(&mut g, "Alice", LVL_BUILDER_LEVEL as Level);
        g.get_char_mut(alice).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(alice);
        g.descriptors.insert(conn, d);

        olc_saveinfo(&mut g, alice);
        let out = &g.descriptors.get(&conn).unwrap().outbuf.clone();
        assert!(out.contains("zone 45"));
        assert!(!out.contains("zone 46"), "Bob's zone must be hidden (#278)");
        olc_remove_from_save_list(45, OLC_SAVE_ROOM);
        olc_remove_from_save_list(46, OLC_SAVE_ROOM);
    }

    #[test]
    fn central_olc_save_dispatches_zone_mob_and_shop_writers() {
        let _guard = olc_test_lock();
        let lib = temp_lib("central-save");
        let mut cfg = Config::default();
        cfg.lib_path = lib.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        g.zones.push(zone(43, "Root"));
        std::fs::write(
            lib.join("world/shp/43.shp"),
            "CircleMUD v3.0 Shop File~\n$~\n",
        )
        .unwrap();
        std::fs::write(lib.join("world/shp/index"), "43.shp\n$\n").unwrap();

        olc_add_to_save_list(43, OLC_SAVE_ZONE);
        olc_add_to_save_list(43, OLC_SAVE_MOB);
        olc_add_to_save_list(43, OLC_SAVE_SHOP);

        olc_save_to_disk(&mut g, 0, OLC_SAVE_ZONE);
        olc_save_to_disk(&mut g, 0, OLC_SAVE_MOB);
        olc_save_to_disk(&mut g, 0, OLC_SAVE_SHOP);

        let zon = lib.join("world/zon/43.zon");
        let mob = lib.join("world/mob/43.mob");
        let shp = lib.join("world/shp/43.shp");
        assert!(std::fs::read_to_string(&zon).unwrap().contains("#43\n"));
        assert_eq!(std::fs::read_to_string(&mob).unwrap(), "$\n");
        assert!(
            std::fs::read_to_string(&shp)
                .unwrap()
                .starts_with("CircleMUD v3.0 Shop File~\n")
        );

        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(ConnId(100));
        let mut d = Descriptor::new(ConnId(100), "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(ConnId(100), d);
        olc_saveinfo(&mut g, ch);
        assert!(
            g.descriptors
                .get(&ConnId(100))
                .unwrap()
                .outbuf
                .contains("The database is up to date.")
        );

        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn dg_attach_input_rejects_overflow_without_shifting_later_tokens() {
        assert_eq!(parse_script_position_vnum("1, 200"), Ok(Some((1, 200))));
        assert_eq!(parse_script_position_vnum("200"), Ok(Some((999, 200))));
        assert_eq!(
            parse_script_position_vnum("2147483648, 200"),
            Err(crate::text::ParseIntError::Overflow)
        );
        assert_eq!(
            parse_script_position_vnum("1, -2147483649"),
            Err(crate::text::ParseIntError::Overflow)
        );
    }

    #[test]
    fn do_copy_copies_room_fields_and_marks_dirty() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(45, "Root"));
        let mut src = crate::room::Room::new(4500, 0, "Source".into(), "Src desc.\r\n".into());
        src.sector_type = crate::room::SectorType::Forest;
        let targ = crate::room::Room::new(4501, 0, "Target".into(), "Tgt desc.\r\n".into());
        g.add_room(src);
        let t = g.add_room(targ);

        let conn = ConnId(120);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);
        g.get_char_mut(ch).unwrap().in_room = Some(t);

        do_copy(&mut g, ch, "room 4500", 0);

        let room = g.room(t);
        assert_eq!(room.name, "Source");
        assert_eq!(room.sector_type, crate::room::SectorType::Forest);
        assert!(
            g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("You copy room 4500 to 4501.")
        );
        // Save-list marks the target zone dirty (C: ROOM == OLC_SAVE_ROOM).
        olc_remove_from_save_list(45, OLC_SAVE_ROOM);
    }

    #[test]
    fn do_rlink_connects_disconnects_and_autocreates() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(46, "Root"));
        let a = g.add_room(crate::room::Room::new(4600, 0, "A".into(), String::new()));
        let b = g.add_room(crate::room::Room::new(4601, 0, "B".into(), String::new()));

        let conn = ConnId(121);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        g.get_char_mut(ch).unwrap().in_room = Some(a);
        let mut d = Descriptor::new(conn, "example.test".to_string());
        d.character = Some(ch);
        g.descriptors.insert(conn, d);

        // One-way connect.
        do_rlink(&mut g, ch, "east connect 1 4601", 0);
        assert_eq!(g.room(a).exits[EAST].as_ref().unwrap().to_room, 4601);
        assert!(g.room(b).exits[WEST].is_none());
        assert!(
            g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("You make an exit east to room 4601.")
        );

        // Two-way connect builds the reciprocal exit.
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        g.get_char_mut(ch).unwrap().in_room = Some(b);
        do_rlink(&mut g, ch, "west connect 2 4600", 0);
        assert_eq!(g.room(b).exits[WEST].as_ref().unwrap().to_room, 4600);
        assert_eq!(g.room(a).exits[EAST].as_ref().unwrap().to_room, 4601);

        // Two-way disconnect removes both sides. The stored destination is a
        // vnum (4600), deliberately unlike its arena rnum (0).
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_rlink(&mut g, ch, "west disconnect 2 4600", 0);
        assert!(g.room(b).exits[WEST].is_none());
        assert!(g.room(a).exits[EAST].is_none());

        // Restore B -> A for the separate one-way-disconnect assertion.
        do_rlink(&mut g, ch, "west connect 2 4600", 0);

        // Disconnect removes the own exit (stand in B, own the west exit).
        // C quirk kept: despite the usage string's "[target]", the parse
        // demands a numeric target even for disconnect (is_number("") fails).
        g.get_char_mut(ch).unwrap().in_room = Some(b);
        do_rlink(&mut g, ch, "west disconnect 1 4600", 0);
        assert!(g.room(b).exits[WEST].is_none());
        assert!(
            g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Exit deleted.")
        );

        // Auto-create: omitting the target makes the first free vnum in the zone.
        g.get_char_mut(ch).unwrap().in_room = Some(a);
        do_rlink(&mut g, ch, "south connect 1", 0);
        assert_eq!(
            g.room(a).exits[SOUTH].as_ref().map(|e| e.to_room),
            Some(4602)
        );
        assert_eq!(
            g.room(g.real_room(4602).unwrap()).name,
            "An unfinished room"
        );
        olc_remove_from_save_list(46, OLC_SAVE_ROOM);
    }

    #[test]
    fn rlink_two_way_disconnect_validates_reciprocal_and_tracks_both_zones() {
        let _guard = olc_test_lock();
        let mut g = GameState::new(Config::default());
        g.zones.push(zone(46, "Root"));
        g.zones.push(zone(47, "Root"));
        let base = g.add_room(crate::room::Room::new(
            4600,
            0,
            "Base".into(),
            String::new(),
        ));
        let unrelated = g.add_room(crate::room::Room::new(
            4601,
            0,
            "Unrelated".into(),
            String::new(),
        ));
        let target = g.add_room(crate::room::Room::new(
            4700,
            1,
            "Target".into(),
            String::new(),
        ));
        assert_ne!(base, 4600usize);
        assert_ne!(target, 4700usize);

        let conn = ConnId(122);
        let ch = player(&mut g, "Root", LVL_IMPL);
        g.get_char_mut(ch).unwrap().desc = Some(conn);
        g.get_char_mut(ch).unwrap().in_room = Some(base);
        let mut descriptor = Descriptor::new(conn, "example.test".to_string());
        descriptor.character = Some(ch);
        g.descriptors.insert(conn, descriptor);

        do_rlink(&mut g, ch, "east connect 2 4700", 0);
        let dirty = with_save_list(|list| list.clone());
        assert!(dirty.contains(&(46, OLC_SAVE_ROOM)));
        assert!(dirty.contains(&(47, OLC_SAVE_ROOM)));
        olc_remove_from_save_list(46, OLC_SAVE_ROOM);
        olc_remove_from_save_list(47, OLC_SAVE_ROOM);

        // A missing reciprocal is a transactional failure: keep the own exit.
        free_dir(&mut g, target, WEST);
        do_rlink(&mut g, ch, "east disconnect 2 4700", 0);
        assert_eq!(
            g.room(base).exits[EAST].as_ref().map(|e| e.to_room),
            Some(4700)
        );
        assert!(g.room(target).exits[WEST].is_none());
        assert!(
            g.descriptors[&conn]
                .outbuf
                .contains("no matching reciprocal exit")
        );

        // A reciprocal owned by another builder is never deleted either.
        create_dir(&mut g, target, WEST);
        g.room_mut(target).exits[WEST].as_mut().unwrap().to_room = 4601;
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_rlink(&mut g, ch, "east disconnect 2 4700", 0);
        assert_eq!(
            g.room(base).exits[EAST].as_ref().map(|e| e.to_room),
            Some(4700)
        );
        assert_eq!(
            g.room(target).exits[WEST].as_ref().map(|e| e.to_room),
            Some(g.room(unrelated).number)
        );

        // Once the reciprocal points back, both sides are removed and both
        // zones are marked dirty.
        g.room_mut(target).exits[WEST].as_mut().unwrap().to_room = 4600;
        do_rlink(&mut g, ch, "east disconnect 2 4700", 0);
        assert!(g.room(base).exits[EAST].is_none());
        assert!(g.room(target).exits[WEST].is_none());
        let dirty = with_save_list(|list| list.clone());
        assert!(dirty.contains(&(46, OLC_SAVE_ROOM)));
        assert!(dirty.contains(&(47, OLC_SAVE_ROOM)));
        olc_remove_from_save_list(46, OLC_SAVE_ROOM);
        olc_remove_from_save_list(47, OLC_SAVE_ROOM);
    }
}
