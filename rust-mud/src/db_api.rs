// db_api.rs — the database abstraction boundary (CircleMUD dbinterface.c).
//
// `DatabaseInterface` is the single port the game talks through: the MySQL
// `Database` and the in-memory `MockDatabase` implement it, and
// `TimedDatabase` (database_timeout.rs) decorates it with per-operation
// deadlines. The outcome/state types below are the narrow vocabulary of that
// boundary; the `impl DatabaseInterface for database::Database` shim forwards
// to the inherent `database::Database` methods.

use anyhow::Result;

use crate::character;
use crate::database;

/// Result of the one-time, offline administrative bootstrap. The concrete
/// database owns the check-and-promote critical section so two processes
/// cannot both pass an application-side "no Implementor" check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplementorBootstrapOutcome {
    Promoted,
    AlreadyExists(String),
    TargetNotFound,
}

/// Result of a targeted credential write. Login-time legacy upgrades use the
/// hash-mismatch distinction as a compare-and-swap guard so they never replace
/// a password another session changed after authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordHashUpdateOutcome {
    Updated,
    IdentityMismatch,
    CurrentHashMismatch,
}

/// The complete durable command-authority state changed by `advance`.
/// Keeping this snapshot narrow lets the database compare every security-
/// relevant precondition without rewriting unrelated Character fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerAuthorityState {
    pub level: u8,
    pub trust: i32,
    pub exp: i64,
    pub godcmds1: i64,
    pub godcmds2: i64,
    pub godcmds3: i64,
    pub godcmds4: i64,
}

/// Result of a targeted authority compare-and-swap. A changed precondition is
/// an ordinary race outcome: the caller must not apply its stale replacement
/// to the live Character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityUpdateOutcome {
    Updated,
    PreconditionsChanged,
}

#[async_trait::async_trait]
pub trait DatabaseInterface: Send + Sync {
    async fn init_tables(&self) -> Result<()>;
    async fn verify_schema(&self) -> Result<()>;
    async fn player_exists(&self, name: &str) -> Result<bool>;
    async fn create_player(&self, character: &character::Character, password: &str) -> Result<i64>;
    /// Create a player using a freshly generated, policy-compliant Argon2id
    /// PHC string. The caller owns hashing so interactive creation never keeps
    /// plaintext through the remaining character-creation questionnaire or
    /// performs the KDF twice.
    async fn create_player_with_password_hash(
        &self,
        character: &character::Character,
        password_hash: &str,
    ) -> Result<i64>;
    async fn load_player(&self, name: &str) -> Result<character::Character>;
    async fn save_player(&self, character: &character::Character) -> Result<()>;
    async fn save_player_with_host(
        &self,
        character: &character::Character,
        host: &str,
    ) -> Result<()> {
        let _ = host;
        self.save_player(character).await
    }
    /// Atomically change only `player_main.name` when `idnum` still owns the
    /// expected old name and the destination remains unclaimed.  `false`
    /// means the identity/collision precondition changed; no row was changed.
    async fn rename_player_if_current(
        &self,
        idnum: i64,
        expected_old_name: &str,
        new_name: &str,
    ) -> Result<bool>;
    /// Narrow identity read used to resolve an indeterminate rename/rollback
    /// error without loading the player's child rows.
    async fn player_name_by_id(&self, idnum: i64) -> Result<Option<String>>;
    /// Read the exact durable state which governs a player's command authority,
    /// without loading the broad Character row or its child tables.
    async fn player_authority_by_id(
        &self,
        idnum: i64,
    ) -> Result<Option<(String, PlayerAuthorityState)>>;
    /// Atomically replace only command-authority fields when durable identity
    /// and every expected authority value still match.
    async fn update_authority_if_current(
        &self,
        idnum: i64,
        expected_name: &str,
        expected: PlayerAuthorityState,
        replacement: PlayerAuthorityState,
    ) -> Result<AuthorityUpdateOutcome>;
    /// Update only the credential column when both durable identity fields are
    /// still current. Supplying `expected_current_hash` adds an exact CAS guard;
    /// `None` intentionally gives interactive/admin changes last-writer-wins
    /// semantics within that stable identity.
    async fn update_password_hash(
        &self,
        idnum: i64,
        expected_name: &str,
        expected_current_hash: Option<&str>,
        password_hash: &str,
    ) -> Result<PasswordHashUpdateOutcome>;
    async fn verify_password(&self, name: &str, password: &str) -> Result<bool>;
    /// The stored password hash, for the automatic legacy-hash upgrade at
    /// login (C interpreter.c:1914-1952 password_needs_upgrade path) (#219).
    async fn get_password_hash(&self, name: &str) -> Result<Option<String>>;
    /// Serialize the one-time no-existing-Implementor check with the narrow
    /// identity promotion in the database's own scope, excluding the live
    /// server and every other offline maintenance mode on the same schema.
    async fn bootstrap_implementor(&self, name: &str) -> Result<ImplementorBootstrapOutcome>;
    async fn delete_deleted_players(&self) -> Result<u64>;
    /// Delete only the already-audited/tombstoned identities supplied by
    /// pfileclean. The explicit ids prevent a row flagged after sidecar
    /// discovery from being swept without its own cleanup (#413).
    async fn delete_deleted_players_by_idnums(&self, idnums: Vec<i64>) -> Result<u64>;
    async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>>;
    /// C clan.c:242-255 clan_destroy: shift every player row's clan past the
    /// destroyed one and clear the destroyed clan's members (offline rows
    /// included) (#165).
    async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()>;
    /// C clan.c:388-405 lower_entire_clan: set clan_rank = 1 for every
    /// member of `clan` whose rank != -1 (#165).
    async fn clan_lower_ranks(&self, clan: i32) -> Result<()>;
    /// Every player's index row {idnum,name,level,trust,last_logon,host} for the
    /// boot-time player_table build (C build_player_index, db.c).
    async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>>;
}

