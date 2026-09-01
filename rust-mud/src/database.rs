// Standard persistence (MySQL via mysql_async).
//
// This targets the C-compatible 83-column `player_main` table plus
// `player_affects` and `player_skills`, so the on-disk format round-trips
// losslessly and is cross-compatible with the original C DeltaMUD. All
// row<->Character translation lives in database_compat.rs (the single source
// of truth for the column set/order); this file only owns the SQL plumbing.

use crate::character::Character;
use crate::database_compat as compat;
use anyhow::{Context, Result, bail};
use mysql_async::{Conn, Opts, Pool, Row, Value, params, prelude::*};
use sha2::{Digest, Sha256};

pub const EXPECTED_SCHEMA_VERSION: u64 = 4;

/// One cooperative, database-scoped exclusion domain for the live server and
/// every offline maintenance mode which can change durable state behind the
/// server's single-owner in-memory world. The SHA-256 result is exactly MySQL's
/// 64-byte named-lock limit and does not expose the database name in metadata.
const RUNTIME_MAINTENANCE_LOCK_SQL: &str =
    "SELECT SHA2(CONCAT('deltamud-runtime-maintenance:', DATABASE()), 256)";

struct Migration {
    version: u64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "player_main",
        sql: include_str!("../migrations/0001_player_main.sql"),
    },
    Migration {
        version: 2,
        name: "player_affects",
        sql: include_str!("../migrations/0002_player_affects.sql"),
    },
    Migration {
        version: 3,
        name: "player_skills",
        sql: include_str!("../migrations/0003_player_skills.sql"),
    },
    Migration {
        version: 4,
        name: "password_phc_capacity",
        sql: include_str!("../migrations/0004_password_phc_capacity.sql"),
    },
];

const PLAYER_AFFECT_COLUMNS: &[&str] = &[
    "idnum",
    "type",
    "duration",
    "modifier",
    "location",
    "bitvector",
];
const PLAYER_SKILL_COLUMNS: &[&str] = &["idnum", "skill", "learned"];
const SCHEMA_MIGRATION_COLUMNS: &[&str] = &["version", "name", "checksum", "applied_at"];

