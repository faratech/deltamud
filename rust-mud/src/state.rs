// GameState: the single owner of the entire world, mirroring CircleMUD's
// single-threaded heartbeat. Every entity lives in an id-indexed arena here;
// commands and handlers operate on `&mut GameState`. Async I/O lives outside
// (game.rs / connection.rs) and communicates only through Descriptor::outbuf.

use crate::character::Character;
use crate::config::Config;
use crate::connection::Descriptor;
use crate::object::{ObjLoc, Object};
use crate::rng::Rng;
use crate::room::Room;
use crate::types::*;
use crate::world::{MobileProto, ObjectProto, Zone};
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};

/// Process-global copy of the listening socket fd (C `mother_desc`). main.rs
/// publishes it here right after TcpListener::bind so do_copyover (running in
/// the Game task, which never sees main's local listener) can clear FD_CLOEXEC
/// on it and hand it to the re-exec'd binary. -1 until the listener is bound.
static LISTENER_FD: AtomicI32 = AtomicI32::new(-1);

/// Publish the bound listener fd (main.rs, at boot). Mirrors C setting
/// `mother_desc` before init_game so copyover can inherit it.
pub fn set_listener_fd(fd: std::os::unix::io::RawFd) {
    LISTENER_FD.store(fd, Ordering::SeqCst);
}

/// Read the published listener fd (do_copyover). -1 if not yet bound.
pub fn listener_fd() -> std::os::unix::io::RawFd {
    LISTENER_FD.load(Ordering::SeqCst)
}

/// One row of the in-memory player index (C `struct player_index_element`,
/// db.h). C carries only {name, id, level}; the Rust port additionally caches
/// selected `player_main` fields so offline reports that C answers directly
/// from SQL (last/roster/autowiz) do not silently omit logged-off players.
/// `name` keeps the player's stored capitalisation (C lowercases its copy;
/// get_name_by_id then returns the lowercased form — we keep the canonical name
/// so callers that display it, e.g. the ignore listing, match the C
/// `last`/listing output).
#[derive(Debug, Clone)]
pub struct PlayerIndex {
    pub idnum: i64,
    pub name: String,
    pub level: u8,
    pub class: crate::types::Class,
    pub last_logon: i64,
    pub host: String,
    pub act_flags: i64,
    pub clan: i32,
    pub clan_rank: i32,
}

/// A deferred immortal command against an OFFLINE player (the async bridge for
/// C's do_set/do_stat/do_show, which load a logged-off player's full record via
/// retrieve_player_entry, edit it, and save). The synchronous command handler
/// can't await the async DB, so when it finds the target offline-but-indexed it
/// queues one of these instead of degrading to "no such player". The async Game
/// loop drains the queue (game.rs), loads the player into the world, REPLAYS
/// `command` through command_interpreter so the existing online handler logic
/// applies, then persists + extracts. `requester` is the immortal who typed it;
/// `target` is the offline player's name; `command` is the immortal's ORIGINAL
/// command verbatim (e.g. "set Mortvictim gold 5000") so the replay re-parses
/// the identical field/value.
#[derive(Debug, Clone)]
pub struct OfflineOp {
    pub requester: CharId,
    pub target: String,
    pub command: String,
}

pub struct GameState {
    // Static world (loaded at boot; mutated by resets / OLC).
    pub rooms: Vec<Room>,
    pub room_index: HashMap<RoomVnum, RoomRnum>,
    pub zones: Vec<Zone>,
    pub mob_protos: HashMap<MobVnum, MobileProto>,
    pub obj_protos: HashMap<ObjVnum, ObjectProto>,

    // Live instances. `IndexMap` is an ordered map: O(1) insert/get/swap_remove
    // *and* ordered iteration, so it replaces the old `HashMap` + separate
    // `*_list: Vec` (which carried C's character_list/object_list order at the
    // cost of O(n) prepend + O(n) Vec removal on every spawn/extract). Insertion
    // order is preserved (oldest-first); extraction uses swap_remove (O(1),
    // reorders the tail). Iteration order is internal-only — no observable
    // behavior depends on it. The id<->struct lookup is the map itself.
    pub chars: IndexMap<CharId, Character>,
    pub objs: IndexMap<ObjId, Object>,

    // Connections (the Descriptor lives here; the async output channel lives
    // in the Game wrapper keyed by the same ConnId).
    pub descriptors: HashMap<ConnId, Descriptor>,
    pub players_by_name: HashMap<String, CharId>,