#[async_trait::async_trait]
impl DatabaseInterface for database::Database {
    async fn init_tables(&self) -> Result<()> {
        self.init_tables().await
    }
    async fn verify_schema(&self) -> Result<()> {
        self.verify_schema().await
    }
    async fn player_exists(&self, name: &str) -> Result<bool> {
        self.player_exists(name).await
    }
    async fn create_player(&self, c: &character::Character, p: &str) -> Result<i64> {
        self.create_player(c, p).await
    }
    async fn create_player_with_password_hash(
        &self,
        c: &character::Character,
        password_hash: &str,
    ) -> Result<i64> {
        self.create_player_with_password_hash(c, password_hash)
            .await
    }
    async fn load_player(&self, name: &str) -> Result<character::Character> {
        self.load_player(name).await
    }
    async fn save_player(&self, c: &character::Character) -> Result<()> {
        self.save_player(c).await
    }
    async fn save_player_with_host(&self, c: &character::Character, host: &str) -> Result<()> {
        self.save_player_with_host(c, host).await
    }
    async fn rename_player_if_current(
        &self,
        idnum: i64,
        expected_old_name: &str,
        new_name: &str,
    ) -> Result<bool> {
        self.rename_player_if_current(idnum, expected_old_name, new_name)
            .await
    }
    async fn player_name_by_id(&self, idnum: i64) -> Result<Option<String>> {
        self.player_name_by_id(idnum).await
    }
    async fn player_authority_by_id(
        &self,
        idnum: i64,
    ) -> Result<Option<(String, PlayerAuthorityState)>> {
        self.player_authority_by_id(idnum).await
    }
    async fn update_authority_if_current(
        &self,
        idnum: i64,
        expected_name: &str,
        expected: PlayerAuthorityState,
        replacement: PlayerAuthorityState,
    ) -> Result<AuthorityUpdateOutcome> {
        self.update_authority_if_current(idnum, expected_name, expected, replacement)
            .await
    }
    async fn update_password_hash(
        &self,
        idnum: i64,
        expected_name: &str,
        expected_current_hash: Option<&str>,
        password_hash: &str,
    ) -> Result<PasswordHashUpdateOutcome> {
        self.update_password_hash(idnum, expected_name, expected_current_hash, password_hash)
            .await
    }
    async fn verify_password(&self, name: &str, p: &str) -> Result<bool> {
        self.verify_password(name, p).await
    }
    async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        self.get_password_hash(name).await
    }
    async fn bootstrap_implementor(&self, name: &str) -> Result<ImplementorBootstrapOutcome> {
        self.bootstrap_implementor(name).await
    }
    async fn delete_deleted_players(&self) -> Result<u64> {
        self.delete_deleted_players().await
    }
    async fn delete_deleted_players_by_idnums(&self, idnums: Vec<i64>) -> Result<u64> {
        self.delete_deleted_players_by_idnums(idnums).await
    }
    async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>> {
        self.clan_member_counts().await
    }
    async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()> {
        self.clan_destroy_fixup(destroyed).await
    }
    async fn clan_lower_ranks(&self, clan: i32) -> Result<()> {
        self.clan_lower_ranks(clan).await
    }
    async fn list_players(&self) -> Result<Vec<crate::state::PlayerIndex>> {
        self.list_players().await
    }
}
