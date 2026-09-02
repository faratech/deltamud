//! The 10 Hz heartbeat: pulse stages, zone resets, idle-password reaping, autoreboot.
//!
//! Split out of game/mod.rs (phase 2); `use super::*` inherits the
//! Game struct, its fields, and the module's imports.

use super::*;

impl Game {
    pub(crate) fn heartbeat(&mut self) {
        // Crash-isolate the whole pulse: a panic in any handler (a mob script,
        // combat, weather, ...) must NOT kill the single Game task and freeze the
        // server. Catch it, log it (the panic hook also records a backtrace), and
        // continue on the next pulse. (Does not protect against a stack overflow /
        // abort — those are not unwinding panics.)
        // Time the whole pulse (the perf-relevant work lives in heartbeat_inner).
        // std::time::Instant is monotonic and cheap; record the duration in
        // microseconds into the lock-free metrics so /metrics can expose a tiny
        // deltamud_heartbeat_tick_micros and its high-water mark.
        let start = std::time::Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.heartbeat_inner();
        }));
        let micros = start.elapsed().as_micros() as u64;
        self.metrics.record_tick_micros(micros);
        self.metrics.set_pulse(self.state.pulse);

        // Refresh the world-size gauges periodically (every 10 pulses ~= 1s) to
        // keep the per-pulse cost negligible. players = playing descriptors;
        // mobs = NPC characters; objs = world objects.
        if self.state.pulse % 10 == 0 {
            self.refresh_who_snapshot();
            let players = self
                .state
                .descriptors
                .values()
                .filter(|d| d.state == ConState::Playing && d.character.is_some())
                .count() as u64;
            let total_chars = self.state.chars.len() as u64;
            // mobs = all characters minus the player-controlled ones.
            let mobs = total_chars.saturating_sub(players);
            self.metrics.set_players(players);
            self.metrics.set_mobs(mobs);
            self.metrics.set_objs(self.state.objs.len() as u64);
        }

        if let Err(e) = r {
            log::error!(
                "PANIC in heartbeat (pulse {}): {} — skipping rest of pulse",
                self.state.pulse,
                panic_payload_str(&e)
            );
        }
    }

    pub(crate) fn heartbeat_inner(&mut self) {
        self.state.pulse = self.state.pulse.wrapping_add(1);
        let pulse = self.state.pulse;

        // Drain queued player input through the WAIT_STATE gate (C game_loop:
        // `--d->wait <= 0 && get_from_q(...)`), one command per descriptor.
        self.process_input_queues();

        // C comm.c:1001-1058 heartbeat(): stage order and cadences below
        // mirror the oracle exactly (issues #192/#225). Input draining is the
        // game_loop's job and stays above.
        crate::dg_event::process_events(&mut self.state);
        // PULSE_DG_SCRIPT (dg_scripts.h): random/idle trigger scan.
        if pulse % PULSE_DG_SCRIPT == 0 {
            crate::dg_scripts::script_trigger_check(&mut self.state);
        }
        if pulse % PULSE_ZONE == 0 {
            self.zone_update();
        }
        // PULSE_IDLE_PASSWORD: reap sockets sitting at login prompts, auctions.
        if pulse % PULSE_IDLE_PASSWORD == 0 {
            self.check_idle_passwords();
        }
        if pulse % PULSE_IDLE_PASSWORD == 0 {
            crate::auction::auction_update(&mut self.state);
        }
        if pulse % PULSE_MOBILE == 0 {
            crate::mobact::mobile_activity(&mut self.state);
        }
        if pulse % PULSE_VIOLENCE == 0 {
            combat::perform_violence(&mut self.state);
        }
        // Live surface weather (storms spawn/move/collide/expire) every 30
        // RL-seconds.
        if pulse % PULSE_WEATHER_ACTIVITY == 0 {
            crate::maputils::weather_activity(&mut self.state);
        }
        // Autoquest update + room blood decay, every minute.
        if pulse % PULSE_MINUTE == 0 {
            crate::quest::quest_update(&mut self.state);
            crate::maputils::blood_update(&mut self.state);
            self.autoreboot_check();
        }
        // Mud-hour block (PULSE_MUD_HOUR = SECS_PER_MUD_HOUR * PASSES_PER_SEC):
        // calendar/sky, affect aging (comm.c:1038, #96), then regen/conditions.
        if pulse % PULSE_MUD_HOUR == 0 {
            crate::weather::weather_and_time(&mut self.state);
            crate::magic::affect_update(&mut self.state);
            crate::limits::point_update(&mut self.state);
        }
        // 1-minute autosave block (C: auto_save && pulse % 60s) with the
        // autosave_time (config.c:174 = 5) minute gate: Crash_save_all +
        // House_save_all (#192; the old 75-second crash-save tick was 4x
        // C's cadence and houses were never saved at all).
        if pulse % PULSE_MINUTE == 0 {
            self.mins_since_crashsave += 1;
            if self.mins_since_crashsave >= crate::config::AUTOSAVE_TIME {
                self.mins_since_crashsave = 0;
                crate::objsave::crash_save_all(&mut self.state);
                crate::house::house_save_all(&mut self.state);
            }
        }

        // GMCP drain (W5): mob pulses, combat rounds and regen all ran above
        // and marked stale connections; push fresh snapshots so a client's
        // gauges track mob-initiated damage without waiting for a command.
        let stale: Vec<ConnId> = self.state.gmcp_dirty.drain().collect();
        for conn_id in stale {
            if let Some(d) = self.state.descriptors.get(&conn_id) {
                if d.gmcp && d.state == ConState::Playing {
                    self.push_gmcp_update(conn_id);
                }
            }
        }
    }

    /// C comm.c:2049-2069 check_idle_passwords(): a descriptor sitting at a
    /// name/password prompt for two consecutive 15-second ticks is disconnected
    /// with C's message.
    pub(crate) fn check_idle_passwords(&mut self) {
        let mut to_close: Vec<ConnId> = Vec::new();
        for (cid, d) in self.state.descriptors.iter_mut() {
            if matches!(
                d.state,
                ConState::GetName
                    | ConState::GetOldPassword
                    | ConState::GetNewPassword
                    | ConState::ConfirmPassword
                    | ConState::ConfirmName
            ) {
                d.idle_tics += 1;
                if d.idle_tics >= 2 {
                    d.write("\r\nTimed out... goodbye.\r\n");
                    to_close.push(*cid);
                }
            }
        }
        for cid in to_close {
            if let Some(d) = self.state.descriptors.get_mut(&cid) {
                d.state = ConState::Close;
            }
        }
    }

    pub(crate) fn zone_update(&mut self) {
        // C db.c:1877-1952 zone_update (#231). A 60-second accumulator ages
        // the zones (NOT one age tick per 10-second PULSE_ZONE call); zones
        // reaching their lifespan are queued (age = ZO_DEAD) and at most ONE
        // queued zone is reset per tick, gated on room emptiness unless
        // reset_mode == 2.
        const ZO_DEAD: i32 = crate::world::ZONE_DEAD;
        self.zone_minute_timer += 1;
        if (self.zone_minute_timer * PULSE_ZONE) / PASSES_PER_SEC >= 60 {
            self.zone_minute_timer = 0;
            let mut enqueue: Vec<i32> = Vec::new();
            for z in self.state.zones.iter_mut() {
                if z.age < z.lifespan && z.reset_mode != 0 {
                    z.age += 1;
                }
                if z.age >= z.lifespan && z.age < ZO_DEAD && z.reset_mode != 0 {
                    enqueue.push(z.number);
                    z.age = ZO_DEAD;
                }
            }
            self.zone_reset_queue.extend(enqueue);
        }
        if self.zone_reset_queue.is_empty() {
            return;
        }
        let mut idx = 0;
        while idx < self.zone_reset_queue.len() {
            let zn = self.zone_reset_queue[idx];
            let reset_mode = self
                .state
                .zones
                .iter()
                .find(|z| z.number == zn)
                .map(|z| z.reset_mode)
                .unwrap_or(0);
            if reset_mode == 2 || self.zone_is_empty(zn) {
                self.zone_reset_queue.remove(idx);
                self.state.reset_zone(zn);
                let name = self
                    .state
                    .zones
                    .iter()
                    .find(|z| z.number == zn)
                    .map(|z| z.name.clone())
                    .unwrap_or_default();
                crate::syslog::mudlog(
                    &mut self.state,
                    &format!("Auto zone reset: {}", name),
                    crate::syslog::CMP,
                    LVL_GOD,
                );
                break;
            }
            idx += 1;
        }
    }

    /// C db.c:2150 is_empty(zone_nr): true when no playing descriptor's
    /// character stands in the zone.
    pub(crate) fn zone_is_empty(&self, zone_number: i32) -> bool {
        for d in self.state.descriptors.values() {
            if d.state != ConState::Playing {
                continue;
            }
            if let Some(cid) = d.character {
                if let Some(c) = self.state.get_char(cid) {
                    if let Some(rnum) = c.in_room {
                        if let Some(room) = self.state.room_opt(rnum) {
                            if room.zone == zone_number {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        true
    }

    // ---- Output flushing ------------------------------------------------
    /// C comm.c:762 auto-reboot clock (finish-the-game activation): once a
    /// minute, compare wall-clock time to the setreboot schedule; warn at the
    /// warn time and save-all + graceful shutdown at the reboot time.
    pub(crate) fn autoreboot_check(&mut self) {
        if !self.state.config.autoreboot {
            return;
        }
        let (rh, rm, wh, wm) = crate::cmd_wizard::reboot_schedule();
        if rh < 0 {
            return;
        }
        use chrono::Timelike;
        let now = chrono::Utc::now();
        let (hr, min) = (now.hour() as i32, now.minute() as i32);
        self.autoreboot_check_at((rh, rm, wh, wm), hr, min);
    }

    /// Time-injected half of the autoreboot clock. Keeping the wall clock at
    /// the thin wrapper above makes the trigger and its fail-closed OLC gate
    /// deterministic in unit tests.
    pub(crate) fn autoreboot_check_at(
        &mut self,
        (rh, rm, wh, wm): (i32, i32, i32, i32),
        hr: i32,
        min: i32,
    ) {
        if hr == wh && min == wm && !self.reboot_warned {
            self.reboot_warned = true;
            let msg = format!(
                "&m[&YINFO&m]&n The game will reboot in {} minutes. Please rent.\r\n",
                if rm >= wm { rm - wm } else { 60 - (wm - rm) }
            );
            self.state.send_to_all_players(&msg);
            crate::syslog::mudlog(
                &mut self.state,
                "Automatic reboot imminent.",
                crate::syslog::NRM,
                0,
            );
        }
        if hr == rh && min == rm {
            if let Err(error) = crate::olc::flush_save_list_to_disk(&mut self.state) {
                warn!("Auto-reboot aborted because pending OLC could not be saved: {error}");
                crate::syslog::mudlog(
                    &mut self.state,
                    "Automatic reboot aborted: pending OLC changes could not be saved.",
                    crate::syslog::NRM,
                    0,
                );
                self.state.send_to_all_players(
                    "&m[&RERROR&m]&n Automatic reboot aborted because pending OLC changes could not be saved.\r\n",
                );
                return;
            }
            info!("Auto-reboot triggered; saving and restarting.");
            crate::syslog::mudlog(
                &mut self.state,
                "Automatic reboot triggered.",
                crate::syslog::NRM,
                0,
            );
            crate::objsave::crash_save_all(&mut self.state);
            crate::house::house_save_all(&mut self.state);
            self.state.shutdown_requested =
                Some(ShutdownRequest::System(ProcessDisposition::Restart));
        }
    }

    pub(crate) async fn flush_all(&mut self) {
        let conns: Vec<ConnId> = self.state.descriptors.keys().copied().collect();
        let mut to_close = Vec::new();
        for conn_id in conns {
            let (text, closing, mut overflowed) = {
                let d = match self.state.descriptors.get_mut(&conn_id) {
                    Some(d) => d,
                    None => continue,
                };
                let (text, overflowed) = d.take_output_status();
                (text, d.state == ConState::Close, overflowed)
            };
            if !text.is_empty() {
                // C comm.c:1637-1642 (#221): the whole buffer (output + prompt)
                // is proc_color'd with the viewer's colour mode — mortals in a
                // magic-fog room get the -1 scramble, others get
                // clr(ch, C_NRM) (level >= 2 renders, below strips).
                let mode = {
                    let ch_id = self
                        .state
                        .descriptors
                        .get(&conn_id)
                        .and_then(|d| d.character);
                    match ch_id.map(|c| self.state.get_char(c)).flatten() {
                        Some(c) => {
                            let in_fog = c
                                .in_room
                                .map(|r| {
                                    self.state.room(r).weather
                                        == crate::maputils::WEATHER_MAGICFOG as i32
                                })
                                .unwrap_or(false);
                            if in_fog && c.player.level < LVL_IMMORT {
                                -1
                            } else if crate::olc::colour_level(&self.state, ch_id.unwrap()) >= 2 {
                                1
                            } else {
                                0
                            }
                        }
                        None => 1,
                    }
                };
                let mut rendered = crate::connection::proc_color(&text, mode, |max| {
                    1 + self.state.rng.dice(1, max)
                });
                if rendered.len() > crate::connection::DESCRIPTOR_OUTPUT_LIMIT {
                    crate::text::truncate_utf8_bytes(
                        &mut rendered,
                        crate::connection::DESCRIPTOR_OUTPUT_LIMIT
                            .saturating_sub(crate::connection::OUTPUT_OVERFLOW_MARKER.len()),
                    );
                    rendered.push_str(crate::connection::OUTPUT_OVERFLOW_MARKER);
                    overflowed = true;
                }
                if overflowed {
                    self.metrics.inc_output_overflow();
                }
                if let Some(tx) = self.outputs.get(&conn_id) {
                    // C comm.c:1713 closes on would-block rather than waiting:
                    // a client that stops reading must not park the Game task
                    // (a full bounded channel means the writer is stalled on
                    // TCP backpressure). try_send + close on Full is the
                    // non-blocking equivalent; the loop's to_close pass
                    // disconnects the descriptor below.
                    if tx
                        .try_send(OutputFrame::data(rendered.into_bytes()))
                        .is_err()
                    {
                        self.metrics.inc_output_closed_client();
                        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                            d.state = ConState::Close;
                        }
                        to_close.push(conn_id);
                    }
                }
            }
            if closing && !to_close.contains(&conn_id) {
                to_close.push(conn_id);
            }
        }
        for conn_id in to_close {
            self.disconnect(conn_id).await;
        }
    }
}
