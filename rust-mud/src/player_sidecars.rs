//! One lifecycle for name-keyed player sidecars (#413).
//!
//! SQL identifies a player by idnum, while rent and alias data are keyed by
//! the lower-cased player name. Permanent deletion and rename therefore have
//! to treat both files as one lifecycle. Missing files are valid (a player may
//! have no rent or aliases); every other filesystem error is returned to the
//! caller so it can fail closed and emit an audit record.

use std::ffi::CString;
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct PlayerSidecarError {
    failures: Vec<String>,
    rollback_incomplete: bool,
}

impl PlayerSidecarError {
    fn new(failures: Vec<String>) -> Self {
        Self {
            failures,
            rollback_incomplete: false,
        }
    }

    fn with_incomplete_rollback(failures: Vec<String>) -> Self {
        Self {
            failures,
            rollback_incomplete: true,
        }
    }

    /// A forward rename failed after at least one sidecar moved, and restoring
    /// one of those moves also failed. Callers must surface a critical
    /// cross-store inconsistency rather than claiming the old identity is
    /// fully restored.
    pub fn rollback_incomplete(&self) -> bool {
        self.rollback_incomplete
    }
}

impl fmt::Display for PlayerSidecarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.failures.join("; "))
    }
}

impl std::error::Error for PlayerSidecarError {}

#[derive(Debug)]
struct SidecarMove {
    label: &'static str,
    from: PathBuf,
    to: PathBuf,
}

fn paths_for(lib_path: &str, name: &str) -> Result<[(&'static str, PathBuf); 2], String> {
    let rent = crate::objsave::crash_filename(lib_path, name)
        .ok_or_else(|| format!("invalid player name '{name}' for rent sidecar"))?;
    let alias = crate::alias::alias_filename(lib_path, name)
        .ok_or_else(|| format!("invalid player name '{name}' for alias sidecar"))?;
    Ok([("rent", rent), ("alias", alias)])
}

fn remove_if_present(label: &str, path: &Path, failures: &mut Vec<String>) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => failures.push(format!("{label} sidecar {}: {error}", path.display())),
    }
}

/// Remove every durable sidecar and the idnum-keyed live alias cache.
///
/// Both removals are attempted even when the first fails, allowing a retry to
/// converge. The caller must retain an authoritative DB tombstone when this
/// returns an error; a missing sidecar is already-clean success.
pub fn delete_player_sidecars(
    lib_path: &str,
    name: &str,
    idnum: i64,
) -> Result<(), PlayerSidecarError> {
    crate::alias::clear_aliases(idnum);
    let paths = paths_for(lib_path, name).map_err(|error| PlayerSidecarError::new(vec![error]))?;
    let mut failures = Vec::new();
    for (label, path) in paths {
        remove_if_present(label, &path, &mut failures);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(PlayerSidecarError::new(failures))
    }
}

fn metadata_for_preflight(
    label: &str,
    path: &Path,
    failures: &mut Vec<String>,
) -> Option<std::fs::Metadata> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            failures.push(format!(
                "cannot inspect {label} sidecar {}: {error}",
                path.display()
            ));
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target path contains NUL"))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    // DeltaMUD is deployed on Linux. Keep non-Linux development builds
    // fail-closed by atomically reserving the destination with a hard link,
    // then removing the old name.
    std::fs::hard_link(from, to)?;
    if let Err(error) = std::fs::remove_file(from) {
        let _ = std::fs::remove_file(to);
        return Err(error);
    }
    Ok(())
}

