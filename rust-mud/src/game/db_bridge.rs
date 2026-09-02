//! The await_database single-op executor and the thin typed db_* wrappers.
//!
//! Split out of game/mod.rs (phase 2); `use super::*` inherits the
//! Game struct, its fields, and the module's imports.

use super::*;

impl Game {
    pub(crate) async fn await_database<T, F>(&mut self, future: F) -> T
    where
        F: Future<Output = T>,
    {
        if self.game_rx.is_none() {
            return future.await;
        }

        tokio::pin!(future);
        let mut tick = interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // interval() fires immediately; consume that artificial first tick so
        // an SQL call does not add a bonus heartbeat.
        tick.tick().await;

        loop {
            tokio::select! {
                result = &mut future => return result,
                _ = tick.tick() => {
                    self.heartbeat();
                    self.flush_all().await;
                }
                message = self.game_rx.as_mut().expect("receiver installed").recv() => {
                    let Some(message) = message else {
                        // The sender side is gone; the bounded DB wrapper will
                        // still complete or time out this operation.
                        return future.await;
                    };
                    self.service_message_during_database_wait(message).await;
                    self.flush_all().await;
                }
            }
        }
    }

    pub(crate) async fn service_message_during_database_wait(&mut self, message: GameMessage) {
        match message {
            GameMessage::NewConnection {
                id,
                host,
                peer_ip,
                verified_hostname,
                raw_fd,
                output_tx,
            } => {
                info!("New connection from {}", host);
                self.metrics.inc_connections();
                let mut descriptor =
                    Descriptor::with_identity(id, host, peer_ip, verified_hostname, raw_fd);
                descriptor.write(ANSI_QUESTION);
                self.state.descriptors.insert(id, descriptor);
                self.outputs.insert(id, output_tx);
                self.write_prompt(id);
            }
            GameMessage::Input { conn_id, input }
                if self.state.descriptors.get(&conn_id).map(|d| d.state)
                    == Some(ConState::Playing) =>
            {
                // Playing input never performs SQL. Boxing makes the recursive
                // async call graph explicit while the state guard prevents a
                // second database wait from nesting here.
                Box::pin(self.handle_input(conn_id, input)).await;
            }
            GameMessage::Gmcp { conn_id, event } => self.handle_gmcp_event(conn_id, event),
            GameMessage::SendMssp { conn_id } => self.send_mssp(conn_id),
            GameMessage::Disconnect { conn_id } => self.disconnect(conn_id).await,
            other => self.deferred_messages.push_back(other),
        }
    }

    pub(crate) async fn db_player_exists(&mut self, name: &str) -> Result<bool> {
        let db = self.db.clone();
        let name = name.to_string();
        self.await_database(async move { db.player_exists(&name).await })
            .await
    }

    pub(crate) async fn db_verify_password(&mut self, name: &str, password: &str) -> Result<bool> {
        let db = self.db.clone();
        let name = name.to_string();
        let password = password.to_string();
        self.await_database(async move { db.verify_password(&name, &password).await })
            .await
    }

    pub(crate) async fn db_get_password_hash(&mut self, name: &str) -> Result<Option<String>> {
        let db = self.db.clone();
        let name = name.to_string();
        self.await_database(async move { db.get_password_hash(&name).await })
            .await
    }

    pub(crate) async fn db_update_password_hash(
        &mut self,
        idnum: i64,
        expected_name: &str,
        expected_current_hash: Option<&str>,
        password_hash: &str,
    ) -> Result<crate::PasswordHashUpdateOutcome> {
        let db = self.db.clone();
        let expected_name = expected_name.to_string();
        let expected_current_hash = expected_current_hash.map(str::to_string);
        let password_hash = password_hash.to_string();
        self.await_database(async move {
            db.update_password_hash(
                idnum,
                &expected_name,
                expected_current_hash.as_deref(),
                &password_hash,
            )
            .await
        })
        .await
    }

    /// A timed/network error can arrive after MySQL committed an UPDATE. Read
    /// the narrow credential back before deciding whether to publish success,
    /// failure, or an explicitly indeterminate outcome.
    pub(crate) async fn resolve_password_update_error(
        &mut self,
        name: &str,
        requested_hash: &str,
        update_error: anyhow::Error,
    ) -> Result<crate::PasswordHashUpdateOutcome> {
        match self.db_get_password_hash(name).await {
            Ok(Some(current)) if current == requested_hash => {
                Ok(crate::PasswordHashUpdateOutcome::Updated)
            }
            Ok(Some(_)) => Ok(crate::PasswordHashUpdateOutcome::CurrentHashMismatch),
            Ok(None) => Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch),
            Err(read_error) => Err(anyhow::anyhow!(
                "password update failed ({update_error}); credential readback also failed ({read_error})"
            )),
        }
    }

    pub(crate) async fn db_load_player(
        &mut self,
        name: &str,
    ) -> Result<crate::character::Character> {
        let db = self.db.clone();
        let name = name.to_string();
        self.await_database(async move { db.load_player(&name).await })
            .await
    }

    pub(crate) async fn db_save_player(
        &mut self,
        character: &crate::character::Character,
    ) -> Result<()> {
        let db = self.db.clone();
        let character = character.clone();
        self.await_database(async move { db.save_player(&character).await })
            .await
    }

    pub(crate) async fn db_save_player_with_host(
        &mut self,
        character: &crate::character::Character,
        host: &str,
    ) -> Result<()> {
        let db = self.db.clone();
        let character = character.clone();
        let host = host.to_string();
        self.await_database(async move { db.save_player_with_host(&character, &host).await })
            .await
    }

    pub(crate) async fn db_create_player_with_password_hash(
        &mut self,
        character: &crate::character::Character,
        password_hash: &str,
    ) -> Result<i64> {
        let db = self.db.clone();
        let character = character.clone();
        let password_hash = password_hash.to_string();
        self.await_database(async move {
            db.create_player_with_password_hash(&character, &password_hash)
                .await
        })
        .await
    }

    pub(crate) async fn db_clan_destroy_fixup(&mut self, clan: i32) -> Result<()> {
        let db = self.db.clone();
        self.await_database(async move { db.clan_destroy_fixup(clan).await })
            .await
    }

    pub(crate) async fn db_clan_lower_ranks(&mut self, clan: i32) -> Result<()> {
        let db = self.db.clone();
        self.await_database(async move { db.clan_lower_ranks(clan).await })
            .await
    }

    pub(crate) async fn db_list_players(&mut self) -> Result<Vec<crate::state::PlayerIndex>> {
        let db = self.db.clone();
        self.await_database(async move { db.list_players().await })
            .await
    }

    /// Graceful-shutdown sequence (CircleMUD's SIGTERM/hupsig + Crash_save_all):
    /// crash-save every in-world player and their objects to disk, push the
    /// final "shutting down" notice + any buffered output to every descriptor,
    /// log the count, and return so `run` exits cleanly instead of being killed
    /// with unsaved state.
    pub(crate) async fn load_player_latest(
        &mut self,
        name: &str,
    ) -> Result<crate::character::Character> {
        if let Some(snapshot) = self.pending_player_snapshot(name) {
            return Ok(snapshot);
        }
        self.db_load_player(name).await
    }
}
