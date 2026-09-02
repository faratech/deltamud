//! Graceful shutdown, shutdown_save, and copyover execution/persistence.
//!
//! Split out of game/mod.rs (phase 2); `use super::*` inherits the
//! Game struct, its fields, and the module's imports.

use super::*;

impl Game {
    pub(crate) async fn shutdown(&mut self) -> bool {
        if !self.state.authority_quarantine.is_empty() {
            warn!(
                "Shutdown aborted because {} player authority update(s) have an indeterminate durable outcome",
                self.state.authority_quarantine.len()
            );
            self.state.shutdown_requested = None;
            self.notify_shutdown_aborted(
                "\r\nShutdown aborted: an administrative authority change still needs durable reconciliation. The server will remain online.\r\n",
            );
            self.flush_all().await;
            return false;
        }
        match self.shutdown_save().await {
            Ok(report) if report.persistence_succeeded() => {
                info!(
                    "Shutting down, saved {}/{} player row(s), {}/{} alias file(s), {}/{} crash file(s), and the calendar (output attempted={}, acknowledged={}, failed={}, timed out={}).",
                    report.players_saved,
                    report.player_saves_attempted,
                    report.aliases_written,
                    report.alias_writes_attempted,
                    report.crash_saves_written,
                    report.crash_saves_attempted,
                    report.output_attempted,
                    report.output_acknowledged,
                    report.output_failed,
                    report.output_timed_out,
                );
                true
            }
            Ok(report) => {
                warn!(
                    "Shutdown aborted after persistence failures: database={}, aliases={}, crash files={}, calendar={} (saved {}/{} player row(s), {}/{} alias file(s), {}/{} crash file(s)).",
                    report.database_errors,
                    report.alias_errors,
                    report.crash_save_errors,
                    report.calendar_errors,
                    report.players_saved,
                    report.player_saves_attempted,
                    report.aliases_written,
                    report.alias_writes_attempted,
                    report.crash_saves_written,
                    report.crash_saves_attempted,
                );
                self.state.shutdown_requested = None;
                self.notify_shutdown_aborted(
                    "\r\nShutdown aborted: player or world persistence failed. The server will remain online; shutdown can be retried after recovery.\r\n",
                );
                self.flush_all().await;
                false
            }
            Err(error) => {
                warn!("Shutdown aborted because pending OLC could not be saved: {error}");
                self.state.shutdown_requested = None;
                self.notify_shutdown_aborted(
                    "\r\nShutdown aborted: pending OLC changes could not be saved. The server will remain online.\r\n",
                );
                self.flush_all().await;
                false
            }
        }
    }

    pub(crate) fn notify_shutdown_aborted(&mut self, message: &str) {
        let connections: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        for connection in connections {
            self.out(connection, message);
        }
    }

    pub(crate) async fn execute_copyover(&mut self, requester: CharId) {
        if !self.state.authority_quarantine.is_empty() {
            warn!(
                "Copyover aborted because {} player authority update(s) have an indeterminate durable outcome",
                self.state.authority_quarantine.len()
            );
            self.state.send_to_char(
                requester,
                "Copyover authority reconciliation failed; reboot aborted. Retry the rank change after database recovery.\n\r",
            );
            return;
        }
        if let Err(error) = crate::olc::flush_save_list_to_disk(&mut self.state) {
            warn!("Copyover aborted because pending OLC could not be saved: {error}");
            self.state.send_to_char(
                requester,
                "Copyover OLC save failed; reboot aborted. Unsaved OLC entries remain pending.\n\r",
            );
            return;
        }
        if self.persist_copyover_players().await != 0 {
            self.state.send_to_char(
                requester,
                "Copyover database save failed; reboot aborted.\n\r",
            );
            return;
        }
        // The replacement process seeds its clock from `etc/date_record`.
        // Persist through the effective configured lib root and fail closed so
        // copyover cannot silently roll the world calendar back (#410).
        if let Err(error) = crate::weather::try_write_mud_date_to_file(&self.state) {
            warn!("copyover mud-date save failed: {error}");
            self.state.send_to_char(
                requester,
                "Copyover calendar save failed; reboot aborted.\n\r",
            );
            return;
        }
        if !self.flush_outputs_for_copyover().await {
            self.state.send_to_char(
                requester,
                "Copyover socket flush failed; reboot aborted.\n\r",
            );
            return;
        }
        // Do not consume arena backups in the old process. The durable SQL and
        // recovery snapshots already project their process-exit state; a
        // successful exec discards this memory, while any returned failure can
        // continue with the exact live arena/session state intact.
        crate::cmd_wizard::perform_copyover(&mut self.state, requester);
    }

