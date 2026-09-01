use crate::DatabaseInterface;
use crate::character::Character;
use crate::state::PlayerIndex;
use anyhow::{Result, anyhow};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Applies one wall-clock deadline to every database operation, including
/// pool acquisition and TCP/query I/O performed inside the concrete driver.
pub struct TimedDatabase {
    inner: Arc<dyn DatabaseInterface>,
    timeout: Duration,
}

impl TimedDatabase {
    pub fn new(inner: Arc<dyn DatabaseInterface>, timeout: Duration) -> Self {
        Self { inner, timeout }
    }

    async fn bounded<T, F>(&self, operation: &'static str, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        tokio::time::timeout(self.timeout, future)
            .await
            .map_err(|_| {
                anyhow!(
                    "database {operation} timed out after {}ms",
                    self.timeout.as_millis()
                )
            })?
    }
}

#[async_trait::async_trait]
impl DatabaseInterface for TimedDatabase {
    async fn init_tables(&self) -> Result<()> {
        self.bounded("init_tables", self.inner.init_tables()).await
    }

    async fn player_exists(&self, name: &str) -> Result<bool> {
        self.bounded("player_exists", self.inner.player_exists(name))
            .await
    }

    async fn create_player(&self, character: &Character, password: &str) -> Result<i64> {
        self.bounded(
            "create_player",
            self.inner.create_player(character, password),
        )
        .await
    }

    async fn load_player(&self, name: &str) -> Result<Character> {
        self.bounded("load_player", self.inner.load_player(name))
            .await
    }

    async fn save_player(&self, character: &Character) -> Result<()> {
        self.bounded("save_player", self.inner.save_player(character))
            .await
    }

    async fn save_player_with_host(&self, character: &Character, host: &str) -> Result<()> {
        self.bounded(
            "save_player_with_host",
            self.inner.save_player_with_host(character, host),
        )
        .await
    }

    async fn rename_player_if_current(
        &self,
        idnum: i64,
        expected_old_name: &str,
        new_name: &str,
    ) -> Result<bool> {
        self.bounded(
            "rename_player_if_current",
            self.inner
                .rename_player_if_current(idnum, expected_old_name, new_name),
        )
        .await
    }

    async fn player_name_by_id(&self, idnum: i64) -> Result<Option<String>> {
        self.bounded("player_name_by_id", self.inner.player_name_by_id(idnum))
            .await
    }

    async fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        self.bounded(
            "verify_password",
            self.inner.verify_password(name, password),
        )
        .await
    }

    async fn get_password_hash(&self, name: &str) -> Result<Option<String>> {
        self.bounded("get_password_hash", self.inner.get_password_hash(name))
            .await
    }

    async fn delete_deleted_players(&self) -> Result<u64> {
        self.bounded(
            "delete_deleted_players",
            self.inner.delete_deleted_players(),
        )
        .await
    }

    async fn delete_deleted_players_by_idnums(&self, idnums: Vec<i64>) -> Result<u64> {
        self.bounded(
            "delete_deleted_players_by_idnums",
            self.inner.delete_deleted_players_by_idnums(idnums),
        )
        .await
    }

    async fn clan_member_counts(&self) -> Result<Vec<(i32, i32)>> {
        self.bounded("clan_member_counts", self.inner.clan_member_counts())
            .await
    }

    async fn clan_destroy_fixup(&self, destroyed: i32) -> Result<()> {
        self.bounded(
            "clan_destroy_fixup",
            self.inner.clan_destroy_fixup(destroyed),
        )
        .await
    }

    async fn clan_lower_ranks(&self, clan: i32) -> Result<()> {
        self.bounded("clan_lower_ranks", self.inner.clan_lower_ranks(clan))
            .await
    }

    async fn list_players(&self) -> Result<Vec<PlayerIndex>> {
        self.bounded("list_players", self.inner.list_players())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_database::MockDatabase;

    #[tokio::test]
    async fn bounded_operation_surfaces_a_timeout_error() {
        let db = TimedDatabase::new(Arc::new(MockDatabase::new()), Duration::from_millis(10));
        let result = db
            .bounded("hung_test", std::future::pending::<Result<()>>())
            .await;
        let error = result.expect_err("pending operation must time out");
        assert!(error.to_string().contains("hung_test timed out after 10ms"));
    }
}
