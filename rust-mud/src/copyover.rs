use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SNAPSHOT_VERSION: u32 = 2;
const MAX_SNAPSHOT_AGE_SECS: i64 = 300;
const MAX_SNAPSHOT_FUTURE_SKEW_SECS: i64 = 30;
const MAX_SNAPSHOT_CONNECTIONS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AffectSnapshot {
    pub spell_type: i32,
    pub duration: i32,
    pub modifier: i32,
    pub location: i32,
    pub bitvector: i64,
}

impl From<&crate::character::Affect> for AffectSnapshot {
    fn from(affect: &crate::character::Affect) -> Self {
        Self {
            spell_type: affect.spell_type,
            duration: affect.duration,
            modifier: affect.modifier,
            location: affect.location,
            bitvector: affect.bitvector,
        }
    }
}

impl From<&AffectSnapshot> for crate::character::Affect {
    fn from(affect: &AffectSnapshot) -> Self {
        Self {
            spell_type: affect.spell_type,
            duration: affect.duration,
            modifier: affect.modifier,
            location: affect.location,
            bitvector: affect.bitvector,
            // Runtime CharIds do not survive exec. The SQL affect format also
            // deliberately omits caster identity.
            caster: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CharacterSnapshot {
    pub idnum: i64,
    pub name: String,
    pub level: u8,
    pub class: u8,
    pub race: u8,
    pub sex: u8,
    pub alignment: i32,
    pub hometown: i32,
    pub gold: i32,
    pub bank_gold: i32,
    pub exp: i64,
    pub hit: i32,
    pub max_hit: i32,
    pub mana: i32,
    pub max_mana: i32,
    pub move_points: i32,
    pub max_move: i32,
    pub armor: i16,
    pub hitroll: i16,
    pub damroll: i16,
    pub strength: i8,
    pub strength_add: i8,
    pub intelligence: i8,
    pub wisdom: i8,
    pub dexterity: i8,
    pub constitution: i8,
    pub charisma: i8,
    pub affect_flags: i64,
    pub affected: Vec<AffectSnapshot>,
    pub wimp_level: i32,
    pub recall_level: i32,
    pub title: Option<String>,
    pub temporary_load_room: i64,
    pub map_x: i64,
    pub map_y: i64,
}

impl CharacterSnapshot {
    pub fn from_character(character: &crate::character::Character) -> Self {
        Self {
            idnum: character.idnum,
            name: character.player.name.clone(),
            level: character.player.level,
            class: character.player.class as u8,
            race: character.player.race as u8,
            sex: character.player.sex as u8,
            alignment: character.alignment,
            hometown: character.player.hometown,
            gold: character.points.gold,
            bank_gold: character.points.bank_gold,
            exp: character.points.exp,
            hit: character.points.hit,
            max_hit: character.points.max_hit,
            mana: character.points.mana,
            max_mana: character.points.max_mana,
            move_points: character.points.move_points,
            max_move: character.points.max_move,
            armor: character.points.armor,
            hitroll: character.points.hitroll,
            damroll: character.points.damroll,
            strength: character.real_abils.str,
            strength_add: character.real_abils.str_add,
            intelligence: character.real_abils.intel,
            wisdom: character.real_abils.wis,
            dexterity: character.real_abils.dex,
            constitution: character.real_abils.con,
            charisma: character.real_abils.cha,
            affect_flags: character.affect_flags,
            affected: character
                .affected
                .iter()
                .map(AffectSnapshot::from)
                .collect(),
            wimp_level: character.wimp_level,
            recall_level: character.recall_level,
            title: character.player.title.clone(),
            temporary_load_room: character.tloadroom,
            map_x: character.mapx,
            map_y: character.mapy,
        }
    }

    pub fn to_character(&self) -> crate::character::Character {
        let mut character = crate::character::Character::new_player(
            self.name.clone(),
            crate::types::Class::from_u8(self.class),
            crate::types::Race::from_u8(self.race),
        );
        character.idnum = self.idnum;
        character.player.level = self.level;
        character.player.sex = crate::types::Gender::from_u8(self.sex);
        character.alignment = self.alignment;
        character.player.hometown = self.hometown;
        crate::gold::set(
            &mut character,
            crate::gold::Account::Carried,
            i64::from(self.gold),
        );
        crate::gold::set(
            &mut character,
            crate::gold::Account::Bank,
            i64::from(self.bank_gold),
        );
        character.points.exp = self.exp;
        character.points.hit = self.hit;
        character.points.max_hit = self.max_hit;
        character.points.mana = self.mana;
        character.points.max_mana = self.max_mana;
        character.points.move_points = self.move_points;
        character.points.max_move = self.max_move;
        character.points.armor = self.armor;
        character.points.hitroll = self.hitroll;
        character.points.damroll = self.damroll;
        character.real_abils.str = self.strength;
        character.real_abils.str_add = self.strength_add;
        character.real_abils.intel = self.intelligence;
        character.real_abils.wis = self.wisdom;
        character.real_abils.dex = self.dexterity;
        character.real_abils.con = self.constitution;
        character.real_abils.cha = self.charisma;
        character.aff_abils = character.real_abils;
        character.affect_flags = self.affect_flags;
        character.affected = self
            .affected
            .iter()
            .map(crate::character::Affect::from)
            .collect();
        character.wimp_level = self.wimp_level;
        character.recall_level = self.recall_level;
        character.player.title = self.title.clone();
        character.tloadroom = self.temporary_load_room;
        character.mapx = self.map_x;
        character.mapy = self.map_y;
        character
    }

    fn validate(&self) -> Result<()> {
        if self.idnum <= 0 {
            bail!("copyover character {} has invalid idnum", self.name);
        }
        if !(2..=20).contains(&self.name.len())
            || !self.name.bytes().all(|byte| byte.is_ascii_alphabetic())
        {
            bail!("copyover character has invalid name {:?}", self.name);
        }
        if self.class > 4 || self.race > 13 || self.sex > 2 {
            bail!("copyover character {} has invalid enum values", self.name);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionSnapshot {
    pub fd: RawFd,
    pub host: String,
    pub character: CharacterSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotPayload {
    pub listener_fd: RawFd,
    pub entries: Vec<ConnectionSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotEnvelope {
    version: u32,
    created_unix_secs: i64,
    count: usize,
    complete: bool,
    checksum_sha256: String,
    payload: SnapshotPayload,
}

#[derive(Serialize)]
struct ChecksumMaterial<'a> {
    version: u32,
    created_unix_secs: i64,
    count: usize,
    complete: bool,
    payload: &'a SnapshotPayload,
}

fn checksum(
    version: u32,
    created_unix_secs: i64,
    count: usize,
    complete: bool,
    payload: &SnapshotPayload,
) -> Result<String> {
    let material = ChecksumMaterial {
        version,
        created_unix_secs,
        count,
        complete,
        payload,
    };
    let bytes = serde_json::to_vec(&material).context("serialize copyover checksum material")?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_payload(payload: &SnapshotPayload, expected_listener: Option<RawFd>) -> Result<()> {
    if payload.listener_fd < 3 {
        bail!("copyover snapshot has invalid listener fd");
    }
    if let Some(expected) = expected_listener {
        if payload.listener_fd != expected {
            bail!(
                "copyover listener mismatch: snapshot {}, argv {}",
                payload.listener_fd,
                expected
            );
        }
    }
    if payload.entries.len() > MAX_SNAPSHOT_CONNECTIONS {
        bail!("copyover snapshot contains too many connections");
    }
    let mut fds = HashSet::new();
    let mut names = HashSet::new();
    let mut ids = HashSet::new();
    for entry in &payload.entries {
        if entry.fd < 3 || entry.fd == payload.listener_fd || !fds.insert(entry.fd) {
            bail!("copyover snapshot contains an invalid or duplicate client fd");
        }
        entry.character.validate()?;
        if entry.host.len() > 255 || entry.host.chars().any(char::is_control) {
            bail!("copyover snapshot contains an invalid host");
        }
        if !names.insert(entry.character.name.to_ascii_lowercase()) {
            bail!("copyover snapshot contains a duplicate player name");
        }
        if !ids.insert(entry.character.idnum) {
            bail!("copyover snapshot contains a duplicate player id");
        }
    }
    Ok(())
}

pub fn encode(payload: SnapshotPayload) -> Result<Vec<u8>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs() as i64;
    encode_at(payload, now)
}

fn encode_at(payload: SnapshotPayload, created_unix_secs: i64) -> Result<Vec<u8>> {
    validate_payload(&payload, None)?;
    let count = payload.entries.len();
    let complete = true;
    let envelope = SnapshotEnvelope {
        version: SNAPSHOT_VERSION,
        created_unix_secs,
        count,
        complete,
        checksum_sha256: checksum(
            SNAPSHOT_VERSION,
            created_unix_secs,
            count,
            complete,
            &payload,
        )?,
        payload,
    };
    serde_json::to_vec(&envelope).context("serialize copyover snapshot")
}

pub fn decode(bytes: &[u8], expected_listener: RawFd) -> Result<SnapshotPayload> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs() as i64;
    decode_at(bytes, expected_listener, now)
}

fn decode_at(
    bytes: &[u8],
    expected_listener: RawFd,
    now_unix_secs: i64,
) -> Result<SnapshotPayload> {
    let envelope: SnapshotEnvelope =
        serde_json::from_slice(bytes).context("parse copyover snapshot")?;
    if envelope.version != SNAPSHOT_VERSION {
        bail!("unsupported copyover snapshot version {}", envelope.version);
    }
    if !envelope.complete {
        bail!("copyover snapshot is incomplete");
    }
    let age = now_unix_secs.saturating_sub(envelope.created_unix_secs);
    if age > MAX_SNAPSHOT_AGE_SECS
        || envelope.created_unix_secs > now_unix_secs + MAX_SNAPSHOT_FUTURE_SKEW_SECS
    {
        bail!("copyover snapshot is stale or has an invalid timestamp");
    }
    if envelope.count != envelope.payload.entries.len() {
        bail!("copyover snapshot record count mismatch");
    }
    let actual_checksum = checksum(
        envelope.version,
        envelope.created_unix_secs,
        envelope.count,
        envelope.complete,
        &envelope.payload,
    )?;
    if envelope.checksum_sha256 != actual_checksum {
        bail!("copyover snapshot checksum mismatch");
    }
    validate_payload(&envelope.payload, Some(expected_listener))?;
    Ok(envelope.payload)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "copyover.snapshot".into());
    name.push(format!(".new.{}", std::process::id()));
    path.with_file_name(name)
}

fn write_encoded<W: Write>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    writer
        .write_all(bytes)
        .context("write complete copyover snapshot")?;
    writer.flush().context("flush copyover snapshot")
}

pub fn write_atomic(path: &Path, payload: SnapshotPayload) -> Result<()> {
    let bytes = encode(payload)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("copyover snapshot has no parent directory"))?;
    std::fs::create_dir_all(parent).context("create copyover snapshot directory")?;
    let temporary = temp_path(path);
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .with_context(|| format!("open {}", temporary.display()))?;
        write_encoded(&mut file, &bytes)?;
        file.sync_all().context("sync copyover snapshot")?;
        drop(file);
        std::fs::rename(&temporary, path).context("publish copyover snapshot")?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .context("sync copyover snapshot directory")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn read_validated(path: &Path, expected_listener: RawFd) -> Result<SnapshotPayload> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read copyover snapshot {}", path.display()))?;
    decode(&bytes, expected_listener)
}

/// A validated recovery file is deliberately inert until `commit` is called.
/// Any parse, fd-validation, database, or stream-setup error simply drops this
/// value and leaves the snapshot on disk for diagnosis/retry.
pub struct RecoverySnapshot {
    path: PathBuf,
    payload: SnapshotPayload,
}

impl RecoverySnapshot {
    pub fn open(path: &Path, expected_listener: RawFd) -> Result<Self> {
        let payload = read_validated(path, expected_listener)?;
        Ok(Self {
            path: path.to_path_buf(),
            payload,
        })
    }

    pub fn payload(&self) -> &SnapshotPayload {
        &self.payload
    }

    /// Consume the recovery evidence only after the caller has prepared the
    /// entire database/socket set. A failed unlink remains an error and does
    /// not permit partial adoption.
    pub fn commit(self) -> Result<()> {
        std::fs::remove_file(&self.path)
            .with_context(|| format!("consume copyover snapshot {}", self.path.display()))
    }
}

fn fd_flags(fd: RawFd) -> Result<libc::c_int> {
    if fd < 3 {
        bail!("copyover fd {fd} is reserved or invalid");
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("inspect copyover fd {fd}"));
    }
    Ok(flags)
}

fn socket_int_option(fd: RawFd, option: libc::c_int) -> Result<libc::c_int> {
    let mut value = 0 as libc::c_int;
    let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            &mut value as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("inspect socket option on copyover fd {fd}"));
    }
    Ok(value)
}