    // In-memory player name<->idnum index (C `player_table`, built by
    // build_player_index() at boot from a `SELECT idnum,name,level FROM
    // player_main`). Lets name<->id lookups, `last`, and ignore resolve
    // OFFLINE players without an async DB hit. We additionally cache
    // last_logon + host so `do_last` can render an offline player's record
    // straight from the index. Kept fresh by update_player_index() on
    // create/enter/save (the C MUD rebuilds the whole table after a
    // create_entry; we upsert the single row).
    pub player_table: Vec<PlayerIndex>,

    // Deferred immortal commands against OFFLINE players (set/stat/show on a
    // logged-off player's full record). The sync command path can't await the
    // async DB, so it queues an OfflineOp here; the async Game loop (game.rs)
    // drains this each heartbeat — loads the player into the world, replays the
    // command, then saves + extracts. Empty in steady state.
    pub offline_ops: Vec<OfflineOp>,
    pub pfileclean_requested: bool,
    pub player_save_requests: Vec<CharId>,

    next_char_id: u64,
    next_obj_id: u64,

    pub rng: Rng,
    pub credits: String,
    pub news: String,
    pub info: String,
    pub handbook: String,
    pub policies: String,
    pub motd: String,
    pub imotd: String,
    pub circlemud: String,
    pub config: Config,
    pub pulse: u64,
    /// Set by `do_shutdown` (C `circle_shutdown = 1`). The Game run loop checks
    /// it after each command/pulse and exits via the graceful-shutdown path, so
    /// the `shutdown` immortal command actually halts the server (it previously
    /// only broadcast a message and logged — the process never stopped).
    pub shutdown_requested: bool,
    /// C `pk_allowed` (config.c:53, `int pk_allowed = NO`). The live PvP gate
    /// read by do_hit/do_kill/murder, the killer-flagging path in fight.c and
    /// the PvP spell guards; toggled by `set Legal_PKS ON|OFF`
    /// (act.wizard.c:3914-3921).
    pub pk_allowed: bool,

    // Surface ("outside") world-map splice (maputils.c read_map). The 99x99
    // grid of map cells is appended to `rooms` *after* the real-room block, so
    // real-room rnums (and real_room(vnum)) are untouched. `map_start_rnum` is
    // the rnum of the first map cell (1-based grid (1,1)); cell (x,y) lives at
    // `map_start_rnum + (y-1)*max_map_x + (x-1)` (C find_room_by_coords). None
    // until integrate_map_rooms() runs (or the worldmap file is missing).
    pub map_start_rnum: Option<RoomRnum>,
    pub max_map_x: i32,
    pub max_map_y: i32,
}

impl GameState {
    pub fn new(config: Config) -> Self {
        GameState {
            rooms: Vec::new(),
            room_index: HashMap::new(),
            zones: Vec::new(),
            mob_protos: HashMap::new(),
            obj_protos: HashMap::new(),
            chars: IndexMap::new(),
            objs: IndexMap::new(),
            descriptors: HashMap::new(),
            players_by_name: HashMap::new(),
            player_table: Vec::new(),
            offline_ops: Vec::new(),
            pfileclean_requested: false,
            player_save_requests: Vec::new(),
            next_char_id: 1,
            next_obj_id: 1,
            rng: Rng::default(),
            shutdown_requested: false,
            pk_allowed: false,
            credits: String::new(),
            news: String::new(),
            info: String::new(),
            handbook: String::new(),
            policies: String::new(),
            motd: String::new(),
            imotd: String::new(),
            circlemud: String::new(),
            config,
            pulse: 0,
            map_start_rnum: None,
            max_map_x: 0,
            max_map_y: 0,
        }
    }

    /// find_room_by_coords (maputils.c): the rnum of the 1-based map cell (x,y),
    /// with the world wrapping (it is "ROUND!"). None when the surface map has
    /// not been spliced in (map_start_rnum is None / dimensions are 0).
    pub fn map_coords_to_rnum(&self, x: i32, y: i32) -> Option<RoomRnum> {
        let start = self.map_start_rnum?;
        if self.max_map_x <= 0 || self.max_map_y <= 0 {
            return None;
        }
        // WRAPX / WRAPY (maputils.c): fold the coordinate into 1..=max.
        let mut nx = x;
        let mut ny = y;
        while nx > self.max_map_x {
            nx -= self.max_map_x;
        }
        while nx < 1 {
            nx += self.max_map_x;
        }
        while ny > self.max_map_y {
            ny -= self.max_map_y;
        }
        while ny < 1 {
            ny += self.max_map_y;
        }
        Some(start + ((ny - 1) * self.max_map_x + (nx - 1)) as usize)
    }