    pub(crate) async fn flush_outputs_for_copyover(&mut self) -> bool {
        // Game::run flushes descriptor outbufs immediately before dispatching
        // the queued copyover. This barrier only proves every writer has
        // completed its already-enqueued work; it deliberately does not drain
        // or remove any descriptor/output owner on refusal.
        let writers: Vec<(ConnId, mpsc::Sender<OutputFrame>)> = self
            .outputs
            .iter()
            .map(|(&conn, writer)| (conn, writer.clone()))
            .collect();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut waits = Vec::with_capacity(writers.len());
        let mut ok = true;
        for (conn, writer) in writers {
            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
            match writer.try_send(OutputFrame::flush_barrier(ack_tx)) {
                Ok(()) => waits.push((conn, ack_rx)),
                Err(error) => {
                    warn!("copyover flush barrier enqueue failed for {conn}: {error}");
                    ok = false;
                }
            }
        }
        let waits = waits
            .into_iter()
            .map(|(conn, ack)| async move { (conn, tokio::time::timeout_at(deadline, ack).await) });
        for (conn, result) in futures_util::future::join_all(waits).await {
            match result {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) | Ok(Err(_)) => {
                    warn!("copyover socket flush failed for {conn}");
                    ok = false;
                }
                Err(_) => {
                    warn!("copyover socket flush timed out for {conn}");
                    ok = false;
                }
            }
        }
        ok
    }

    pub(crate) async fn persist_copyover_players(&mut self) -> u32 {
        // Finish any disconnect generation first. New snapshots are then
        // chained and awaited, so no stale task can outlive exec and no player
        // is recovered from an older SQL row.
        let mut failures = self.await_all_player_saves().await;
        let mut seen_players = HashSet::new();
        let players: Vec<(CharId, String)> = self
            .state
            .descriptors
            .values()
            .filter_map(|descriptor| {
                (descriptor.state == ConState::Playing)
                    .then(|| {
                        descriptor
                            .original
                            .or(descriptor.character)
                            .map(|player| (player, descriptor.host.clone()))
                    })
                    .flatten()
            })
            .filter(|(player, _)| seen_players.insert(*player))
            .collect();
        for (player, host) in players {
            let room_stamp = self
                .state
                .get_char(player)
                .and_then(|character| character.in_room)
                .and_then(|room| self.state.rooms.get(room))
                .map(|room| (room.number, room.map_x.zip(room.map_y)));
            let Some(mut snapshot) = self.snapshot_online_player_for_shutdown(player) else {
                continue;
            };
            if let Some((vnum, coordinates)) = room_stamp {
                if let Some((x, y)) = coordinates {
                    snapshot.tloadroom = -1;
                    snapshot.mapx = x as i64;
                    snapshot.mapy = y as i64;
                } else {
                    snapshot.tloadroom = vnum as i64;
                    snapshot.mapx = -1;
                    snapshot.mapy = -1;
                }
            }
            self.queue_player_save(snapshot, host);
        }
        failures = failures.saturating_add(self.await_all_player_saves().await);
        failures
    }

    /// Persist shutdown state first, then perform irreversible process-exit and
    /// output teardown only after every durability outcome is clean. A failed
    /// pass leaves descriptors, output senders, arena backups, and dirty crash
    /// flags available for a later retry.
    pub(crate) async fn shutdown_save(
        &mut self,
    ) -> std::result::Result<ShutdownReport, crate::olc::OlcFlushError> {
        // C comm.c:458-510: flush the OLC save list before stopping (#262).
        crate::olc::flush_save_list_to_disk(&mut self.state)?;
        let conn_ids: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        let mut report = ShutdownReport::default();

        // A prior disconnect save cannot be allowed to outlive the final
        // snapshot. Account for its outcome before queueing current rows.
        let pending_attempted = u32::try_from(self.pending_player_saves.len()).unwrap_or(u32::MAX);
        let pending_failures = self.await_all_player_saves().await;
        report.player_saves_attempted = report
            .player_saves_attempted
            .saturating_add(pending_attempted);
        report.players_saved = report
            .players_saved
            .saturating_add(pending_attempted.saturating_sub(pending_failures));
        report.database_errors = report.database_errors.saturating_add(pending_failures);

        // Crash-save only the connected playing PCs whose inventory is dirty,
        // matching Crash_save_all, but retain each result. Successful writes
        // clear PLR_CRASH, so failures elsewhere restore it before refusing the
        // shutdown to keep the whole pass retryable.
        let mut crash_players = Vec::new();
        let mut seen_crash_players = HashSet::new();
        for descriptor in self.state.descriptors.values() {
            if descriptor.state != ConState::Playing {
                continue;
            }
            for ch in descriptor.original.into_iter().chain(descriptor.character) {
                let needs_crash_save = self.state.get_char(ch).is_some_and(|character| {
                    !character.is_npc && character.act_flags & crate::objsave::PLR_CRASH != 0
                });
                if needs_crash_save && seen_crash_players.insert(ch) {
                    crash_players.push(ch);
                }
            }
        }
        let mut successful_crash_saves = Vec::new();
        for ch in crash_players {
            report.crash_saves_attempted = report.crash_saves_attempted.saturating_add(1);
            let lib_path = self.state.config.lib_path.clone();
            if crate::objsave::crash_save(&mut self.state, ch, &lib_path) {
                report.crash_saves_written = report.crash_saves_written.saturating_add(1);
                successful_crash_saves.push(ch);
            } else {
                report.crash_save_errors = report.crash_save_errors.saturating_add(1);
            }
        }

        match crate::weather::try_write_mud_date_to_file(&self.state) {
            Ok(()) => report.calendar_saved = true,
            Err(error) => {
                warn!("shutdown mud-date save failed: {error}");
                report.calendar_errors = report.calendar_errors.saturating_add(1);
            }
        }

        // One current snapshot per attached PC. The detached clone carries an
        // arena-safe process-exit projection and updated play time, while the
        // live Character remains untouched until the pass commits.
        let mut player_connections = Vec::new();
        let mut seen_players = HashSet::new();
        for (&conn, descriptor) in &self.state.descriptors {
            for ch in descriptor.original.into_iter().chain(descriptor.character) {
                if seen_players.insert(ch) {
                    player_connections.push((conn, ch, descriptor.host.clone()));
                }
            }
        }
        let mut current_player_saves = 0u32;
        for (_conn, ch, host) in player_connections {
            let Some(snapshot) = self.snapshot_online_player_for_shutdown(ch) else {
                continue;
            };
            report.alias_writes_attempted = report.alias_writes_attempted.saturating_add(1);
            if let Err(error) = crate::alias::write_aliases(
                &self.state,
                &self.state.config.lib_path,
                snapshot.get_name(),
                snapshot.idnum,
            ) {
                warn!(
                    "shutdown write_aliases(g, {}) failed: {}",
                    snapshot.get_name(),
                    error
                );
                report.alias_errors = report.alias_errors.saturating_add(1);
            } else {
                report.aliases_written = report.aliases_written.saturating_add(1);
            }
            current_player_saves = current_player_saves.saturating_add(1);
            self.queue_player_save(snapshot, host);
        }
        report.player_saves_attempted = report
            .player_saves_attempted
            .saturating_add(current_player_saves);
        let current_database_errors = self.await_all_player_saves().await;
        report.players_saved = report
            .players_saved
            .saturating_add(current_player_saves.saturating_sub(current_database_errors));
        report.database_errors = report
            .database_errors
            .saturating_add(current_database_errors);
        report.finish_persistence_counts();

        if !report.persistence_succeeded() {
            for ch in successful_crash_saves {
                if let Some(character) = self.state.get_char_mut(ch) {
                    character.act_flags |= crate::objsave::PLR_CRASH;
                }
            }
            return Ok(report);
        }

        // All restart-critical data is durable. Only now consume arena
        // backups, publish the final notice, and close writer ownership.
        crate::arena::prepare_process_exit(&mut self.state);
        for cid in &conn_ids {
            self.out(
                *cid,
                "\r\nThe server is shutting down. Saving and disconnecting...\r\n",
            );
        }

        // Snapshot writers before flushing: `flush_all` deliberately removes a
        // descriptor whose channel is full/closed, but shutdown reporting must
        // still record that connection's failed final-delivery attempt.
        let writers: Vec<(ConnId, mpsc::Sender<OutputFrame>)> = self
            .outputs
            .iter()
            .map(|(&conn, tx)| (conn, tx.clone()))
            .collect();
        // Flush all buffered output (the shutdown notice) to the writer tasks.
        self.flush_all().await;
        // A queue becoming empty only proves that the writer task dequeued the
        // bytes. Ordered barriers acknowledge after the socket write+flush.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut acknowledgements = Vec::with_capacity(writers.len());
        for (conn, tx) in writers {
            report.output_attempted = report.output_attempted.saturating_add(1);
            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
            match tx.try_send(OutputFrame::shutdown_barrier(ack_tx)) {
                Ok(()) => acknowledgements.push((conn, ack_rx)),
                Err(_) => {
                    warn!("shutdown output barrier enqueue failed for {}", conn);
                    report.output_failed = report.output_failed.saturating_add(1);
                }
            }
        }
        let waits = acknowledgements
            .into_iter()
            .map(|(conn, ack)| async move { (conn, tokio::time::timeout_at(deadline, ack).await) });
        for (conn, outcome) in futures_util::future::join_all(waits).await {
            match outcome {
                Ok(Ok(true)) => {
                    report.output_acknowledged = report.output_acknowledged.saturating_add(1);
                }
                Err(_) => {
                    warn!("shutdown output flush timed out for {}", conn);
                    report.output_timed_out = report.output_timed_out.saturating_add(1);
                }
                Ok(Ok(false)) | Ok(Err(_)) => {
                    warn!("shutdown output flush failed for {}", conn);
                    report.output_failed = report.output_failed.saturating_add(1);
                }
            }
        }
        report.output_failures = report.output_failed.saturating_add(report.output_timed_out);
        // Closing every sender lets writers without a barrier terminate too;
        // main owns and deterministically joins/aborts the connection tasks.
        self.outputs.clear();
        Ok(report)
    }
}
