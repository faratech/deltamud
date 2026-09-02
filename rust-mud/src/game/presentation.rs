//! Output flush, playing prompts, GMCP/MSSP pushes, and the who-snapshot refresh.
//!
//! Split out of game/mod.rs (phase 2); `use super::*` inherits the
//! Game struct, its fields, and the module's imports.

use super::*;

impl Game {
    pub(crate) fn make_playing_prompt(&mut self, conn_id: ConnId) -> String {
        use crate::flags::{
            PRF_AFK, PRF_DISPEXP, PRF_DISPHP, PRF_DISPMANA, PRF_DISPMOVE, PRF2_DISPMOB,
        };
        let Some(cid) = self
            .state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.character)
        else {
            return String::new();
        };
        let c = match self.state.get_char(cid) {
            Some(c) => c,
            None => return String::new(),
        };
        let mut prompt = String::new();
        let invis = c.invis_level;
        if invis > 0 {
            prompt.push_str(&format!("&Ri&Y{}&n ", invis));
        }
        if c.prf_flags & PRF_DISPHP != 0 {
            prompt.push_str(&format!("&G{}&ghp&w ", c.points.hit));
        }
        if c.prf_flags & PRF_DISPMANA != 0 {
            prompt.push_str(&format!("&C{}&cmp&w ", c.points.mana));
        }
        if c.prf_flags & PRF_DISPMOVE != 0 {
            match c.riding.map(|rid| self.state.get_char(rid)).flatten() {
                Some(mount) => {
                    prompt.push_str(&format!("&M{}&m&ym&mmv&w ", mount.points.move_points))
                }
                None => prompt.push_str(&format!("&M{}&mmv&w ", c.points.move_points)),
            }
        }
        let mut fighting_diag: Option<String> = None;
        if c.prf_flags & PRF_AFK != 0 {
            prompt.push_str("&W(&naway&W)&n ");
        } else {
            if c.prf_flags & PRF_DISPEXP != 0 && c.player.level < LVL_HERO {
                let need = crate::class::exp_to_level(c.player.level as i32);
                prompt.push_str(&format!("&W(&n{}&W) ", need - c.points.exp));
            }
            if c.prf_flags & PRF2_DISPMOB != 0 {
                if let Some(vict) = c.fighting {
                    if let Some(v) = self.state.get_char(vict) {
                        let percent = if v.points.max_hit > 0 {
                            (100 * v.points.hit) / v.points.max_hit
                        } else {
                            -1
                        };
                        // C act.informative.c:239-266 prompt_diag.
                        let word = match percent {
                            p if p >= 100 => "excellent",
                            p if p >= 90 => "scratched",
                            p if p >= 75 => "bruised",
                            p if p >= 50 => "wounded",
                            p if p >= 30 => "nasty",
                            p if p >= 15 => "hurt",
                            p if p >= 0 => "awful",
                            _ => "bleeding",
                        };
                        fighting_diag = Some(word.to_string());
                    }
                }
            }
        }
        if let Some(word) = fighting_diag {
            prompt.push_str(&format!("&R(&n{}&R) ", word));
        }
        let idnum = c.idnum;
        if crate::mail::has_mail(&self.state, idnum) {
            prompt.push_str("&B(&Ymail&B)&n ");
        }
        if c.conditions[DRUNK] > 4 {
            prompt.push_str("&G(&ndrunk&G)&n ");
        }
        prompt.push_str("&R>&w ");
        prompt
    }

    pub(crate) fn write_prompt(&mut self, conn_id: ConnId) {
        // C make_prompt (comm.c:1220-1226): an active pager or string editor
        // owns the prompt, whatever the connection state (#229).
        if crate::modify::page_active(&self.state, conn_id) {
            let (page, count) = crate::modify::page_position(&self.state, conn_id);
            let prompt = format!(
                "\r[ Return to continue, (q)uit, (r)efresh, (b)ack, or page number ({}/{}) ]",
                page, count
            );
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.write(&prompt);
            }
            return;
        }
        if crate::modify::editing_any(&self.state, conn_id) {
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.write("] ");
            }
            return;
        }
        let state = match self.state.descriptors.get(&conn_id) {
            Some(d) => d.state,
            None => return,
        };
        let name = self
            .state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.temp_name.clone());
        let prompt = match state {
            ConState::QAnsi => String::new(), // question sent on connect / on retry
            ConState::GetName => ASK_NAME.to_string(),
            ConState::ConfirmName => {
                // C interpreter.c:1759: "Did I get that right, %s &c(&YY&c/&YN&c)&n? "
                format!(
                    "Did I get that right, {} &c(&YY&c/&YN&c)&n? ",
                    name.unwrap_or_default()
                )
            }
            ConState::GetOldPassword => "Password: ".to_string(),
            ConState::GetNewPassword => {
                format!("Give me a password for {}: ", name.unwrap_or_default())
            }
            ConState::ConfirmPassword => "\r\nPlease retype password: ".to_string(),
            ConState::GetNewbie => {
                "Are you completely new to MUDing &c(&YY&c/&YN&c)&n? ".to_string()
            }
            ConState::GetSex => "\r\nWhat is your sex &c(&YM&c/&YF&c)&n? ".to_string(),
            ConState::GetRace => format!(
                "{}\r\nTo see a race's average statistics type help <race letter>.\r\nRace: ",
                crate::races::RACE_MENU
            ),
            ConState::GetDeity => format!("{}\r\nDeity: ", crate::deity::DEITY_MENU),
            ConState::GetClass => {
                format!("{}\r\nClass: ", crate::class::CLASS_MENU)
            }
            ConState::GetHometown => format!("{}\r\nTown: ", crate::class::TOWN_MENU),
            ConState::RollStats => self
                .pending
                .get(&conn_id)
                .map(|p| stat_roll_prompt(p.rolled))
                .unwrap_or_default(),
            ConState::ReadMotd => String::new(), // "*** PRESS RETURN" sent on transition
            ConState::Menu => String::new(),     // MENU sent on transition
            ConState::ExDesc => String::new(),   // string editor owns the input
            ConState::ChPwdGetOld => "\r\nEnter your old password: ".to_string(),
            ConState::ChPwdGetNew => "\r\nEnter a new password: ".to_string(),
            ConState::ChPwdVerify => "\r\nPlease retype password: ".to_string(),
            ConState::DelCnf1 => "\r\nEnter your password for verification: ".to_string(),
            ConState::DelCnf2 => "\r\nYOU ARE ABOUT TO DELETE THIS CHARACTER PERMANENTLY.\r\n\
 ARE YOU ABSOLUTELY SURE?\r\n\r\nPlease type \"yes\" to confirm: "
                .to_string(),
            ConState::Playing => {
                // C comm.c:1213-1293 make_prompt: the full PRF_* chain (#220).
                self.make_playing_prompt(conn_id)
            }
            _ => String::new(),
        };
        // Before a password prompt, tell the client the server WILL echo so it
        // suppresses local echo (cleartext password no longer shows). The IAC
        // bytes go straight down the output channel; the prompt text follows in
        // the next outbuf flush, so the client sees WILL-ECHO first.
        if is_password_state(state) {
            self.send_raw_bytes(conn_id, &IAC_WILL_ECHO);
        }
        if !prompt.is_empty() {
            if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
                d.write(&prompt);
            }
        }

        // Out-of-band GMCP push: after the prompt, but only when something
        // actually made this connection's state stale since the last push
        // (W5 event-driven GMCP: idle players no longer get a per-command
        // re-send of identical JSON; players in combat DO get fresh vitals
        // from mob pulses via the heartbeat drain below).
        if state == ConState::Playing && self.state.gmcp_dirty.remove(&conn_id) {
            self.push_gmcp_update(conn_id);
        }
    }

    // ---- small helpers --------------------------------------------------
    pub(crate) fn out(&mut self, conn_id: ConnId, msg: &str) {
        if let Some(d) = self.state.descriptors.get_mut(&conn_id) {
            d.write(msg);
        }
    }

    /// Send raw bytes straight down a connection's output channel, bypassing the
    /// outbuf/render_color String pipeline (used for telnet IAC control
    /// sequences whose lone 0xFF byte must not pass through `.chars()`). Mirrors
    /// connection.rs's negotiation-refusal path: the writer only ever calls
    /// `.as_bytes()`, so wrapping arbitrary bytes in a String is lossless.
    pub(crate) fn send_raw_bytes(&mut self, conn_id: ConnId, bytes: &[u8]) {
        if bytes.len() > crate::connection::DESCRIPTOR_OUTPUT_LIMIT {
            self.metrics.inc_output_overflow();
            if let Some(descriptor) = self.state.descriptors.get_mut(&conn_id) {
                descriptor.state = ConState::Close;
            }
            return;
        }
        if let Some(tx) = self.outputs.get(&conn_id) {
            // The channel carries raw bytes: telnet frames are NOT valid
            // UTF-8 (IAC = 0xFF), and Vec<u8> makes that contract
            // compile-enforced instead of a from_utf8_unchecked UB risk.
            // try_send avoids making this async; the bounded(256) channel is
            // effectively never full for a 3-byte control sequence, and dropping
            // an echo-negotiation byte under extreme backpressure is harmless.
            if tx.try_send(OutputFrame::data(bytes.to_vec())).is_err() {
                self.metrics.inc_output_closed_client();
                if let Some(descriptor) = self.state.descriptors.get_mut(&conn_id) {
                    descriptor.state = ConState::Close;
                }
            }
        }
    }
    pub(crate) fn descriptor_name(&self, conn_id: ConnId) -> String {
        self.state
            .descriptors
            .get(&conn_id)
            .and_then(|d| d.temp_name.clone())
            .unwrap_or_default()
    }

    // ---- GMCP (out-of-band JSON) ---------------------------------------

    /// Apply negotiation/Core metadata parsed at the socket edge. An already
    /// playing descriptor gets one immediate snapshot only on the disabled ->
    /// enabled transition; duplicate DO messages cannot amplify output. DONT
    /// clears both the send gate and all client-advertised package state.
    pub(crate) fn handle_gmcp_event(
        &mut self,
        conn_id: ConnId,
        event: crate::connection::GmcpClientEvent,
    ) {
        let (became_enabled, playing, enabled) = match self.state.descriptors.get_mut(&conn_id) {
            Some(descriptor) => {
                let playing = descriptor.state == ConState::Playing;
                let became_enabled = descriptor.apply_gmcp_event(event);
                (became_enabled, playing, descriptor.gmcp)
            }
            None => return,
        };
        if !enabled {
            self.state.gmcp_dirty.remove(&conn_id);
        }
        if became_enabled && playing {
            self.push_gmcp_update(conn_id);
        }
    }

    /// Send the per-command GMCP snapshot (`Char.Vitals` + `Room.Info`) to a
    /// GMCP-enabled descriptor that has a playing character. JSON is hand-rolled
    /// (no serde dep): small, one-line, with `"`/`\` escaped in names. Bytes go
    /// down the raw-bytes channel verbatim, never through render_color.
    pub(crate) fn push_gmcp_update(&mut self, conn_id: ConnId) {
        for message in self.gmcp_snapshots(conn_id) {
            self.send_raw_bytes(conn_id, &telnet_subneg(TELOPT_GMCP, message.as_bytes()));
        }
    }

    /// Pure snapshot builder: the GMCP messages (names + JSON payloads) for a
    /// connection, or empty when the connection is not GMCP-enabled/playing.
    /// Split from push_gmcp_update so tests can assert on payloads without a
    /// live output channel.
    pub(crate) fn gmcp_snapshots(&self, conn_id: ConnId) -> Vec<String> {
        let d = match self.state.descriptors.get(&conn_id) {
            Some(d) if d.gmcp => d,
            _ => return Vec::new(),
        };
        let ch = match d.character {
            Some(c) => c,
            None => return Vec::new(),
        };
        let c = match self.state.get_char(ch) {
            Some(c) => c,
            None => return Vec::new(),
        };
        let mut messages = Vec::with_capacity(2);

        // Char.Vitals — current/max HP, mana, move.
        let p = &c.points;
        let vitals = gmcp_message(
            "Char.Vitals",
            &serde_json::json!({
                "hp": p.hit,
                "maxhp": p.max_hit,
                "mana": p.mana,
                "maxmana": p.max_mana,
                "move": p.move_points,
                "maxmove": p.max_move,
                "level": c.player.level,
            }),
        );
        messages.push(vitals);

        // Room.Info — vnum, name, zone, exits as {dir: dest-vnum}, plus the
        // closed/locked door lists the mapper needs (W5). Occupancy lists the
        // other characters in the room so GUIs can draw fellow players.
        if let Some(rnum) = c.in_room {
            if let Some(room) = self.state.room_opt(rnum) {
                let zone_name = self
                    .state
                    .zones
                    .get(room.zone as usize)
                    .map(|z| z.name.as_str())
                    .unwrap_or("");
                let dir_keys = ["n", "e", "s", "w", "u", "d"];
                let mut exits = serde_json::Map::new();
                let mut doors: Vec<&str> = Vec::new();
                let mut locked: Vec<&str> = Vec::new();
                for (i, key) in dir_keys.iter().enumerate() {
                    if let Some(ex) = room.exits.get(i).and_then(|e| e.as_ref()) {
                        exits.insert(key.to_string(), serde_json::json!(ex.to_room));
                        if ex.exit_info & crate::room::EX_CLOSED != 0 {
                            doors.push(key);
                            if ex.exit_info & crate::room::EX_LOCKED != 0 {
                                locked.push(key);
                            }
                        }
                    }
                }
                let occupants: Vec<String> = room
                    .people
                    .iter()
                    .filter(|&&other| other != ch)
                    .filter_map(|&other| self.state.get_char(other))
                    .filter(|other| !other.is_npc)
                    .map(|other| other.get_name().to_string())
                    .collect();
                let room_info = gmcp_message(
                    "Room.Info",
                    &serde_json::json!({
                        "num": room.number,
                        "name": gmcp_clean(&room.name),
                        "zone": gmcp_clean(zone_name),
                        "exits": exits,
                        "doors": doors,
                        "locked": locked,
                        "players": occupants,
                        "map": {
                            "x": room.map_x.unwrap_or(0),
                            "y": room.map_y.unwrap_or(0),
                        },
                    }),
                );
                messages.push(room_info);
            }
        }
        messages
    }

    /// Rebuild the /api/who JSON snapshot (same visibility rules as the
    /// who2html walk: playing, non-npc, no invis level, not AFF_INVISIBLE).
    pub(crate) fn refresh_who_snapshot(&mut self) {
        use serde_json::json;
        let mut entries: Vec<(u8, serde_json::Value)> = Vec::new();
        let ids: Vec<CharId> = self.state.players_by_name.values().copied().collect();
        for cid in ids {
            let Some(c) = self.state.get_char(cid) else {
                continue;
            };
            if c.is_npc {
                continue;
            }
            if c.invis_level > 0 || c.affect_flags & crate::flags::AFF_INVISIBLE != 0 {
                continue;
            }
            entries.push((
                c.player.level,
                json!({
                    "name": c.get_name(),
                    "level": c.player.level,
                    "race": crate::whohtml::race_name(&self.state, cid),
                    "class": crate::whohtml::class_name(&self.state, cid),
                    "immortal": c.player.level >= LVL_IMMORT,
                    "title": c.player.title.clone().unwrap_or_default(),
                }),
            ));
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let names: Vec<serde_json::Value> = entries.into_iter().map(|(_, v)| v).collect();
        let doc = json!({
            "count": names.len(),
            "players": names,
            "generated_at": self.started_at,
        });
        if let Ok(mut slot) = self.who_snapshot.write() {
            *slot = doc.to_string();
        }
    }

    // ---- MSSP (Mud Server Status Protocol) -----------------------------

    /// Handle `GameMessage::SendMssp`: build and send the one-shot MSSP status
    /// block (`IAC SB MSSP <VAR name VAL value>... IAC SE`). Crawlers/listing
    /// sites read this to index the server. PLAYERS/UPTIME need the live Game,
    /// which is why this is driven from here rather than connection.rs.
    pub(crate) fn send_mssp(&mut self, conn_id: ConnId) {
        // Count players currently in-world (a character attached, in Playing).
        let players = self
            .state
            .descriptors
            .values()
            .filter(|d| d.state == ConState::Playing && d.character.is_some())
            .count();
        // Listen port from the boot configuration (no environment reads on
        // the presentation path).
        let port: u16 = self.state.config.port;

        let mut payload: Vec<u8> = Vec::with_capacity(128);
        let mut add = |name: &str, value: &str| {
            payload.push(MSSP_VAR);
            payload.extend_from_slice(name.as_bytes());
            payload.push(MSSP_VAL);
            payload.extend_from_slice(value.as_bytes());
        };
        add("NAME", "DeltaMUD");
        add("PLAYERS", &players.to_string());
        // MSSP UPTIME = unix timestamp the server booted.
        add("UPTIME", &self.started_at.to_string());
        add("PORT", &port.to_string());
        add("CODEBASE", "DeltaMUD-Rust");
        add("FAMILY", "CircleMUD");

        self.send_raw_bytes(conn_id, &telnet_subneg(TELOPT_MSSP, &payload));
    }
}

/// Pending character-creation choices held between nanny steps.
#[derive(Clone, Copy)]
struct PendingChoices {
    sex: Gender,
    class: Class,
    race: Race,
    race_index: i32,
    newbie: u8,
    deity: u8,
    hometown: RoomVnum,
    rolled: Abilities,
}