fn migration_checksum(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

type AuthorityRow = (
    String,
    Option<i32>,
    Option<i32>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn validate_authority_identity(idnum: i64, expected_name: Option<&str>) -> Result<()> {
    if idnum <= 0 {
        bail!("player authority idnum must be positive");
    }
    if let Some(expected_name) = expected_name {
        compat::validate_persisted_player_name(expected_name)
            .context("invalid expected player authority name")?;
    }
    Ok(())
}

fn validate_authority_state(label: &str, state: crate::PlayerAuthorityState) -> Result<()> {
    compat::validated_player_level(i32::from(state.level))
        .with_context(|| format!("invalid {label} player authority level"))?;
    compat::validated_player_trust(state.trust)
        .with_context(|| format!("invalid {label} player command trust"))?;
    Ok(())
}

fn decode_authority_row(
    idnum: i64,
    row: AuthorityRow,
) -> Result<(String, crate::PlayerAuthorityState)> {
    let (name, level, trust, exp, godcmds1, godcmds2, godcmds3, godcmds4) = row;
    compat::validate_persisted_player_name(&name)
        .with_context(|| format!("player_main idnum {idnum} has an invalid name"))?;
    let level = level
        .ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has a NULL authority level"))?;
    let level = compat::validated_player_level(level)
        .with_context(|| format!("player_main idnum {idnum} has an invalid authority level"))?;
    let trust =
        trust.ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has NULL command trust"))?;
    let trust = compat::validated_player_trust(trust)
        .with_context(|| format!("player_main idnum {idnum} has invalid command trust"))?;
    let exp =
        exp.ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has NULL experience"))?;
    let godcmds1 =
        godcmds1.ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has NULL godcmds1"))?;
    let godcmds2 =
        godcmds2.ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has NULL godcmds2"))?;
    let godcmds3 =
        godcmds3.ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has NULL godcmds3"))?;
    let godcmds4 =
        godcmds4.ok_or_else(|| anyhow::anyhow!("player_main idnum {idnum} has NULL godcmds4"))?;
    let state = crate::PlayerAuthorityState {
        level,
        trust,
        exp,
        godcmds1,
        godcmds2,
        godcmds3,
        godcmds4,
    };
    validate_authority_state("durable", state)?;
    Ok((name, state))
}

pub struct Database {
    pool: Pool,
    /// A dedicated lease connection must not come from `pool`: dropping a
    /// pooled connection can return its MySQL session to the pool while named
    /// locks remain session-owned. A standalone connection closes on drop.
    connection_opts: Opts,
}

/// Process-lifetime exclusion against another Rust server or an offline
/// migration/bootstrap command using the same durable MySQL schema.
pub struct RuntimeDatabaseLease {
    conn: Option<Conn>,
    lock_name: String,
}

fn require_lock_acquired(acquired: Option<i64>, purpose: &str) -> Result<()> {
    if acquired == Some(1) {
        Ok(())
    } else {
        bail!(
            "cannot start {purpose}: this database is in use by a running DeltaMUD server or another maintenance command"
        )
    }
}

fn require_lock_still_owned(owned_by_this_connection: Option<i64>) -> Result<()> {
    if owned_by_this_connection == Some(1) {
        Ok(())
    } else {
        bail!("runtime database exclusion lease is no longer owned by this process")
    }
}

async fn release_exclusive_connection(
    mut conn: Conn,
    lock_name: &str,
    purpose: &str,
) -> Result<()> {
    let release_result: Result<Option<i64>> = conn
        .exec_first("SELECT RELEASE_LOCK(?)", (lock_name,))
        .await
        .with_context(|| format!("release {purpose} database exclusion lease"));
    // Always close this standalone session. Even if RELEASE_LOCK failed, a
    // confirmed connection close is the final backstop which relinquishes all
    // session-owned advisory locks.
    let disconnect_result = conn
        .disconnect()
        .await
        .with_context(|| format!("disconnect {purpose} database lease session"));

    match (release_result, disconnect_result) {
        (Ok(Some(1)), Ok(())) => Ok(()),
        (Ok(released), Ok(())) => {
            bail!("{purpose} database exclusion lease release returned {released:?}")
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(Some(1)), Err(error)) => Err(error),
        (Ok(released), Err(disconnect_error)) => Err(disconnect_error.context(format!(
            "{purpose} database exclusion lease release returned {released:?}"
        ))),
        (Err(error), Err(disconnect_error)) => Err(error.context(format!(
            "{purpose} database lease session also failed to disconnect: {disconnect_error}"
        ))),
    }
}

impl RuntimeDatabaseLease {
    /// Verify both connectivity and ownership on the exact session which
    /// acquired the lock. `mysql_async::Conn` does not transparently replace a
    /// standalone connection, so a broken session cannot silently reacquire.
    pub async fn verify_owned(&mut self) -> Result<()> {
        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("runtime database lease was already released"))?;
        let owned: Option<i64> = conn
            .exec_first(
                "SELECT IS_USED_LOCK(?) = CONNECTION_ID()",
                (&self.lock_name,),
            )
            .await
            .context("verify runtime database exclusion lease")?;
        require_lock_still_owned(owned)
    }

    /// Release and close the dedicated session. The `Drop` fallback also
    /// closes a still-present standalone connection on early-return paths.
    pub async fn release(mut self) -> Result<()> {
        let conn = self
            .conn
            .take()
            .ok_or_else(|| anyhow::anyhow!("runtime database lease was already released"))?;
        release_exclusive_connection(conn, &self.lock_name, "runtime").await
    }
}

impl Database {
    pub fn new(database_url: &str) -> Result<Self> {
        // Parse once through the fallible path so neither Pool nor the
        // standalone lease connection can panic on malformed configuration.
        let connection_opts = Opts::from_url(database_url).context("invalid DATABASE_URL")?;
        Ok(Database {
            pool: Pool::new(connection_opts.clone()),
            connection_opts,
        })
    }

    async fn acquire_exclusive_connection(
        &self,
        purpose: &str,
        wait_seconds: u32,
    ) -> Result<(Conn, String)> {
        let mut conn = Conn::new(self.connection_opts.clone())
            .await
            .with_context(|| format!("connect {purpose} database lease session"))?;
        let acquisition_result: Result<String> = async {
            let lock_name: String = conn
                .query_first(RUNTIME_MAINTENANCE_LOCK_SQL)
                .await?
                .flatten()
                .ok_or_else(|| anyhow::anyhow!("cannot derive database exclusion lock name"))?;
            let acquired: Option<i64> = conn
                .exec_first("SELECT GET_LOCK(?, ?)", (&lock_name, wait_seconds))
                .await?;
            require_lock_acquired(acquired, purpose)?;
            Ok(lock_name)
        }
        .await;

        match acquisition_result {
            Ok(lock_name) => Ok((conn, lock_name)),
            Err(error) => {
                // Do not return a potentially lock-bearing session to a pool;
                // this connection is standalone and is closed explicitly.
                if let Err(disconnect_error) = conn.disconnect().await {
                    return Err(error.context(format!(
                        "failed to close rejected {purpose} lease session: {disconnect_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    /// Acquire the lease before normal schema verification/world boot and hold
    /// it until the game process has stopped. The one-second bounded wait lets
    /// MySQL observe the old lease socket closing across `execv` copyover; a
    /// genuinely concurrent server or maintenance command still fails startup.
    pub async fn acquire_runtime_lease(&self) -> Result<RuntimeDatabaseLease> {
        let (conn, lock_name) = self
            .acquire_exclusive_connection("server runtime", 1)
            .await?;
        Ok(RuntimeDatabaseLease {
            conn: Some(conn),
            lock_name,
        })
    }

    pub async fn init_tables(&self) -> Result<()> {
        self.apply_migrations().await
    }

    /// Apply the ordered, checksummed schema migration set. Every statement is
    /// idempotent because MySQL DDL commits independently; the migration row is
    /// recorded only after its statement succeeds.
    pub async fn apply_migrations(&self) -> Result<()> {
        let (mut conn, lock_name) = self
            .acquire_exclusive_connection("schema migration", 1)
            .await?;

        let migration_result = async {
            conn.query_drop(
                r"CREATE TABLE IF NOT EXISTS schema_migrations (
                    version BIGINT UNSIGNED PRIMARY KEY,
                    name VARCHAR(128) NOT NULL,
                    checksum CHAR(64) NOT NULL,
                    applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
                ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            )
            .await?;

            for migration in MIGRATIONS {
                let checksum = migration_checksum(migration.sql);
                let existing: Option<(String, String)> = conn
                    .exec_first(
                        "SELECT name, checksum FROM schema_migrations WHERE version = ?",
                        (migration.version,),
                    )
                    .await?;
                if let Some((existing_name, existing_checksum)) = existing {
                    if existing_name != migration.name || existing_checksum != checksum {
                        bail!(
                            "schema migration {} ({}) metadata mismatch",
                            migration.version,
                            migration.name
                        );
                    }
                    continue;
                }

                conn.query_drop(migration.sql).await.with_context(|| {
                    format!(
                        "apply schema migration {} ({})",
                        migration.version, migration.name
                    )
                })?;
                conn.exec_drop(
                    "INSERT INTO schema_migrations (version, name, checksum) VALUES (?, ?, ?)",
                    (migration.version, migration.name, &checksum),
                )
                .await
                .with_context(|| {
                    format!(
                        "record schema migration {} ({})",
                        migration.version, migration.name
                    )
                })?;
            }
            self.verify_schema_with_conn(&mut conn).await
        }
        .await;

        let release_result =
            release_exclusive_connection(conn, &lock_name, "schema migration").await;
        match (migration_result, release_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(release_error)) => Err(error.context(format!(
                "schema migration also failed to release its database exclusion lease: {release_error}"
            ))),
        }
    }

    pub async fn verify_schema(&self) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        self.verify_schema_with_conn(&mut conn).await
    }

    async fn verify_schema_with_conn(&self, conn: &mut mysql_async::Conn) -> Result<()> {
        let table_exists: Option<u8> = conn
            .exec_first(
                "SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = DATABASE() AND table_name = 'schema_migrations'",
                (),
            )
            .await?;
        if table_exists.is_none() {
            bail!("database schema is not initialized; run deltamud --migrate");
        }

        let highest: Option<u64> = conn
            .query_first("SELECT MAX(version) FROM schema_migrations")
            .await?
            .flatten();
        if highest != Some(EXPECTED_SCHEMA_VERSION) {
            bail!(
                "database schema version is {:?}, expected {}",
                highest,
                EXPECTED_SCHEMA_VERSION
            );
        }
        for migration in MIGRATIONS {
            let existing: Option<(String, String)> = conn
                .exec_first(
                    "SELECT name, checksum FROM schema_migrations WHERE version = ?",
                    (migration.version,),
                )
                .await?;
            let Some((name, checksum)) = existing else {
                bail!("database schema is missing migration {}", migration.version);
            };
            if name != migration.name || checksum != migration_checksum(migration.sql) {
                bail!(
                    "database schema migration {} metadata does not match this binary",
                    migration.version
                );
            }
        }
        self.verify_storage_shape(conn).await?;
        Ok(())
    }

    async fn verify_storage_shape(&self, conn: &mut mysql_async::Conn) -> Result<()> {
        self.verify_table_columns(conn, "player_main", compat::PLAYER_MAIN_COLUMNS)
            .await?;
        self.verify_table_columns(conn, "player_affects", PLAYER_AFFECT_COLUMNS)
            .await?;
        self.verify_table_columns(conn, "player_skills", PLAYER_SKILL_COLUMNS)
            .await?;
        self.verify_table_columns(conn, "schema_migrations", SCHEMA_MIGRATION_COLUMNS)
            .await?;
        self.verify_single_column_primary_key(conn, "player_main", "idnum")
            .await?;
        self.verify_single_column_primary_key(conn, "schema_migrations", "version")
            .await?;

        let password_column: Option<(String, Option<u64>)> = conn
            .exec_first(
                "SELECT data_type, character_maximum_length \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = 'player_main' \
                 AND column_name = 'pwd'",
                (),
            )
            .await?;
        let Some((data_type, width)) = password_column else {
            bail!("player_main.pwd is missing");
        };
        if !data_type.eq_ignore_ascii_case("varchar") || width.is_none_or(|width| width < 255) {
            bail!("player_main.pwd must be VARCHAR(255) or wider");
        }

        let name_column: Option<(String, Option<u64>, Option<String>, String)> = conn
            .exec_first(
                "SELECT data_type, character_maximum_length, collation_name, is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = 'player_main' \
                 AND column_name = 'name'",
                (),
            )
            .await?;
        let Some((name_type, name_width, name_collation, name_nullable)) = name_column else {
            bail!("player_main.name is missing");
        };
        let name_is_case_insensitive = name_collation
            .as_deref()
            .is_some_and(|collation| collation.to_ascii_lowercase().ends_with("_ci"));
        if !name_type.eq_ignore_ascii_case("varchar")
            || name_width.is_none_or(|width| width < 30)
            || !name_is_case_insensitive
            || !name_nullable.eq_ignore_ascii_case("NO")
        {
            bail!(
                "player_main.name must be a non-null VARCHAR(30) or wider with a case-insensitive collation"
            );
        }

        let level_type: Option<String> = conn
            .exec_first(
                "SELECT data_type FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = 'player_main' \
                 AND column_name = 'level'",
                (),
            )
            .await?;
        if !level_type.is_some_and(|kind| kind.eq_ignore_ascii_case("tinyint")) {
            bail!("player_main.level must be TINYINT");
        }

        let idnum_column: Option<(String, String)> = conn
            .exec_first(
                "SELECT data_type, is_nullable FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = 'player_main' \
                 AND column_name = 'idnum'",
                (),
            )
            .await?;
        if !idnum_column.is_some_and(|(kind, nullable)| {
            kind.eq_ignore_ascii_case("int") && nullable.eq_ignore_ascii_case("NO")
        }) {
            bail!("player_main.idnum must be a non-null INT primary key");
        }

        let unique_name_indexes: Option<u64> = conn
            .query_first(
                "SELECT COUNT(*) FROM ( \
                    SELECT index_name \
                    FROM information_schema.statistics \
                    WHERE table_schema = DATABASE() AND table_name = 'player_main' \
                      AND non_unique = 0 \
                    GROUP BY index_name \
                    HAVING COUNT(*) = 1 AND MAX(column_name = 'name') = 1 \
                 ) AS unique_name_indexes",
            )
            .await?;
        if unique_name_indexes.unwrap_or(0) < 1 {
            bail!("player_main.name must have a single-column unique index");
        }
        self.verify_player_identity_rows(conn).await?;
        Ok(())
    }

    async fn verify_player_identity_rows(&self, conn: &mut mysql_async::Conn) -> Result<()> {
        let identities: Vec<(i64, String, Option<i32>, Option<i32>)> = conn
            .query("SELECT idnum, name, level, trust FROM player_main ORDER BY idnum")
            .await
            .context("read durable player identities for validation")?;
        for (idnum, name, level, trust) in identities {
            if idnum <= 0 {
                bail!("player_main contains non-positive player idnum {idnum}");
            }
            compat::validate_persisted_player_name(&name)
                .with_context(|| format!("player_main idnum {idnum} has an invalid name"))?;
            let level = level.ok_or_else(|| {
                anyhow::anyhow!("player_main idnum {idnum} has a NULL authorization level")
            })?;
            compat::validated_player_level(level)
                .with_context(|| format!("player_main idnum {idnum} has an invalid level"))?;
            let trust = trust.ok_or_else(|| {
                anyhow::anyhow!("player_main idnum {idnum} has a NULL command trust")
            })?;
            compat::validated_player_trust(trust)
                .with_context(|| format!("player_main idnum {idnum} has invalid command trust"))?;
        }
        Ok(())
    }

    async fn verify_single_column_primary_key(
        &self,
        conn: &mut mysql_async::Conn,
        table: &str,
        column: &str,
    ) -> Result<()> {
        let components: Vec<(u64, String)> = conn
            .exec(
                "SELECT seq_in_index, column_name \
                 FROM information_schema.statistics \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                   AND index_name = 'PRIMARY' AND non_unique = 0 \
                 ORDER BY seq_in_index",
                (table,),
            )
            .await?;
        if components.len() != 1 || components[0].0 != 1 || components[0].1 != column {
            bail!("database table {table} must have PRIMARY KEY ({column}) only");
        }
        Ok(())
    }

    async fn verify_table_columns(
        &self,
        conn: &mut mysql_async::Conn,
        table: &str,
        expected: &[&str],
    ) -> Result<()> {
        let actual: Vec<String> = conn
            .exec(
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY ordinal_position",
                (table,),
            )
            .await?;
        let actual_set = actual
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let expected_set = expected
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if actual_set != expected_set || actual.len() != expected.len() {
            let missing = expected_set
                .difference(&actual_set)
                .copied()
                .collect::<Vec<_>>();
            let unexpected = actual_set
                .difference(&expected_set)
                .copied()
                .collect::<Vec<_>>();
            bail!(
                "database table {table} has the wrong columns (missing: {missing:?}; unexpected: {unexpected:?})"
            );
        }
        Ok(())
    }

    pub async fn player_exists(&self, name: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let row: Option<Row> = conn
            .exec_first("SELECT idnum FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(row.is_some())
    }

    pub async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        let mut conn = self.pool.get_conn().await?;
        let stored: Option<String> = conn
            .exec_first("SELECT pwd FROM player_main WHERE name = ?", (name,))
            .await?;
        Ok(stored)
    }

    /// Change only the durable credential for the exact player identity. A
    /// full Character save is deliberately inappropriate here: it rewrites 83
    /// parent columns and two child tables from a potentially stale snapshot.
    pub async fn update_password_hash(
        &self,
        idnum: i64,
        expected_name: &str,
        expected_current_hash: Option<&str>,
        password_hash: &str,
    ) -> Result<crate::PasswordHashUpdateOutcome> {
        if idnum <= 0 || expected_name.is_empty() {
            return Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch);
        }
        if password_hash.is_empty() {
            bail!("refusing to persist an empty password hash");
        }

        let mut conn = self.pool.get_conn().await?;
        match expected_current_hash {
            Some(expected_current_hash) => {
                conn.exec_drop(
                    "UPDATE player_main SET pwd = ? \
                     WHERE idnum = ? AND name = ? AND BINARY pwd = BINARY ?",
                    (password_hash, idnum, expected_name, expected_current_hash),
                )
                .await?;
            }
            None => {
                conn.exec_drop(
                    "UPDATE player_main SET pwd = ? WHERE idnum = ? AND name = ?",
                    (password_hash, idnum, expected_name),
                )
                .await?;
            }
        }
        if conn.affected_rows() == 1 {
            return Ok(crate::PasswordHashUpdateOutcome::Updated);
        }

        // MySQL reports zero affected rows when the requested value is already
        // stored. The narrow read also distinguishes a CAS miss from an
        // identity mismatch without loading the Character or child tables.
        let current: Option<String> = conn
            .exec_first(
                "SELECT pwd FROM player_main WHERE idnum = ? AND name = ?",
                (idnum, expected_name),
            )
            .await?;
        Ok(match current {
            None => crate::PasswordHashUpdateOutcome::IdentityMismatch,
            Some(current) if current == password_hash => crate::PasswordHashUpdateOutcome::Updated,
            Some(_) => crate::PasswordHashUpdateOutcome::CurrentHashMismatch,
        })
    }

    /// Promote one existing mortal through the same database-scoped exclusion
    /// domain held for the live server. This both serializes bootstrap commands
    /// and prevents an offline privilege change behind a running world whose
    /// in-memory authorization state would otherwise remain stale.
    pub async fn bootstrap_implementor(
        &self,
        name: &str,
    ) -> Result<crate::ImplementorBootstrapOutcome> {
        let (mut conn, lock_name) = self
            .acquire_exclusive_connection("Implementor bootstrap", 1)
            .await?;

        let operation_result = async {
            let existing: Option<String> = conn
                .exec_first(
                    "SELECT name FROM player_main \
                     WHERE COALESCE(trust, 0) >= ? AND (COALESCE(act, 0) & ?) = 0 \
                     ORDER BY idnum LIMIT 1",
                    (crate::types::LVL_IMPL, crate::flags::PLR_DELETED),
                )
                .await?;
            if let Some(existing) = existing {
                return Ok(crate::ImplementorBootstrapOutcome::AlreadyExists(existing));
            }

            let (godcmds1, godcmds2, godcmds3, godcmds4) = crate::implementor_command_grants();
            conn.exec_drop(
                "UPDATE player_main \
                 SET level = ?, trust = ?, title = ?, godcmds1 = ?, godcmds2 = ?, \
                     godcmds3 = ?, godcmds4 = ? \
                 WHERE name = ? AND idnum > 0 AND COALESCE(trust, 0) < ? \
                   AND (COALESCE(act, 0) & ?) = 0",
                (
                    crate::types::LVL_IMPL,
                    i32::from(crate::types::LVL_IMPL),
                    "the Implementor",
                    godcmds1,
                    godcmds2,
                    godcmds3,
                    godcmds4,
                    name,
                    crate::types::LVL_IMPL,
                    crate::flags::PLR_DELETED,
                ),
            )
            .await?;
            if conn.affected_rows() == 1 {
                Ok(crate::ImplementorBootstrapOutcome::Promoted)
            } else {
                Ok(crate::ImplementorBootstrapOutcome::TargetNotFound)
            }
        }
        .await;

        let release_result =
            release_exclusive_connection(conn, &lock_name, "Implementor bootstrap").await;
        if let Err(release_error) = &release_result {
            log::warn!(
                "Implementor bootstrap database lease release was not confirmed: {release_error:#}"
            );
            // Once the narrow UPDATE was acknowledged, a later lock-release
            // failure must not turn the committed promotion into false failure.
            if operation_result
                .as_ref()
                .is_ok_and(|outcome| *outcome == crate::ImplementorBootstrapOutcome::Promoted)
            {
                return operation_result;
            }
        }
        match (operation_result, release_result) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(release_error)) => Err(error.context(format!(
                "Implementor bootstrap also failed to release its database exclusion lease: {release_error}"
            ))),
        }
    }

    pub async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let stored: Option<String> = conn
            .exec_first("SELECT pwd FROM player_main WHERE name = ?", (name,))
            .await?;
        let Some(stored) = stored else {
            return Ok(false);
        };
        Ok(crate::password::check_password_async(stored, password.to_owned()).await)
    }

    pub async fn create_player(&self, ch: &Character, password: &str) -> Result<i64> {
        let password_hash = crate::password::hash_password_async(password.to_owned())
            .await
            .ok_or_else(|| anyhow::anyhow!("password hashing worker failed"))?;
        self.create_player_with_password_hash(ch, &password_hash)
            .await
    }

    pub async fn create_player_with_password_hash(
        &self,
        ch: &Character,
        password_hash: &str,
    ) -> Result<i64> {
        if crate::password::password_needs_upgrade(password_hash) {
            bail!("new player credential is not a current bounded Argon2id hash");
        }
        // Hashing is complete before entering this short database critical
        // section. ID allocation and the collision-safe INSERT are serialized
        // per schema so concurrent same-name creators have one winner and
        // different-name creators cannot select the same MAX(idnum)+1.
        // Advisory locks are connection-owned. Keep this session standalone so
        // cancellation by TimedDatabase closes it instead of returning a
        // potentially lock-bearing session to a caller-configured pool whose
        // `reset_connection` option may be disabled.
        let mut conn = Conn::new(self.connection_opts.clone())
            .await
            .context("connect player creation lease session")?;
        let lock_name: String = conn
            .query_first("SELECT SHA2(CONCAT('deltamud-create-player:', DATABASE()), 256)")
            .await?
            .flatten()
            .ok_or_else(|| anyhow::anyhow!("cannot derive player creation lock name"))?;
        let lock_acquired: Option<i64> = conn
            .exec_first("SELECT GET_LOCK(?, 1)", (&lock_name,))
            .await?;
        if lock_acquired != Some(1) {
            let _ = conn.disconnect().await;
            bail!("another player creation is already allocating an identity");
        }

        let mut transaction = conn
            .start_transaction(mysql_async::TxOpts::default())
            .await?;
        let mutation_result: Result<i64> = async {
            let existing: Option<i64> = transaction
                .exec_first(
                    "SELECT idnum FROM player_main WHERE name = ? FOR UPDATE",
                    (ch.get_name(),),
                )
                .await?;
            if existing.is_some() {
                bail!("player name {} already exists", ch.get_name());
            }

            // This allocation remains under the database-scoped advisory lock
            // until commit; every server process using this binary observes one
            // ordered MAX+1 sequence. The primary key remains the final guard
            // against a non-cooperating writer.
            let max_idnum: Option<i64> = transaction
                .query_first("SELECT MAX(idnum) FROM player_main")
                .await?
                .flatten();
            let idnum = max_idnum
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("player idnum space is exhausted"))?;
            let mut stored = ch.clone();
            stored.idnum = idnum;

            let columns = compat::PLAYER_MAIN_COLUMNS;
            let values = compat::player_main_values(&stored, password_hash, "");
            let column_list = columns
                .iter()
                .map(|column| format!("`{column}`"))
                .collect::<Vec<_>>()
                .join(",");
            let placeholders = vec!["?"; columns.len()].join(",");
            transaction
                .exec_drop(
                    format!("INSERT INTO player_main ({column_list}) VALUES ({placeholders})"),
                    values,
                )
                .await
                .with_context(|| format!("insert new player identity {}", ch.get_name()))?;

            let skill_rows = compat::skill_rows(&stored);
            if !skill_rows.is_empty() {
                let params: Vec<_> = skill_rows
                    .into_iter()
                    .map(|(skill, learned)| {
                        params! { "idnum" => idnum, "skill" => skill, "learned" => learned }
                    })
                    .collect();
                transaction
                    .exec_batch(
                        "INSERT INTO player_skills (idnum,skill,learned) \
                         VALUES (:idnum,:skill,:learned)",
                        params,
                    )
                    .await?;
            }
            let affect_rows = compat::affect_rows(&stored);
            if !affect_rows.is_empty() {
                let params: Vec<_> = affect_rows
                    .into_iter()
                    .map(|(affect_type, duration, modifier, location, bitvector)| {
                        params! {
                            "idnum" => idnum,
                            "type" => affect_type,
                            "duration" => duration,
                            "modifier" => modifier,
                            "location" => location,
                            "bitvector" => bitvector,
                        }
                    })
                    .collect();
                transaction
                    .exec_batch(
                        "INSERT INTO player_affects \
                         (idnum,type,duration,modifier,location,bitvector) \
                         VALUES (:idnum,:type,:duration,:modifier,:location,:bitvector)",
                        params,
                    )
                    .await?;
            }
            Ok(idnum)
        }
        .await;

        let creation_result = match mutation_result {
            Ok(idnum) => match transaction.commit().await {
                Ok(()) => Ok(idnum),
                Err(commit_error) => {
                    // COMMIT errors can be outcome-ambiguous. Confirm the
                    // exact identity and credential before reporting success.
                    // Try the same checked-out session first; if it was broken
                    // by the ambiguous COMMIT, use another standalone session.
                    // Never request a second pooled connection while retaining
                    // the first one (which deadlocks with pool_max=1).
                    let same_session_observed: Option<(i64, String)> = conn
                        .exec_first(
                            "SELECT idnum, pwd FROM player_main WHERE name = ?",
                            (ch.get_name(),),
                        )
                        .await
                        .unwrap_or(None);
                    let observed = if same_session_observed.is_some() {
                        same_session_observed
                    } else {
                        match Conn::new(self.connection_opts.clone()).await {
                            Ok(mut read_conn) => {
                                let observed = read_conn
                                    .exec_first(
                                        "SELECT idnum, pwd FROM player_main WHERE name = ?",
                                        (ch.get_name(),),
                                    )
                                    .await
                                    .unwrap_or(None);
                                let _ = read_conn.disconnect().await;
                                observed
                            }
                            Err(_) => None,
                        }
                    };
                    if observed.as_ref().is_some_and(|(observed_id, hash)| {
                        *observed_id == idnum && hash == password_hash
                    }) {
                        log::warn!(
                            "player creation COMMIT for {} errored but exact durable identity was confirmed: {}",
                            ch.get_name(),
                            commit_error
                        );
                        Ok(idnum)
                    } else {
                        Err(anyhow::Error::new(commit_error).context(format!(
                            "commit new player {}; durable outcome was not confirmed",
                            ch.get_name()
                        )))
                    }
                }
            },
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    log::warn!(
                        "rollback new player {} after error also failed: {}",
                        ch.get_name(),
                        rollback_error
                    );
                }
                Err(error)
            }
        };

        let release_result =
            release_exclusive_connection(conn, &lock_name, "player creation").await;
        if let Err(release_error) = &release_result {
            log::warn!(
                "player creation advisory lock release was not confirmed: {release_error:#}"
            );
        }
        creation_result
    }

    pub async fn load_player(&self, name: &str) -> Result<Character> {
        let mut conn = self.pool.get_conn().await?;
        let row: Option<Row> = conn
            .exec_first("SELECT * FROM player_main WHERE name = ?", (name,))
            .await?;
        let row = row.ok_or_else(|| anyhow::anyhow!("Player not found"))?;

        let mut ch = compat::player_main_to_character(&row)?;

        // Merge affects (dbmodify_player_affects MODE_RETRIEVE).
        let aff_rows: Vec<Row> = conn
            .exec(
                "SELECT type,duration,modifier,location,bitvector FROM player_affects WHERE idnum = ?",
                (ch.idnum,),
            )
            .await?;
        compat::apply_affect_rows(&mut ch, &aff_rows);

        // Merge skills (dbmodify_player_skills MODE_RETRIEVE).
        let skill_rows: Vec<Row> = conn
            .exec(
                "SELECT idnum,skill,learned FROM player_skills WHERE idnum = ?",
                (ch.idnum,),
            )
            .await?;
        compat::apply_skill_rows(&mut ch, &skill_rows);

        Ok(ch)
    }

    /// build_player_index() (db.c): read every player's index row. C selects
    /// Pull the player_main fields needed for the in-memory player index and
    /// offline reports. last_logon is a unix BIGINT in player_main (see
    /// init_tables DDL), so it maps directly to the PlayerIndex i64.
    pub async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>> {
        let mut conn = self.pool.get_conn().await?;
        let rows: Vec<Row> = conn
            .exec(
                "SELECT idnum,name,level,trust,class,last_logon,host,act,clan,clan_rank FROM player_main",
                (),
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let idnum: i64 = row.get(0).unwrap_or(-1);
            let name: String = row.get(1).unwrap_or_default();
            let raw_level: i32 = row
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("player index row {idnum} has no level"))?;
            let level = compat::validated_player_level(raw_level).map_err(|error| {
                anyhow::anyhow!("player index row {idnum} has invalid level: {error}")
            })?;
            let trust: i32 = row
                .get(3)
                .ok_or_else(|| anyhow::anyhow!("player index row {idnum} has no trust"))?;
            compat::validated_player_trust(trust).map_err(|error| {
                anyhow::anyhow!("player index row {idnum} has invalid trust: {error}")
            })?;
            let class = crate::types::Class::from_u8(row.get::<u8, _>(4).unwrap_or(3));
            let last_logon: i64 = row.get(5).unwrap_or(0);
            let host: String = row.get(6).unwrap_or_default();
            let act_flags: i64 = row.get(7).unwrap_or(0);
            let clan: i32 = row.get(8).unwrap_or(-1);
            let clan_rank: i32 = row.get(9).unwrap_or(-1);
            out.push(crate::state::PlayerIndex {
                idnum,
                name,
                level,
                trust,
                class,
                last_logon,
                host,
                act_flags,
                clan,
                clan_rank,
            });
        }
        Ok(out)
    }

    /// delete_player_entry() for every PLR_DELETED player. Child tables are
    /// joined before player_main is removed so the act bit remains available.
    pub async fn delete_deleted_players(&self) -> Result<u64> {
        let mut conn = self.pool.get_conn().await?;
        let bit = crate::flags::PLR_DELETED;
        conn.exec_drop(
            r"DELETE pa FROM player_affects pa
              JOIN player_main pm ON pm.idnum = pa.idnum
              WHERE (pm.act & ?) <> 0",
            (bit,),
        )
        .await?;
        conn.exec_drop(
            r"DELETE ps FROM player_skills ps
              JOIN player_main pm ON pm.idnum = ps.idnum
              WHERE (pm.act & ?) <> 0",
            (bit,),
        )
        .await?;
        conn.exec_drop("DELETE FROM player_main WHERE (act & ?) <> 0", (bit,))
            .await?;
        Ok(conn.affected_rows())
    }

    /// Delete the exact PLR_DELETED identities whose name-keyed sidecars were
    /// already cleaned by pfileclean. Each row is rechecked under the DELETE;
    /// an admin who cleared the tombstone in the meantime keeps the row and
    /// its child records. The transaction prevents partial child/main removal.
    pub async fn delete_deleted_players_by_idnums(&self, idnums: Vec<i64>) -> Result<u64> {
        if idnums.is_empty() {
            return Ok(0);
        }
        let mut conn = self.pool.get_conn().await?;
        let mut transaction = conn
            .start_transaction(mysql_async::TxOpts::default())
            .await?;
        let bit = crate::flags::PLR_DELETED;
        let mut deleted = 0u64;
        for idnum in idnums {
            transaction
                .exec_drop(
                    r"DELETE pa FROM player_affects pa
                      JOIN player_main pm ON pm.idnum = pa.idnum
                      WHERE pm.idnum = ? AND (pm.act & ?) <> 0",
                    (idnum, bit),
                )
                .await?;
            transaction
                .exec_drop(
                    r"DELETE ps FROM player_skills ps
                      JOIN player_main pm ON pm.idnum = ps.idnum
                      WHERE pm.idnum = ? AND (pm.act & ?) <> 0",
                    (idnum, bit),
                )
                .await?;
            transaction
                .exec_drop(
                    "DELETE FROM player_main WHERE idnum = ? AND (act & ?) <> 0",
                    (idnum, bit),
                )
                .await?;
            deleted = deleted.saturating_add(transaction.affected_rows());
        }
        transaction.commit().await?;
        Ok(deleted)
    }

    /// C boot_clans() recounts from player_main on every boot.
    pub async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>> {
        let mut conn = self.pool.get_conn().await?;
        let rows: Vec<Row> = conn
            .exec(
                "SELECT clan, COUNT(*) FROM player_main WHERE clan >= 0 AND clan_rank != -1 GROUP BY clan",
                (),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let clan: Option<i32> = row.get(0);
                let count: Option<u64> = row.get(1);
                clan.zip(count).map(|(clan, count)| (clan, count as i32))
            })
            .collect())
    }

    /// C clan.c:242-255 clan_destroy SQL: clear the destroyed clan's members
    /// and shift higher clans down one (offline rows included) (#165).
    pub async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            "UPDATE player_main SET clan=-1, clan_rank=-1 WHERE clan=?",
            (destroyed,),
        )
        .await?;
        conn.exec_drop(
            "UPDATE player_main SET clan=clan-1 WHERE clan>?",
            (destroyed,),
        )
        .await?;
        Ok(())
    }

    /// C clan.c:388-405 lower_entire_clan SQL (#165).
    pub async fn clan_lower_ranks(&self, clan: i32) -> Result<()> {
        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            "UPDATE player_main SET clan_rank=1 WHERE clan=? AND clan_rank!=-1",
            (clan,),
        )
        .await?;
        Ok(())
    }

    pub async fn save_player(&self, ch: &Character) -> Result<()> {
        self.save_player_with_host(ch, "").await
    }

    pub async fn save_player_with_host(&self, ch: &Character, host: &str) -> Result<()> {
        // A generic Character save must never carry credentials. The old
        // SELECT-pwd / drop-connection / REPLACE sequence could straddle a
        // targeted password change and resurrect the stale hash. This UPDATE
        // excludes pwd (and preserves an omitted host) in the SQL statement
        // itself, so password changes and ordinary saves commute safely.
        self.update_player_main_preserving_password(ch, host)
            .await?;
        self.write_skills(ch).await?;
        self.write_affects(ch).await?;
        Ok(())
    }

    /// Rename only the durable identity column, guarded by both sides of the
    /// operation.  Unlike the ordinary 83-column REPLACE save, this cannot
    /// delete/recreate `player_main` or disturb the skills/affects child rows.
    /// The row and destination-name key are locked in one transaction so a
    /// concurrent rename/create is either observed here or rejected by the
    /// unique name constraint; `false` always means no change was committed.
    pub async fn rename_player_if_current(
        &self,
        idnum: i64,
        expected_old_name: &str,
        new_name: &str,
    ) -> Result<bool> {
        let mut conn = self.pool.get_conn().await?;
        let mut transaction = conn
            .start_transaction(mysql_async::TxOpts::default())
            .await?;

        let current_name: Option<String> = transaction
            .exec_first(
                "SELECT name FROM player_main WHERE idnum = ? FOR UPDATE",
                (idnum,),
            )
            .await?;
        if !current_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(expected_old_name))
        {
            transaction.rollback().await?;
            return Ok(false);
        }

        let destination_owner: Option<i64> = transaction
            .exec_first(
                "SELECT idnum FROM player_main WHERE name = ? FOR UPDATE",
                (new_name,),
            )
            .await?;
        if destination_owner.is_some_and(|owner| owner != idnum) {
            transaction.rollback().await?;
            return Ok(false);
        }

        transaction
            .exec_drop(
                "UPDATE player_main SET name = ? WHERE idnum = ? AND name = ?",
                (new_name, idnum, expected_old_name),
            )
            .await?;
        if transaction.affected_rows() != 1 {
            transaction.rollback().await?;
            return Ok(false);
        }
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn player_name_by_id(&self, idnum: i64) -> Result<Option<String>> {
        let mut conn = self.pool.get_conn().await?;
        Ok(conn
            .exec_first("SELECT name FROM player_main WHERE idnum = ?", (idnum,))
            .await?)
    }

    /// Read only the fields which govern command authority. This intentionally
    /// avoids a broad Character load so callers can reconcile an indeterminate
    /// targeted update without observing or rewriting unrelated state.
    pub async fn player_authority_by_id(
        &self,
        idnum: i64,
    ) -> Result<Option<(String, crate::PlayerAuthorityState)>> {
        validate_authority_identity(idnum, None)?;
        let mut conn = self.pool.get_conn().await?;
        let row: Option<AuthorityRow> = conn
            .exec_first(
                "SELECT name, level, trust, exp, godcmds1, godcmds2, godcmds3, godcmds4 \
                 FROM player_main WHERE idnum = ?",
                (idnum,),
            )
            .await?;
        row.map(|row| decode_authority_row(idnum, row)).transpose()
    }

    /// Atomically replace the complete durable command-authority snapshot only
    /// while identity and every expected value remain current. No unrelated
    /// player column or child table participates in this write.
    pub async fn update_authority_if_current(
        &self,
        idnum: i64,
        expected_name: &str,
        expected: crate::PlayerAuthorityState,
        replacement: crate::PlayerAuthorityState,
    ) -> Result<crate::AuthorityUpdateOutcome> {
        validate_authority_identity(idnum, Some(expected_name))?;
        validate_authority_state("expected", expected)?;
        validate_authority_state("replacement", replacement)?;

        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(
            "UPDATE player_main \
             SET level = :replacement_level, trust = :replacement_trust, \
                 exp = :replacement_exp, godcmds1 = :replacement_godcmds1, \
                 godcmds2 = :replacement_godcmds2, godcmds3 = :replacement_godcmds3, \
                 godcmds4 = :replacement_godcmds4 \
             WHERE idnum = :idnum \
               AND BINARY name = BINARY :expected_name \
               AND level = :expected_level AND trust = :expected_trust \
               AND exp = :expected_exp AND godcmds1 = :expected_godcmds1 \
               AND godcmds2 = :expected_godcmds2 AND godcmds3 = :expected_godcmds3 \
               AND godcmds4 = :expected_godcmds4",
            params! {
                "replacement_level" => replacement.level,
                "replacement_trust" => replacement.trust,
                "replacement_exp" => replacement.exp,
                "replacement_godcmds1" => replacement.godcmds1,
                "replacement_godcmds2" => replacement.godcmds2,
                "replacement_godcmds3" => replacement.godcmds3,
                "replacement_godcmds4" => replacement.godcmds4,
                "idnum" => idnum,
                "expected_name" => expected_name,
                "expected_level" => expected.level,
                "expected_trust" => expected.trust,
                "expected_exp" => expected.exp,
                "expected_godcmds1" => expected.godcmds1,
                "expected_godcmds2" => expected.godcmds2,
                "expected_godcmds3" => expected.godcmds3,
                "expected_godcmds4" => expected.godcmds4,
            },
        )
        .await?;

        match conn.affected_rows() {
            1 => Ok(crate::AuthorityUpdateOutcome::Updated),
            0 => {
                // MySQL reports zero changed rows when replacement already
                // equals current state. A narrow exact readback distinguishes
                // that idempotent success from a stale CAS precondition.
                let row: Option<AuthorityRow> = conn
                    .exec_first(
                        "SELECT name, level, trust, exp, godcmds1, godcmds2, godcmds3, godcmds4 \
                         FROM player_main WHERE idnum = ?",
                        (idnum,),
                    )
                    .await?;
                let current = row
                    .map(|row| decode_authority_row(idnum, row))
                    .transpose()?;
                if current
                    .as_ref()
                    .is_some_and(|(name, state)| name == expected_name && *state == replacement)
                {
                    Ok(crate::AuthorityUpdateOutcome::Updated)
                } else {
                    Ok(crate::AuthorityUpdateOutcome::PreconditionsChanged)
                }
            }
            count => bail!("authority compare-and-swap changed {count} player rows"),
        }
    }

    // ---- internal write helpers ----------------------------------------

    /// Update an existing player row without ever reading or writing `pwd`.
    /// `idnum` and `name` are stable identity predicates rather than mutable
    /// assignments. An empty `host` means preserve the current durable host.
    async fn update_player_main_preserving_password(
        &self,
        ch: &Character,
        host: &str,
    ) -> Result<()> {
        let columns = compat::PLAYER_MAIN_COLUMNS;
        let row_values = compat::player_main_values(ch, "", host);
        let mut assignments = Vec::with_capacity(columns.len());
        let mut values = Vec::with_capacity(columns.len());
        for (column, value) in columns.iter().zip(row_values) {
            if matches!(*column, "idnum" | "name" | "pwd") || (*column == "host" && host.is_empty())
            {
                continue;
            }
            assignments.push(format!("`{column}` = ?"));
            values.push(value);
        }
        values.push(Value::from(ch.idnum));
        values.push(Value::from(ch.get_name()));
        let sql = format!(
            "UPDATE player_main SET {} WHERE idnum = ? AND name = ?",
            assignments.join(",")
        );

        let mut conn = self.pool.get_conn().await?;
        conn.exec_drop(sql, values).await?;
        if conn.affected_rows() == 1 {
            return Ok(());
        }
        // Identical snapshots legitimately affect zero rows; distinguish that
        // from a vanished/renamed identity without broad-loading child data.
        let exists: Option<u8> = conn
            .exec_first(
                "SELECT 1 FROM player_main WHERE idnum = ? AND name = ?",
                (ch.idnum, ch.get_name()),
            )
            .await?;
        if exists == Some(1) {
            Ok(())
        } else {
            bail!(
                "save_player_with_host: no durable row matches idnum {} and name {}; refusing to create a player from a generic save",
                ch.idnum,
                ch.get_name()
            )
        }
    }

    /// DELETE + re-INSERT skills>0 (dbmodify_player_skills MODE_DELETE/STORE).
    async fn write_skills(&self, ch: &Character) -> Result<()> {
        // Transaction (issue #387): a failure between DELETE and INSERT used
        // to persist an empty skills set permanently.
        let mut conn = self.pool.get_conn().await?;
        let mut tx = conn
            .start_transaction(mysql_async::TxOpts::default())
            .await?;
        tx.exec_drop("DELETE FROM player_skills WHERE idnum = ?", (ch.idnum,))
            .await?;
        let rows = compat::skill_rows(ch);
        if !rows.is_empty() {
            let params: Vec<_> = rows
                .into_iter()
                .map(|(skill, learned)| {
                    params! { "idnum" => ch.idnum, "skill" => skill, "learned" => learned }
                })
                .collect();
            tx.exec_batch(
                "INSERT INTO player_skills (idnum,skill,learned) VALUES (:idnum,:skill,:learned)",
                params,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// DELETE + re-INSERT affects (dbmodify_player_affects MODE_DELETE/STORE).
    async fn write_affects(&self, ch: &Character) -> Result<()> {
        // Transaction (issue #387), same rationale as write_skills.
        let mut conn = self.pool.get_conn().await?;
        let mut tx = conn
            .start_transaction(mysql_async::TxOpts::default())
            .await?;
        tx.exec_drop("DELETE FROM player_affects WHERE idnum = ?", (ch.idnum,))
            .await?;
        let rows = compat::affect_rows(ch);
        if !rows.is_empty() {
            let params: Vec<_> = rows
                .into_iter()
                .map(|(t, dur, m, loc, bv)| {
                    params! {
                        "idnum" => ch.idnum, "type" => t, "duration" => dur,
                        "modifier" => m, "location" => loc, "bitvector" => bv,
                    }
                })
                .collect();
            tx.exec_batch(
                r"INSERT INTO player_affects (idnum,type,duration,modifier,location,bitvector)
                  VALUES (:idnum,:type,:duration,:modifier,:location,:bitvector)",
                params,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn migration_registry_is_contiguous_and_checksums_are_stable_shape() {
        assert_eq!(MIGRATIONS.len() as u64, EXPECTED_SCHEMA_VERSION);
        let mut names = HashSet::new();
        let mut checksums = HashSet::new();
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(migration.version, index as u64 + 1);
            assert!(!migration.name.trim().is_empty());
            assert!(names.insert(migration.name));
            assert!(!migration.sql.trim().is_empty());
            let checksum = migration_checksum(migration.sql);
            assert_eq!(checksum.len(), 64);
            assert!(checksum.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(checksums.insert(checksum));
        }
    }

    #[test]
    fn malformed_database_urls_return_redacted_configuration_errors() {
        const SECRET: &str = "DoNotEchoThisPassword";
        for url in [
            format!("postgres://operator:{SECRET}@localhost/deltamud"),
            format!("mysql://operator:{SECRET}@localhost/deltamud?unknown_option=true"),
            format!("mysql://operator:{SECRET}@localhost/deltamud?pool_min=10&pool_max=1"),
        ] {
            let error = Database::new(&url).err().expect("URL must be rejected");
            let rendered = format!("{error:#}");
            assert!(rendered.contains("invalid DATABASE_URL"));
            assert!(!rendered.contains(SECRET));
        }
    }

    #[test]
    fn database_lease_result_validation_is_fail_closed() {
        assert!(require_lock_acquired(Some(1), "test").is_ok());
        for result in [Some(0), None, Some(-1)] {
            let error = require_lock_acquired(result, "test").unwrap_err();
            assert!(error.to_string().contains("database is in use"));
        }

        assert!(require_lock_still_owned(Some(1)).is_ok());
        for result in [Some(0), None, Some(-1)] {
            let error = require_lock_still_owned(result).unwrap_err();
            assert!(error.to_string().contains("no longer owned"));
        }
    }
}

// ---------------------------------------------------------------------------
// MySQL integration tests (Deltania Breathes W1). These run against a REAL
// MariaDB and are opt-in: each test returns early unless MUD_TEST_DATABASE_URL
// is set (scripts/db-check.sh boots a throwaway mariadbd on a dynamically
// selected private Unix socket and exports it). The production database must
// never be a target — the URL the script exports points at its own throwaway
// instance only.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod mysql_integration {
    use super::*;
    use crate::character::{Affect, Character};
    use crate::types::{Class, Race};

    fn test_db() -> Option<Database> {
        let url = std::env::var("MUD_TEST_DATABASE_URL").ok()?;
        Some(Database::new(&url).expect("connect to throwaway test db"))
    }

    /// Unique per-run player names: the throwaway db is reused across runs,
    /// and name collisions would fail create_player. Persisted identities are
    /// intentionally restricted to 2-20 ASCII letters, so encode the time,
    /// process and call counter in base 26 instead of appending decimal digits.
    fn unique_name(base: &str) -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        const SUFFIX_LEN: usize = 10;
        const SPACE: u64 = 141_167_095_653_376; // 26^10

        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let counter = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut encoded = millis
            .wrapping_mul(65_536)
            .wrapping_add(u64::from(std::process::id()))
            .wrapping_add(counter)
            % SPACE;
        let mut suffix = [b'a'; SUFFIX_LEN];
        for byte in suffix.iter_mut().rev() {
            *byte = b'a' + (encoded % 26) as u8;
            encoded /= 26;
        }
        let prefix = &base.as_bytes()[..base.len().min(20 - SUFFIX_LEN)];
        format!(
            "{}{}",
            std::str::from_utf8(prefix).expect("test prefixes are ASCII"),
            std::str::from_utf8(&suffix).expect("base-26 suffix is ASCII")
        )
    }

    fn rich_player(name: &str) -> Character {
        let mut ch = Character::new_player(name.to_string(), Class::Cleric, Race::Dwarf);
        ch.player.level = 34;
        ch.player.hometown = 300;
        ch.player.sex = crate::types::Gender::Female;
        ch.player.deity = 7;
        ch.alignment = -420;
        // The DB row carries the BARE BASE (real_points), not the eq/affect-
        // layered points (see Character.real_points doc); set the base and
        // mirror it into points the way affect_total would.
        ch.real_points.hit = 88;
        ch.real_points.max_hit = 210;
        ch.real_points.mana = 40;
        ch.real_points.max_mana = 333;
        ch.real_points.move_points = 22;
        ch.real_points.max_move = 144;
        ch.real_points.armor = -87;
        ch.real_points.hitroll = 9;
        ch.real_points.damroll = 11;
        ch.points = ch.real_points.clone();
        crate::gold::set(&mut ch, crate::gold::Account::Carried, 12_345);
        crate::gold::set(&mut ch, crate::gold::Account::Bank, 98_765);
        ch.points.exp = 4_242_424;
        ch.real_abils.str = 18;
        ch.real_abils.str_add = 30;
        ch.real_abils.intel = 14;
        ch.real_abils.wis = 19;
        ch.real_abils.dex = 12;
        ch.real_abils.con = 17;
        ch.real_abils.cha = 8;
        ch.aff_abils = ch.real_abils;
        ch.player.title = Some("the Courier".to_string());
        ch.spells_to_learn = 6;
        ch.quest_points = 77;
        ch.quest_mob = -3;
        ch.quest_obj = 9011;
        ch.next_quest = 12;
        ch.quest_countdown = 9;
        ch.tloadroom = 310;
        ch.act_flags |= (1 << 16) | (1 << 2); // PLR_QUESTOR | POSTALIZED whatever bits round-trip
        ch
    }

    #[tokio::test]
    async fn migrations_are_recorded_verified_and_idempotent() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        db.verify_schema().await.unwrap();
        db.init_tables().await.unwrap();

        let mut conn = db.pool.get_conn().await.unwrap();
        let rows: Vec<(u64, String, String)> = conn
            .query("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
            .await
            .unwrap();
        assert_eq!(rows.len(), MIGRATIONS.len());
        for (row, migration) in rows.iter().zip(MIGRATIONS) {
            assert_eq!(row.0, migration.version);
            assert_eq!(row.1, migration.name);
            assert_eq!(row.2, migration_checksum(migration.sql));
        }

        let password_width: Option<u64> = conn
            .exec_first(
                "SELECT CHARACTER_MAXIMUM_LENGTH FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = 'player_main' \
                 AND column_name = 'pwd'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(password_width, Some(255));
    }

    #[tokio::test]
    async fn runtime_lease_excludes_servers_migrations_and_bootstrap() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();

        let mut runtime_lease = db.acquire_runtime_lease().await.unwrap();
        runtime_lease.verify_owned().await.unwrap();

        let second = test_db().unwrap();
        let second_server_error = second
            .acquire_runtime_lease()
            .await
            .err()
            .expect("a second server must not share one durable database");
        assert!(
            second_server_error
                .to_string()
                .contains("database is in use")
        );

        let migration_error = second
            .init_tables()
            .await
            .expect_err("migration must not run behind a live server");
        assert!(migration_error.to_string().contains("database is in use"));

        let bootstrap_error = second
            .bootstrap_implementor("Doesnotmatter")
            .await
            .expect_err("bootstrap must not run behind a live server");
        assert!(bootstrap_error.to_string().contains("database is in use"));

        runtime_lease.release().await.unwrap();
        let replacement_lease = second.acquire_runtime_lease().await.unwrap();
        replacement_lease.release().await.unwrap();
    }

    #[tokio::test]
    async fn schema_verification_rejects_identity_shape_and_imported_name_ambiguity() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let mut conn = db.pool.get_conn().await.unwrap();

        conn.query_drop(
            "ALTER TABLE player_main MODIFY name VARCHAR(30) CHARACTER SET utf8mb4 \
             COLLATE utf8mb4_bin NOT NULL",
        )
        .await
        .unwrap();
        let collation_error = db.verify_schema().await.unwrap_err();
        conn.query_drop(
            "ALTER TABLE player_main MODIFY name VARCHAR(30) CHARACTER SET utf8mb4 \
             COLLATE utf8mb4_unicode_ci NOT NULL",
        )
        .await
        .unwrap();
        assert!(
            collation_error
                .to_string()
                .contains("case-insensitive collation")
        );

        conn.query_drop("ALTER TABLE player_main DROP PRIMARY KEY, ADD PRIMARY KEY (idnum, name)")
            .await
            .unwrap();
        let primary_key_error = db.verify_schema().await.unwrap_err();
        conn.query_drop("ALTER TABLE player_main DROP PRIMARY KEY, ADD PRIMARY KEY (idnum)")
            .await
            .unwrap();
        assert!(
            primary_key_error
                .to_string()
                .contains("PRIMARY KEY (idnum) only")
        );

        const IMPORT_TEST_ID: i64 = 2_000_000_001;
        conn.exec_drop(
            "INSERT INTO player_main (idnum, name, level, pwd) VALUES (?, ?, ?, ?)",
            (IMPORT_TEST_ID, "Élodie", 1, "import-test"),
        )
        .await
        .unwrap();
        let imported_name_error = db.verify_schema().await.unwrap_err();
        conn.exec_drop("DELETE FROM player_main WHERE idnum = ?", (IMPORT_TEST_ID,))
            .await
            .unwrap();
        assert!(
            imported_name_error
                .to_string()
                .contains("has an invalid name")
        );
        db.verify_schema().await.unwrap();
    }

    #[tokio::test]
    async fn create_load_roundtrip_preserves_the_row() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();

        let name = unique_name("RoundTripper");
        let mut ch = rich_player(&name);
        let idnum = db.create_player(&ch, "s3cretpw").await.unwrap();
        ch.idnum = idnum;
        ch.act_flags |= 0;
        db.save_player_with_host(&ch, "courier.example.test")
            .await
            .unwrap();

        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.idnum, idnum);
        assert_eq!(loaded.player.name, name);
        assert_eq!(loaded.player.level, 34);
        assert_eq!(loaded.points.gold, 12_345);
        assert_eq!(loaded.points.bank_gold, 98_765);
        assert_eq!(loaded.points.exp, 4_242_424);
        assert_eq!(loaded.points.max_hit, 210);
        assert_eq!(loaded.points.max_mana, 333);
        assert_eq!(loaded.points.max_move, 144);
        // NOTE: armor/hitroll/damroll are apply targets recomputed by
        // affect_total on load — they are derived stats, not persisted ones.
        assert_eq!(loaded.real_abils.str, 18);
        assert_eq!(loaded.real_abils.str_add, 30);
        assert_eq!(loaded.real_abils.wis, 19);
        assert_eq!(loaded.player.title.as_deref(), Some("the Courier"));
        assert_eq!(loaded.alignment, -420);
        assert_eq!(loaded.spells_to_learn, 6);
        assert_eq!(loaded.quest_points, 77);
        assert_eq!(loaded.quest_mob, -3);
        assert_eq!(loaded.quest_obj, 9011);
        assert_eq!(loaded.next_quest, 12);
        assert_eq!(loaded.quest_countdown, 9);
        assert_eq!(loaded.tloadroom, 310);
        assert!(loaded.act_flags & (1 << 16) != 0);
        assert!(loaded.act_flags & (1 << 2) != 0);

        // Password path.
        assert!(db.verify_password(&name, "s3cretpw").await.unwrap());
        assert!(!db.verify_password(&name, "wrong").await.unwrap());
    }

    #[tokio::test]
    async fn save_is_idempotent_across_repeated_writes() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let name = unique_name("IdemPotent");
        let mut ch = rich_player(&name);
        let idnum = db.create_player(&ch, "pw").await.unwrap();
        ch.idnum = idnum;
        db.save_player_with_host(&ch, "host-a").await.unwrap();
        let first = db.load_player(&name).await.unwrap();
        db.save_player(&ch).await.unwrap();
        let second = db.load_player(&name).await.unwrap();

        assert_eq!(first.points.gold, second.points.gold);
        assert_eq!(first.points.exp, second.points.exp);
        assert_eq!(first.quest_mob, second.quest_mob);
        assert_eq!(first.real_abils.str, second.real_abils.str);
        assert_eq!(first.real_abils.str_add, second.real_abils.str_add);
        assert_eq!(first.real_abils.wis, second.real_abils.wis);
    }

    #[tokio::test]
    async fn mutation_then_reload_reflects_the_new_state() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let name = unique_name("Mutator");
        let mut ch = rich_player(&name);
        let idnum = db.create_player(&ch, "pw").await.unwrap();
        ch.idnum = idnum;
        crate::gold::set(&mut ch, crate::gold::Account::Carried, 1);
        ch.player.level = 61;
        ch.quest_mob = 0;
        ch.quest_obj = 0;
        db.save_player(&ch).await.unwrap();
        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.player.level, 61);
        assert_eq!(loaded.points.gold, 1);
        assert_eq!(loaded.quest_mob, 0);
        assert_eq!(loaded.quest_obj, 0);
    }

    #[tokio::test]
    async fn skills_and_affects_replace_and_clear() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();

        let name = unique_name("SideRows");
        let mut ch = rich_player(&name);
        ch.skills.insert(7, 42);
        ch.skills.insert(19, 88);
        ch.affected.push(Affect {
            spell_type: 17,
            duration: 9,
            modifier: -3,
            location: 4,
            bitvector: 1 << 12,
            caster: None,
        });
        let idnum = db.create_player(&ch, "pw").await.unwrap();
        ch.idnum = idnum;

        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.skills.get(&7), Some(&42));
        assert_eq!(loaded.skills.get(&19), Some(&88));
        assert_eq!(loaded.affected.len(), 1);
        assert_eq!(loaded.affected[0].spell_type, 17);
        assert_eq!(loaded.affected[0].duration, 9);
        assert_eq!(loaded.affected[0].modifier, -3);
        assert_eq!(loaded.affected[0].location, 4);
        assert_eq!(loaded.affected[0].bitvector, 1 << 12);

        ch.skills.clear();
        ch.skills.insert(23, 61);
        ch.affected.clear();
        ch.affected.push(Affect {
            spell_type: 29,
            duration: 3,
            modifier: 5,
            location: 2,
            bitvector: 1 << 20,
            caster: None,
        });
        db.save_player(&ch).await.unwrap();

        let replaced = db.load_player(&name).await.unwrap();
        assert_eq!(replaced.skills.len(), 1);
        assert_eq!(replaced.skills.get(&23), Some(&61));
        assert_eq!(replaced.affected.len(), 1);
        assert_eq!(replaced.affected[0].spell_type, 29);
        assert_eq!(replaced.affected[0].duration, 3);
        assert_eq!(replaced.affected[0].modifier, 5);
        assert_eq!(replaced.affected[0].location, 2);
        assert_eq!(replaced.affected[0].bitvector, 1 << 20);

        ch.skills.clear();
        ch.affected.clear();
        db.save_player(&ch).await.unwrap();

        let cleared = db.load_player(&name).await.unwrap();
        assert!(cleared.skills.is_empty());
        assert!(cleared.affected.is_empty());
    }

    #[tokio::test]
    async fn failed_affect_insert_rolls_back_the_delete() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();

        let name = unique_name("AffectRollback");
        let mut ch = rich_player(&name);
        ch.affected.push(Affect {
            spell_type: 11,
            duration: 8,
            modifier: 2,
            location: 1,
            bitvector: 1 << 6,
            caster: None,
        });
        let idnum = db.create_player(&ch, "pw").await.unwrap();
        ch.idnum = idnum;

        // Fail only this player's replacement INSERT. If DELETE and INSERT are
        // not one transaction, the original row is permanently lost.
        let trigger = format!("test_affect_rollback_{idnum}");
        let mut conn = db.pool.get_conn().await.unwrap();
        conn.query_drop(format!(
            "CREATE TRIGGER `{trigger}` BEFORE INSERT ON player_affects \
             FOR EACH ROW BEGIN \
             IF NEW.idnum = {idnum} THEN \
             SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'forced affect insert failure'; \
             END IF; END"
        ))
        .await
        .unwrap();
        drop(conn);

        ch.affected.clear();
        ch.affected.push(Affect {
            spell_type: 99,
            duration: 1,
            modifier: 7,
            location: 3,
            bitvector: 1 << 18,
            caster: None,
        });
        let write_result = db.write_affects(&ch).await;

        let mut conn = db.pool.get_conn().await.unwrap();
        conn.query_drop(format!("DROP TRIGGER `{trigger}`"))
            .await
            .unwrap();
        assert!(
            write_result.is_err(),
            "the test trigger must reject the INSERT"
        );

        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.affected.len(), 1);
        assert_eq!(loaded.affected[0].spell_type, 11);
        assert_eq!(loaded.affected[0].duration, 8);
        assert_eq!(loaded.affected[0].modifier, 2);
        assert_eq!(loaded.affected[0].location, 1);
        assert_eq!(loaded.affected[0].bitvector, 1 << 6);
    }

    #[tokio::test]
    async fn targeted_password_cas_commutes_with_a_generic_player_save() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let name = unique_name("Credential");
        let ch = rich_player(&name);
        let idnum = db.create_player(&ch, "oldpass").await.unwrap();
        let snapshot = db.load_player(&name).await.unwrap();
        let old_hash = db.get_password_hash(&name).await.unwrap().unwrap();
        let new_hash = crate::password::hash_password("newpass");

        let (save_result, update_result) = tokio::join!(
            db.save_player(&snapshot),
            db.update_password_hash(idnum, &name, Some(&old_hash), &new_hash)
        );
        save_result.unwrap();
        assert_eq!(
            update_result.unwrap(),
            crate::PasswordHashUpdateOutcome::Updated
        );
        assert!(db.verify_password(&name, "newpass").await.unwrap());
        assert!(!db.verify_password(&name, "oldpass").await.unwrap());

        let rejected_hash = crate::password::hash_password("must-not-win");
        assert_eq!(
            db.update_password_hash(idnum, &name, Some(&old_hash), &rejected_hash)
                .await
                .unwrap(),
            crate::PasswordHashUpdateOutcome::CurrentHashMismatch
        );
        assert!(db.verify_password(&name, "newpass").await.unwrap());
        assert!(!db.verify_password(&name, "must-not-win").await.unwrap());
    }

    #[tokio::test]
    async fn targeted_authority_cas_checks_every_field_and_updates_nothing_else() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let name = unique_name("Authority");
        let mut ch = rich_player(&name);
        // Legacy gameplay can persist negative XP; it is an exact signed CAS
        // value, not an authority range violation.
        ch.points.exp = -500;
        let idnum = db.create_player(&ch, "authority-pass").await.unwrap();
        let (durable_name, expected) = db.player_authority_by_id(idnum).await.unwrap().unwrap();
        assert_eq!(durable_name, name);
        let replacement = crate::PlayerAuthorityState {
            level: crate::types::LVL_IMMORT,
            trust: i32::from(crate::types::LVL_IMMORT),
            exp: 8_484_848,
            godcmds1: 11,
            godcmds2: 22,
            godcmds3: 33,
            godcmds4: 44,
        };

        let stale_states = [
            crate::PlayerAuthorityState {
                level: expected.level.saturating_add(1),
                ..expected
            },
            crate::PlayerAuthorityState {
                trust: expected.trust.saturating_add(1),
                ..expected
            },
            crate::PlayerAuthorityState {
                exp: expected.exp.saturating_add(1),
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds1: expected.godcmds1 ^ 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds2: expected.godcmds2 ^ 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds3: expected.godcmds3 ^ 1,
                ..expected
            },
            crate::PlayerAuthorityState {
                godcmds4: expected.godcmds4 ^ 1,
                ..expected
            },
        ];
        for stale in stale_states {
            assert_eq!(
                db.update_authority_if_current(idnum, &name, stale, replacement)
                    .await
                    .unwrap(),
                crate::AuthorityUpdateOutcome::PreconditionsChanged
            );
        }
        assert_eq!(
            db.update_authority_if_current(
                idnum,
                &name.to_ascii_lowercase(),
                expected,
                replacement,
            )
            .await
            .unwrap(),
            crate::AuthorityUpdateOutcome::PreconditionsChanged
        );
        assert_eq!(
            db.update_authority_if_current(idnum, &name, expected, replacement)
                .await
                .unwrap(),
            crate::AuthorityUpdateOutcome::Updated
        );
        assert_eq!(
            db.player_authority_by_id(idnum).await.unwrap(),
            Some((name.clone(), replacement))
        );
        assert_eq!(
            db.update_authority_if_current(idnum, &name, expected, replacement)
                .await
                .unwrap(),
            crate::AuthorityUpdateOutcome::Updated
        );
        let loaded = db.load_player(&name).await.unwrap();
        assert_eq!(loaded.points.gold, 12_345);
        assert_eq!(loaded.player.title.as_deref(), Some("the Courier"));

        let mut conn = db.pool.get_conn().await.unwrap();
        for table in ["player_affects", "player_skills", "player_main"] {
            conn.exec_drop(format!("DELETE FROM {table} WHERE idnum = ?"), (idnum,))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_player_creation_has_collision_safe_names_and_ids() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();

        let same_name = unique_name("Samecreate");
        let same_character = Character::new_player(same_name.clone(), Class::Warrior, Race::Human);
        let (same_first, same_second) = tokio::join!(
            db.create_player(&same_character, "first-pass"),
            db.create_player(&same_character, "second-pass")
        );
        let same_outcomes = [same_first, same_second];
        assert_eq!(
            same_outcomes
                .iter()
                .filter(|outcome| outcome.is_ok())
                .count(),
            1
        );
        assert_eq!(
            same_outcomes
                .iter()
                .filter(|outcome| outcome.is_err())
                .count(),
            1
        );
        let same_id = *same_outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().ok())
            .unwrap();
        assert!(
            db.verify_password(&same_name, "first-pass").await.unwrap()
                || db.verify_password(&same_name, "second-pass").await.unwrap()
        );

        let first_name = unique_name("Diffcreatea");
        let second_name = unique_name("Diffcreateb");
        let first_character =
            Character::new_player(first_name.clone(), Class::Warrior, Race::Human);
        let second_character = Character::new_player(second_name.clone(), Class::Cleric, Race::Elf);
        let (first_id, second_id) = tokio::join!(
            db.create_player(&first_character, "first-pass"),
            db.create_player(&second_character, "second-pass")
        );
        let first_id = first_id.unwrap();
        let second_id = second_id.unwrap();
        assert_ne!(first_id, second_id);
        assert_ne!(same_id, first_id);
        assert_ne!(same_id, second_id);

        let mut conn = db.pool.get_conn().await.unwrap();
        for table in ["player_affects", "player_skills", "player_main"] {
            conn.exec_drop(
                format!("DELETE FROM {table} WHERE idnum IN (?, ?, ?)"),
                (same_id, first_id, second_id),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn implementor_bootstrap_is_serialized_and_updates_only_one_identity() {
        let Some(db) = test_db() else { return };
        db.init_tables().await.unwrap();
        let mut setup_conn = db.pool.get_conn().await.unwrap();
        let existing_effective: Option<u8> = setup_conn
            .exec_first(
                "SELECT 1 FROM player_main WHERE trust >= ? \
                 AND (COALESCE(act, 0) & ?) = 0 LIMIT 1",
                (crate::types::LVL_IMPL, crate::flags::PLR_DELETED),
            )
            .await
            .unwrap();
        if existing_effective.is_some() {
            // The opt-in database is expected to be disposable, but do not
            // demote an administrator created outside this test.
            return;
        }

        let first_name = unique_name("Bootfirst");
        let second_name = unique_name("Bootsecond");
        let first = Character::new_player(first_name.clone(), Class::Warrior, Race::Human);
        let second = Character::new_player(second_name.clone(), Class::Cleric, Race::Elf);
        let first_id = db.create_player(&first, "first-pass").await.unwrap();
        let second_id = db.create_player(&second, "second-pass").await.unwrap();
        // Display level is not command authority. A stale/high level with low
        // trust must neither block bootstrap nor count as the effective
        // Implementor after one of these identities is promoted.
        setup_conn
            .exec_drop(
                "UPDATE player_main SET level = ?, trust = 1 WHERE idnum = ?",
                (crate::types::LVL_IMPL, first_id),
            )
            .await
            .unwrap();
        drop(setup_conn);

        let (first_outcome, second_outcome) = tokio::join!(
            db.bootstrap_implementor(&first_name),
            db.bootstrap_implementor(&second_name)
        );
        let outcomes = [first_outcome.unwrap(), second_outcome.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == crate::ImplementorBootstrapOutcome::Promoted)
                .count(),
            1
        );
        let first_loaded = db.load_player(&first_name).await.unwrap();
        let second_loaded = db.load_player(&second_name).await.unwrap();
        assert_eq!(
            [first_loaded.trust, second_loaded.trust]
                .into_iter()
                .filter(|trust| *trust >= i32::from(crate::types::LVL_IMPL))
                .count(),
            1
        );

        // Leave the reusable throwaway schema with no administrator created by
        // this test, and remove only this test's explicit identities.
        let mut conn = db.pool.get_conn().await.unwrap();
        conn.exec_drop(
            "DELETE FROM player_affects WHERE idnum IN (?, ?)",
            (first_id, second_id),
        )
        .await
        .unwrap();
        conn.exec_drop(
            "DELETE FROM player_skills WHERE idnum IN (?, ?)",
            (first_id, second_id),
        )
        .await
        .unwrap();
        conn.exec_drop(
            "DELETE FROM player_main WHERE idnum IN (?, ?)",
            (first_id, second_id),
        )
        .await
        .unwrap();
    }
}
