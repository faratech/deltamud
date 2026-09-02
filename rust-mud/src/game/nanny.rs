//! Login/character-creation state machine (nanny), input entry (handle_input), and connection removal (disconnect).
//!
//! Split out of game/mod.rs (phase 2); `use super::*` inherits the
//! Game struct, its fields, and the module's imports.

use super::*;

impl Game {
    pub(crate) async fn handle_input(&mut self, conn_id: ConnId, input: String) {
        // Any input proves the player is alive: reset the login-prompt idle
        // counter (C clears it on entering each password state; the old
        // one-way counter booted ACTIVE players after 30s of accumulated
        // thinking time across creation states).
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.idle_tics = 0;
        }
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };

        // C comm.c process_input (1836-1960), applied to every completed line
        // regardless of connection state:
        //   1. every '$' is doubled on entry (act() renders '$$' as one
        //      literal '$', so 'say Hi $n' says 'Hi $n') (#222);
        //   2. the line is capped at MAX_INPUT_LENGTH (256) with C's
        //      'Line too long. Truncated to:' notice (#224);
        //   3. '!' repeats last_input and '^old^new' performs the csh-style
        //      substitution on it; otherwise last_input records the line.
        let mut doubled = String::with_capacity(input.len() + 8);
        for c in input.chars() {
            if c == '$' {
                doubled.push_str("$$");
            } else {
                doubled.push(c);
            }
        }
        let max_len = crate::types::MAX_INPUT_LENGTH;
        let mut line = if doubled.len() > max_len {
            let truncated = crate::text::utf8_prefix(&doubled, max_len).to_string();
            self.out(
                conn_id,
                &format!("Line too long.  Truncated to:\r\n{}\r\n", truncated),
            );
            truncated
        } else {
            doubled
        };
        if line.starts_with('!') {
            let last = self
                .state
                .descriptors
                .get(&conn_id)
                .map(|d| d.last_input.clone())
                .unwrap_or_default();
            line = last;
        } else if line.starts_with('^') {
            let last = self
                .state
                .descriptors
                .get(&conn_id)
                .map(|d| d.last_input.clone())
                .unwrap_or_default();
            match Game::perform_subst(&last, &line) {
                Some(new) => {
                    line = new;
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.last_input = line.clone();
                    }
                }
                None => {
                    self.out(conn_id, "Invalid substitution.\r\n");
                    return;
                }
            }
        } else if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.last_input = line.clone();
        }
        let input = line;

        if state == ConState::Playing {
            if crate::modify::page_active(&self.state, conn_id) {
                crate::modify::page_input(&mut self.state, conn_id, &input);
            } else if crate::modify::editing(&self.state, conn_id) {
                if !crate::modify::editor_input(&mut self.state, conn_id, &input) {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.editors.pop();
                    }
                }
            } else if crate::olc::in_olc(&self.state, conn_id) {
                crate::olc::olc_input(&mut self.state, conn_id, &input);
            } else {
                // Gameplay command: queue it instead of dispatching now. The
                // heartbeat's process_input_queues drains one per pulse once the
                // descriptor's WAIT_STATE lag (d.wait) expires, and sends the
                // prompt after the command actually runs.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    // C comm.c drops the CONNECTION when its input buffer
                    // overflows; a hard cap here stops a flood client from
                    // growing the queue unbounded (drain rate is 1/pulse).
                    const MAX_QUEUED_COMMANDS: usize = 32;
                    if d.input_queue.len() >= MAX_QUEUED_COMMANDS {
                        d.write("\r\nInput queue full.\r\n");
                        d.state = ConState::Close;
                        return;
                    }
                    d.input_queue.push_back(QueuedInput::raw(input));
                }
                return;
            }
        } else {
            self.nanny(conn_id, input).await;
        }

        // Re-send the appropriate prompt unless the connection is closing or
        // the nanny arm printed its own inline prompt.
        let suppress = self
            .state
            .descriptors
            .get_mut(&conn_id)
            .map(|d| d.suppress_prompt)
            .unwrap_or(false);
        if suppress {
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.suppress_prompt = false;
            }
        }
        let st = self.state.descriptors.get(&conn_id).map(|d| d.state);
        if st.is_some() && st != Some(ConState::Close) && !suppress {
            self.write_prompt(conn_id);
        }
    }

    /// Drain one queued command per descriptor whose WAIT_STATE lag has expired
    /// (C game_loop: `if ((--d->wait) <= 0 && get_from_q(...))`). Decrement every
    /// playing descriptor's wait each pulse; when it reaches <= 0 and input is
    /// queued, run one command (resetting wait to 1 first, so a command's own
    /// WAIT_STATE call overrides it) and send the prompt.
    pub(crate) async fn nanny(&mut self, conn_id: ConnId, input: String) {
        let input = input.trim().to_string();
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };

        match state {
            ConState::QAnsi => {
                // C interpreter.c:1706-1735 CON_QANSI (#198).
                let first = input.chars().next().map(|c| c.to_ascii_lowercase());
                if input.is_empty() || first == Some('y') {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.wants_colour = Some(true);
                    }
                    self.out(conn_id, "Your terminal will now receive color.\r\n\r\n\r\n");
                } else if first == Some('n') {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.wants_colour = Some(false);
                    }
                    self.out(conn_id, "Your terminal will not receive color.\r\n\r\n\r\n");
                } else {
                    self.out(conn_id, "That is not a proper response.\r\n\r\n");
                    self.out(conn_id, ANSI_QUESTION);
                    return;
                }
                let startup = self.state.startup.clone();
                self.out(conn_id, &startup);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::GetName;
                }
            }
            ConState::GetName => {
                if input.is_empty() {
                    // C interpreter.c:1744: an empty name closes the socket.
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }
                let name = normalize_name(&input);
                // C interpreter.c:1747-1752: _parse_name length/alpha checks,
                // fill_word/reserved_word, and Valid_Name (xnames substrings +
                // mob-keyword collisions) (#223).
                if !valid_name(&name)
                    || reserved_or_fill_word(&name)
                    || !crate::ban::valid_name_in(&self.state, &name)
                {
                    // C interpreter.c:1739: the message carries its own
                    // 'Name: ' prompt.
                    self.out(conn_id, "Invalid name, please try another.\r\nName: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.suppress_prompt = true;
                    }
                    return;
                }
                let exists = match self.db_player_exists(&name).await {
                    Ok(exists) => exists,
                    Err(error) => {
                        warn!("check player name {} failed: {}", name, error);
                        self.out(
                            conn_id,
                            "Unable to check that name right now; please try again.\r\nName: ",
                        );
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.suppress_prompt = true;
                        }
                        return;
                    }
                };
                if !exists && crate::olc::name_reserved_by_zone_acl(&self.state, &name) {
                    self.out(conn_id, "Invalid name, please try another.\r\nName: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.temp_name = None;
                        d.state = ConState::GetName;
                        d.suppress_prompt = true;
                    }
                    return;
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_name = Some(name.clone());
                    d.state = if exists {
                        ConState::GetOldPassword
                    } else {
                        ConState::ConfirmName
                    };
                }
            }
            ConState::ConfirmName => {
                let yes = input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes");
                if yes {
                    let requested_name = self.descriptor_name(conn_id);
                    if crate::olc::name_reserved_by_zone_acl(&self.state, &requested_name) {
                        self.out(conn_id, "Invalid name, please try another.\r\nName: ");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.temp_name = None;
                            d.state = ConState::GetName;
                            d.suppress_prompt = true;
                        }
                        return;
                    }
                    let host = self.descriptor_host(conn_id);
                    let banned = self.descriptor_ban_type(conn_id);
                    if banned >= crate::ban::BanType::New {
                        self.out(
                            conn_id,
                            "Sorry, new characters are not allowed from your site!\r\n",
                        );
                        warn!(
                            "Request for new char {} denied from [{}] (siteban)",
                            self.descriptor_name(conn_id),
                            host
                        );
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        return;
                    }
                    // C interpreter.c:1826: wizlock refuses NEW characters too.
                    if crate::cmd_wizard::circle_restrict() > 0 {
                        warn!(
                            "Request for new char {} denied from [{}] (wizlock)",
                            self.descriptor_name(conn_id),
                            host
                        );
                        self.out(
                            conn_id,
                            "Sorry, new players can't be created at the moment.\r\n",
                        );
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        return;
                    }
                    self.out(conn_id, "New character.\r\n");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetNewPassword;
                    }
                } else if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_name = None;
                    d.state = ConState::GetName;
                }
            }
            ConState::GetOldPassword => {
                // C interpreter.c:1869-2020 CON_PASSWORD.
                let name = self.descriptor_name(conn_id);
                // Fetch the exact durable hash once: it authenticates this
                // attempt and becomes the session cache unless a legacy
                // upgrade commits. This avoids a second DB read and a fresh,
                // unnecessary Argon2 hash on every successful login.
                let stored_hash = if input.len() > crate::password::MAX_PASSWORD_INPUT_BYTES {
                    None
                } else {
                    match self.db_get_password_hash(&name).await {
                        Ok(Some(hash)) => Some(hash),
                        Ok(None) => None,
                        Err(error) => {
                            warn!("read password hash for {} failed: {}", name, error);
                            None
                        }
                    }
                };
                let ok = match stored_hash.as_ref() {
                    Some(hash) => {
                        self.await_database(crate::password::check_password_async(
                            hash.clone(),
                            input.clone(),
                        ))
                        .await
                    }
                    None => false,
                };
                if !ok {
                    // C 1897-1911: mudlog the attempt, bump GET_BAD_PWS (and
                    // persist it), re-prompt; disconnect at max_bad_pws (#194).
                    let host = self.descriptor_host(conn_id);
                    warn!("Bad PW: {} [{}]", name, host);
                    if let Ok(mut rec) = self.load_player_latest(&name).await {
                        rec.bad_pws = rec.bad_pws.saturating_add(1);
                        let _ = self.db_save_player(&rec).await;
                    }
                    let tries = {
                        let d = self
                            .state
                            .descriptors
                            .get_mut(&conn_id)
                            .expect("descriptor present in its own state arm");
                        d.bad_pws += 1;
                        d.bad_pws
                    };
                    if tries >= crate::config::MAX_BAD_PWS as u32 {
                        self.out(conn_id, "Wrong password... disconnecting.\r\n");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                    } else {
                        self.out(conn_id, "Wrong password.\r\nPassword: ");
                        // Stay in GetOldPassword; echo stays off.
                    }
                    return;
                }

                // Password was correct.
                let host = self.descriptor_host(conn_id);
                let mut rec = match self.load_player_latest(&name).await {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("load player {} failed: {}", name, e);
                        self.out(conn_id, "Error loading your character.\r\n");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        return;
                    }
                };
                let load_result = rec.bad_pws;
                if load_result > 0 {
                    rec.bad_pws = 0;
                    let _ = self.db_save_player(&rec).await;
                }

                // C 1914-1952: automatic upgrade of legacy password hashes
                // (#219), narrowed to the credential column. An upgrade write
                // failure is audited but never blocks a password that already
                // verified; the old durable hash remains the session truth.
                let mut session_hash = stored_hash.expect("successful password check had a hash");
                if crate::password::password_needs_upgrade(&session_hash) {
                    info!("Upgrading password security for {}", name);
                    if let Some(upgraded_hash) = self
                        .await_database(crate::password::hash_password_async(input.clone()))
                        .await
                    {
                        let upgrade_result = match self
                            .db_update_password_hash(
                                rec.idnum,
                                &name,
                                Some(&session_hash),
                                &upgraded_hash,
                            )
                            .await
                        {
                            Err(error) => {
                                self.resolve_password_update_error(&name, &upgraded_hash, error)
                                    .await
                            }
                            result => result,
                        };
                        match upgrade_result {
                            Ok(crate::PasswordHashUpdateOutcome::Updated) => {
                                session_hash = upgraded_hash
                            }
                            Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch) => warn!(
                                "AUDIT: legacy password upgrade for {} was rejected because its durable identity changed; login continues with the prior hash",
                                name
                            ),
                            Ok(crate::PasswordHashUpdateOutcome::CurrentHashMismatch) => {
                                warn!(
                                    "AUDIT: legacy password upgrade for {} lost a credential compare-and-swap race; the concurrent password is preserved and this authenticated login continues",
                                    name
                                );
                                // Keep unlock verification aligned with the
                                // credential that won the race. A read failure is
                                // non-fatal: this login already authenticated
                                // against the previously observed durable hash.
                                match self.db_get_password_hash(&name).await {
                                    Ok(Some(current_hash)) => session_hash = current_hash,
                                    Ok(None) => warn!(
                                        "AUDIT: credential readback for {} disappeared after a legacy-upgrade CAS miss",
                                        name
                                    ),
                                    Err(error) => warn!(
                                        "AUDIT: credential readback for {} failed after a legacy-upgrade CAS miss: {}",
                                        name, error
                                    ),
                                }
                            }
                            Err(error) => warn!(
                                "AUDIT: legacy password upgrade for {} has an indeterminate durable outcome: {}; authenticated login continues",
                                name, error
                            ),
                        }
                    } else {
                        warn!(
                            "AUDIT: legacy password upgrade for {} could not start its bounded hashing worker; login continues with the prior hash",
                            name
                        );
                    }
                }

                // Cache the exact durable session hash so `unlock <password>`
                // (act.other.c do_lockout) verifies the real account password.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.password_hash = Some(session_hash);
                }

                // Persisted trust, not the cosmetic/display level, controls
                // every login-time staff exception and staff-only disclosure.
                // Corrupt authority fails closed before the account enters a
                // world/menu state.
                let Some(account_trust) = persisted_player_trust(&rec) else {
                    error!(
                        "AUDIT: login for {} denied because persisted trust {} is outside 0..={}",
                        name, rec.trust, LVL_IMPL
                    );
                    self.out(
                        conn_id,
                        "Your account authority record is invalid. Please contact an administrator.\r\n",
                    );
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                };

                // C 1957-1967: BAN_SELECT without PLR_SITEOK.
                let banned = self.descriptor_ban_type(conn_id);
                if banned >= crate::ban::BanType::Select && rec.act_flags & PLR_SITEOK == 0 {
                    self.out(
                        conn_id,
                        "Sorry, this char has not been cleared for login from your site!\r\n",
                    );
                    warn!("Connection attempt for {} denied from {}", name, host);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }

                // C 1968-1979: multiplay gate (comm.c check_multiplaying;
                // the C build returns 1 immediately — dev-mode bypass kept).
                if !crate::cmd_comm::check_multiplaying(&self.state, &host)
                    && account_trust < i32::from(LVL_IMMORT)
                    && rec.act_flags & crate::flags::PLR_MULTIOK == 0
                {
                    self.out(
                        conn_id,
                        "\r\nSorry, there is already more then one connection to the MUD from your host.\r\n\
If you are playing from a shared connection please e-mail help@deltamud.net\r\n\
for access.\r\n\r\n",
                    );
                    warn!(
                        "Connection attempt for {} denied from {} - multi-play",
                        name, host
                    );
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }
                // C 1980-1989: wizlock (#202).
                let restrict = crate::cmd_wizard::circle_restrict();
                if restrict > 0 && account_trust < restrict {
                    self.out(
                        conn_id,
                        "The game is temporarily restricted.. try again later.\r\n",
                    );
                    warn!("Request for login denied for {} [{}] (wizlock)", name, host);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                    return;
                }
                // C 1990: perform_dupe_check — on dupe, take over the live
                // body and go straight to Playing (no MOTD) (#218).
                if self.perform_dupe_check(conn_id, rec.idnum).await {
                    return;
                }

                // C 1991-2019: motd/imotd, "has connected" mudlog, the
                // bad-pw notice, do_time, and PRESS RETURN -> CON_RMOTD.
                self.pending_load.insert(conn_id, rec.clone());
                let motd = if account_trust >= i32::from(LVL_IMMORT) {
                    self.state.imotd.clone()
                } else {
                    self.state.motd.clone()
                };
                self.out(conn_id, &motd);
                self.user_cntr(conn_id);
                info!("{} [{}] has connected.", name, host);
                if load_result > 0 {
                    self.out(
                        conn_id,
                        &format!(
                            "\r\n\r\n\x07\x07\x07{} LOGIN FAILURE{} SINCE LAST SUCCESSFUL LOGIN.\r\n",
                            load_result,
                            if load_result > 1 { "S" } else { "" }
                        ),
                    );
                }
                self.out(conn_id, "\r\n");
                {
                    // C runs do_time for the (still-unplaced) character.
                    let stub = self.login_stub(conn_id);
                    crate::cmd_informative::do_time(&mut self.state, stub, "", 0);
                    self.state.extract_char(stub);
                }
                self.out(conn_id, "\r\n\n*** PRESS RETURN: ");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            ConState::GetNewPassword => {
                // C interpreter.c:2043-2045: empty, >64, <3, or equal to the
                // name are all 'Illegal password.' with a 'Password: ' retry.
                if input.is_empty()
                    || input.len() > crate::password::MAX_PASSWORD_INPUT_BYTES
                    || input.len() < 3
                    || input.eq_ignore_ascii_case(&self.descriptor_name(conn_id))
                {
                    self.out(conn_id, "\r\nIllegal password.\r\nPassword: ");
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.suppress_prompt = true;
                    }
                    return;
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_password = Some(input);
                    d.state = ConState::ConfirmPassword;
                }
            }
            ConState::ConfirmPassword => {
                let matches = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .and_then(|d| d.temp_password.as_deref())
                    == Some(input.as_str());
                if matches {
                    let password = self
                        .state
                        .descriptors
                        .get_mut(&conn_id)
                        .and_then(|descriptor| descriptor.temp_password.take())
                        .expect("matching confirmation has a staged password");
                    let Some(password_hash) = self
                        .await_database(crate::password::hash_password_async(password))
                        .await
                    else {
                        self.out(
                            conn_id,
                            "\r\nPassword setup is temporarily unavailable; please try again.\r\nPassword: ",
                        );
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.temp_password = None;
                            d.state = ConState::GetNewPassword;
                            d.suppress_prompt = true;
                        }
                        return;
                    };
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetNewbie;
                        // Session password hash, for the `unlock` gate.
                        d.password_hash = Some(password_hash);
                    }
                } else {
                    // C interpreter.c:2057: '...start over.' + inline prompt.
                    self.out(
                        conn_id,
                        "\r\nPasswords don't match... start over.\r\nPassword: ",
                    );
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.temp_password = None;
                        d.state = ConState::GetNewPassword;
                        d.suppress_prompt = true;
                    }
                }
            }
            ConState::GetNewbie => {
                match input.chars().next().map(|c| c.to_ascii_lowercase()) {
                    Some('y') => self.pending.entry(conn_id).or_default().newbie = 1,
                    Some('n') => self.pending.entry(conn_id).or_default().newbie = 0,
                    _ => {
                        self.out(conn_id, "Please type Yes or No: ");
                        return;
                    }
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::GetSex;
                }
            }
            ConState::GetSex => {
                let sex = match input.to_lowercase().chars().next() {
                    Some('m') => Some(Gender::Male),
                    Some('f') => Some(Gender::Female),
                    _ => None,
                };
                match sex {
                    Some(s) => {
                        self.set_temp_sex(conn_id, s);
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::GetRace;
                        }
                    }
                    None => {
                        // C interpreter.c:2145: the retry carries its own
                        // 'What IS your sex? ' prompt.
                        self.out(conn_id, "That is not a sex..\r\nWhat IS your sex? ");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.suppress_prompt = true;
                        }
                    }
                }
            }
            ConState::GetRace => {
                if input
                    .get(..4)
                    .map(|s| s.eq_ignore_ascii_case("help"))
                    .unwrap_or(false)
                {
                    let race_letter = input.chars().nth(5).unwrap_or(' ');
                    let race = crate::races::parse_race(race_letter);
                    if race == crate::races::RACE_UNDEFINED {
                        self.out(conn_id, "\r\nThat's not a race.\r\n");
                    } else {
                        let avg = |stat| {
                            (crate::races::get_race_min(race, stat)
                                + crate::races::get_race_max(race, stat))
                                / 2
                        };
                        self.out(
                            conn_id,
                            &format!(
                                "\r\nAt 11 as the universal statistic average, your race averages the following abilities:\r\n\
Str: {:2} Int: {:2} Wis: {:2} Dex: {:2} Con: {:2} Cha: {:2}\r\n",
                                avg(1),
                                avg(2),
                                avg(3),
                                avg(4),
                                avg(5),
                                avg(6)
                            ),
                        );
                    }
                    return;
                }

                let parsed = input
                    .chars()
                    .next()
                    .map(crate::races::parse_race)
                    .unwrap_or(crate::races::RACE_UNDEFINED);
                if parsed == crate::races::RACE_UNDEFINED {
                    self.out(conn_id, "\r\nThat's not a race.\r\n");
                } else {
                    self.set_temp_race(conn_id, Race::from_u8(parsed as u8), parsed);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetDeity;
                    }
                }
            }
            ConState::GetDeity => {
                let parsed = input
                    .chars()
                    .next()
                    .map(crate::deity::parse_deity)
                    .unwrap_or(crate::deity::DEITY_UNDEFINED);
                if parsed == crate::deity::DEITY_UNDEFINED {
                    self.out(conn_id, "\r\nThat's not a deity.\r\n");
                } else {
                    self.pending.entry(conn_id).or_default().deity = parsed as u8;
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::GetClass;
                    }
                }
            }
            ConState::GetClass => {
                let parsed = input
                    .chars()
                    .next()
                    .map(crate::class::parse_class)
                    .unwrap_or(crate::class::CLASS_UNDEFINED);
                if parsed == crate::class::CLASS_UNDEFINED {
                    self.out(conn_id, "\r\nThat's not a class.\r\n");
                } else {
                    self.set_temp_class(conn_id, Class::from_u8(parsed as u8));
                    let newbie = self.pending.get(&conn_id).map(|p| p.newbie).unwrap_or(1);
                    if newbie == 0 {
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::GetHometown;
                        }
                    } else {
                        self.pending.entry(conn_id).or_default().hometown = 1;
                        self.out(
                            conn_id,
                            "\r\nYour hometown has been set to the capital city of Anacreon.\r\n\r\n",
                        );
                        self.begin_stat_roll(conn_id, true);
                    }
                }
            }
            ConState::GetHometown => {
                let parsed = input
                    .chars()
                    .next()
                    .map(crate::class::parse_town)
                    .unwrap_or(-1);
                if parsed < 0 {
                    self.out(conn_id, "\r\nThat's not a town.\r\n");
                } else {
                    self.pending.entry(conn_id).or_default().hometown = parsed as RoomVnum;
                    self.begin_stat_roll(conn_id, false);
                }
            }
            ConState::RollStats => match input.chars().next().map(|c| c.to_ascii_lowercase()) {
                Some('y') => {
                    self.create_and_enter(conn_id).await;
                }
                _ => self.begin_stat_roll(conn_id, false),
            },
            ConState::ReadMotd => {
                // C interpreter.c:2243-2246 CON_RMOTD: any input -> MENU (#198).
                self.out(conn_id, MENU);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Menu;
                }
            }
            ConState::Menu => self.menu_choice(conn_id, &input).await,
            ConState::ExDesc => {
                // The string editor owns this input (modify::editing is checked
                // before the nanny); if we ever get here the editor is gone —
                // return to the menu like C's fall-through.
                self.out(conn_id, MENU);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Menu;
                }
            }
            ConState::ChPwdGetOld => {
                // C interpreter.c:2348-2364.
                let name = self.descriptor_name(conn_id);
                let stored_hash = if input.len() > crate::password::MAX_PASSWORD_INPUT_BYTES {
                    Ok(None)
                } else {
                    self.db_get_password_hash(&name).await
                };
                let authenticated_hash = match stored_hash {
                    Ok(Some(hash)) => {
                        let matches = self
                            .await_database(crate::password::check_password_async(
                                hash.clone(),
                                input.clone(),
                            ))
                            .await;
                        matches.then_some(hash)
                    }
                    Ok(None) => None,
                    Err(error) => {
                        warn!(
                            "load password-change credential for {} failed: {}",
                            name, error
                        );
                        self.out(
                            conn_id,
                            "\r\nPassword verification is temporarily unavailable; please try again.\r\n",
                        );
                        self.out(conn_id, MENU);
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.password_change_expected_hash = None;
                            d.state = ConState::Menu;
                        }
                        return;
                    }
                };
                if let Some(expected_hash) = authenticated_hash {
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.password_change_expected_hash = Some(expected_hash);
                        d.state = ConState::ChPwdGetNew;
                    }
                } else {
                    self.out(conn_id, "\r\nIncorrect password.\r\n");
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.password_change_expected_hash = None;
                        d.state = ConState::Menu;
                    }
                }
            }
            ConState::ChPwdGetNew => {
                // C interpreter.c:2022-2039 CON_NEWPASSWD (shared).
                if input.is_empty()
                    || input.len() > crate::password::MAX_PASSWORD_INPUT_BYTES
                    || input.len() < 3
                    || input.eq_ignore_ascii_case(&self.descriptor_name(conn_id))
                {
                    self.out(conn_id, "\r\nIllegal password.\r\nPassword: ");
                    return;
                }
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_password = Some(input);
                    d.state = ConState::ChPwdVerify;
                }
            }
            ConState::ChPwdVerify => {
                // C interpreter.c:2041-2068 CON_CHPWD_VRFY: persist before
                // publishing success, but update only the credential column.
                let matches = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .and_then(|d| d.temp_password.clone())
                    .map(|p| p == input)
                    .unwrap_or(false);
                if !matches {
                    self.out(
                        conn_id,
                        "\r\nPasswords don't match... start over.\r\nPassword: ",
                    );
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::ChPwdGetNew;
                    }
                    return;
                }
                let name = self.descriptor_name(conn_id);
                let identity = self
                    .pending_load
                    .get(&conn_id)
                    .filter(|character| character.get_name().eq_ignore_ascii_case(&name))
                    .map(|character| character.idnum);
                let idnum = match identity {
                    Some(idnum) => idnum,
                    None => match self.load_player_latest(&name).await {
                        Ok(character) => character.idnum,
                        Err(error) => {
                            warn!(
                                "load password-change identity for {} failed: {}",
                                name, error
                            );
                            self.out(
                                conn_id,
                                "\r\nPassword change failed; your old password is unchanged.\r\n",
                            );
                            self.out(conn_id, MENU);
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.temp_password = None;
                                d.password_change_expected_hash = None;
                                d.state = ConState::Menu;
                            }
                            return;
                        }
                    },
                };
                let Some(password_hash) = self
                    .await_database(crate::password::hash_password_async(input.clone()))
                    .await
                else {
                    self.out(
                        conn_id,
                        "\r\nPassword change is temporarily unavailable; your old password is unchanged.\r\n",
                    );
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.temp_password = None;
                        d.password_change_expected_hash = None;
                        d.state = ConState::Menu;
                    }
                    return;
                };
                let expected_hash = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .and_then(|descriptor| descriptor.password_change_expected_hash.clone());
                let Some(expected_hash) = expected_hash else {
                    self.out(
                        conn_id,
                        "\r\nPassword change authorization expired. Reconnect and authenticate again.\r\n",
                    );
                    self.out(conn_id, MENU);
                    if let Some(descriptor) = self.state.descriptors.get_mut(&conn_id) {
                        descriptor.temp_password = None;
                        descriptor.state = ConState::Menu;
                    }
                    return;
                };
                let durable = match self
                    .db_update_password_hash(idnum, &name, Some(&expected_hash), &password_hash)
                    .await
                {
                    Err(error) => {
                        self.resolve_password_update_error(&name, &password_hash, error)
                            .await
                    }
                    result => result,
                };
                match durable {
                    Ok(crate::PasswordHashUpdateOutcome::Updated) => {
                        self.out(conn_id, "\r\nDone.\n\r");
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.password_hash = Some(password_hash);
                        }
                    }
                    Ok(crate::PasswordHashUpdateOutcome::IdentityMismatch) => {
                        warn!(
                            "AUDIT: password change for {} was rejected because its durable identity changed",
                            name
                        );
                        self.out(
                            conn_id,
                            "\r\nPassword change failed; your old password is unchanged.\r\n",
                        );
                    }
                    Ok(crate::PasswordHashUpdateOutcome::CurrentHashMismatch) => {
                        warn!(
                            "AUDIT: password change for {} lost its credential CAS; a concurrent reset won",
                            name
                        );
                        self.out(
                            conn_id,
                            "\r\nYour account password changed during this operation. The requested password was not installed; reconnect and authenticate again.\r\n",
                        );
                    }
                    Err(error) => {
                        warn!(
                            "AUDIT: password change for {} has an indeterminate durable outcome: {}",
                            name, error
                        );
                        self.out(
                            conn_id,
                            "\r\nPassword change could not be confirmed. Reconnect and try the new password, then the old password.\r\n",
                        );
                    }
                }
                self.out(conn_id, MENU);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.temp_password = None;
                    d.password_change_expected_hash = None;
                    d.state = ConState::Menu;
                }
            }
            ConState::DelCnf1 => {
                // C interpreter.c:2366-2387 CON_DELCNF1.
                let name = self.descriptor_name(conn_id);
                let ok = if input.len() > crate::password::MAX_PASSWORD_INPUT_BYTES {
                    false
                } else {
                    self.db_verify_password(&name, &input)
                        .await
                        .unwrap_or(false)
                };
                if ok {
                    self.out(
                        conn_id,
                        "\r\nYOU ARE ABOUT TO DELETE THIS CHARACTER PERMANENTLY.\r\n\
ARE YOU ABSOLUTELY SURE?\r\n\r\nPlease type \"yes\" to confirm: ",
                    );
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::DelCnf2;
                    }
                } else {
                    self.out(conn_id, "\r\nIncorrect password.\r\n");
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Menu;
                    }
                }
            }
            ConState::DelCnf2 => {
                // C interpreter.c:2389-2430 CON_DELCNF2.
                if input == "yes" || input == "YES" {
                    let name = self.descriptor_name(conn_id);
                    if let Ok(mut rec) = self.load_player_latest(&name).await {
                        if rec.act_flags & crate::flags::PLR_FROZEN != 0 {
                            self.out(
                                conn_id,
                                "You try to kill yourself, but the ice stops you.\r\nCharacter not deleted.\r\n\r\n",
                            );
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.state = ConState::Close;
                            }
                            return;
                        }
                        let Some(account_trust) = persisted_player_trust(&rec) else {
                            error!(
                                "AUDIT: self-delete for {} denied because persisted trust {} is invalid",
                                name, rec.trust
                            );
                            self.out(
                                conn_id,
                                "Character not deleted because the account authority record is invalid. Please contact an administrator.\r\n",
                            );
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.state = ConState::Close;
                            }
                            return;
                        };
                        if account_trust >= i32::from(LVL_GRGOD) {
                            warn!(
                                "AUDIT: protected staff account {} refused self-deletion at trust {}",
                                name, account_trust
                            );
                            self.out(
                                conn_id,
                                "Privileged characters cannot self-delete. Character not deleted.\r\n",
                            );
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.state = ConState::Close;
                            }
                            return;
                        }
                        rec.act_flags |= crate::flags::PLR_DELETED;
                        let level = rec.player.level;
                        if let Err(error) = self.db_save_player(&rec).await {
                            error!(
                                "self-delete for {} failed before sidecar cleanup: {}",
                                name, error
                            );
                            self.out(
                                conn_id,
                                "Character deletion failed; no files were removed. Please contact an administrator.\r\n",
                            );
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.state = ConState::Close;
                            }
                            return;
                        }

                        // Policy: the durable DB tombstone is authoritative.
                        // Missing sidecars are success; any other cleanup error
                        // is explicitly surfaced and audited instead of falsely
                        // claiming that deletion completed.
                        let lib_path = self.state.config.lib_path.clone();
                        if let Err(cleanup_error) = crate::player_sidecars::delete_player_sidecars(
                            &mut self.state,
                            &lib_path,
                            &name,
                            rec.idnum,
                        ) {
                            error!(
                                "AUDIT: {} (lev {}) was DB-tombstoned but sidecar cleanup is incomplete: {}",
                                name, level, cleanup_error
                            );
                            self.out(
                                conn_id,
                                "Character marked deleted, but file cleanup is incomplete. Administrators have been notified.\r\nGoodbye.\r\n",
                            );
                            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                                d.state = ConState::Close;
                            }
                            return;
                        }
                        self.out(
                            conn_id,
                            &format!("Character '{}' deleted!\r\nGoodbye.\r\n", name),
                        );
                        info!("{} (lev {}) has self-deleted.", name, level);
                    }
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Close;
                    }
                } else {
                    self.out(
                        conn_id,
                        "\r\nThat was not \"yes\". Character not deleted.\r\n",
                    );
                    self.out(conn_id, MENU);
                    if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                        d.state = ConState::Menu;
                    }
                }
            }
            _ => {}
        }

        // If this input was a password entry and we have now transitioned OUT of
        // the password flow (login success -> Playing, login fail -> Close, or
        // new-password confirmed -> GetNewbie), tell the client the server WONT echo
        // so normal local echo resumes. Staying within the password flow
        // (GetNewPassword -> ConfirmPassword, or a retry) keeps echo suppressed.
        if is_password_state(state) {
            let new_state = self.state.descriptors.get(&conn_id).map(|d| d.state);
            let still_password = new_state.map(is_password_state).unwrap_or(false);
            if !still_password {
                self.send_raw_bytes(conn_id, &IAC_WONT_ECHO);
            }
        }
    }

    // Pending creation choices are held between C nanny states until stat
    // acceptance finalizes the new player.
    pub(crate) fn set_temp_sex(&mut self, conn_id: ConnId, s: Gender) {
        self.pending.entry(conn_id).or_default().sex = s;
    }
    pub(crate) fn set_temp_class(&mut self, conn_id: ConnId, c: Class) {
        self.pending.entry(conn_id).or_default().class = c;
    }
    pub(crate) fn set_temp_race(&mut self, conn_id: ConnId, r: Race, race_index: i32) {
        let pending = self.pending.entry(conn_id).or_default();
        pending.race = r;
        pending.race_index = race_index;
    }

    pub(crate) fn begin_stat_roll(&mut self, conn_id: ConnId, explain: bool) {
        let (class, race_index) = {
            let pending = self.pending.entry(conn_id).or_default();
            (pending.class, pending.race_index)
        };
        let rolled = crate::class::roll_abilities_for(&mut self.state, class, race_index);
        self.pending.entry(conn_id).or_default().rolled = rolled;
        if explain {
            self.out(conn_id, NEWBIE_STAT_EXPLANATION);
        }
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.state = ConState::RollStats;
        }
    }

    pub(crate) fn descriptor_host(&self, conn_id: ConnId) -> String {
        self.state
            .descriptors
            .get(&conn_id)
            .map(|d| d.host.clone())
            .unwrap_or_default()
    }

    pub(crate) fn descriptor_ban_type(&self, conn_id: ConnId) -> crate::ban::BanType {
        let Some(descriptor) = self.state.descriptors.get(&conn_id) else {
            return crate::ban::BanType::None;
        };
        crate::ban::isbanned_connection(
            &self.state,
            &descriptor.peer_ip,
            descriptor.verified_hostname.as_deref(),
        )
    }

    pub(crate) async fn create_and_enter(&mut self, conn_id: ConnId) {
        let (name, password_hash) = {
            let d = match self.state.descriptors.get(&conn_id) {
                Some(d) => d,
                None => return,
            };
            (
                d.temp_name.clone().unwrap_or_default(),
                d.password_hash.clone().unwrap_or_default(),
            )
        };
        let choices = self.pending.remove(&conn_id).unwrap_or_default();
        let mut ch =
            crate::character::Character::new_player(name.clone(), choices.class, choices.race);
        ch.player.sex = choices.sex;
        ch.player.deity = choices.deity;
        ch.player.hometown = choices.hometown;
        ch.newbie = choices.newbie;
        ch.real_abils = if choices.rolled.str > 0 {
            choices.rolled
        } else {
            crate::class::roll_abilities_for(&mut self.state, choices.class, choices.race_index)
        };
        ch.aff_abils = ch.real_abils;
        ch.clan = -1;
        ch.clan_rank = -1;
        ch.tloadroom = -1;
        ch.mapx = -1;
        ch.mapy = -1;
        ch.prf_flags |= crate::flags::PRF_NOLOOKSTACK
            | crate::flags::PRF_DISPHP
            | crate::flags::PRF_DISPMANA
            | crate::flags::PRF_DISPMOVE
            | crate::flags::PRF_DISPEXP;
        ch.prf2_flags |= crate::flags::PRF2_DISPMOB;

        let temp_id = self.state.create_char(ch);
        crate::class::do_start_init(&mut self.state, temp_id);
        let mut ch = match self.state.get_char(temp_id).cloned() {
            Some(ch) => ch,
            None => {
                self.out(conn_id, "Couldn't create your character. Try later.\r\n");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Close;
                }
                return;
            }
        };
        self.state.extract_char(temp_id);

        match self
            .db_create_player_with_password_hash(&ch, &password_hash)
            .await
        {
            Ok(idnum) => {
                // The in-memory char must take the identity allocated by the
                // collision-safe creation transaction before any targeted
                // generic save can match the durable row.
                ch.idnum = idnum;
                let host = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .map(|d| d.host.clone())
                    .unwrap_or_default();
                if let Err(e) = self.db_save_player_with_host(&ch, &host).await {
                    warn!("save new player {} failed: {}", name, e);
                }
                crate::alias::clear_aliases(&mut self.state, ch.idnum);
                // Register the new player in the in-memory index immediately (C
                // create_entry appends to player_table) so name<->idnum lookups
                // — ignore-by-name, mail, `last` — resolve them at once, before
                // they ever log in elsewhere. enter_game refreshes last_logon.
                let host = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .map(|d| d.host.clone())
                    .unwrap_or_default();
                self.state.update_player_index_from_character(
                    &ch,
                    ch.last_logon.timestamp(),
                    &host,
                );
                crate::mail::mail_register_player(&mut self.state, ch.idnum, &name);
                // C interpreter.c start_player (1637-1653): the new character
                // gets the MOTD + PRESS RETURN and lands at the MENU; the
                // actual world-enter happens at menu option 1.
                self.just_created.insert(conn_id);
                self.pending_load.insert(conn_id, ch);
                let motd = self.state.motd.clone();
                self.out(conn_id, &motd);
                self.out(conn_id, "\r\n\n*** PRESS RETURN: ");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Err(e) => {
                warn!("create player {} failed: {}", name, e);
                self.out(conn_id, "Couldn't create your character. Try later.\r\n");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Close;
                }
            }
        }
    }

    /// C act.informative.c:2934 user_cntr: bump the raw binary USRCNT logon
    /// counter (8-byte long, beside lib/ as in C's cwd) and tell the player
    /// their ordinal (#347).
    pub(crate) fn user_cntr(&mut self, conn_id: ConnId) {
        // C resolves "USRCNT" against the server cwd, which is always the
        // directory containing lib/. Prefer the configured lib's parent.
        let lib = if !self.state.config.lib_path.is_empty() && self.state.config.lib_path != "./lib"
        {
            self.state.config.lib_path.clone()
        } else {
            self.state.config.lib_path.clone()
        };
        let path = std::path::Path::new(&lib)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.join("USRCNT"))
            .unwrap_or_else(|| std::path::PathBuf::from("USRCNT"));
        let mut count: i64 = std::fs::read(&path)
            .ok()
            .and_then(|bytes| {
                if bytes.len() >= 8 {
                    Some(i64::from_le_bytes(bytes[..8].try_into().unwrap()))
                } else {
                    None
                }
            })
            .unwrap_or(0);
        count += 1;
        if std::fs::write(&path, count.to_le_bytes()).is_ok() {
            self.out(
                conn_id,
                &format!(
                    "\r\n  You are player #{} to logon since April 13, 1998\r\n",
                    count
                ),
            );
        }
    }

    /// C interpreter.c:2254-2360 CON_MENU: the DeltaMUD main menu (#198).
    pub(crate) async fn menu_choice(&mut self, conn_id: ConnId, input: &str) {
        match input.chars().next() {
            Some('0') => {
                self.out(
                    conn_id,
                    "\r\nYou awaken, and find yourself in a land called reality.\r\nWe hope you come back to Deltania soon!\r\n\r\n",
                );
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::Close;
                }
            }
            Some('1') => {
                self.enter_game(conn_id, false).await;
            }
            Some('2') => {
                // C 2287-2307 CON_EXDESC: the string editor writes the new
                // description; it is applied to the player at enter-game.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    if d.temp_description.is_some() {
                        d.write("Current description:\r\n");
                        d.write(&d.temp_description.clone().unwrap_or_default());
                        d.write("\r\n");
                    }
                    d.write(
                        "Enter the new text you'd like others to see when they look at you.\r\n(/s saves /h for help)\r\n",
                    );
                }
                crate::modify::start_login_description_editing(&mut self.state, conn_id, 8192);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ExDesc;
                }
            }
            Some('3') => {
                let background = self.state.background.clone();
                crate::modify::page_string(&mut self.state, conn_id, &background);
                // C sets CON_RMOTD: when paging (or RETURN) ends, the next
                // input re-shows the menu.
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('4') => {
                let news = self.state.news.clone();
                crate::modify::page_string(&mut self.state, conn_id, &news);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('5') => {
                let policies = self.state.policies.clone();
                crate::modify::page_string(&mut self.state, conn_id, &policies);
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('6') => {
                // C 2339-2344: run do_who against a transient stand-in
                // character (not registered in players_by_name, so it does not
                // list itself), then back to the menu via PRESS RETURN.
                let stub = self.login_stub(conn_id);
                crate::cmd_informative::do_who(&mut self.state, stub, "", 0);
                self.state.extract_char(stub);
                self.out(conn_id, "\r\n\n*** PRESS RETURN: ");
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ReadMotd;
                }
            }
            Some('7') => {
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::ChPwdGetOld;
                }
            }
            Some('8') => {
                if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                    d.state = ConState::DelCnf1;
                }
            }
            _ => {
                self.out(conn_id, "\r\nThat's not a menu choice!\r\n");
                self.out(conn_id, MENU);
            }
        }
    }

    /// A transient stand-in Character for pre-login menu commands (who /
    /// do_time). Carries the login name + loaded record's level so
    /// CAN_SEE/level checks behave; never placed in a room; extracted by the
    /// caller. Extracting requires the id NOT to be in players_by_name.
    pub(crate) fn login_stub(&mut self, conn_id: ConnId) -> CharId {
        let name = self.descriptor_name(conn_id);
        let rec = self.pending_load.get(&conn_id).cloned();
        let mut ch = crate::character::Character::new_player(
            name.into(),
            crate::types::Class::Warrior,
            crate::types::Race::Human,
        );
        if let Some(rec) = &rec {
            ch.player.level = rec.player.level;
            ch.prf_flags = rec.prf_flags;
        }
        // Route the stub's output to the logging-in connection (C runs these
        // commands on d->character, which IS attached to the descriptor).
        ch.desc = Some(conn_id);
        self.state.create_char(ch)
    }

    /// C interpreter.c:1418-1530 perform_dupe_check: disconnect other
    /// descriptors controlling the same idnum and adopt the live body
    /// (#218). Returns true when THIS connection should go straight to
    /// Playing (dupe handled).
    pub(crate) async fn perform_dupe_check(&mut self, conn_id: ConnId, idnum: i64) -> bool {
        // --- Pre-enter_game window (issue #396): a descriptor parked at the
        // MOTD/menu holds its loaded Character in `pending_load` with
        // character == None, so the body match below could not see it -- two
        // logins of one account then both pressed 1 and created two playing
        // bodies (and crash_load duplicated every rented item). Disconnect
        // any OTHER pre-menu connection carrying the same idnum.
        let stale_prelogin: Vec<ConnId> = self
            .pending_load
            .iter()
            .filter(|(c, rec)| **c != conn_id && rec.idnum == idnum)
            .map(|(&c, _)| c)
            .collect();
        for stale in stale_prelogin {
            self.pending_load.remove(&stale);
            if let Some(d) = self.state.descriptors.get_mut(&stale) {
                d.write("\r\nYour body was taken over by a newer login.\r\n");
                d.state = ConState::Close;
            }
        }

        // C also sweeps the character list after descriptors. A linkless body
        // can survive a dropped socket or an interrupted save; creating a new
        // body beside it duplicates every crash-loaded object. Include both
        // descriptor roles (`character` and a switched immortal's `original`)
        // and every descriptor-less live PC when selecting one canonical body.
        let live_bodies: Vec<CharId> = self
            .state
            .chars
            .iter()
            .filter_map(|(&cid, ch)| (!ch.is_npc && ch.idnum == idnum).then_some(cid))
            .collect();
        if live_bodies.is_empty() {
            return false;
        }

        let registered = self
            .state
            .players_by_name
            .values()
            .copied()
            .find(|cid| live_bodies.contains(cid));
        let descriptor_body = self.state.descriptors.iter().find_map(|(&old_conn, d)| {
            if old_conn == conn_id || d.state != ConState::Playing {
                return None;
            }
            d.original
                .into_iter()
                .chain(d.character)
                .find(|cid| live_bodies.contains(cid))
        });
        let body = registered.or(descriptor_body).unwrap_or(live_bodies[0]);

        let dupes: Vec<(ConnId, Option<CharId>, Option<CharId>, bool)> = self
            .state
            .descriptors
            .iter()
            .filter(|&(old_conn, d)| {
                *old_conn != conn_id
                    && d.character
                        .into_iter()
                        .chain(d.original)
                        .any(|cid| live_bodies.contains(&cid))
            })
            .map(|(&old_conn, d)| {
                (
                    old_conn,
                    d.character,
                    d.original,
                    d.state == ConState::Playing,
                )
            })
            .collect();
        let mut announced_usurp = false;
        for (old_conn, controlled, original, was_playing) in dupes {
            if was_playing && !announced_usurp {
                // USURP: the old socket is told its body was taken.
                self.out(old_conn, "\r\nThis body has been usurped!\r\n");
                announced_usurp = true;
            }
            self.out(
                old_conn,
                "\r\nMultiple login detected -- disconnecting.\r\n",
            );
            if let Some(d) = self.state.descriptors.get_mut(&old_conn) {
                // Detach WITHOUT the save/extract disconnect path: the body
                // lives on under this connection (C: k->character = NULL).
                d.character = None;
                d.original = None;
                d.state = ConState::Close;
            }
            for detached in controlled.into_iter().chain(original) {
                if let Some(ch) = self.state.get_char_mut(detached) {
                    if ch.desc == Some(old_conn) {
                        ch.desc = None;
                    }
                }
            }
            // C 1521-1533: USURP room line + messages to the taker.
            if was_playing {
                crate::act::act(
                    &mut self.state,
                    "$n suddenly keels over in pain, surrounded by a white aura...\r\n$n's body has been taken over by a new spirit!",
                    true,
                    body,
                    None,
                    crate::act::ActArg::None,
                    crate::act::To::Room,
                );
                self.out(conn_id, "You take over your own body, already in use!\r\n");
            } else {
                self.out(conn_id, "Reconnecting.\r\n");
            }
            info!(
                "{} has re-logged in ... disconnecting old socket.",
                self.descriptor_name(conn_id)
            );
        }

        // If a prior failure already left two bodies, retain the canonical
        // registered/connected one and destroy the duplicate body's copied
        // inventory before extraction. `extract_char` normally drops gear in
        // the room, which would preserve the duplication we are repairing.
        for duplicate in live_bodies.into_iter().filter(|&cid| cid != body) {
            let copied_objects: Vec<ObjId> = self
                .state
                .get_char(duplicate)
                .map(|ch| {
                    ch.carrying
                        .iter()
                        .copied()
                        .chain(ch.equipment.iter().flatten().copied())
                        .collect()
                })
                .unwrap_or_default();
            if !self.state.extract_objs(copied_objects) {
                warn!(
                    "refused to remove duplicate live body {:?} for persistent player id {} because its object graph is malformed",
                    duplicate, idnum
                );
                continue;
            }
            self.state.extract_char(duplicate);
            warn!(
                "removed duplicate live body {:?} for persistent player id {}",
                duplicate, idnum
            );
        }

        // Re-attach this descriptor to the existing entity.
        if let Some(c) = self.state.get_char_mut(body) {
            c.desc = Some(conn_id);
        }
        self.state
            .players_by_name
            .insert(self.descriptor_name(conn_id).to_lowercase(), body);
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.character = Some(body);
            d.state = ConState::Playing;
        }
        self.write_prompt(conn_id);
        true
    }

    /// Load (or, for fresh chars, re-load) the player, place them in the
    /// world, and start play.
    pub(crate) async fn enter_game(&mut self, conn_id: ConnId, _is_new: bool) {
        // C interpreter.c enter_player_game. The record was usually already
        // loaded at password-verify (pending_load) — consume it so login hits
        // the DB once.
        let name = self.descriptor_name(conn_id);
        let mut ch = match self.pending_load.remove(&conn_id) {
            Some(c) if c.get_name().eq_ignore_ascii_case(&name) => c,
            _ => match self.load_player_latest(&name).await {
                Ok(c) => c,
                Err(e) => {
                    warn!("load player {} failed: {}", name, e);
                    self.out(conn_id, "Error loading your character.\r\n");
                    return;
                }
            },
        };
        // Re-run the same-id gate immediately before create_char. The password
        // gate normally makes two pending menu sessions impossible, but this is
        // the final invariant boundary: even a pre-existing/raced pending login
        // or descriptor-less body is closed/adopted instead of materializing a
        // second body and loading the same rent file twice (#396).
        if self.perform_dupe_check(conn_id, ch.idnum).await {
            self.just_created.remove(&conn_id);
            return;
        }
        // CON_QANSI answer and menu option 2's description land here, the way
        // C carries them on d->character (#198).
        if let Some(d) = self.state.descriptors.get(&conn_id) {
            if let Some(want) = d.wants_colour {
                if want {
                    ch.prf_flags |= crate::flags::PRF_COLOR_1 | crate::flags::PRF_COLOR_2;
                } else {
                    ch.prf_flags &= !(crate::flags::PRF_COLOR_1 | crate::flags::PRF_COLOR_2);
                }
            }
            if let Some(desc) = &d.temp_description {
                ch.player.description = desc.clone();
            }
        }
        let is_new_char = self.just_created.remove(&conn_id);
        let lib_path = self.state.config.lib_path.clone();
        if let Err(e) =
            crate::alias::read_aliases(&mut self.state, &lib_path, ch.get_name(), ch.idnum)
        {
            warn!("read_aliases(g, {}) failed: {}", ch.get_name(), e);
        }
        ch.desc = Some(conn_id);
        ch.aff_abils = ch.real_abils;
        // The player file/DB carries no object references (C semantics): the real
        // objects come entirely from the rent/crash file via crash_load below.
        // The mock DB clones the whole Character, so its carrying/equipment hold
        // stale ObjIds from the previous session — clear them or crash_load's
        // auto_equip sees the slots "occupied" and dumps worn items to inventory.
        ch.carrying.clear();
        ch.equipment = [None; NUM_WEARS];
        let id = self.state.create_char(ch);
        self.state.affect_total(id);
        self.state.players_by_name.insert(name.to_lowercase(), id);
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.character = Some(id);
            d.state = ConState::Playing;
        }

        // Refresh the index for this login: stamp last_logon to now and record
        // the connecting host (C sets GET_LAST_LOGON/host at enter), so a later
        // `last <name>` for this player shows their most recent session.
        let host = self
            .state
            .descriptors
            .get(&conn_id)
            .map(|d| d.host.clone())
            .unwrap_or_default();
        let now = chrono::Utc::now().timestamp();
        let index_snapshot = if let Some(c) = self.state.get_char_mut(id) {
            c.last_logon = chrono::Utc::now();
            Some(c.clone())
        } else {
            None
        };
        if let Some(c) = index_snapshot.as_ref() {
            self.state.update_player_index_from_character(c, now, &host);
        }

        // Room selection — interpreter.c enter_player_game (BUG #15). The C
        // precedence: GET_LOADROOM (saved.load_room) is honored first; a valid
        // saved.tloadroom (temporary, higher-priority — this is what do_copyover
        // stamps with the player's CURRENT room) overrides it and is then
        // cleared; valid surface-map coordinates (mapx/mapy) override both and
        // are cleared; finally, if nothing resolved, fall back to the normal
        // start room. Without this a copyover dumped everyone at the temple.
        //
        // PLR_* bits use the C structs.h values (the runtime act_flags column is
        // the raw C bitfield); defined locally to match enter_player_game.
        const PLR_FROZEN_C: i64 = 1 << 2;
        const PLR_KILLER_C: i64 = 1 << 0;
        // Snapshot the saved room fields + flags (clone scalars before any
        // mutation; house style).
        let (saved_load, saved_tload, saved_mapx, saved_mapy, newbie, level, act_flags, prf2_flags) =
            self.state
                .get_char(id)
                .map(|c| {
                    (
                        c.load_room,
                        c.tloadroom,
                        c.mapx,
                        c.mapy,
                        c.newbie,
                        c.player.level,
                        c.act_flags,
                        c.prf2_flags,
                    )
                })
                .unwrap_or((crate::types::NOWHERE, 0, -1, -1, 0, 1, 0, 0));

        // GET_LOADROOM: real_room(saved.load_room) if it's a real vnum.
        let mut load_rnum: Option<RoomRnum> = if saved_load != crate::types::NOWHERE {
            self.state.real_room(saved_load)
        } else {
            None
        };

        // tloadroom (temporary copyover loadroom): if it resolves to a real
        // room, it WINS over load_room, and C clears it (set to -1) so it is
        // one-shot. C only clears tloadroom when it WAS valid (the assignment is
        // inside the `if (real_room(tloadroom) != NOWHERE)` block).
        //
        // C's saved.tloadroom sentinel is -1 (NOWHERE), but this port defaults
        // the field to 0 and may persist 0, and room vnum 0 ("The Void") IS a
        // real loadable room — so without a >=1 guard a normal (non-copyover)
        // login with tloadroom==0 would teleport into the Void. do_copyover only
        // ever stamps a real, positive room vnum, so treat anything < 1 as unset.
        let tload_vnum = saved_tload as crate::types::RoomVnum;
        if saved_tload >= 1 {
            if let Some(rnum) = self.state.real_room(tload_vnum) {
                load_rnum = Some(rnum);
                if let Some(c) = self.state.get_char_mut(id) {
                    c.tloadroom = -1; // C: saved.tloadroom = -1; (one-shot)
                }
            }
        }

        // If the resolved load_room is an IMPL-only room (ROOM_IMPROOM) and the
        // player is below LVL_GRGOD, discard it so they fall through to the start
        // room (C interpreter.c enter_player_game 1579-1581).
        const ROOM_IMPROOM_C: u32 = 1 << 16;
        if let Some(rnum) = load_rnum {
            if level < crate::types::LVL_GRGOD
                && self.state.room(rnum).room_flags.bits() & ROOM_IMPROOM_C != 0
            {
                load_rnum = None;
            }
        }

        // newbie loadroom (C: newbie == 1 && level < 5 -> newbie_room).
        if newbie == 1 && level < 5 {
            if let Some(rnum) = self.state.real_room(self.state.config.newbie_room) {
                load_rnum = Some(rnum);
            }
        }

        // Surface-map coordinates override (C: find_room_by_coords of mapx/mapy
        // when 1 <= mapx <= max_map_x && 1 <= mapy <= max_map_y), then C clears
        // mapx/mapy back to -1 unconditionally.
        if saved_mapx >= 1
            && saved_mapx <= self.state.max_map_x as i64
            && saved_mapy >= 1
            && saved_mapy <= self.state.max_map_y as i64
        {
            if let Some(rnum) = self
                .state
                .map_coords_to_rnum(saved_mapx as i32, saved_mapy as i32)
            {
                load_rnum = Some(rnum);
            }
        }
        if let Some(c) = self.state.get_char_mut(id) {
            c.mapx = -1;
            c.mapy = -1;
        }

        // Fall back to the normal start room when nothing above resolved (C: if
        // load_room == NOWHERE -> immort/mortal start room). Preserve the
        // existing Rust fallback chain (vnum 100 / hometown / 3001 / first room).
        let home = self
            .state
            .get_char(id)
            .map(|c| c.player.hometown)
            .unwrap_or(100);
        if load_rnum.is_none() {
            let start_vnum = if level >= crate::types::LVL_IMMORT {
                crate::config::IMMORT_START_ROOM
            } else {
                100
            };
            load_rnum = self
                .state
                .real_room(start_vnum)
                .or_else(|| self.state.real_room(home))
                .or_else(|| self.state.real_room(3001))
                .or_else(|| (!self.state.rooms.is_empty()).then_some(0));
        }

        // Frozen, then killer (C applies them in this order AFTER the fallback,
        // so killer wins if a player is somehow both). Each only overrides when
        // the override room actually exists, else the prior choice stands.
        if act_flags & PLR_FROZEN_C != 0 {
            if let Some(r) = self.state.real_room(crate::config::FROZEN_START_ROOM) {
                load_rnum = Some(r);
            }
        }
        if act_flags & PLR_KILLER_C != 0 {
            if let Some(r) = self.state.real_room(self.state.config.jail_num) {
                load_rnum = Some(r);
            }
        }

        // A ghost (PRF2_INTANGIBLE) who is not actively map-building
        // (PRF2_MBUILDING) always enters at room 99. This is the LAST override in
        // enter_player_game, so it wins over frozen/killer (C 1616-1618).
        const PRF2_INTANGIBLE_C: i64 = 1 << 9;
        const PRF2_MBUILDING_C: i64 = 1 << 6;
        if prf2_flags & PRF2_INTANGIBLE_C != 0 && prf2_flags & PRF2_MBUILDING_C == 0 {
            if let Some(r) = self.state.real_room(99) {
                load_rnum = Some(r);
            }
        }

        if let Some(rnum) = load_rnum {
            self.state.char_to_room(id, rnum);
        }
        // Restore the player's rented/crash-saved objects (objsave.c).
        let lib_path = self.state.config.lib_path.clone();
        crate::objsave::crash_load(&mut self.state, id, &lib_path);

        // C interpreter.c menu '1' (2261-2268): WELC_MESSG, then for a fresh
        // character do_start + START_MESSG + do_newbie; then the first look.
        // do_start ran in create_and_enter (before the DB write); do_newbie —
        // the starter item (obj 190, "an unfinished player's guide"),
        // recall level and wimpy 1 — runs here, in the world, past crash_load
        // (issue #207).
        self.state.send_to_char(id, WELC_MESSG);
        if is_new_char {
            self.state.send_to_char(id, START_MESSG);
            crate::class::do_newbie(&mut self.state, id);
        }
        crate::cmd_informative::look_at_room(&mut self.state, id, true);
        // C 2271-2272: "You have mail waiting."
        let idnum = self.state.get_char(id).map(|c| c.idnum).unwrap_or(0);
        if crate::mail::has_mail(&self.state, idnum) {
            self.state.send_to_char(id, "You have mail waiting.\r\n");
        }
        let rnum = self.state.get_char(id).and_then(|c| c.in_room);
        if let Some(rnum) = rnum {
            crate::act::act(
                &mut self.state,
                "$n has entered the game.",
                true,
                id,
                None,
                crate::act::ActArg::None,
                crate::act::To::Room,
            );
            let _ = rnum;
        }
    }

    /// Copyover recovery (comm.c copyover_recover, per-player branch). The socket
    /// fd was inherited across execv and `name` was playing before the reboot.
    /// Register the descriptor (already wired to the live writer), load the player
    /// straight into Playing state (no nanny), and send the C "reboot completed"
    /// message. If the player file/DB load fails, send the C "lost in copyover"
    /// line and close the socket.
    pub(crate) async fn recover_player(
        &mut self,
        conn_id: ConnId,
        host: String,
        peer_ip: String,
        verified_hostname: Option<String>,
        raw_fd: RawFd,
        name: String,
        output_tx: mpsc::Sender<OutputFrame>,
    ) {
        info!("Copyover recovery: re-attaching {} (fd {})", name, raw_fd);
        let mut d = Descriptor::with_identity(conn_id, host, peer_ip, verified_hostname, raw_fd);
        // The player was already greeted/logged-in pre-reboot; mark the name so
        // descriptor_name() / enter_game pick the right pfile, and start in
        // GetName so enter_game's state transition to Playing is well-defined.
        d.temp_name = Some(name.clone());
        d.state = ConState::GetName;
        self.state.descriptors.insert(conn_id, d);
        self.outputs.insert(conn_id, output_tx);

        // "\n\rRestoring from copyover...\n\r" was already written to the fd by
        // the OLD process right before exec (do_copyover); here we emit the C
        // "reboot completed" confirmation, then enter the world.
        let exists = self.db_player_exists(&name).await.unwrap_or(false);
        if !exists {
            // C: "\n\rSomehow, your character was lost in the copyover. Sorry.\n\r"
            self.out(
                conn_id,
                "\n\rSomehow, your character was lost in the copyover. Sorry.\n\r",
            );
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.state = ConState::Close;
            }
            return;
        }
        self.out(
            conn_id,
            "\n\rThe reboot has been completed. You may continue playing.\n\r",
        );
        // enter_game loads the pfile by descriptor_name(), places the char, runs
        // crash_load + look_at_room + "$n has entered the game." — exactly the
        // tail of copyover_recover (enter_player_game + look_at_room).
        self.enter_game(conn_id, false).await;
        self.write_prompt(conn_id);
    }

    pub(crate) async fn disconnect(&mut self, conn_id: ConnId) {
        // If the player was mid-OLC, drop the editor's working copy and release
        // the lock on the edited vnum (C frees the editor on connection
        // teardown; without this the per-conn state + vnum lock leak until the
        // next reboot — BUG #21). No-op if not editing.
        crate::olc::abort_editor(&mut self.state, conn_id);
        // String-editor + pager state for this connection must go too: ConnIds
        // are never reused, so a pager holding a full paginated document (or an
        // editor buffer) leaks forever (issue #397).
        crate::modify::abort_conn(&mut self.state, conn_id);
        // Login-side per-conn state (issue #397): pending_load holds an entire
        // 83-column Character clone, pending/just_created hold creation
        // choices -- ConnIds are never reused, so anything left behind after
        // this point leaks forever.
        self.pending_load.remove(&conn_id);
        self.pending.remove(&conn_id);
        self.just_created.remove(&conn_id);
        let ch = self
            .state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.character);
        if let Some(cid) = ch {
            let mut alias_id_to_clear = None;
            // C comm.c:2010 — arena combatants get their backed-up affects,
            // wimpy and recall restored BEFORE the save, or the zeroed values
            // persist to SQL (issue #390).
            crate::arena::on_link_lost(&mut self.state, cid);
            // Persist then remove the character from the world.
            if let Some(snapshot) = self.snapshot_online_player_for_save(cid) {
                // Keep the index current with the saved record (level can
                // have changed this session); host carries over the last
                // login's host (update_player_index ignores an empty host).
                let (idnum, pname, llogon) = (
                    snapshot.idnum,
                    snapshot.get_name().to_string(),
                    snapshot.last_logon.timestamp(),
                );
                alias_id_to_clear = Some(idnum);
                self.state
                    .update_player_index_from_character(&snapshot, llogon, "");
                if let Err(err) = crate::alias::write_aliases(
                    &self.state,
                    &self.state.config.lib_path,
                    &pname,
                    idnum,
                ) {
                    warn!("write_aliases(g, {}) failed: {}", pname, err);
                }
                let host = self
                    .state
                    .descriptors
                    .get(&conn_id)
                    .map(|d| d.host.clone())
                    .unwrap_or_default();
                self.queue_player_save(snapshot, host);
            }
            let lib_path = self.state.config.lib_path.clone();
            crate::objsave::crash_save(&mut self.state, cid, &lib_path);
            self.state.extract_char(cid);
            if let Some(idnum) = alias_id_to_clear {
                crate::alias::clear_aliases(&mut self.state, idnum);
            }
        }
        self.state.descriptors.remove(&conn_id);
        self.outputs.remove(&conn_id);
        info!("Connection {} closed", conn_id);
    }
}
