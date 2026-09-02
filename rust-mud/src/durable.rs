// durable.rs — THE canonical durable-file publication layer (phase 3).
//
// Every durably-written runtime file (rent/crash objects, boards, clans.dat,
// mail store, house control + objects, OLC zone/world files, who.html,
// copyover snapshot) publishes through one of the helpers here. Guarantees,
// with no opt-outs:
//   * unique sibling temp file in the target directory (never a caller-chosen
//     or guessable name);
//   * checked `write_all` + `flush` + `sync_all` of the temp file;
//   * `rename(2)` publish (atomic on one filesystem; never unlink-first);
//   * `fsync` of the parent directory after publish;
//   * pre-publish failure leaves the target byte-identical and removes the
//     temp file;
//   * post-publish parent-fsync failure returns
//     [`replacement_was_published`] == true ("published but durability
//     unconfirmed") so callers retain their pending-save marker;
//   * permissions preserved from an existing target, 0o644 default for new.
//
// All publication takes the shared publication lock so compare-and-replace
// callers can validate preflight bytes without another writer racing rename.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Every in-process durable publication participates in one critical section.
/// Compare-and-replace callers rely on this to validate their exact source
/// bytes without another writer changing the target before rename.
fn publication_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Marker carried inside an [`io::Error`] when the final rename succeeded but
/// syncing the parent directory did not.  At that point callers must not claim
/// that the old file is still live: the replacement is already visible.  Editors
/// use this distinction to reconcile their in-memory view while retaining the
/// dirty marker so a later save can confirm crash durability.
#[derive(Debug)]
pub struct PublishedButDurabilityUnconfirmed {
    source: io::Error,
}

impl PublishedButDurabilityUnconfirmed {
    pub fn new(source: io::Error) -> Self {
        PublishedButDurabilityUnconfirmed { source }
    }
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

/// Build the "published but incomplete" error used by no-clobber publication
/// when the sibling temp link cannot be removed after a successful hard link.
pub fn published_but_incomplete(context: impl Into<String>, source: io::Error) -> io::Error {
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

/// True only for the post-rename failure states described above.  Ordinary
/// errors mean publication never happened and the old durable bytes remain.
pub fn replacement_was_published(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|inner| {
        inner
            .downcast_ref::<PublishedButDurabilityUnconfirmed>()
            .is_some()
            || inner.downcast_ref::<PublishedButIncomplete>().is_some()
    })
}

/// Durably replace `path` with `bytes` without first unlinking the live file.
/// A failure before rename leaves the old file untouched.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace_with(path, bytes, |_| Ok(()))
}

/// Replace `path` only when it still contains the exact bytes read during
/// preflight. All durable writers share the same publication lock, so this
/// closes the in-process read/rename race instead of silently discarding
/// another save.
pub fn replace_if_unchanged(path: &Path, expected: &[u8], replacement: &[u8]) -> io::Result<()> {
    let _publication_guard = lock_publication();
    validate_exact_contents(path, expected)?;
    replace_with_hooks_unlocked(path, replacement, |_| Ok(()), sync_parent_directory)
}

/// Durably create `path` from a fully-synced unique sibling without replacing
/// an existing target. Linking the sibling into place is atomic and fails with
/// `AlreadyExists` if another writer won the name after the caller's preflight.
/// This is the no-clobber counterpart to [`replace`], used for new-zone
/// component files whose pre-existing contents must never be overwritten.
pub fn create_no_clobber(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _publication_guard = lock_publication();
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
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
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
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
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

/// Durably unlink `path`: fsync the parent directory afterwards so the removal
/// itself is crash-durable.
pub fn remove_durable(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    if let Some(parent) = path.parent() {
        sync_parent_directory(parent)?;
    }
    Ok(())
}

/// Revalidate and durably confirm an already-visible idempotent publication.
/// Deliberately in the same critical section as replacement.
pub fn confirm_publication_unchanged(path: &Path, expected: &[u8]) -> io::Result<()> {
    let _publication_guard = lock_publication();
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

pub(crate) fn lock_publication() -> std::sync::MutexGuard<'static, ()> {
    crate::lock_ok::lock(publication_lock())
}

/// Checked write + fsync + rename with hooks; the shared implementation behind
/// [`replace`] and [`replace_if_unchanged`]. Caller must hold the publication
/// lock (or accept racing preflights for plain `replace`).
fn replace_with<F>(path: &Path, bytes: &[u8], before_rename: F) -> io::Result<()>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let _publication_guard = lock_publication();
    replace_with_hooks_unlocked(path, bytes, before_rename, sync_parent_directory)
}

/// Internal, lock-free core. `before_rename` runs on the fully-synced temp
/// path (e.g. permission fixups); `sync_parent` runs after rename.
fn replace_with_hooks_unlocked<F, S>(
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
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "deltamud-durable-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replace_is_atomic_and_leaves_no_temporaries() {
        let dir = temp_dir("replace");
        let path = dir.join("target");
        std::fs::write(&path, b"old").unwrap();

        replace(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with("target"))
            .collect();
        assert!(leftovers.is_empty(), "stray temporaries: {leftovers:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_if_unchanged_rejects_changed_targets_without_touching_them() {
        let dir = temp_dir("cas");
        let path = dir.join("target");
        std::fs::write(&path, b"actual").unwrap();

        let error = replace_if_unchanged(&path, b"expected", b"new").unwrap_err();
        assert!(!replacement_was_published(&error));
        assert_eq!(std::fs::read(&path).unwrap(), b"actual");

        replace_if_unchanged(&path, b"actual", b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_no_clobber_never_overwrites_an_existing_target() {
        let dir = temp_dir("no-clobber");
        let path = dir.join("target");
        std::fs::write(&path, b"keep me").unwrap();

        let error = create_no_clobber(&path, b"incoming").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");

        std::fs::remove_file(&path).unwrap();
        create_no_clobber(&path, b"incoming").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"incoming");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remove_durable_is_idempotent_and_syncs_the_parent() {
        let dir = temp_dir("remove");
        let path = dir.join("target");
        std::fs::write(&path, b"data").unwrap();

        remove_durable(&path).unwrap();
        assert!(!path.exists());
        remove_durable(&path).unwrap(); // NotFound is success
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
