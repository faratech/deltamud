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
use std::collections::HashMap;

pub struct GameState {
    // Static world (loaded at boot; mutated by resets / OLC).
    pub rooms: Vec<Room>,
    pub room_index: HashMap<RoomVnum, RoomRnum>,
    pub zones: Vec<Zone>,
    pub mob_protos: HashMap<MobVnum, MobileProto>,
    pub obj_protos: HashMap<ObjVnum, ObjectProto>,

    // Live instances. `*_list` preserve C's character_list/object_list order
    // (newest first) for iteration parity; the maps are for id lookup.
    pub chars: HashMap<CharId, Character>,
    pub char_list: Vec<CharId>,
    pub objs: HashMap<ObjId, Object>,
    pub obj_list: Vec<ObjId>,

    // Connections (the Descriptor lives here; the async output channel lives
    // in the Game wrapper keyed by the same ConnId).
    pub descriptors: HashMap<ConnId, Descriptor>,
    pub players_by_name: HashMap<String, CharId>,

    next_char_id: u64,
    next_obj_id: u64,

    pub rng: Rng,
    pub motd: String,
    pub config: Config,
    pub pulse: u64,

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
            chars: HashMap::new(),
            char_list: Vec::new(),
            objs: HashMap::new(),
            obj_list: Vec::new(),
            descriptors: HashMap::new(),
            players_by_name: HashMap::new(),
            next_char_id: 1,
            next_obj_id: 1,
            rng: Rng::default(),
            motd: String::new(),
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

    /// Insert a character into the world (assigns id, prepends to char_list
    /// like CircleMUD's character_list). Does NOT place it in a room.
    pub fn create_char(&mut self, mut ch: Character) -> CharId {
        let id = CharId(self.next_char_id);
        self.next_char_id += 1;
        ch.id = id;
        self.chars.insert(id, ch);
        self.char_list.insert(0, id);
        id
    }

    pub fn find_player_by_name(&self, name: &str) -> Option<CharId> {
        self.players_by_name.get(&name.to_lowercase()).copied()
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
        self.obj_list.insert(0, id);
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
        self.objs.remove(&id);
        self.obj_list.retain(|&o| o != id);
    }

    /// WAIT_STATE(ch, cycles) (utils.h): impose `cycles` pulses of command lag
    /// on the character's descriptor. The heartbeat's input drain won't run the
    /// next queued command until this counter decrements to <= 0.
    pub fn set_wait_state(&mut self, id: CharId, cycles: i32) {
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
                if let Some(c) = self.chars.get_mut(&cid) {
                    c.carrying.retain(|&o| o != oid);
                }
            }
            ObjLoc::Worn(cid, pos) => {
                if let Some(c) = self.chars.get_mut(&cid) {
                    if pos < NUM_WEARS && c.equipment[pos] == Some(oid) {
                        c.equipment[pos] = None;
                    }
                }
            }
            ObjLoc::Contained(container) => {
                if let Some(c) = self.objs.get_mut(&container) {
                    c.contains.retain(|&o| o != oid);
                }
            }
            ObjLoc::Nowhere => {}
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Nowhere;
        }
    }
}
