//! Drain engines for deferred DB ops, offline ops, authority/password/rename/pfileclean, and the generation-chained player-save queue.
//!
//! Split out of game/mod.rs (phase 2); `use super::*` inherits the
//! Game struct, its fields, and the module's imports.

use super::*;

impl Game {
    pub(crate) fn snapshot_online_player_for_save(
        &mut self,
        ch: CharId,
    ) -> Option<crate::character::Character> {
        let now = chrono::Utc::now();
        let c = self.state.get_char_mut(ch)?;
        if c.is_npc {
            return None;
        }
        let elapsed = (now - c.last_logon).num_seconds().max(0);
        c.player.time_played = c.player.time_played.saturating_add(elapsed);
        c.last_logon = now;
        Some(c.clone())
    }

    /// Build the process-exit player row without mutating live session state.
    /// The normal save helper advances the live play-time clock; shutdown must
    /// be able to abort and retry with the exact live state intact. Arena
    /// backups are projected onto this clone because they must survive restart,
    /// but remain attached to the live combatant until durability succeeds.
    pub(crate) fn snapshot_online_player_for_shutdown(
        &self,
        ch: CharId,
    ) -> Option<crate::character::Character> {
        let now = chrono::Utc::now();
        let mut snapshot = self.state.get_char(ch)?.clone();
        if snapshot.is_npc {
            return None;
        }
        let elapsed = (now - snapshot.last_logon).num_seconds().max(0);
        snapshot.player.time_played = snapshot.player.time_played.saturating_add(elapsed);
        snapshot.last_logon = now;
        crate::arena::apply_process_exit_state_to_snapshot(&self.state, ch, &mut snapshot);
        Some(snapshot)
    }