fn validate_socket_fd(fd: RawFd, listener: bool) -> Result<()> {
    fd_flags(fd)?;
    if socket_int_option(fd, libc::SO_TYPE)? != libc::SOCK_STREAM {
        bail!("copyover fd {fd} is not a stream socket");
    }
    let accepts = socket_int_option(fd, libc::SO_ACCEPTCONN)? != 0;
    if accepts != listener {
        if listener {
            bail!("copyover listener fd {fd} is not listening");
        }
        bail!("copyover client fd {fd} is a listening socket");
    }
    if !listener {
        let mut address = std::mem::MaybeUninit::<libc::sockaddr_storage>::uninit();
        let mut length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let result = unsafe {
            libc::getpeername(fd, address.as_mut_ptr() as *mut libc::sockaddr, &mut length)
        };
        if result < 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("validate connected copyover client fd {fd}"));
        }
    }
    Ok(())
}

/// Verify the complete inherited descriptor set without taking ownership of a
/// single fd. Structural validation already rejects duplicate numbers; these
/// checks reject closed/reused files, wrong socket types, listeners in client
/// slots, and unconnected stream sockets.
pub fn validate_inherited_fds(payload: &SnapshotPayload) -> Result<()> {
    validate_payload(payload, Some(payload.listener_fd))?;
    validate_socket_fd(payload.listener_fd, true)?;
    for entry in &payload.entries {
        validate_socket_fd(entry.fd, false)?;
    }
    Ok(())
}