    // ---- Rooms ----------------------------------------------------------
    pub fn real_room(&self, vnum: RoomVnum) -> Option<RoomRnum> {
        self.room_index.get(&vnum).copied()
    }
    pub fn room(&self, rnum: RoomRnum) -> &Room {
        &self.rooms[rnum]
    }
    pub fn room_mut(&mut self, rnum: RoomRnum) -> &mut Room {
        &mut self.rooms[rnum]
    }
    pub fn room_opt(&self, rnum: RoomRnum) -> Option<&Room> {
        self.rooms.get(rnum)
    }
    pub fn add_room(&mut self, room: Room) -> RoomRnum {
        let vnum = room.number;
        let rnum = self.rooms.len();
        self.rooms.push(room);
        self.room_index.insert(vnum, rnum);
        rnum
    }

    // ---- Characters -----------------------------------------------------
    pub fn get_char(&self, id: CharId) -> Option<&Character> {
        self.chars.get(&id)
    }
    pub fn get_char_mut(&mut self, id: CharId) -> Option<&mut Character> {
        self.chars.get_mut(&id)
    }
    pub fn char_exists(&self, id: CharId) -> bool {
        self.chars.contains_key(&id)
    }

    /// Insert a character into the world (assigns id, appends to the ordered
    /// arena — CircleMUD prepends to character_list, but iteration order is
    /// internal-only here, so O(1) append is used). Does NOT place it in a room.
    pub fn create_char(&mut self, mut ch: Character) -> CharId {
        let id = CharId(self.next_char_id);
        self.next_char_id += 1;
        ch.id = id;
        self.chars.insert(id, ch);
        id
    }

    /// Snapshot of all live character ids (insertion order). Replaces the old
    /// `char_list.clone()` the hot loops took before iterating + mutating.
    pub fn char_ids(&self) -> Vec<CharId> {
        self.chars.keys().copied().collect()
    }

    /// Snapshot of all live object ids (insertion order).
    pub fn obj_ids(&self) -> Vec<ObjId> {
        self.objs.keys().copied().collect()
    }

    pub fn find_player_by_name(&self, name: &str) -> Option<CharId> {
        self.players_by_name.get(&name.to_lowercase()).copied()
    }

    // ---- Player index (C player_table / get_id_by_name / get_name_by_id) ----