/// Move rent and alias sidecars as one fail-closed rename lifecycle.
///
/// Preflight validates every source and destination before moving anything.
/// Destinations are never overwritten. If a later move fails, earlier moves
/// are rolled back; the error includes any rollback failure for audit. A live
/// alias table with no source file is materialized first so a successful rename
/// cannot leave those aliases durable only in memory.
pub fn rename_player_sidecars(
    lib_path: &str,
    old_name: &str,
    new_name: &str,
    idnum: i64,
) -> Result<(), PlayerSidecarError> {
    let old_paths =
        paths_for(lib_path, old_name).map_err(|error| PlayerSidecarError::new(vec![error]))?;
    let new_paths =
        paths_for(lib_path, new_name).map_err(|error| PlayerSidecarError::new(vec![error]))?;

    // If aliases exist only in the live table, make the old identity durable
    // before the transaction. We only call write_aliases after proving the old
    // path is absent, so it cannot unlink a known-good source on write failure.
    let old_alias = &old_paths[1].1;
    if !crate::alias::get_aliases(idnum).is_empty() {
        match std::fs::symlink_metadata(old_alias) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(PlayerSidecarError::new(vec![format!(
                    "alias sidecar source {} is not a regular file",
                    old_alias.display()
                )]));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                crate::alias::write_aliases(lib_path, old_name, idnum).map_err(|error| {
                    PlayerSidecarError::new(vec![format!(
                        "cannot materialize alias sidecar {}: {error}",
                        old_alias.display()
                    )])
                })?;
            }
            Err(error) => {
                return Err(PlayerSidecarError::new(vec![format!(
                    "cannot inspect alias sidecar {}: {error}",
                    old_alias.display()
                )]));
            }
        }
    }

    let mut failures = Vec::new();
    let mut moves = Vec::new();
    for ((label, from), (_, to)) in old_paths.into_iter().zip(new_paths) {
        if from == to {
            continue;
        }
        let Some(source_metadata) = metadata_for_preflight(label, &from, &mut failures) else {
            continue;
        };
        if !source_metadata.file_type().is_file() {
            failures.push(format!(
                "{label} sidecar source {} is not a regular file",
                from.display()
            ));
            continue;
        }
        if metadata_for_preflight(label, &to, &mut failures).is_some() {
            failures.push(format!(
                "{label} sidecar destination {} already exists",
                to.display()
            ));
            continue;
        }
        moves.push(SidecarMove { label, from, to });
    }

    if !failures.is_empty() {
        return Err(PlayerSidecarError::new(failures));
    }

    for sidecar in &moves {
        if let Some(parent) = sidecar.to.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            failures.push(format!(
                "cannot create {} sidecar directory {}: {error}",
                sidecar.label,
                parent.display()
            ));
        }
    }
    if !failures.is_empty() {
        return Err(PlayerSidecarError::new(failures));
    }

    let mut moved: Vec<SidecarMove> = Vec::new();
    for sidecar in moves {
        if let Err(error) = rename_no_replace(&sidecar.from, &sidecar.to) {
            failures.push(format!(
                "cannot rename {} sidecar {} to {}: {error}",
                sidecar.label,
                sidecar.from.display(),
                sidecar.to.display()
            ));
            let mut rollback_incomplete = false;
            for completed in moved.iter().rev() {
                if let Err(rollback_error) = rename_no_replace(&completed.to, &completed.from) {
                    rollback_incomplete = true;
                    failures.push(format!(
                        "cannot roll back {} sidecar {} to {}: {rollback_error}",
                        completed.label,
                        completed.to.display(),
                        completed.from.display()
                    ));
                }
            }
            return Err(if rollback_incomplete {
                PlayerSidecarError::with_incomplete_rollback(failures)
            } else {
                PlayerSidecarError::new(failures)
            });
        }
        moved.push(sidecar);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alias::{AliasEntry, clear_aliases, set_aliases};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_lib(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "deltamud-player-sidecars-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn seed_sidecars(lib: &Path, name: &str, idnum: i64) -> (PathBuf, PathBuf) {
        let lib = lib.to_str().unwrap();
        let rent = crate::objsave::crash_filename(lib, name).unwrap();
        std::fs::create_dir_all(rent.parent().unwrap()).unwrap();
        std::fs::write(&rent, b"rent").unwrap();
        set_aliases(
            idnum,
            vec![AliasEntry {
                alias: "greet".into(),
                replacement: "say hello".into(),
                atype: 0,
            }],
        );
        crate::alias::write_aliases(lib, name, idnum).unwrap();
        let alias = crate::alias::alias_filename(lib, name).unwrap();
        (rent, alias)
    }

    #[test]
    fn rename_moves_both_mixed_case_sidecars_without_clearing_live_aliases() {
        let lib = temp_lib("rename");
        let idnum = 9_413_001;
        let (old_rent, old_alias) = seed_sidecars(&lib, "OlDnAmE", idnum);
        let new_rent = crate::objsave::crash_filename(lib.to_str().unwrap(), "NeWnAmE").unwrap();
        let new_alias = crate::alias::alias_filename(lib.to_str().unwrap(), "NeWnAmE").unwrap();

        rename_player_sidecars(lib.to_str().unwrap(), "OlDnAmE", "NeWnAmE", idnum).unwrap();

        assert!(!old_rent.exists() && !old_alias.exists());
        assert!(new_rent.is_file() && new_alias.is_file());
        assert_eq!(crate::alias::get_aliases(idnum).len(), 1);
        clear_aliases(idnum);
        std::fs::remove_dir_all(lib).unwrap();
    }

    #[test]
    fn rename_accepts_missing_sidecars() {
        let lib = temp_lib("missing");
        rename_player_sidecars(lib.to_str().unwrap(), "Oldname", "Newname", 9_413_002).unwrap();
        std::fs::remove_dir_all(lib).unwrap();
    }

    #[test]
    fn blocked_destination_fails_before_moving_either_source() {
        let lib = temp_lib("blocked");
        let idnum = 9_413_003;
        let (old_rent, old_alias) = seed_sidecars(&lib, "Oldname", idnum);
        let blocked_alias = crate::alias::alias_filename(lib.to_str().unwrap(), "Newname").unwrap();
        std::fs::create_dir_all(blocked_alias.parent().unwrap()).unwrap();
        std::fs::write(&blocked_alias, b"belongs to another identity").unwrap();

        let error =
            rename_player_sidecars(lib.to_str().unwrap(), "Oldname", "Newname", idnum).unwrap_err();

        assert!(error.to_string().contains("destination"));
        assert!(old_rent.is_file() && old_alias.is_file());
        assert_eq!(
            std::fs::read(&blocked_alias).unwrap(),
            b"belongs to another identity"
        );
        clear_aliases(idnum);
        std::fs::remove_dir_all(lib).unwrap();
    }

    #[test]
    fn delete_attempts_both_files_and_clears_live_aliases() {
        let lib = temp_lib("delete");
        let idnum = 9_413_004;
        let (rent, alias) = seed_sidecars(&lib, "DeleteMe", idnum);

        delete_player_sidecars(lib.to_str().unwrap(), "dElEtEmE", idnum).unwrap();

        assert!(!rent.exists() && !alias.exists());
        assert!(crate::alias::get_aliases(idnum).is_empty());
        std::fs::remove_dir_all(lib).unwrap();
    }
}