/// Transactionally clear FD_CLOEXEC for the descriptors that must survive the
/// imminent exec. Original flags are restored on every returned/aborted path.
pub struct InheritedFdGuard {
    original_flags: Vec<(RawFd, libc::c_int)>,
    armed: bool,
}

impl InheritedFdGuard {
    pub fn clear_for_exec(fds: &[RawFd]) -> Result<Self> {
        Self::clear_for_exec_with_setter(fds, |fd, flags| {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }

    fn clear_for_exec_with_setter(
        fds: &[RawFd],
        mut set_flags: impl FnMut(RawFd, libc::c_int) -> io::Result<()>,
    ) -> Result<Self> {
        let mut unique = HashSet::new();
        let mut original_flags = Vec::with_capacity(fds.len());
        for &fd in fds {
            if !unique.insert(fd) {
                bail!("copyover inheritance set contains duplicate fd {fd}");
            }
            original_flags.push((fd, fd_flags(fd)?));
        }

        let mut guard = Self {
            original_flags,
            armed: true,
        };
        for index in 0..guard.original_flags.len() {
            let (fd, flags) = guard.original_flags[index];
            if flags & libc::FD_CLOEXEC == 0 {
                continue;
            }
            if let Err(clear_error) = set_flags(fd, flags & !libc::FD_CLOEXEC) {
                let rollback_error = guard.restore().err();
                if let Some(rollback_error) = rollback_error {
                    bail!(
                        "clear FD_CLOEXEC on fd {fd}: {clear_error}; rollback also failed: {rollback_error:#}"
                    );
                }
                return Err(clear_error).with_context(|| format!("clear FD_CLOEXEC on fd {fd}"));
            }
        }
        Ok(guard)
    }

    fn restore(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        let mut errors = Vec::new();
        for &(fd, flags) in self.original_flags.iter().rev() {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
                errors.push(format!("fd {fd}: {}", io::Error::last_os_error()));
            }
        }
        if errors.is_empty() {
            self.armed = false;
            Ok(())
        } else {
            bail!("restore copyover fd flags failed ({})", errors.join(", "))
        }
    }

    pub fn rollback(mut self) -> Result<()> {
        self.restore()
    }
}

impl Drop for InheritedFdGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn payload() -> SnapshotPayload {
        let mut character = crate::character::Character::new_player(
            "Copytester".into(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        character.idnum = 42;
        character.player.title = Some("the | delimiter-safe \"hero\"".into());
        SnapshotPayload {
            listener_fd: 7,
            entries: vec![ConnectionSnapshot {
                fd: 8,
                host: "example.test".into(),
                character: CharacterSnapshot::from_character(&character),
            }],
        }
    }

    #[test]
    fn structured_snapshot_round_trips_separator_titles() {
        let original = payload();
        let decoded = decode(&encode(original.clone()).unwrap(), 7).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn snapshot_hydration_clamps_gold_to_the_shared_invariant() {
        let mut snapshot = payload().entries.remove(0).character;
        snapshot.gold = i32::MAX;
        snapshot.bank_gold = -1;
        snapshot.affect_flags = crate::flags::AFF_INVISIBLE;
        snapshot.wimp_level = 17;
        snapshot.recall_level = 23;
        snapshot.affected.push(AffectSnapshot {
            spell_type: 1,
            duration: 2,
            modifier: 3,
            location: 4,
            bitvector: crate::flags::AFF_INVISIBLE,
        });

        let character = snapshot.to_character();

        assert_eq!(
            crate::gold::balance(&character, crate::gold::Account::Carried),
            crate::gold::GOLD_CAP
        );
        assert_eq!(
            crate::gold::balance(&character, crate::gold::Account::Bank),
            0
        );
        assert_eq!(character.affect_flags, crate::flags::AFF_INVISIBLE);
        assert_eq!(character.wimp_level, 17);
        assert_eq!(character.recall_level, 23);
        assert_eq!(character.affected.len(), 1);
        assert_eq!(character.affected[0].caster, None);
    }

    #[test]
    fn validation_rejects_truncation_checksum_count_and_duplicates() {
        let bytes = encode(payload()).unwrap();
        assert!(decode(&bytes[..bytes.len() - 1], 7).is_err());

        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        envelope["count"] = 2.into();
        assert!(decode(&serde_json::to_vec(&envelope).unwrap(), 7).is_err());

        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        envelope["complete"] = false.into();
        assert!(decode(&serde_json::to_vec(&envelope).unwrap(), 7).is_err());

        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let duplicate = envelope["payload"]["entries"][0].clone();
        envelope["payload"]["entries"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(decode(&serde_json::to_vec(&envelope).unwrap(), 7).is_err());

        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        envelope["checksum_sha256"] = "00".repeat(32).into();
        assert!(decode(&serde_json::to_vec(&envelope).unwrap(), 7).is_err());
    }

    #[test]
    fn validation_rejects_stale_and_future_snapshots() {
        let bytes = encode_at(payload(), 1_000).unwrap();
        assert!(decode_at(&bytes, 7, 1_000 + MAX_SNAPSHOT_AGE_SECS).is_ok());
        assert!(decode_at(&bytes, 7, 1_001 + MAX_SNAPSHOT_AGE_SECS).is_err());

        let bytes = encode_at(payload(), 2_000).unwrap();
        assert!(decode_at(&bytes, 7, 2_000 - MAX_SNAPSHOT_FUTURE_SKEW_SECS).is_ok());
        assert!(decode_at(&bytes, 7, 1_999 - MAX_SNAPSHOT_FUTURE_SKEW_SECS).is_err());
    }

    fn temporary_snapshot_path(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "deltamud-copyover-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn recovery_snapshot_is_preserved_until_explicit_commit() {
        let path = temporary_snapshot_path("commit");
        write_atomic(&path, payload()).unwrap();

        assert!(RecoverySnapshot::open(&path, 99).is_err());
        assert!(
            path.exists(),
            "listener mismatch consumed recovery evidence"
        );

        let recovery = RecoverySnapshot::open(&path, 7).unwrap();
        assert_eq!(recovery.payload().entries.len(), 1);
        drop(recovery);
        assert!(
            path.exists(),
            "dropping setup state consumed recovery evidence"
        );

        RecoverySnapshot::open(&path, 7).unwrap().commit().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn malformed_recovery_snapshot_remains_on_disk() {
        let path = temporary_snapshot_path("malformed");
        std::fs::write(&path, b"{\"version\":2").unwrap();
        assert!(RecoverySnapshot::open(&path, 7).is_err());
        assert!(path.exists());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn inherited_fd_validation_checks_the_complete_socket_set() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut actual = payload();
        actual.listener_fd = listener.as_raw_fd();
        actual.entries[0].fd = server.as_raw_fd();
        validate_inherited_fds(&actual).unwrap();

        actual.entries[0].fd = listener.as_raw_fd();
        assert!(validate_inherited_fds(&actual).is_err());

        let ordinary_file = File::open("/dev/null").unwrap();
        actual.entries[0].fd = ordinary_file.as_raw_fd();
        assert!(validate_inherited_fds(&actual).is_err());
        drop(client);
    }

    fn set_cloexec(fd: RawFd, enabled: bool) {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        let updated = if enabled {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, updated) }, 0);
    }

    fn has_cloexec(fd: RawFd) -> bool {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn cloexec_guard_restores_original_flags_on_abort() {
        let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        set_cloexec(left.as_raw_fd(), true);
        set_cloexec(right.as_raw_fd(), false);

        let guard =
            InheritedFdGuard::clear_for_exec(&[left.as_raw_fd(), right.as_raw_fd()]).unwrap();
        assert!(!has_cloexec(left.as_raw_fd()));
        assert!(!has_cloexec(right.as_raw_fd()));
        guard.rollback().unwrap();

        assert!(has_cloexec(left.as_raw_fd()));
        assert!(!has_cloexec(right.as_raw_fd()));
    }

    #[test]
    fn cloexec_partial_failure_rolls_back_already_changed_fds() {
        let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        set_cloexec(left.as_raw_fd(), true);
        set_cloexec(right.as_raw_fd(), true);

        // Inject the second F_SETFD failure without closing a process-wide fd.
        // Closing it here made this test race every other parallel test that
        // opens a file or socket: the kernel could reuse the number before the
        // guard reached it, turning the intended failure into a false success.
        let fail_fd = right.as_raw_fd();
        let result = InheritedFdGuard::clear_for_exec_with_setter(
            &[left.as_raw_fd(), fail_fd],
            |fd, flags| {
                if fd == fail_fd {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert!(has_cloexec(left.as_raw_fd()));
        assert!(has_cloexec(right.as_raw_fd()));
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ShortWriter(Vec<u8>);

    impl Write for ShortWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let count = buf.len().min(3);
            self.0.extend_from_slice(&buf[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn checked_writer_handles_short_writes_and_surfaces_enospc() {
        let bytes = encode(payload()).unwrap();
        let mut short = ShortWriter(Vec::new());
        write_encoded(&mut short, &bytes).unwrap();
        assert_eq!(short.0, bytes);
        assert!(write_encoded(&mut FailingWriter, &bytes).is_err());
    }

    #[test]
    fn failed_atomic_write_preserves_the_published_snapshot() {
        let path = temporary_snapshot_path("preserve-old");
        let previous = b"previous recovery evidence";
        std::fs::write(&path, previous).unwrap();

        // Occupying the sibling temporary path with a directory forces the
        // new publication to fail before rename. The previously published
        // recovery evidence must remain byte-for-byte intact.
        let temporary = temp_path(&path);
        std::fs::create_dir(&temporary).unwrap();
        assert!(write_atomic(&path, payload()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), previous);

        std::fs::remove_dir(temporary).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