    /// get_id_by_name() (db.c): the persistent idnum for `name`, or None if no
    /// player by that name exists. Case-insensitive (C lowercases both sides),
    /// resolves OFFLINE players from the boot-loaded index. Like C, only the
    /// first whitespace token of `name` is considered.
    pub fn get_id_by_name(&self, name: &str) -> Option<i64> {
        let arg = name.split_whitespace().next().unwrap_or("");
        if arg.is_empty() {
            return None;
        }
        self.player_table
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(arg))
            .map(|p| p.idnum)
    }

    /// get_name_by_id() (db.c): the stored (canonical-cased) name for an idnum,
    /// or None. Resolves offline players from the index.
    pub fn get_name_by_id(&self, id: i64) -> Option<String> {
        self.player_table
            .iter()
            .find(|p| p.idnum == id)
            .map(|p| p.name.clone())
    }

    /// The full index row for `name` (level / last_logon / host), or None.
    /// Case-insensitive; used by `do_last` to render an offline player.
    pub fn player_index(&self, name: &str) -> Option<&PlayerIndex> {
        let arg = name.split_whitespace().next().unwrap_or("");
        if arg.is_empty() {
            return None;
        }
        self.player_table
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(arg))
    }

    /// UPSERT a player_table row (keyed on idnum), keeping the index fresh as
    /// players are created/saved/enter. C rebuilds the whole table after a
    /// create_entry(); we update the single row in place (or append it). A
    /// negative idnum or empty name is ignored (mobs / not-yet-allocated).
    pub fn update_player_index(
        &mut self,
        idnum: i64,
        name: &str,
        level: u8,
        last_logon: i64,
        host: &str,
    ) {
        if idnum < 0 || name.is_empty() {
            return;
        }
        if let Some(p) = self.player_table.iter_mut().find(|p| p.idnum == idnum) {
            p.name = name.to_string();
            p.level = level;
            p.last_logon = last_logon;
            // Preserve a known host if the caller has none (a save with no live
            // descriptor shouldn't blank the host the last login recorded).
            if !host.is_empty() {
                p.host = host.to_string();
            }
        } else {
            self.player_table.push(PlayerIndex {
                idnum,
                name: name.to_string(),
                level,
                class: crate::types::Class::Warrior,
                last_logon,
                host: host.to_string(),
                act_flags: 0,
                clan: -1,
                clan_rank: -1,
            });
        }
    }

    pub fn update_player_index_from_character(
        &mut self,
        ch: &crate::character::Character,
        last_logon: i64,
        host: &str,
    ) {
        self.update_player_index(ch.idnum, ch.get_name(), ch.player.level, last_logon, host);
        if let Some(p) = self.player_table.iter_mut().find(|p| p.idnum == ch.idnum) {
            p.class = ch.player.class;
            p.act_flags = ch.act_flags;
            p.clan = ch.clan;
            p.clan_rank = ch.clan_rank;
        }
    }

    /// Queue a deferred immortal command against an OFFLINE player (the async
    /// bridge for set/stat/show on a logged-off record). Called from cmd_wizard
    /// when the target is offline-but-indexed; drained next heartbeat by game.rs.
    /// `command` must be the immortal's ORIGINAL command verbatim so the replay
    /// re-parses the identical field/value.
    pub fn queue_offline_op(&mut self, requester: CharId, target: &str, command: &str) {
        self.offline_ops.push(OfflineOp {
            requester,
            target: target.to_string(),
            command: command.to_string(),
        });
    }

    /// Queue `pfileclean`'s async DB cleanup. The command path is synchronous,
    /// so game.rs drains this between awaits and rebuilds player_table after
    /// deleting PLR_DELETED rows from persistent storage.
    pub fn queue_pfileclean(&mut self) {
        self.pfileclean_requested = true;
    }

    pub fn take_pfileclean_request(&mut self) -> bool {
        std::mem::take(&mut self.pfileclean_requested)
    }

    /// Queue a live PC row save for the async game loop. This is the sync
    /// command equivalent of C `save_char(ch, NOWHERE)` for handlers that cannot
    /// await the database directly.
    pub fn request_player_save(&mut self, ch: CharId) {
        if !self.player_save_requests.contains(&ch) {
            self.player_save_requests.push(ch);
        }
    }

    pub fn take_player_save_requests(&mut self) -> Vec<CharId> {
        std::mem::take(&mut self.player_save_requests)
    }

    // ---- Objects --------------------------------------------------------
    pub fn get_obj(&self, id: ObjId) -> Option<&Object> {
        self.objs.get(&id)
    }
    pub fn get_obj_mut(&mut self, id: ObjId) -> Option<&mut Object> {
        self.objs.get_mut(&id)
    }
    pub fn create_obj(&mut self, mut obj: Object) -> ObjId {
        let id = ObjId(self.next_obj_id);
        self.next_obj_id += 1;
        obj.id = id;
        self.objs.insert(id, obj);
        id
    }

    /// Remove an object from the world entirely (recursively extracts any
    /// contents). Caller must have already detached it from its location.
    pub fn extract_obj(&mut self, id: ObjId) {
        let contents = self
            .objs
            .get(&id)
            .map(|o| o.contains.clone())
            .unwrap_or_default();
        for c in contents {
            self.extract_obj(c);
        }
        // swap_remove: O(1) removal from the ordered arena (reorders the tail,
        // which is fine — iteration order is internal-only).
        self.objs.swap_remove(&id);
    }

    /// WAIT_STATE(ch, cycles) (utils.h): impose `cycles` pulses of command lag.
    /// PCs store this on their descriptor; NPCs use char_specials.wait_state
    /// (`mob_wait`) so perform_violence can count down bash/trip recovery.
    pub fn set_wait_state(&mut self, id: CharId, cycles: i32) {
        if let Some(c) = self.chars.get_mut(&id) {
            if c.is_npc {
                c.mob_wait = cycles;
                return;
            }
        }
        if let Some(conn) = self.chars.get(&id).and_then(|c| c.desc) {
            if let Some(d) = self.descriptors.get_mut(&conn) {
                d.wait = cycles;
            }
        }
    }

    // ---- Output ---------------------------------------------------------
    /// Append raw text to a character's connection buffer (C send_to_char).
    pub fn send_to_char(&mut self, id: CharId, msg: &str) {
        if msg.is_empty() {
            return;
        }
        let conn = match self.chars.get(&id).and_then(|c| c.desc) {
            Some(c) => c,
            None => return,
        };
        if let Some(d) = self.descriptors.get_mut(&conn) {
            d.outbuf.push_str(msg);
        }
        // Snoop relay (comm.c process_output): if this character is being
        // snooped, tee its output to the snooper, prefixed "% " / suffixed "%%".
        if let Some(snooper) = self.chars.get(&id).and_then(|c| c.snoop_by) {
            if let Some(sconn) = self.chars.get(&snooper).and_then(|c| c.desc) {
                if let Some(sd) = self.descriptors.get_mut(&sconn) {
                    sd.outbuf.push_str("% ");
                    sd.outbuf.push_str(msg);
                    sd.outbuf.push_str("%%");
                }
            }
        }
    }

    /// Convenience: append a line (adds CRLF), matching most C send_to_char
    /// callers that include "\r\n".
    pub fn send_line(&mut self, id: CharId, msg: &str) {
        let conn = match self.chars.get(&id).and_then(|c| c.desc) {
            Some(c) => c,
            None => return,
        };
        if let Some(d) = self.descriptors.get_mut(&conn) {
            d.outbuf.push_str(msg);
            d.outbuf.push_str("\r\n");
        }
    }

    /// Send to everyone in a room except optionally one character.
    pub fn send_to_room(&mut self, rnum: RoomRnum, msg: &str, exclude: Option<CharId>) {
        let people = match self.rooms.get(rnum) {
            Some(r) => r.people.clone(),
            None => return,
        };
        for id in people {
            if Some(id) == exclude {
                continue;
            }
            self.send_to_char(id, msg);
        }
    }

    /// Send to every playing descriptor (for shouts / wiznet later).
    pub fn send_to_all_players(&mut self, msg: &str) {
        let ids: Vec<CharId> = self.players_by_name.values().copied().collect();
        for id in ids {
            self.send_to_char(id, msg);
        }
    }

    // ---- Misc -----------------------------------------------------------
    /// Equivalent of CircleMUD's GET_ROOM_VNUM(IN_ROOM(ch)).
    pub fn char_room_vnum(&self, id: CharId) -> Option<RoomVnum> {
        let rnum = self.chars.get(&id)?.in_room?;
        Some(self.rooms[rnum].number)
    }

    /// Set PLR_CRASH on a non-NPC (C handler.c SET_BIT(PLR_FLAGS, PLR_CRASH)
    /// in obj_to_char / obj_from_char / (un)equip_char). Flags the player for the
    /// next crash_save_all so carried/worn objects survive a crash/restart
    /// (BUG 14). No-op for NPCs / missing chars.
    pub fn mark_crash(&mut self, cid: CharId) {
        if let Some(c) = self.chars.get_mut(&cid) {
            if !c.is_npc {
                c.act_flags |= crate::objsave::PLR_CRASH;
            }
        }
    }

    /// Detach an object from wherever it currently sits (room/char/container).
    /// Leaves the object in the arena with loc = Nowhere.
    pub fn obj_from_anywhere(&mut self, oid: ObjId) {
        let loc = match self.objs.get(&oid) {
            Some(o) => o.loc,
            None => return,
        };
        match loc {
            ObjLoc::Room(rnum) => {
                if let Some(r) = self.rooms.get_mut(rnum) {
                    r.contents.retain(|&o| o != oid);
                }
            }
            ObjLoc::Carried(cid) => {
                // Mirror C obj_from_char (handler.c:551): drop from the carry
                // list AND decrement the carrier's encumbrance (BUG 7 — the
                // weight/count were leaking, so get->drop netted a positive
                // weight every round). Symmetric with obj_to_char, which adds
                // both. Then flag PLR_CRASH (BUG 14).
                let weight = self.objs.get(&oid).map(|o| o.weight).unwrap_or(0);
                if let Some(c) = self.chars.get_mut(&cid) {
                    c.carrying.retain(|&o| o != oid);
                    c.carry_weight -= weight;
                    c.carry_items = c.carry_items.saturating_sub(1);
                }
                self.mark_crash(cid);
            }
            ObjLoc::Worn(cid, pos) => {
                // Worn items are NOT counted in carry_weight/carry_items (C
                // equip_char never adds them), so removing one only clears the
                // slot — no encumbrance adjustment. Flag PLR_CRASH (BUG 14).
                if let Some(c) = self.chars.get_mut(&cid) {
                    if pos < NUM_WEARS && c.equipment[pos] == Some(oid) {
                        c.equipment[pos] = None;
                    }
                }
                self.mark_crash(cid);
            }
            ObjLoc::Contained(container) => {
                let weight = self.objs.get(&oid).map(|o| o.weight).unwrap_or(0);
                if let Some(c) = self.objs.get_mut(&container) {
                    c.contains.retain(|&o| o != oid);
                }
                self.adjust_container_chain_weight(container, -weight);
            }
            ObjLoc::Nowhere => {}
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Nowhere;
        }
    }
}