    pub(crate) fn queue_player_save(
        &mut self,
        snapshot: crate::character::Character,
        host: String,
    ) {
        let idnum = snapshot.idnum;
        let name = snapshot.get_name().to_string();
        let prior = self
            .pending_player_saves
            .remove(&idnum)
            .map(|save| save.task);
        let db = self.db.clone();
        let timeout = Duration::from_secs(self.state.config.db_timeout_secs.max(1));
        let task_name = name.clone();
        let task_snapshot = snapshot.clone();
        let task = tokio::spawn(async move {
            let mut errors = Vec::new();
            if let Some(prior) = prior {
                match prior.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => errors.push(error),
                    Err(error) => errors.push(format!("prior save task failed: {error}")),
                }
            }
            match tokio::time::timeout(timeout, db.save_player_with_host(&task_snapshot, &host))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error.to_string()),
                Err(_) => errors.push(format!(
                    "database save timed out after {}s",
                    timeout.as_secs()
                )),
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(format!("{}: {}", task_name, errors.join("; ")))
            }
        });
        self.pending_player_saves.insert(
            idnum,
            PendingPlayerSave {
                name,
                snapshot,
                task,
            },
        );
    }

    pub(crate) fn pending_player_snapshot(
        &self,
        name: &str,
    ) -> Option<crate::character::Character> {
        self.pending_player_saves
            .values()
            .find(|save| save.name.eq_ignore_ascii_case(name))
            .map(|save| save.snapshot.clone())
    }

    pub(crate) async fn reap_completed_player_saves(&mut self) {
        let completed: Vec<i64> = self
            .pending_player_saves
            .iter()
            .filter_map(|(&idnum, save)| save.task.is_finished().then_some(idnum))
            .collect();
        for idnum in completed {
            let Some(save) = self.pending_player_saves.remove(&idnum) else {
                continue;
            };
            match save.task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.player_save_failures = self.player_save_failures.saturating_add(1);
                    warn!("ordered player save failed: {error}");
                }
                Err(error) => {
                    self.player_save_failures = self.player_save_failures.saturating_add(1);
                    warn!("ordered player save task for {} failed: {error}", save.name);
                }
            }
        }
    }

    pub(crate) async fn await_all_player_saves(&mut self) -> u32 {
        let pending = std::mem::take(&mut self.pending_player_saves);
        let mut failures = 0u32;
        for (_, save) in pending {
            match save.task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failures = failures.saturating_add(1);
                    warn!("ordered player save failed: {error}");
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    warn!("ordered player save task for {} failed: {error}", save.name);
                }
            }
        }
        self.player_save_failures = self.player_save_failures.saturating_add(failures);
        failures
    }

    pub(crate) async fn drain_deferred_db_ops(&mut self) {
        let ops: Vec<crate::state::DeferredDbOp> = std::mem::take(&mut self.state.deferred_db_ops);
        for op in ops {
            let r = match op {
                crate::state::DeferredDbOp::ClanDestroyFixup(n) => {
                    self.db_clan_destroy_fixup(n).await
                }
                crate::state::DeferredDbOp::ClanLowerRanks(n) => self.db_clan_lower_ranks(n).await,
            };
            if let Err(e) = r {
                log::warn!("deferred clan DB op failed: {}", e);
            }
        }
    }

    pub(crate) async fn drain_offline_ops(&mut self) {
        // Take the queue so a replayed command that itself queued (it won't,
        // since the target is now present) wouldn't be processed re-entrantly.
        let ops = std::mem::take(&mut self.state.offline_ops);
        for op in ops {
            // The requester must still be online to receive the output.
            if !self.state.char_exists(op.requester) {
                continue;
            }
            // If the target raced back online (logged in between queue + drain),
            // just replay against the live char — no load/extract needed.
            let key = op.target.to_lowercase();
            if let Some(target) = self.state.players_by_name.get(&key).copied() {
                let target_trust = self
                    .state
                    .get_char(target)
                    .map(|character| character.trust)
                    .unwrap_or(i32::MAX);
                if op.authority == OfflineOpAuthority::InspectPlayer
                    && !self
                        .state
                        .can_inspect_player_authority(op.requester, target_trust)
                {
                    self.state
                        .send_to_char(op.requester, PLAYER_INSPECTION_DENIED);
                    continue;
                }
                dispatch_command_isolated(
                    &mut self.state,
                    op.requester,
                    &op.command,
                    "offline-op-live",
                );
                continue;
            }

            let mut chr = match self.load_player_latest(&op.target).await {
                Ok(c) => c,
                Err(_) => {
                    self.state
                        .send_to_char(op.requester, "There is no such player.\r\n");
                    continue;
                }
            };
            // The player_table gate in cmd_wizard is only a queue-time
            // snapshot. Re-authorize against the freshly loaded DB row before
            // exposing any fields or splicing the target into the world; this
            // closes the level-change TOCTOU window. The replayed online
            // handler applies this same predicate once more.
            if op.authority == OfflineOpAuthority::InspectPlayer
                && !self
                    .state
                    .can_inspect_player_authority(op.requester, chr.trust)
            {
                self.state
                    .send_to_char(op.requester, PLAYER_INSPECTION_DENIED);
                continue;
            }
            // No live connection; clear stale object refs (the DB clone carries
            // last session's ObjIds — same hygiene as enter_game) so nothing in
            // the world dangles when we extract.
            chr.desc = None;
            chr.carrying.clear();
            chr.equipment = [None; NUM_WEARS];

            // Splice into the world and register the name so the replayed
            // command's online lookup (get_player_vis / find_player_by_name)
            // resolves it.
            let id = self.state.create_char(chr);
            self.state.affect_total(id);
            self.state.players_by_name.insert(key.clone(), id);
            // Place in a holding room (void vnum 3, else room 0) for in_room
            // safety; immortals target world-wide so the room is immaterial.
            if let Some(r) = self.state.real_room(3).or_else(|| self.state.real_room(0)) {
                self.state.char_to_room(id, r);
            }

            // Replay the immortal's verbatim command. Because the target is now
            // present, the handler's normal online branch applies the change (and
            // the immortal sees the standard output); the offline branch can't
            // re-trigger (the name resolves), so there's no re-deferral.
            let password_requests_before = self.state.password_update_requests.len();
            dispatch_command_isolated(
                &mut self.state,
                op.requester,
                &op.command,
                "offline-op-replay",
            );

            // `set passwd` queues its own typed, targeted credential update.
            // It intentionally does not mutate the temporary Character, so a
            // broad snapshot save here would add unrelated writes and could
            // race the password-only operation with a stale stored hash.
            let password_only = self
                .state
                .password_update_requests
                .get(password_requests_before..)
                .unwrap_or_default()
                .iter()
                .any(|request| {
                    request.victim == id
                        && request.authorization.requester_body == op.requester
                        && request.idnum
                            == self
                                .state
                                .get_char(id)
                                .map(|character| character.idnum)
                                .unwrap_or(0)
                });

            // Snapshot the (possibly edited) record, drop it from the world, and
            // persist — mirroring C's save_char(ch, NOWHERE) after the edit.
            let snap = self.state.get_char(id).cloned();
            self.state.players_by_name.remove(&key);
            if let Some(ref s) = snap {
                self.state
                    .update_player_index_from_character(s, s.last_logon.timestamp(), "");
            }
            self.state.extract_char(id);
            if let Some(s) = snap.filter(|_| !password_only) {
                self.queue_player_save(s, String::new());
            }
        }
    }

    /// Verify AFK-terminal unlock passwords without running a KDF in the
    /// synchronous command dispatcher. `await_database` continues servicing
    /// world messages while the bounded blocking worker runs, so the exact
    /// descriptor/principal/hash relationship is checked both before and after.
    pub(crate) async fn drain_lockout_unlock_requests(&mut self) {
        let requests = self.state.take_lockout_unlock_requests();
        for request in requests {
            if !lockout_unlock_is_current(
                &self.state,
                request.character,
                request.principal,
                request.descriptor,
                request.idnum,
                &request.name,
                &request.expected_hash,
            ) {
                if self.state.char_exists(request.character) {
                    self.state.send_to_char(
                        request.character,
                        "Password verification expired because the authenticated session changed; the terminal remains locked.\r\n",
                    );
                }
                continue;
            }

            let verified = self
                .await_database(crate::password::check_password_async(
                    request.expected_hash.clone(),
                    request.plaintext_password,
                ))
                .await;
            if !lockout_unlock_is_current(
                &self.state,
                request.character,
                request.principal,
                request.descriptor,
                request.idnum,
                &request.name,
                &request.expected_hash,
            ) {
                if self.state.char_exists(request.character) {
                    self.state.send_to_char(
                        request.character,
                        "Password verification expired because the authenticated session changed; the terminal remains locked.\r\n",
                    );
                }
                continue;
            }
            if verified {
                crate::cmd_other::complete_lockout_unlock(&mut self.state, request.character);
            } else {
                self.state.send_to_char(
                    request.character,
                    "Password mismatch! Sorry.\r\nTo unlock please type 'unlock <yourpassword>'\r\n",
                );
            }
        }
    }

    /// Commit exact player-authority transitions while the single-owner world
    /// is quiescent. The command path only queues a request; no live rank,
    /// capability, success message, or audit event is published until this
    /// drain confirms the complete durable tuple.
    pub(crate) async fn drain_authority_update_requests(&mut self) {
        enum Resolution {
            Committed,
            Rejected,
            Reconcile(crate::PlayerAuthorityState),
            Quarantine,
        }

        let requests = self.state.take_authority_update_requests();
        for request in requests {
            if !authority_update_request_is_current(&self.state, &request) {
                warn!(
                    "AUDIT: authority update for {} (id {}) failed its drain-time principal, identity, hierarchy, or canonical-state check",
                    request.name, request.idnum
                );
                if self.state.char_exists(request.authorization.requester_body) {
                    self.state.send_to_char(
                        request.authorization.requester_body,
                        "Authority change failed because identity, authority, or the requested transition changed; no authority change was made.\r\n",
                    );
                }
                continue;
            }

            // A previously launched broad save contains an older copy of every
            // authority field. It must finish before the narrow CAS so it can
            // never commit later and resurrect the superseded tuple.
            if let Some(save) = self.pending_player_saves.remove(&request.idnum) {
                let save_result = match save.task.await {
                    Ok(result) => result,
                    Err(error) => Err(format!("prior save task failed: {error}")),
                };
                if let Err(error) = save_result {
                    self.player_save_failures = self.player_save_failures.saturating_add(1);
                    error!(
                        "AUDIT: authority update for {} (id {}) aborted after prior player save failure: {}",
                        request.name, request.idnum, error
                    );
                    if authority_update_request_is_current(&self.state, &request) {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Authority change failed because the player's pending save did not complete; no authority change was made.\r\n",
                        );
                    }
                    continue;
                }
            }

            // Awaiting a prior save is quiescent today, but repeat the exact
            // request/target predicate here so this write stays safe if its
            // scheduling changes later.
            if !authority_update_request_is_current(&self.state, &request) {
                warn!(
                    "AUDIT: authority update for {} (id {}) canceled after its prior-save boundary",
                    request.name, request.idnum
                );
                continue;
            }

            let update = self
                .db
                .update_authority_if_current(
                    request.idnum,
                    &request.name,
                    request.expected,
                    request.replacement,
                )
                .await;
            let resolution = match update {
                Ok(crate::AuthorityUpdateOutcome::Updated) => Resolution::Committed,
                Ok(crate::AuthorityUpdateOutcome::PreconditionsChanged) => {
                    warn!(
                        "AUDIT: authority CAS for {} (id {}) observed changed durable preconditions; resolving by exact readback",
                        request.name, request.idnum
                    );
                    match self.db.player_authority_by_id(request.idnum).await {
                        Ok(Some((name, authority)))
                            if name == request.name && authority == request.replacement =>
                        {
                            Resolution::Committed
                        }
                        Ok(Some((name, authority)))
                            if name == request.name && authority == request.expected =>
                        {
                            Resolution::Rejected
                        }
                        Ok(Some((name, authority))) if name == request.name => {
                            warn!(
                                "AUDIT: authority update for {} (id {}) lost a durable race; reconciling live authority to {:?}",
                                request.name, request.idnum, authority
                            );
                            Resolution::Reconcile(authority)
                        }
                        Ok(observed) => {
                            error!(
                                "AUDIT: CRITICAL authority update for {} (id {}) cannot reconcile identity after a rejected CAS; observed={:?}",
                                request.name, request.idnum, observed
                            );
                            Resolution::Quarantine
                        }
                        Err(error) => {
                            error!(
                                "AUDIT: CRITICAL authority update for {} (id {}) rejected and exact readback failed: {}",
                                request.name, request.idnum, error
                            );
                            Resolution::Quarantine
                        }
                    }
                }
                Err(error) => {
                    error!(
                        "AUDIT: authority CAS for {} (id {}) errored; resolving the potentially committed write by exact readback: {}",
                        request.name, request.idnum, error
                    );
                    match self.db.player_authority_by_id(request.idnum).await {
                        Ok(Some((name, authority)))
                            if name == request.name && authority == request.replacement =>
                        {
                            Resolution::Committed
                        }
                        Ok(Some((name, authority)))
                            if name == request.name && authority == request.expected =>
                        {
                            Resolution::Rejected
                        }
                        Ok(Some((name, authority))) if name == request.name => {
                            warn!(
                                "AUDIT: authority update error for {} (id {}) resolved to another durable tuple {:?}; reconciling live authority",
                                request.name, request.idnum, authority
                            );
                            Resolution::Reconcile(authority)
                        }
                        Ok(observed) => {
                            error!(
                                "AUDIT: CRITICAL authority outcome for {} (id {}) is indeterminate because durable identity differs or is absent; observed={:?}",
                                request.name, request.idnum, observed
                            );
                            Resolution::Quarantine
                        }
                        Err(read_error) => {
                            error!(
                                "AUDIT: CRITICAL authority outcome for {} (id {}) is indeterminate; exact readback also failed: {}",
                                request.name, request.idnum, read_error
                            );
                            Resolution::Quarantine
                        }
                    }
                }
            };

            // Direct database awaits quiesce the world, nevertheless take one
            // final exact snapshot before any live mutation or requester-facing
            // publication. Durable reconciliation below is a system
            // continuation and must still complete after an ambiguous commit.
            let requester_may_receive_result =
                authority_update_request_is_current(&self.state, &request);
            match resolution {
                Resolution::Committed => {
                    if requester_may_receive_result {
                        crate::cmd_wizard::complete_advance(&mut self.state, &request);
                    } else if let Some(victim) = self.state.get_char_mut(request.victim) {
                        apply_player_authority_state(victim, request.replacement);
                    }
                    self.state.authority_quarantine.remove(&request.idnum);
                    if let Some(snapshot) = self.state.get_char(request.victim).cloned() {
                        self.state.update_player_index_from_character(
                            &snapshot,
                            snapshot.last_logon.timestamp(),
                            "",
                        );
                    }
                    // Persist dependent demotion cleanup (invisibility and
                    // preference flags). The complete authority tuple is
                    // already durable, and any later save snapshots it.
                    self.state.request_player_save(request.victim);
                }
                Resolution::Rejected => {
                    self.state.authority_quarantine.remove(&request.idnum);
                    if requester_may_receive_result {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Authority change was rejected because durable state changed; no requested authority change was made.\r\n",
                        );
                    }
                }
                Resolution::Reconcile(authority) => {
                    if let Some(victim) = self.state.get_char_mut(request.victim) {
                        apply_player_authority_state(victim, authority);
                    }
                    self.state.authority_quarantine.remove(&request.idnum);
                    if let Some(snapshot) = self.state.get_char(request.victim).cloned() {
                        self.state.update_player_index_from_character(
                            &snapshot,
                            snapshot.last_logon.timestamp(),
                            "",
                        );
                    }
                    self.state.request_player_save(request.victim);
                    if requester_may_receive_result {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Authority change lost a durable race. Live authority was reconciled to storage; retry after reviewing the target.\r\n",
                        );
                    }
                }
                Resolution::Quarantine => {
                    let safe = least_privileged_authority(request.expected, request.replacement);
                    if let Some(victim) = self.state.get_char_mut(request.victim) {
                        apply_player_authority_state(victim, safe);
                    }
                    self.state.authority_quarantine.insert(request.idnum);
                    if let Some(snapshot) = self.state.get_char(request.victim).cloned() {
                        self.state.update_player_index_from_character(
                            &snapshot,
                            snapshot.last_logon.timestamp(),
                            "",
                        );
                    }
                    if requester_may_receive_result {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "CRITICAL: the durable authority outcome is indeterminate. The account has been privilege-quarantined; check the audit log and database before retrying.\r\n",
                        );
                    }
                    if self.state.char_exists(request.victim) {
                        self.state.send_to_char(
                            request.victim,
                            "Your administrative authority is temporarily quarantined while durable state is reconciled.\r\n",
                        );
                    }
                }
            }
            self.state.revalidate_snoop_links();
        }
    }

    /// Commit authenticated `set passwd` requests through the password-only
    /// database primitive. Authority and target identity are rechecked at the
    /// async boundary; neither the requester nor the victim sees success until
    /// the exact durable row acknowledges the update.
    pub(crate) async fn drain_password_update_requests(&mut self) {
        let requests = self.state.take_password_update_requests();
        for mut request in requests {
            if password_update_request_is_current(&self.state, &request).is_none() {
                warn!(
                    "AUDIT: password update for {} (id {}) failed its drain-time identity/authority check",
                    request.name, request.idnum
                );
                if self.state.authenticated_command_request_is_current(
                    request.authorization,
                    i32::from(LVL_IMPL),
                    1,
                    crate::gcmd::GCMD_SET,
                ) {
                    self.state.send_to_char(
                        request.authorization.requester_body,
                        "Password change failed because authority or the player identity changed; no password change was made.\r\n",
                    );
                }
                continue;
            }
            let requester_name = self
                .state
                .get_char(request.authorization.requester_principal)
                .map(|principal| principal.get_name().to_string())
                .unwrap_or_else(|| "<departed>".to_string());

            // Order this credential change after any already-launched save for
            // the same player. Generic saves now exclude `pwd` atomically, so
            // they cannot resurrect a hash on either side of this boundary.
            if let Some(save) = self.pending_player_saves.remove(&request.idnum) {
                match save.task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(save_error)) => {
                        self.player_save_failures = self.player_save_failures.saturating_add(1);
                        warn!(
                            "ordered save preceding password update for {} failed: {}",
                            request.name, save_error
                        );
                    }
                    Err(save_error) => {
                        self.player_save_failures = self.player_save_failures.saturating_add(1);
                        warn!(
                            "ordered save task preceding password update for {} failed: {}",
                            request.name, save_error
                        );
                    }
                }
            }

            if password_update_request_is_current(&self.state, &request).is_none() {
                warn!(
                    "AUDIT: password update for {} (id {}) canceled after its prior-save boundary",
                    request.name, request.idnum
                );
                continue;
            }

            let plaintext_password = std::mem::take(&mut request.plaintext_password);
            let Some(password_hash) = self
                .await_database(crate::password::hash_password_async(plaintext_password))
                .await
            else {
                warn!(
                    "AUDIT: password update for {} (id {}) could not enter or complete the password KDF",
                    request.name, request.idnum
                );
                if password_update_request_is_current(&self.state, &request).is_some() {
                    self.state.send_to_char(
                        request.authorization.requester_body,
                        "Password change is temporarily unavailable; no password change was made.\r\n",
                    );
                }
                continue;
            };

            // The KDF runs through await_database and may service disconnects,
            // switches, grant changes, or authority transitions. Bind the
            // password write to the exact session and target again now.
            if password_update_request_is_current(&self.state, &request).is_none() {
                warn!(
                    "AUDIT: password update for {} (id {}) canceled after KDF because its authenticated request changed",
                    request.name, request.idnum
                );
                continue;
            }

            let durable = match self
                .db
                .update_password_hash(request.idnum, &request.name, None, &password_hash)
                .await
            {
                Err(error) => {
                    self.resolve_password_update_error(&request.name, &password_hash, error)
                        .await
                }
                result => result,
            };
            let request_current_after_durable =
                password_update_request_is_current(&self.state, &request).is_some();
            let live_target_after_durable =
                password_update_target_is_current(&self.state, &request).flatten();
            match durable {
                Ok(crate::PasswordHashUpdateOutcome::Updated) => {
                    // Updating the target's credential cache reconciles a
                    // confirmed durable commit and is independent of the
                    // requester's continued session. It still requires the
                    // exact target identity observed by the request.
                    if let Some(victim) = live_target_after_durable {
                        if let Some(conn_id) = self.state.get_char(victim).and_then(|c| c.desc) {
                            if let Some(descriptor) = self.state.descriptors.get_mut(&conn_id) {
                                descriptor.password_hash = Some(password_hash.clone());
                            }
                        }
                        if let Some(character) = self.state.get_char_mut(victim) {
                            character.pending_password_hash = None;
                        }
                    }
                    info!(
                        "AUDIT: {} changed the password for {} (id {})",
                        requester_name, request.name, request.idnum
                    );
                    if request_current_after_durable {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            &format!("Password changed for {}.\r\n", request.name),
                        );
                    }
                }
                Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch) => {
                    warn!(
                        "AUDIT: password update for {} (id {}) was rejected by the durable identity predicate",
                        request.name, request.idnum
                    );
                    if request_current_after_durable {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Password change failed because the durable player identity changed; no password change was made.\r\n",
                        );
                    }
                }
                Ok(crate::PasswordHashUpdateOutcome::CurrentHashMismatch) => {
                    warn!(
                        "AUDIT: password update for {} (id {}) was not confirmed; durable readback found another credential",
                        request.name, request.idnum
                    );
                    if request_current_after_durable {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Password change was not confirmed; the requested credential was not active at durable readback. Have the player reconnect and use their current account password.\r\n",
                        );
                    }
                }
                Err(error) => {
                    error!(
                        "AUDIT: password update for {} (id {}) has an indeterminate durable outcome: {}",
                        request.name, request.idnum, error
                    );
                    if request_current_after_durable {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Password change could not be confirmed. Have the player reconnect and try the new password, then the old password.\r\n",
                        );
                    }
                }
            }
        }
    }

    /// Commit queued live-player renames without ever exposing a name which is
    /// only present in memory.  This deliberately quiesces the single-owner
    /// world while the bounded conditional UPDATE runs: servicing a disconnect
    /// or save concurrently could otherwise enqueue an old-name REPLACE after
    /// the rename and silently undo it.  A normal operation is one indexed
    /// UPDATE and should complete in milliseconds; TimedDatabase supplies the
    /// fail-closed upper bound.
    pub(crate) async fn drain_player_rename_requests(&mut self) {
        let requests = self.state.take_player_rename_requests();
        for request in requests {
            if player_rename_request_is_current(&self.state, &request).is_none() {
                warn!(
                    "AUDIT: rename {} (id {}) -> {} failed its drain-time authenticated identity/authority/collision recheck",
                    request.old_name, request.idnum, request.new_name
                );
                continue;
            }
            let old_key = request.old_name.to_lowercase();
            let new_key = request.new_name.to_lowercase();

            // A disconnect save from an earlier iteration may still be running
            // with the old name.  It must finish before the conditional rename
            // so it cannot commit later and restore the old key.
            if let Some(save) = self.pending_player_saves.remove(&request.idnum) {
                let save_result = match save.task.await {
                    Ok(result) => result,
                    Err(error) => Err(format!("prior save task failed: {error}")),
                };
                if let Err(error) = save_result {
                    self.player_save_failures = self.player_save_failures.saturating_add(1);
                    error!(
                        "AUDIT: rename {} (id {}) -> {} aborted after prior player save failure: {}",
                        request.old_name, request.idnum, request.new_name, error
                    );
                    if player_rename_request_is_current(&self.state, &request).is_some() {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Rename failed because the player's pending save did not complete; no name change was made.\r\n",
                        );
                    }
                    continue;
                }
            }

            if player_rename_request_is_current(&self.state, &request).is_none() {
                warn!(
                    "AUDIT: rename {} (id {}) -> {} canceled after its prior-save boundary",
                    request.old_name, request.idnum, request.new_name
                );
                continue;
            }

            // SQL is the authoritative identity.  Do not touch sidecars until
            // this exact id/old-name/destination predicate commits.
            let durable_rename = self
                .db
                .rename_player_if_current(request.idnum, &request.old_name, &request.new_name)
                .await;
            match durable_rename {
                Ok(true) => {}
                Ok(false) => {
                    warn!(
                        "AUDIT: rename {} (id {}) -> {} rejected by the durable identity/collision predicate",
                        request.old_name, request.idnum, request.new_name
                    );
                    if player_rename_request_is_current(&self.state, &request).is_some() {
                        self.state.send_to_char(
                            request.authorization.requester_body,
                            "Rename failed because the durable player identity or destination changed; no name change was made.\r\n",
                        );
                    }
                    continue;
                }
                Err(error) => {
                    error!(
                        "AUDIT: rename {} (id {}) -> {} failed before sidecar publication: {}",
                        request.old_name, request.idnum, request.new_name, error
                    );
                    // A transport timeout while COMMIT is in flight is
                    // inherently outcome-ambiguous. Run the inverse
                    // conditional operation (a no-op when the old name never
                    // changed), then read the exact identity before making any
                    // claim to the administrator. Sidecars are still untouched.
                    let compensation = self
                        .db
                        .rename_player_if_current(
                            request.idnum,
                            &request.new_name,
                            &request.old_name,
                        )
                        .await;
                    if let Err(compensation_error) = &compensation {
                        error!(
                            "AUDIT: rename {} (id {}) -> {} error compensation also failed: {}",
                            request.old_name, request.idnum, request.new_name, compensation_error
                        );
                    }
                    let observed_name = self.db.player_name_by_id(request.idnum).await;
                    let old_name_confirmed = observed_name.as_ref().is_ok_and(|name| {
                        name.as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case(&request.old_name))
                    });
                    if !old_name_confirmed {
                        error!(
                            "AUDIT: CRITICAL rename {} (id {}) -> {} could not confirm the old SQL identity after error compensation; observed={:?}",
                            request.old_name, request.idnum, request.new_name, observed_name
                        );
                    }
                    if player_rename_request_is_current(&self.state, &request).is_some() {
                        let message = if old_name_confirmed {
                            "Rename failed while saving the durable player identity; the database old name was confirmed and no files or live names were changed.\r\n"
                        } else {
                            "CRITICAL: rename database state is indeterminate after a failed compensation. No files or live names were changed; check the audit log immediately.\r\n"
                        };
                        self.state
                            .send_to_char(request.authorization.requester_body, message);
                    }
                    continue;
                }
            }

            // Direct SQL awaits quiesce the world, but recheck after the
            // durable boundary before mutating name-keyed sidecars. If this
            // invariant ever changes, compensate SQL rather than publishing a
            // stale administrator request.
            if player_rename_request_is_current(&self.state, &request).is_none() {
                let rollback = self
                    .db
                    .rename_player_if_current(request.idnum, &request.new_name, &request.old_name)
                    .await;
                error!(
                    "AUDIT: rename {} (id {}) -> {} lost authorization after SQL commit; rollback={:?}",
                    request.old_name, request.idnum, request.new_name, rollback
                );
                continue;
            }

            // The database now owns the new name.  Move both name-keyed files
            // as one preflighted/rollback-capable lifecycle.  If it fails,
            // conditionally restore the SQL name before returning failure.
            // SQL and the filesystem cannot share one atomic commit: process
            // or host loss in this small post-COMMIT window can require manual
            // reconciliation at restart.  We never report success before the
            // window closes, and every recoverable runtime failure below is
            // compensated and audited rather than hidden.
            let lib_path = self.state.config.lib_path.clone();
            if let Err(sidecar_error) = crate::player_sidecars::rename_player_sidecars(
                &mut self.state,
                &lib_path,
                &request.old_name,
                &request.new_name,
                request.idnum,
            ) {
                let rollback = self
                    .db
                    .rename_player_if_current(request.idnum, &request.new_name, &request.old_name)
                    .await;
                let sql_rollback_restored_old_name = match rollback {
                    Ok(true) => {
                        error!(
                            "AUDIT: rename {} (id {}) -> {} rolled SQL back after sidecar failure: {}",
                            request.old_name, request.idnum, request.new_name, sidecar_error
                        );
                        true
                    }
                    Ok(false) => {
                        error!(
                            "AUDIT: CRITICAL rename {} (id {}) -> {} sidecars failed and SQL rollback predicate was rejected: {}",
                            request.old_name, request.idnum, request.new_name, sidecar_error
                        );
                        false
                    }
                    Err(rollback_error) => {
                        error!(
                            "AUDIT: CRITICAL rename {} (id {}) -> {} sidecars failed and SQL rollback errored: {}; rollback: {}",
                            request.old_name,
                            request.idnum,
                            request.new_name,
                            sidecar_error,
                            rollback_error
                        );
                        false
                    }
                };
                let fully_consistent_failure =
                    sql_rollback_restored_old_name && !sidecar_error.rollback_incomplete();
                if !fully_consistent_failure && sidecar_error.rollback_incomplete() {
                    error!(
                        "AUDIT: CRITICAL rename {} (id {}) -> {} left at least one sidecar move incompletely rolled back",
                        request.old_name, request.idnum, request.new_name
                    );
                }
                if player_rename_request_is_current(&self.state, &request).is_some() {
                    let message = if fully_consistent_failure {
                        "Rename failed while moving the player's durable files; the database old name was restored and no live name change was published.\r\n"
                    } else {
                        "CRITICAL: rename storage is inconsistent after a failed rollback. No live name change was published; check the audit log immediately.\r\n"
                    };
                    self.state
                        .send_to_char(request.authorization.requester_body, message);
                }
                continue;
            }

            // No world state can change during the synchronous sidecar move,
            // but make the publication invariant explicit at the exact live
            // index/name mutation boundary.
            let Some(requester_name) = player_rename_request_is_current(&self.state, &request)
            else {
                let lib_path = self.state.config.lib_path.clone();
                let sidecar_rollback = crate::player_sidecars::rename_player_sidecars(
                    &mut self.state,
                    &lib_path,
                    &request.new_name,
                    &request.old_name,
                    request.idnum,
                );
                let sql_rollback = self
                    .db
                    .rename_player_if_current(request.idnum, &request.new_name, &request.old_name)
                    .await;
                error!(
                    "AUDIT: rename {} (id {}) -> {} lost authorization before live publication; sidecar rollback={:?}; SQL rollback={:?}",
                    request.old_name,
                    request.idnum,
                    request.new_name,
                    sidecar_rollback,
                    sql_rollback
                );
                continue;
            };

            // Every durable component now resolves through the new identity.
            // These remaining in-memory operations are infallible; only here
            // may users, indexes, mail, or the audit stream observe success.
            self.state.players_by_name.remove(&old_key);
            if let Some(victim) = self.state.get_char_mut(request.victim) {
                victim.player.name = request.new_name.clone();
            }
            self.state.players_by_name.insert(new_key, request.victim);
            if let Some(snapshot) = self.state.get_char(request.victim).cloned() {
                self.state.update_player_index_from_character(
                    &snapshot,
                    snapshot.last_logon.timestamp(),
                    "",
                );
            }
            crate::mail::mail_register_player(&mut self.state, request.idnum, &request.new_name);

            self.state.send_to_char(
                request.authorization.requester_body,
                &format!(
                    "You have renamed {} to {}.\r\n",
                    request.old_name, request.new_name
                ),
            );
            if self.state.char_exists(request.victim) {
                self.state.send_to_char(
                    request.victim,
                    &format!(
                        "&GYou have been renamed to {} by the gods.&n\r\n",
                        request.new_name
                    ),
                );
            }
            crate::syslog::mudlog(
                &mut self.state,
                &format!(
                    "{} has renamed {} to {}",
                    requester_name, request.old_name, request.new_name
                ),
                crate::syslog::NRM,
                LVL_GOD,
            );
        }
    }

    pub(crate) async fn drain_pfileclean(&mut self) {
        let Some(request) = self.state.take_pfileclean_request() else {
            return;
        };
        if !self.state.authenticated_command_request_is_current(
            request,
            i32::from(LVL_IMMORT),
            3,
            crate::gcmd::GCMD3_PFILECLEAN,
        ) {
            warn!(
                "AUDIT: queued pfileclean canceled because its authenticated authority or grant changed"
            );
            return;
        }

        // Capture the authoritative names/idnums before DELETE so the same
        // lifecycle used by self-delete can remove name-keyed rent/alias data.
        // If discovery or any cleanup fails, retain the PLR_DELETED DB row as
        // the durable audit/tombstone and let a later pfileclean retry.
        let latest_players = match self.db_list_players().await {
            Ok(players) => players,
            Err(err) => {
                warn!("pfileclean aborted before sidecar cleanup: failed to list players: {err}");
                return;
            }
        };
        if !self.state.authenticated_command_request_is_current(
            request,
            i32::from(LVL_IMMORT),
            3,
            crate::gcmd::GCMD3_PFILECLEAN,
        ) {
            warn!(
                "AUDIT: pfileclean canceled after player discovery because its authenticated authority or grant changed"
            );
            return;
        }
        self.state.player_table = latest_players.clone();
        let deleted_players: Vec<_> = latest_players
            .into_iter()
            .filter(|player| player.act_flags & crate::flags::PLR_DELETED != 0)
            .collect();

        if let Some(player) = deleted_players
            .iter()
            .find(|player| self.state.find_player_by_name(&player.name).is_some())
        {
            warn!(
                "AUDIT: pfileclean aborted: deleted player {} (id {}) is still in the world",
                player.name, player.idnum
            );
            return;
        }

        if !self.state.authenticated_command_request_is_current(
            request,
            i32::from(LVL_IMMORT),
            3,
            crate::gcmd::GCMD3_PFILECLEAN,
        ) {
            warn!(
                "AUDIT: pfileclean canceled before sidecar deletion because its authenticated authority or grant changed"
            );
            return;
        }

        let mut cleanup_failures = Vec::new();
        for player in &deleted_players {
            let lib_path = self.state.config.lib_path.clone();
            if let Err(error) = crate::player_sidecars::delete_player_sidecars(
                &mut self.state,
                &lib_path,
                &player.name,
                player.idnum,
            ) {
                cleanup_failures.push(format!("{} (id {}): {}", player.name, player.idnum, error));
            }
        }
        if !cleanup_failures.is_empty() {
            error!(
                "AUDIT: pfileclean retained DB tombstones because sidecar cleanup is incomplete: {}",
                cleanup_failures.join("; ")
            );
            return;
        }

        if !self.state.authenticated_command_request_is_current(
            request,
            i32::from(LVL_IMMORT),
            3,
            crate::gcmd::GCMD3_PFILECLEAN,
        ) {
            warn!(
                "AUDIT: pfileclean retained DB tombstones because authorization changed before row deletion"
            );
            return;
        }
        let audited_idnums: Vec<i64> = deleted_players.iter().map(|player| player.idnum).collect();
        // This destructive call deliberately bypasses await_database: the
        // exact recheck above and the commit are one quiescent world boundary.
        match self
            .db
            .delete_deleted_players_by_idnums(audited_idnums)
            .await
        {
            Ok(deleted) => {
                if !self.state.authenticated_command_request_is_current(
                    request,
                    i32::from(LVL_IMMORT),
                    3,
                    crate::gcmd::GCMD3_PFILECLEAN,
                ) {
                    warn!(
                        "AUDIT: pfileclean requester changed during a quiescent delete; continuing committed-state reconciliation"
                    );
                }
                info!("pfileclean deleted {} PLR_DELETED player row(s)", deleted);
                // Rebuilding the index is reconciliation of an already
                // committed system state and must complete even if a future DB
                // implementation can invalidate the requesting session here.
                match self.db.list_players().await {
                    Ok(players) => {
                        if !self.state.authenticated_command_request_is_current(
                            request,
                            i32::from(LVL_IMMORT),
                            3,
                            crate::gcmd::GCMD3_PFILECLEAN,
                        ) {
                            warn!(
                                "AUDIT: pfileclean requester changed during quiescent index readback; applying committed-state reconciliation only"
                            );
                        }
                        self.state.player_table = players;
                    }
                    Err(err) => {
                        warn!("pfileclean deleted rows but failed to rebuild player index: {err}");
                    }
                }
            }
            Err(err) => {
                warn!("pfileclean failed to delete PLR_DELETED player rows: {err}");
            }
        }
    }

    pub(crate) async fn drain_player_save_requests(&mut self) {
        let requests = self.state.take_player_save_requests();
        if requests.is_empty() {
            return;
        }

        let mut snapshots = Vec::new();
        for cid in requests {
            if let Some(snapshot) = self.snapshot_online_player_for_save(cid) {
                snapshots.push(snapshot);
            }
        }

        for snapshot in snapshots {
            let host = snapshot
                .desc
                .and_then(|conn| self.state.descriptors.get(&conn))
                .map(|d| d.host.clone())
                .unwrap_or_default();
            self.state.update_player_index_from_character(
                &snapshot,
                snapshot.last_logon.timestamp(),
                &host,
            );
            if let Err(err) = crate::alias::write_aliases(
                &self.state,
                &self.state.config.lib_path,
                snapshot.get_name(),
                snapshot.idnum,
            ) {
                warn!(
                    "queued write_aliases(g, {}) failed: {}",
                    snapshot.get_name(),
                    err
                );
            }
            self.queue_player_save(snapshot, host);
        }
    }
}
