// handler.rs — the shared mutators and finders every command relies on
// (CircleMUD handler.c), ported to the id-indexed GameState. Adds them as
// inherent `impl GameState` methods so commands call one canonical version.

use crate::character::{CharPoints, Character};
use crate::flags::*;
use crate::object::{ObjLoc, ObjectGraphOrder, ObjectType, walk_object_graph};
use crate::state::GameState;
use crate::types::*;

const SECS_PER_REAL_SEC: i64 = 1;
const SECS_PER_REAL_MIN: i64 = 60 * SECS_PER_REAL_SEC;
const SECS_PER_REAL_HOUR: i64 = 60 * SECS_PER_REAL_MIN;
const SECS_PER_MUD_HOUR: i64 = 75;
const SECS_PER_MUD_DAY: i64 = 24 * SECS_PER_MUD_HOUR;
const SECS_PER_MUD_MONTH: i64 = 35 * SECS_PER_MUD_DAY;
const SECS_PER_MUD_YEAR: i64 = 17 * SECS_PER_MUD_MONTH;

/// Port of CircleMUD isname() (handler.c:59): true if `arg` abbreviates any
/// whitespace-separated keyword in `namelist` — case-insensitive PREFIX match
/// via is_abbrev, so "swo" matches "sword" (issue #216). is_abbrev rejects an
/// empty abbreviation, matching the guard below.
pub fn isname(arg: &str, namelist: &str) -> bool {
    if arg.is_empty() {
        return false;
    }
    let arg = arg.to_lowercase();
    namelist
        .split_whitespace()
        .any(|word| word.to_lowercase().starts_with(&arg))
}

/// Parse "3.sword" -> (3, "sword"); "sword" -> (1, "sword").
/// "all.coin" -> (i32::MAX, "coin"); "all" -> (i32::MAX, "all" w/ all-flag).
pub fn get_number(arg: &str) -> (i32, String) {
    if let Some((num, name)) = arg.split_once('.') {
        if num.eq_ignore_ascii_case("all") {
            return (i32::MAX, name.to_string());
        }
        match crate::text::parse_i32_strict(num) {
            Ok(n) => return (n, name.to_string()),
            Err(crate::text::ParseIntError::Overflow) => {
                log::warn!("object/character ordinal is outside i32 range: {num:?}");
                return (0, name.to_string());
            }
            Err(crate::text::ParseIntError::Empty | crate::text::ParseIntError::Invalid) => {}
        }
    }
    (1, arg.to_string())
}

impl GameState {
    // ---- GMCP dirty tracking (Deltania Breathes W5) --------------------
    /// Mark a character's connection as having stale GMCP state. No-op for
    /// npcs and characters without a descriptor.
    pub fn note_gmcp(&mut self, cid: CharId) {
        if let Some(conn) = self.chars.get(&cid).and_then(|c| c.desc) {
            self.gmcp_dirty.insert(conn);
        }
    }

    /// Mark every character in a room (room-transfers make the bystanders'
    /// Room.Info / occupancy stale too).
    pub fn note_gmcp_room(&mut self, rnum: RoomRnum) {
        let people = match self.rooms.get(rnum) {
            Some(r) => r.people.clone(),
            None => return,
        };
        for cid in people {
            self.note_gmcp(cid);
        }
    }

    // ---- Character placement -------------------------------------------
    /// CircleMUD char_to_room: prepend to room.people (newest first).
    pub fn char_to_room(&mut self, cid: CharId, rnum: RoomRnum) {
        if rnum >= self.rooms.len() {
            return;
        }
        // Every real relocation converges here. If an arena participant is
        // placed outside arena space, restore/clear them before any later save
        // can snapshot the stripped arena values (#414).
        crate::arena::arena_departure_on_relocation(self, cid, Some(rnum));
        // Everyone in the destination sees an arrival; the mover's Room.Info
        // goes stale (W5 event-driven GMCP).
        self.note_gmcp_room(rnum);
        self.note_gmcp(cid);
        self.rooms[rnum].people.insert(0, cid);
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.in_room = Some(rnum);
        }
        // C handler.c:496-499: entering a room different from your opponent's
        // breaks the fight - both sides stop (#111).
        if let Some(f) = self.chars.get(&cid).and_then(|ch| ch.fighting) {
            let f_room = self.chars.get(&f).and_then(|c| c.in_room);
            if f_room != Some(rnum) {
                crate::combat::stop_fighting(self, f);
                crate::combat::stop_fighting(self, cid);
            }
        }
        self.adjust_room_light_for_char(cid, rnum, 1);
    }

    pub fn char_from_room(&mut self, cid: CharId) {
        let rnum = match self.chars.get(&cid).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        // Departures are visible to the room left behind (W5 event-driven GMCP).
        self.note_gmcp_room(rnum);
        self.adjust_room_light_for_char(cid, rnum, -1);
        if let Some(room) = self.rooms.get_mut(rnum) {
            room.people.retain(|&c| c != cid);
        }
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.in_room = None;
        }
        // C handler.c:431-432: leaving a room stops any fight (#111).
        crate::combat::stop_fighting(self, cid);
    }

    /// C handler.c affect_modify(..., FALSE) + affect_remove: strip every
    /// affect of a spell, CLEAR its affect bits from the flag word (the
    /// old retain()s left bits stuck), then recompute (#98).
    pub fn affect_remove_spell(&mut self, cid: CharId, spell: i32) {
        let mut cleared = 0i64;
        if let Some(ch) = self.chars.get_mut(&cid) {
            for a in ch.affected.iter().filter(|a| a.spell_type == spell) {
                cleared |= a.bitvector;
            }
            ch.affected.retain(|a| a.spell_type != spell);
            ch.affect_flags &= !cleared;
        }
        self.affect_total(cid);
    }

    // ---- Object placement ----------------------------------------------
    pub fn obj_to_room(&mut self, oid: ObjId, rnum: RoomRnum) {
        if rnum >= self.rooms.len() {
            return;
        }
        let Some(object) = self.objs.get(&oid) else {
            log::warn!(
                "SYSERR: obj_to_room rejected missing object {:?} for room {}",
                oid,
                rnum,
            );
            return;
        };
        if object.loc != ObjLoc::Nowhere {
            log::warn!(
                "SYSERR: obj_to_room rejected double-parenting object {:?} vnum {} from {:?} into room {}",
                oid,
                object.item_number,
                object.loc,
                rnum,
            );
            return;
        }
        self.rooms[rnum].contents.insert(0, oid);
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Room(rnum);
        }
        // C handler.c:884-887: dropping an object in a house flags the house
        // for its next crashsave (ROOM_HOUSE_CRASH, C bit 12 - the RoomFlags
        // name for that bit is NO_RECALL, hence the raw test) (#164).
        if self.rooms[rnum]
            .room_flags
            .contains(crate::room::RoomFlags::HOUSE)
            && self.rooms[rnum].room_flags.bits() & (1 << 12) == 0
        {
            let bits = self.rooms[rnum].room_flags.bits() | (1 << 12);
            self.rooms[rnum].room_flags = crate::room::RoomFlags::from_bits_retain(bits);
        }
    }

    pub fn obj_to_char(&mut self, oid: ObjId, cid: CharId) {
        let Some(object) = self.objs.get(&oid) else {
            log::warn!(
                "SYSERR: obj_to_char rejected missing object {:?} for character {:?}",
                oid,
                cid,
            );
            return;
        };
        if !self.chars.contains_key(&cid) {
            log::warn!(
                "SYSERR: obj_to_char rejected object {:?} vnum {} for missing character {:?}",
                oid,
                object.item_number,
                cid,
            );
            return;
        }
        if object.loc != ObjLoc::Nowhere {
            log::warn!(
                "SYSERR: obj_to_char rejected double-parenting object {:?} vnum {} from {:?} onto character {:?}",
                oid,
                object.item_number,
                object.loc,
                cid,
            );
            return;
        }
        let weight = object.weight;
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.carrying.insert(0, oid);
            ch.carry_weight += weight;
            ch.carry_items = ch.carry_items.saturating_add(1);
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Carried(cid);
        }
        // C handler.c:542 — flag the PC for crash-save (BUG 14).
        self.mark_crash(cid);
    }

    pub fn obj_to_obj(&mut self, oid: ObjId, container: ObjId) {
        let object_vnum = self.objs.get(&oid).map(|object| object.item_number);
        let container_vnum = self.objs.get(&container).map(|object| object.item_number);
        if object_vnum.is_none() || container_vnum.is_none() {
            log::warn!(
                "SYSERR: obj_to_obj rejected missing object/container: object={:?} vnum={:?}, container={:?} vnum={:?}",
                oid,
                object_vnum,
                container,
                container_vnum,
            );
            return;
        }
        if oid == container {
            log::warn!(
                "SYSERR: obj_to_obj rejected direct cycle for {:?} (vnum {:?})",
                oid,
                object_vnum,
            );
            return;
        }
        let object_location = self.objs.get(&oid).map(|object| object.loc);
        if object_location != Some(ObjLoc::Nowhere) {
            log::warn!(
                "SYSERR: obj_to_obj rejected double-parenting for {:?} (vnum {:?}); existing location {:?}, requested container {:?} (vnum {:?})",
                oid,
                object_vnum,
                object_location,
                container,
                container_vnum,
            );
            return;
        }

        // Follow the destination's parent chain before mutating either side.
        // If it already reaches `oid`, insertion would create an indirect cycle.
        // A pre-existing malformed cycle in the destination chain is rejected
        // as well, so this mutator never makes a damaged graph harder to repair.
        let mut current = container;
        let mut ancestors = std::collections::HashSet::new();
        loop {
            if current == oid {
                log::warn!(
                    "SYSERR: obj_to_obj rejected indirect cycle: object {:?} (vnum {:?}), container {:?} (vnum {:?})",
                    oid,
                    object_vnum,
                    container,
                    container_vnum,
                );
                return;
            }
            if !ancestors.insert(current) {
                log::warn!(
                    "SYSERR: obj_to_obj rejected malformed destination ancestry near {:?}: object {:?} (vnum {:?}), container {:?} (vnum {:?})",
                    current,
                    oid,
                    object_vnum,
                    container,
                    container_vnum,
                );
                return;
            }
            match self.objs.get(&current).map(|object| object.loc) {
                Some(ObjLoc::Contained(parent)) => current = parent,
                Some(_) => break,
                None => {
                    log::warn!(
                        "SYSERR: obj_to_obj rejected missing ancestor {:?} while placing {:?} (vnum {:?}) into {:?} (vnum {:?})",
                        current,
                        oid,
                        object_vnum,
                        container,
                        container_vnum,
                    );
                    return;
                }
            }
        }

        let weight = self.objs.get(&oid).map(|o| o.weight).unwrap_or(0);
        if let Some(c) = self.objs.get_mut(&container) {
            c.contains.insert(0, oid);
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Contained(container);
        }
        self.adjust_container_chain_weight(container, weight);
    }

    pub(crate) fn adjust_container_chain_weight(&mut self, container: ObjId, delta: i32) {
        let mut current = container;
        let mut visited = std::collections::HashSet::new();
        let mut path = Vec::new();
        let carrier = loop {
            if !visited.insert(current) {
                log::warn!(
                    "SYSERR: container weight update rejected cyclic ancestry near {:?}; no weights changed",
                    current
                );
                return;
            }
            let Some(object) = self.objs.get(&current) else {
                log::warn!(
                    "SYSERR: container weight update rejected missing ancestor {:?}; no weights changed",
                    current
                );
                return;
            };
            path.push(current);
            match object.loc {
                ObjLoc::Contained(parent) => current = parent,
                ObjLoc::Carried(character) => break Some(character),
                _ => break None,
            }
        };

        for id in path {
            if let Some(object) = self.objs.get_mut(&id) {
                object.weight = object.weight.saturating_add(delta);
            }
        }
        if let Some(character) = carrier {
            if let Some(character) = self.chars.get_mut(&character) {
                character.carry_weight = character.carry_weight.saturating_add(delta);
            }
        }
    }

    /// Equip a detached item into an empty worn slot.
    pub fn equip_char(&mut self, cid: CharId, oid: ObjId, pos: usize) {
        if pos >= NUM_WEARS {
            return;
        }
        let Some(object) = self.objs.get(&oid) else {
            log::warn!(
                "SYSERR: equip_char rejected missing object {:?} for character {:?} slot {}",
                oid,
                cid,
                pos,
            );
            return;
        };
        let slot_empty = self
            .chars
            .get(&cid)
            .is_some_and(|character| character.equipment[pos].is_none());
        if !slot_empty {
            log::warn!(
                "SYSERR: equip_char rejected object {:?} vnum {} for missing character {:?} or occupied slot {}",
                oid,
                object.item_number,
                cid,
                pos,
            );
            return;
        }
        if object.loc != ObjLoc::Nowhere {
            log::warn!(
                "SYSERR: equip_char rejected double-parenting object {:?} vnum {} from {:?} onto character {:?} slot {}",
                oid,
                object.item_number,
                object.loc,
                cid,
                pos,
            );
            return;
        }
        // C handler.c:699-707 equip: affect_modify(..., obj bitvector, TRUE)
        // grants the item's affect bits while worn ("wearing this grants
        // infravision/sanctuary/..." items; #98).
        let bv = self.objs.get(&oid).map(|o| o.bitvector).unwrap_or(0);
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.equipment[pos] = Some(oid);
            ch.affect_flags |= bv;
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Worn(cid, pos);
        }
        self.adjust_room_light_for_equipment(cid, oid, pos, 1);
        // C equip path flags the PC for crash-save (BUG 14).
        self.mark_crash(cid);
        self.affect_total(cid);
        self.enforce_weapon_restriction(cid);
    }

    /// C handler.c:665-681: after any equip, an already-wielded weapon whose
    /// damage potential exceeds the wielder's level ceiling is force-unequipped
    /// ('$p fumbles out of $n's inexperienced hands.'). Requires the crate's
    /// weapon-ceiling helper (#122).
    fn enforce_weapon_restriction(&mut self, cid: CharId) {
        use crate::object::ObjectType;
        let level = self
            .get_char(cid)
            .map(|c| c.player.level)
            .unwrap_or(LVL_IMPL);
        if level >= LVL_IMMORT || crate::cmd_item::weapon_restrictions() <= 0 {
            return;
        }
        let wielded = self
            .chars
            .get(&cid)
            .and_then(|c| c.equipment[crate::types::WEAR_WIELD]);
        let Some(w) = wielded else { return };
        let (ty, v1, v2) = match self.objs.get(&w) {
            Some(o) => (o.obj_type, o.values[1], o.values[2]),
            None => return,
        };
        if ty != ObjectType::Weapon {
            return;
        }
        let potential = ((v2 + 1) as f64 / 2.0) * v1 as f64;
        if potential > crate::cmd_item::lvl_maxdmg_weapon(level as usize) as f64 {
            let (on, cid_name) = match (self.objs.get(&w), self.chars.get(&cid)) {
                (Some(o), Some(c)) => (o.short_description.clone(), c.get_name().to_string()),
                _ => return,
            };
            if let Some(oid) = self.unequip_char(cid, crate::types::WEAR_WIELD) {
                self.obj_to_char(oid, cid);
            }
            self.send_to_room(
                self.chars.get(&cid).and_then(|c| c.in_room).unwrap_or(0),
                &format!(
                    "{} fumbles out of {}'s inexperienced hands.\r\n",
                    on, cid_name
                ),
                None,
            );
        }
    }

    pub fn unequip_char(&mut self, cid: CharId, pos: usize) -> Option<ObjId> {
        if pos >= NUM_WEARS {
            return None;
        }
        let oid = self
            .chars
            .get_mut(&cid)
            .and_then(|ch| ch.equipment[pos].take())?;
        // C handler.c:743-757 unequip: the item's affect bits are removed.
        let bv = self.objs.get(&oid).map(|o| o.bitvector).unwrap_or(0);
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.affect_flags &= !bv;
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Nowhere;
        }
        self.adjust_room_light_for_equipment(cid, oid, pos, -1);
        // C unequip path flags the PC for crash-save (BUG 14).
        self.mark_crash(cid);
        self.affect_total(cid);
        Some(oid)
    }

    fn adjust_room_light_for_char(&mut self, cid: CharId, rnum: RoomRnum, delta: i32) {
        let has_lit_light = self
            .chars
            .get(&cid)
            .and_then(|ch| ch.equipment[WEAR_LIGHT])
            .map(|oid| self.is_lit_light(oid))
            .unwrap_or(false);
        if has_lit_light {
            self.adjust_room_light(rnum, delta);
        }
    }

    fn adjust_room_light_for_equipment(&mut self, cid: CharId, oid: ObjId, pos: usize, delta: i32) {
        if pos != WEAR_LIGHT || !self.is_lit_light(oid) {
            return;
        }
        if let Some(rnum) = self.chars.get(&cid).and_then(|c| c.in_room) {
            self.adjust_room_light(rnum, delta);
        }
    }

    fn is_lit_light(&self, oid: ObjId) -> bool {
        self.objs
            .get(&oid)
            .map(|o| o.obj_type == ObjectType::Light && o.values[2] > 0)
            .unwrap_or(false)
    }

    fn adjust_room_light(&mut self, rnum: RoomRnum, delta: i32) {
        if let Some(room) = self.rooms.get_mut(rnum) {
            if delta > 0 {
                room.light = room.light.saturating_add(delta as u8);
            } else if delta < 0 {
                room.light = room.light.saturating_sub((-delta) as u8);
            }
        }
    }

    /// die_follower (utils.c): a character that follows / is followed is being
    /// removed — detach it from its master's follower list and stop every one of
    /// its own followers, clearing BOTH sides of each link so no dangling
    /// master/followers id survives the extract (BUG 22). Mirrors C
    /// handler.c:1080-1081 (`if (ch->followers || ch->master) die_follower(ch)`).
    /// Link-breaking only — the cosmetic "stops following" act() messages are
    /// skipped (the char is leaving the world; its room view is gone).
    fn die_follower(&mut self, cid: CharId) {
        // If we follow someone, remove us from their follower list + drop AFF_*.
        if let Some(master) = self.chars.get(&cid).and_then(|c| c.master) {
            if let Some(m) = self.chars.get_mut(&master) {
                m.followers.retain(|&f| f != cid);
            }
            if let Some(c) = self.chars.get_mut(&cid) {
                c.master = None;
                c.affect_flags &= !(AFF_CHARM | AFF_GROUP);
            }
        }
        // Stop everyone who follows us (clear their master + group/charm flags).
        let followers = self
            .chars
            .get(&cid)
            .map(|c| c.followers.clone())
            .unwrap_or_default();
        for f in followers {
            if let Some(fc) = self.chars.get_mut(&f) {
                fc.master = None;
                fc.affect_flags &= !(AFF_CHARM | AFF_GROUP);
            }
        }
        if let Some(c) = self.chars.get_mut(&cid) {
            c.followers.clear();
        }
    }

    /// Remove a character from the world. Detaches from room, drops fighters,
    /// and extracts inventory/equipment. PCs are normally respawned instead.
    pub fn extract_char(&mut self, cid: CharId) {
        // Restore arena-backed state while the Character still exists. The
        // helper is idempotent, so disconnect/death paths may already have run
        // it before reaching extract_char.
        crate::arena::arena_departure_on_relocation(self, cid, None);

        // C handler.c:1080 — detach from the follow graph before anything else,
        // so no master/follower id is left dangling (BUG 22).
        self.die_follower(cid);

        // Arena bookkeeping: clear ARENA_COMBATANT* / observer references so a
        // dead or purged combatant no longer satisfies is_arena_combatant
        // (issue #392 -- observers otherwise attach to ghosts forever).
        crate::arena::forget_char(self, cid);

        // Stop anyone targeting this character.
        let attackers: Vec<CharId> = self
            .chars
            .iter()
            .filter(|(_, x)| x.fighting == Some(cid))
            .map(|(&c, _)| c)
            .collect();
        for a in attackers {
            if let Some(ch) = self.chars.get_mut(&a) {
                ch.fighting = None;
                if ch.position == Position::Fighting {
                    ch.position = Position::Standing;
                }
            }
        }

        // Break any mount link (handler.c extract_char -> dismount_char).
        let riding = self.chars.get(&cid).and_then(|c| c.riding);
        if let Some(m) = riding {
            if let Some(mc) = self.chars.get_mut(&m) {
                mc.ridden_by = None;
            }
        }
        let ridden_by = self.chars.get(&cid).and_then(|c| c.ridden_by);
        if let Some(r) = ridden_by {
            if let Some(rc) = self.chars.get_mut(&r) {
                rc.riding = None;
            }
        }
        if let Some(c) = self.chars.get_mut(&cid) {
            c.riding = None;
            c.ridden_by = None;
        }

        // Forget snooping (comm.c close_socket / handler.c extract_char). If we
        // were snooping someone, clear their snoop_by; if someone was snooping
        // us, clear their snooping (and they lose the live feed).
        let snooping = self.chars.get(&cid).and_then(|c| c.snooping);
        if let Some(v) = snooping {
            if let Some(vc) = self.chars.get_mut(&v) {
                vc.snoop_by = None;
            }
        }
        let snoop_by = self.chars.get(&cid).and_then(|c| c.snoop_by);
        if let Some(s) = snoop_by {
            self.send_to_char(s, "Your victim is no longer among us.\r\n");
            if let Some(sc) = self.chars.get_mut(&s) {
                sc.snooping = None;
            }
        }

        self.char_from_room(cid);

        // C handler.c:1101-1112: extraction leaves the character's objects
        // behind - carried items and worn equipment are dropped in the room
        // ('purge' / force-rent must not destroy gear; #103).
        let (carried, worn, in_room) = match self.chars.get(&cid) {
            Some(ch) => (
                ch.carrying.clone(),
                ch.equipment.iter().flatten().copied().collect::<Vec<_>>(),
                ch.in_room,
            ),
            None => (Vec::new(), Vec::new(), None),
        };
        if let Some(rnum) = in_room {
            for o in carried {
                self.obj_from_anywhere(o);
                self.obj_to_room(o, rnum);
            }
            for p in 0..NUM_WEARS {
                if let Some(o) = self.unequip_char(cid, p) {
                    self.obj_to_room(o, rnum);
                }
            }
        } else {
            // Nowhere to drop them (void extraction) - C's obj_to_room with
            // NOWHERE is a no-op, so the objects vanish as before.
            for o in worn {
                self.extract_obj(o);
            }
            for o in carried {
                self.extract_obj(o);
            }
        }

        // Unlink descriptor / name index.
        if let Some(ch) = self.chars.get(&cid) {
            if !ch.is_npc {
                self.players_by_name.remove(&ch.player.name.to_lowercase());
            }
            if let Some(conn) = ch.desc {
                if let Some(d) = self.descriptors.get_mut(&conn) {
                    d.character = None;
                }
            }
        }
        // shift_remove (NOT swap_remove): swap_remove re-keys the last-inserted
        // character into the vacated slot while its .id keeps the OLD value —
        // a stale CharId held across an extraction would then resolve to a
        // DIFFERENT character (the extraction-race class). Extraction is
        // rare; the O(n) shift is fine.
        // ordered arena (replaces the old char_list.retain + chars.remove pair).
        self.chars.shift_remove(&cid);
    }

    // ---- Visibility -----------------------------------------------------
    /// CAN_SEE (utils.h): self is always visible; otherwise invis-level gates
    /// first, then ordinary mortal visibility or holylight decides the rest.
    pub fn can_see(&self, viewer: CharId, target: CharId) -> bool {
        if viewer == target {
            return true;
        }
        let v = match self.chars.get(&viewer) {
            Some(v) => v,
            None => return false,
        };
        let t = match self.chars.get(&target) {
            Some(t) => t,
            None => return false,
        };
        // Immortal invisibility is an authority boundary, not a gameplay-level
        // comparison. Switched sessions inherit the authenticated principal's
        // persisted trust; malformed principals fail closed at authority 0.
        let viewer_authority = self
            .principal_authority(viewer)
            .map(|principal| principal.authority)
            .unwrap_or(0);
        if t.invis_level > viewer_authority {
            return false;
        }
        if v.prf_flags & PRF_HOLYLIGHT != 0 {
            return true;
        }
        if v.affect_flags & AFF_BLIND != 0 {
            return false;
        }
        let light_ok = match v.in_room {
            Some(rnum) => !self.is_dark(rnum) || v.affect_flags & AFF_INFRAVISION != 0,
            None => true,
        };
        if !light_ok {
            return false;
        }
        if t.affect_flags & AFF_INVISIBLE != 0 && v.affect_flags & AFF_DETECT_INVIS == 0 {
            return false;
        }
        if t.affect_flags & AFF_HIDE != 0 && v.affect_flags & AFF_SENSE_LIFE == 0 {
            return false;
        }
        if t.prf2_flags & PRF2_INTANGIBLE != 0
            && v.prf2_flags & PRF2_INTANGIBLE == 0
            && t.prf2_flags & PRF2_MBUILDING == 0
        {
            return false;
        }
        true
    }

    // ---- Finders --------------------------------------------------------
    /// Find a visible character in `observer`'s room by keyword (+ optional
    /// N.name ordinal). Mirrors get_char_room_vis.
    pub fn get_char_room_vis(&self, observer: CharId, arg: &str) -> Option<CharId> {
        let rnum = self.chars.get(&observer)?.in_room?;
        // C handler.c:1208-1215: 'self'/'me' resolve to the observer, and a
        // '0.<name>' ordinal resolves the PC by name via get_player_vis
        // (count == 0 after get_number strips the '0.') (#226).
        let (mut count, name) = get_number(arg);
        if name.eq_ignore_ascii_case("self") || name.eq_ignore_ascii_case("me") {
            return Some(observer);
        }
        if count == 0 {
            // 0.<name>: world-wide player lookup.
            for (cid, ch) in self.chars.iter() {
                if !ch.is_npc && isname(&name, &ch.player.name) {
                    return Some(*cid);
                }
            }
            return None;
        }
        for &cid in &self.rooms[rnum].people {
            let ch = match self.chars.get(&cid) {
                Some(c) => c,
                None => continue,
            };
            let names = ch.player.name.clone();
            if isname(&name, &names) && self.can_see(observer, cid) {
                count -= 1;
                if count == 0 {
                    return Some(cid);
                }
            }
        }
        None
    }

    /// Find an object by keyword within a list of object ids (+ ordinal).
    /// C handler.c:1254-1275 gates each candidate on CAN_SEE_OBJ, so
    /// ITEM_INVISIBLE objects need detect-invis to target (#106).
    pub fn get_obj_in_list_vis(
        &self,
        observer: CharId,
        arg: &str,
        list: &[ObjId],
    ) -> Option<ObjId> {
        let (mut count, name) = get_number(arg);
        if count == 0 {
            return None;
        }
        for &oid in list {
            let obj = match self.objs.get(&oid) {
                Some(o) => o,
                None => continue,
            };
            if !crate::cmd_informative::can_see_obj(self, observer, oid) {
                continue;
            }
            if isname(&name, &obj.name) {
                count -= 1;
                if count == 0 {
                    return Some(oid);
                }
            }
        }
        None
    }

    // ---- Affect recomputation ------------------------------------------
    /// Recompute affected ability scores and the AFF_ bitvector from base
    /// stats + equipment affects + active spell affects (CircleMUD
    /// affect_total, Tier-0 scope: abilities + flags).
    pub fn affect_total(&mut self, cid: CharId) {
        // Collect equipment object affects + active spell affects.
        let mut mods: Vec<(i32, i32)> = Vec::new();
        // Start from the current flag word: direct-set bits (camouflage's
        // AFF_HIDE, AFF_GROUP, ...) persist across recomputes, and equipped
        // items contribute their bitvector (C affect_total strips and
        // re-applies each equipped obj_flags.bitvector, handler.c:237-270;
        // #98).
        let mut flagbits: i64 = self.chars.get(&cid).map(|c| c.affect_flags).unwrap_or(0);
        if let Some(ch) = self.chars.get(&cid) {
            for (pos, slot) in ch.equipment.iter().enumerate() {
                if let Some(oid) = *slot {
                    let Some(obj) = self.objs.get(&oid) else {
                        continue;
                    };
                    flagbits |= obj.bitvector;
                    if obj.obj_type == ObjectType::Armor {
                        mods.push((APPLY_DEFENSE, armor_ac_modifier(pos, obj.values[0])));
                    }
                    for a in &obj.affects {
                        mods.push((a.location, a.modifier));
                    }
                }
            }
            for a in &ch.affected {
                mods.push((a.location, a.modifier));
                flagbits |= a.bitvector;
            }
        }

        // Sum the modifiers that target the `points` apply fields (max_hit,
        // max_mana, max_move, defense/mdefense/power/mpower/technique). These are
        // the only point-fields apply_location writes; ability mods are handled
        // separately via aff_abils = real_abils (full reset, like C).
        let new_applied = applied_points_from_mods(&mods);
        let new_personal = personal_applied_from_mods(&mods);

        if let Some(ch) = self.chars.get_mut(&cid) {
            // Recover the bare base for each apply-target field by subtracting the
            // modifiers the PREVIOUS affect_total run layered on (ch.last_applied).
            // This absorbs any external change to `points` since then (e.g.
            // advance_level adding hp) into the base, exactly as C's strip /
            // re-apply keeps such bumps. real_points stores the bare base so it
            // persists with the character (logout->login reproduces the maxima).
            let base = points_sub(&ch.points, &ch.last_applied);
            ch.real_points = base.clone();
            // Re-inflate: points (apply targets) = base + current modifiers.
            points_assign_apply(&mut ch.points, &points_add(&base, &new_applied));
            ch.last_applied = new_applied;

            let base_birth = ch.player.time_birth - ch.last_personal_applied.birth_delta;
            let base_weight = ch.player.weight as i32 - ch.last_personal_applied.weight_delta;
            let base_height = ch.player.height as i32 - ch.last_personal_applied.height_delta;
            ch.player.time_birth = base_birth + new_personal.birth_delta;
            ch.player.weight =
                (base_weight + new_personal.weight_delta).clamp(0, u8::MAX as i32) as u8;
            ch.player.height =
                (base_height + new_personal.height_delta).clamp(0, u8::MAX as i32) as u8;
            ch.last_personal_applied = new_personal;

            // Abilities + AFF_ flags: full reset from real_abils, then re-apply
            // the ability portion of the modifier list (unchanged C semantics).
            ch.aff_abils = ch.real_abils;
            ch.affect_flags = flagbits;
            for (loc, m) in &mods {
                apply_ability(ch, *loc, *m);
            }
        }
    }
}

fn armor_ac_modifier(pos: usize, armor_value: i32) -> i32 {
    let factor = match pos {
        WEAR_BODY => 3,
        WEAR_HEAD | WEAR_LEGS => 2,
        _ => 1,
    };
    factor * armor_value
}

/// check_perm_duration (handler.c): true if `ch` carries a permanent affect
/// matching `bitvector`. C condition:
///   IS_SET(af->bitvector, bitvector) && af->duration == -1 && af->type == -1
/// Such affects (e.g. eq-granted flags reaffected via reaffect_obj_char) must
/// not be stripped by the cure/remove spells. `type == -1` maps to the Rust
/// `spell_type` field.
pub fn check_perm_duration(g: &GameState, ch: CharId, bitvector: i64) -> bool {
    g.get_char(ch)
        .map(|c| {
            c.affected.iter().any(|af| {
                (af.bitvector & bitvector) != 0 && af.duration == -1 && af.spell_type == -1
            })
        })
        .unwrap_or(false)
}

/// Apply one ability-only affect modifier (APPLY_STR..APPLY_CON) to aff_abils.
/// The point-target applies (APPLY_HIT/MANA/MOVE/DEFENSE/...) are NOT handled
/// here — affect_total recomputes those from real_points so they can't balloon
/// across repeated equip/unequip/login (see apply_location for the full table).
pub fn apply_ability(ch: &mut Character, location: i32, modifier: i32) {
    // Abilities are i8 but equipment/affect modifiers are i32: accumulate in
    // i16 and clamp, so multiple large applies can neither overflow (debug
    // panic) nor silently wrap into negative scores (release).
    let apply = |field: &mut i8| {
        *field = ((*field as i16) + (modifier as i16)).clamp(-100, 125) as i8;
    };
    match location {
        APPLY_STR => apply(&mut ch.aff_abils.str),
        APPLY_DEX => apply(&mut ch.aff_abils.dex),
        APPLY_INT => apply(&mut ch.aff_abils.intel),
        APPLY_WIS => apply(&mut ch.aff_abils.wis),
        APPLY_CON => apply(&mut ch.aff_abils.con),
        APPLY_CHA => apply(&mut ch.aff_abils.cha),
        _ => {}
    }
}

/// Apply one affect modifier to a character's affected fields. Kept for callers
/// that want the full CircleMUD apply table in one call (it writes both the
/// ability *and* the point-target fields by ADDING the modifier). affect_total
/// does NOT use this for the point fields — it recomputes them from real_points
/// via apply_ability + the CharPoints accumulator helpers below — but the
/// function remains the faithful 1:1 transcription of apply_location for any
/// one-shot caller.
pub fn apply_location(ch: &mut Character, location: i32, modifier: i32) {
    let apply = |field: &mut i8| {
        *field = ((*field as i16) + (modifier as i16)).clamp(-100, 125) as i8;
    };
    match location {
        APPLY_STR => apply(&mut ch.aff_abils.str),
        APPLY_DEX => apply(&mut ch.aff_abils.dex),
        APPLY_INT => apply(&mut ch.aff_abils.intel),
        APPLY_WIS => apply(&mut ch.aff_abils.wis),
        APPLY_CON => apply(&mut ch.aff_abils.con),
        APPLY_CHA => apply(&mut ch.aff_abils.cha),
        APPLY_AGE => ch.player.time_birth -= modifier as i64 * SECS_PER_MUD_YEAR,
        APPLY_CHAR_WEIGHT => {
            ch.player.weight = (ch.player.weight as i32 + modifier).clamp(0, u8::MAX as i32) as u8;
        }
        APPLY_CHAR_HEIGHT => {
            ch.player.height = (ch.player.height as i32 + modifier).clamp(0, u8::MAX as i32) as u8;
        }
        APPLY_HIT => ch.points.max_hit += modifier,
        APPLY_MANA => ch.points.max_mana += modifier,
        APPLY_MOVE => ch.points.max_move += modifier,
        APPLY_DEFENSE => ch.points.defense += modifier as i16,
        APPLY_MDEFENSE => ch.points.mdefense += modifier as i16,
        APPLY_POWER => ch.points.power += modifier as i16,
        APPLY_MPOWER => ch.points.mpower += modifier as i16,
        APPLY_TECHNIQUE => ch.points.technique += modifier as i16,
        _ => {}
    }
}

// --- CharPoints apply-target accumulator helpers (BUG 2) --------------------
// Only the apply-target fields (max_hit/max_mana/max_move/defense/mdefense/
// power/mpower/technique) are populated/used; all other CharPoints fields stay
// at their default and are ignored by points_assign_apply.

/// Sum a (location, modifier) list into a CharPoints holding ONLY the
/// apply-target deltas (every non-apply field stays zero).
fn applied_points_from_mods(mods: &[(i32, i32)]) -> CharPoints {
    let mut p = CharPoints::default();
    for &(loc, m) in mods {
        match loc {
            APPLY_HIT => p.max_hit += m,
            APPLY_MANA => p.max_mana += m,
            APPLY_MOVE => p.max_move += m,
            APPLY_DEFENSE => p.defense += m as i16,
            APPLY_MDEFENSE => p.mdefense += m as i16,
            APPLY_POWER => p.power += m as i16,
            APPLY_MPOWER => p.mpower += m as i16,
            APPLY_TECHNIQUE => p.technique += m as i16,
            _ => {}
        }
    }
    p
}

fn personal_applied_from_mods(mods: &[(i32, i32)]) -> crate::character::PersonalApplyState {
    let mut p = crate::character::PersonalApplyState::default();
    for &(loc, m) in mods {
        match loc {
            APPLY_AGE => p.birth_delta -= m as i64 * SECS_PER_MUD_YEAR,
            APPLY_CHAR_WEIGHT => p.weight_delta += m,
            APPLY_CHAR_HEIGHT => p.height_delta += m,
            _ => {}
        }
    }
    p
}

/// Field-wise a - b over the apply-target fields (other fields copied from `a`).
fn points_sub(a: &CharPoints, b: &CharPoints) -> CharPoints {
    let mut r = a.clone();
    r.max_hit = a.max_hit - b.max_hit;
    r.max_mana = a.max_mana - b.max_mana;
    r.max_move = a.max_move - b.max_move;
    r.defense = a.defense - b.defense;
    r.mdefense = a.mdefense - b.mdefense;
    r.power = a.power - b.power;
    r.mpower = a.mpower - b.mpower;
    r.technique = a.technique - b.technique;
    r
}

/// Field-wise a + b over the apply-target fields (other fields copied from `a`).
fn points_add(a: &CharPoints, b: &CharPoints) -> CharPoints {
    let mut r = a.clone();
    r.max_hit = a.max_hit + b.max_hit;
    r.max_mana = a.max_mana + b.max_mana;
    r.max_move = a.max_move + b.max_move;
    r.defense = a.defense + b.defense;
    r.mdefense = a.mdefense + b.mdefense;
    r.power = a.power + b.power;
    r.mpower = a.mpower + b.mpower;
    r.technique = a.technique + b.technique;
    r
}

/// Copy ONLY the apply-target fields from `src` into `dst`, leaving the live
/// current-value fields (hit/mana/move_points/gold/exp/...) untouched.
fn points_assign_apply(dst: &mut CharPoints, src: &CharPoints) {
    dst.max_hit = src.max_hit;
    dst.max_mana = src.max_mana;
    dst.max_move = src.max_move;
    dst.defense = src.defense;
    dst.mdefense = src.mdefense;
    dst.power = src.power;
    dst.mpower = src.mpower;
    dst.technique = src.technique;
}

/// Helper used by Object containment weight. The shared bounded walker keeps a
/// corrupt/deep graph from recursing into a stack overflow and counts each
/// identity at most once.
pub fn obj_total_weight(state: &GameState, oid: ObjId) -> i32 {
    walk_object_graph(
        [oid],
        ObjectGraphOrder::Preorder,
        "obj_total_weight",
        |id| state.objs.get(&id).map(|object| object.contains.clone()),
    )
    .visits
    .into_iter()
    .filter_map(|visit| state.objs.get(&visit.id).map(|object| object.weight))
    .fold(0i32, i32::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::{ConState, Descriptor};
    use crate::object::{Object, ObjectAffect, ObjectType};
    use crate::types::{Class, Race};

    #[test]
    fn isname_matches_by_prefix_like_c_is_abbrev() {
        // C isname() tokenizes the namelist and is_abbrev()es each token
        // (handler.c:59-76), so any unambiguous prefix of a keyword hits.
        assert!(isname("swo", "sword long sharp"));
        assert!(isname("SWORD", "sword long sharp"));
        assert!(isname("lon", "sword long sharp"));
        assert!(isname("sword", "sword long sharp"));
        // Not a prefix of any single token.
        assert!(!isname("ord", "sword long sharp"));
        assert!(!isname("swordlong", "sword long sharp"));
        // is_abbrev("") is false in C (utils.c), preserved by the guard.
        assert!(!isname("", "sword long sharp"));
        // Tokens longer than the arg still match; arg longer than token does not.
        assert!(isname("s", "sword"));
        assert!(!isname("swords", "sword"));
    }

    #[test]
    fn get_number_rejects_overflow_instead_of_falling_back_to_first_match() {
        assert_eq!(get_number("2147483647.sword"), (i32::MAX, "sword".into()));
        assert_eq!(get_number("2147483648.sword"), (0, "sword".into()));
        assert_eq!(get_number("-2147483649.sword"), (0, "sword".into()));
        assert_eq!(
            get_number("not-a-number.sword"),
            (1, "not-a-number.sword".into())
        );
    }

    fn fresh_game() -> GameState {
        GameState::new(Config::default())
    }

    /// An item granting +50 max_hit / +10 defense via APPLY affects.
    fn make_hit_item(g: &mut GameState) -> ObjId {
        let mut o = Object::new(1, "wings magic".into(), "a pair of wings".into());
        o.weight = 5;
        o.affects = vec![
            ObjectAffect {
                location: APPLY_HIT,
                modifier: 50,
            },
            ObjectAffect {
                location: APPLY_DEFENSE,
                modifier: 10,
            },
        ];
        g.create_obj(o)
    }

    fn visible_pair() -> (GameState, CharId, CharId) {
        let mut g = fresh_game();
        let viewer = g.create_char(Character::new_player(
            "Viewer".into(),
            Class::Warrior,
            Race::Human,
        ));
        let target = g.create_char(Character::new_player(
            "Target".into(),
            Class::Warrior,
            Race::Human,
        ));
        let rn = g.add_room(crate::room::Room::new(
            10,
            0,
            "Test".into(),
            "A room.".into(),
        ));
        g.char_to_room(viewer, rn);
        g.char_to_room(target, rn);
        (g, viewer, target)
    }

    #[test]
    fn can_see_rejects_blind_viewers() {
        let (mut g, viewer, target) = visible_pair();
        g.get_char_mut(viewer).unwrap().affect_flags |= AFF_BLIND;

        assert!(!g.can_see(viewer, target));
    }

    #[test]
    fn can_see_uses_darkness_and_infravision() {
        let (mut g, viewer, target) = visible_pair();
        let rn = g.get_char(viewer).unwrap().in_room.unwrap();
        g.room_mut(rn)
            .room_flags
            .insert(crate::room::RoomFlags::DARK);

        assert!(!g.can_see(viewer, target));

        g.get_char_mut(viewer).unwrap().affect_flags |= AFF_INFRAVISION;
        assert!(g.can_see(viewer, target));
    }

    #[test]
    fn can_see_requires_sense_life_for_hidden_targets() {
        let (mut g, viewer, target) = visible_pair();
        g.get_char_mut(target).unwrap().affect_flags |= AFF_HIDE;

        assert!(!g.can_see(viewer, target));

        g.get_char_mut(viewer).unwrap().affect_flags |= AFF_SENSE_LIFE;
        assert!(g.can_see(viewer, target));
    }

    #[test]
    fn can_see_respects_intangible_visibility_rules() {
        let (mut g, viewer, target) = visible_pair();
        g.get_char_mut(target).unwrap().prf2_flags |= PRF2_INTANGIBLE;

        assert!(!g.can_see(viewer, target));

        g.get_char_mut(viewer).unwrap().prf2_flags |= PRF2_INTANGIBLE;
        assert!(g.can_see(viewer, target));

        g.get_char_mut(viewer).unwrap().prf2_flags &= !PRF2_INTANGIBLE;
        g.get_char_mut(target).unwrap().prf2_flags |= PRF2_MBUILDING;
        assert!(g.can_see(viewer, target));
    }

    #[test]
    fn can_see_holylight_bypasses_mortal_visibility_but_not_invis_level() {
        let (mut g, viewer, target) = visible_pair();
        {
            let v = g.get_char_mut(viewer).unwrap();
            v.affect_flags |= AFF_BLIND;
            v.prf_flags |= PRF_HOLYLIGHT;
        }
        g.get_char_mut(target).unwrap().affect_flags |= AFF_HIDE | AFF_INVISIBLE;

        assert!(g.can_see(viewer, target));

        g.get_char_mut(target).unwrap().invis_level = 2;
        assert!(!g.can_see(viewer, target));
    }

    #[test]
    fn can_see_uses_persisted_trust_not_spoofable_display_level_for_invis() {
        let (mut g, viewer, target) = visible_pair();
        {
            let v = g.get_char_mut(viewer).unwrap();
            v.player.level = LVL_IMPL;
            v.trust = 1;
            v.prf_flags |= PRF_HOLYLIGHT;
        }
        g.get_char_mut(target).unwrap().invis_level = 2;
        assert!(!g.can_see(viewer, target));

        g.get_char_mut(viewer).unwrap().trust = 2;
        assert!(g.can_see(viewer, target));
    }

    /// BUG 2: repeated equip/unequip must NOT balloon max_hit / defense, and the
    /// inflated value must be exactly base+mods while worn.
    #[test]
    fn affect_total_stable_across_equip_cycles() {
        let mut g = fresh_game();
        let ch = Character::new_player("Tester".into(), Class::Warrior, Race::Human);
        let cid = g.create_char(ch);
        g.affect_total(cid);

        let base_hit = g.get_char(cid).unwrap().points.max_hit; // 20
        let base_def = g.get_char(cid).unwrap().points.defense; // 0
        assert_eq!(base_hit, 20);

        for _ in 0..5 {
            let oid = make_hit_item(&mut g);
            g.equip_char(cid, oid, WEAR_BODY);
            assert_eq!(g.get_char(cid).unwrap().points.max_hit, base_hit + 50);
            assert_eq!(g.get_char(cid).unwrap().points.defense, base_def + 10);
            g.unequip_char(cid, WEAR_BODY);
            assert_eq!(g.get_char(cid).unwrap().points.max_hit, base_hit);
            assert_eq!(g.get_char(cid).unwrap().points.defense, base_def);
            // tidy: drop the loose object back out of the world
            g.obj_from_anywhere(oid);
            g.extract_obj(oid);
        }
        // Repeated bare affect_total calls (mirrors per-login recompute) stay put.
        for _ in 0..10 {
            g.affect_total(cid);
        }
        assert_eq!(g.get_char(cid).unwrap().points.max_hit, base_hit);
    }

    #[test]
    fn armor_value_applies_slot_scaled_defense_without_doubling() {
        let mut g = fresh_game();
        let cid = g.create_char(Character::new_player(
            "Armored".into(),
            Class::Warrior,
            Race::Human,
        ));
        g.affect_total(cid);
        let base_def = g.get_char(cid).unwrap().points.defense;

        let mut armor = Object::new(2, "armor".into(), "some armor".into());
        armor.obj_type = ObjectType::Armor;
        armor.values[0] = 7;
        let oid = g.create_obj(armor);

        for (slot, expected_bonus) in [
            (WEAR_BODY, 21),
            (WEAR_HEAD, 14),
            (WEAR_LEGS, 14),
            (WEAR_SHIELD, 7),
        ] {
            g.equip_char(cid, oid, slot);
            assert_eq!(
                g.get_char(cid).unwrap().points.defense,
                base_def + expected_bonus
            );
            for _ in 0..3 {
                g.affect_total(cid);
            }
            assert_eq!(
                g.get_char(cid).unwrap().points.defense,
                base_def + expected_bonus
            );
            assert_eq!(g.unequip_char(cid, slot), Some(oid));
            assert_eq!(g.get_char(cid).unwrap().points.defense, base_def);
        }
    }

    /// BUG 2: an external base change (advance_level-style points bump) survives
    /// the affect_total strip/re-apply and combines correctly with equipment.
    #[test]
    fn affect_total_absorbs_external_base_change() {
        let mut g = fresh_game();
        let ch = Character::new_player("Grower".into(), Class::Warrior, Race::Human);
        let cid = g.create_char(ch);
        g.affect_total(cid);
        assert_eq!(g.get_char(cid).unwrap().points.max_hit, 20);

        // Simulate advance_level: bump live max_hit by 30 (no real_points edit).
        g.get_char_mut(cid).unwrap().points.max_hit += 30;
        g.affect_total(cid);
        assert_eq!(g.get_char(cid).unwrap().points.max_hit, 50); // bump preserved

        let oid = make_hit_item(&mut g);
        g.equip_char(cid, oid, WEAR_BODY);
        assert_eq!(g.get_char(cid).unwrap().points.max_hit, 100); // 50 base + 50
        g.unequip_char(cid, WEAR_BODY);
        assert_eq!(g.get_char(cid).unwrap().points.max_hit, 50);
    }

    #[test]
    fn affect_total_applies_charisma_age_weight_and_height_without_drift() {
        let mut g = fresh_game();
        let mut ch = Character::new_player("Applied".into(), Class::Warrior, Race::Human);
        ch.real_abils.cha = 12;
        ch.aff_abils = ch.real_abils;
        ch.player.time_birth = 1_000_000;
        ch.player.weight = 150;
        ch.player.height = 170;
        let cid = g.create_char(ch);

        let mut obj = Object::new(99, "trinket".into(), "a trinket".into());
        obj.affects.extend([
            crate::object::ObjectAffect {
                location: APPLY_CHA,
                modifier: 3,
            },
            crate::object::ObjectAffect {
                location: APPLY_AGE,
                modifier: 2,
            },
            crate::object::ObjectAffect {
                location: APPLY_CHAR_WEIGHT,
                modifier: 5,
            },
            crate::object::ObjectAffect {
                location: APPLY_CHAR_HEIGHT,
                modifier: -4,
            },
        ]);
        let oid = g.create_obj(obj);

        g.equip_char(cid, oid, WEAR_HOLD);
        for _ in 0..5 {
            g.affect_total(cid);
        }
        let worn = g.get_char(cid).unwrap();
        assert_eq!(worn.aff_abils.cha, 15);
        assert_eq!(worn.player.time_birth, 1_000_000 - 2 * SECS_PER_MUD_YEAR);
        assert_eq!(worn.player.weight, 155);
        assert_eq!(worn.player.height, 166);

        g.unequip_char(cid, WEAR_HOLD);
        for _ in 0..5 {
            g.affect_total(cid);
        }
        let bare = g.get_char(cid).unwrap();
        assert_eq!(bare.aff_abils.cha, 12);
        assert_eq!(bare.player.time_birth, 1_000_000);
        assert_eq!(bare.player.weight, 150);
        assert_eq!(bare.player.height, 170);
    }

    /// BUG 2 (persistence): a logout->login round-trip (clone the Character, as
    /// the mock DB does, then re-run affect_total with eq cleared) yields the
    /// SAME max_hit, not a doubled one — even if logged out while wearing eq.
    #[test]
    fn affect_total_round_trip_no_doubling() {
        let mut g = fresh_game();
        let ch = Character::new_player("Saver".into(), Class::Warrior, Race::Human);
        let cid = g.create_char(ch);
        g.affect_total(cid);
        let oid = make_hit_item(&mut g);
        g.equip_char(cid, oid, WEAR_BODY);
        assert_eq!(g.get_char(cid).unwrap().points.max_hit, 70); // worn

        // "Save": clone the inflated character (mock DB stores the clone).
        let saved = g.get_char(cid).unwrap().clone();

        // "Load" into a fresh world: restore the clone, clear eq (enter_game
        // line 647-648), then affect_total (line 650).
        let mut g2 = fresh_game();
        let mut loaded = saved;
        loaded.id = CharId(0);
        loaded.carrying.clear();
        loaded.equipment = [None; NUM_WEARS];
        loaded.aff_abils = loaded.real_abils;
        let cid2 = g2.create_char(loaded);
        g2.affect_total(cid2);
        // Eq is gone, so max_hit must be the bare base (20), NOT 70 or 120.
        assert_eq!(g2.get_char(cid2).unwrap().points.max_hit, 20);
    }

    /// BUG 7: a get -> drop round-trip nets zero carry weight / items.
    #[test]
    fn carry_weight_round_trips_to_zero() {
        let mut g = fresh_game();
        let ch = Character::new_player("Hauler".into(), Class::Warrior, Race::Human);
        let cid = g.create_char(ch);
        assert_eq!(g.get_char(cid).unwrap().carry_weight, 0);
        assert_eq!(g.get_char(cid).unwrap().carry_items, 0);

        for _ in 0..5 {
            let mut o = Object::new(2, "rock".into(), "a rock".into());
            o.weight = 17;
            let oid = g.create_obj(o);
            g.obj_to_char(oid, cid); // get
            assert_eq!(g.get_char(cid).unwrap().carry_weight, 17);
            assert_eq!(g.get_char(cid).unwrap().carry_items, 1);
            g.obj_from_anywhere(oid); // drop
            assert_eq!(g.get_char(cid).unwrap().carry_weight, 0);
            assert_eq!(g.get_char(cid).unwrap().carry_items, 0);
            g.extract_obj(oid);
        }
    }

    #[test]
    fn obj_to_obj_propagates_weight_up_container_chain() {
        let mut g = fresh_game();
        let cid = g.create_char(Character::new_player(
            "Carrier".into(),
            Class::Warrior,
            Race::Human,
        ));
        let mut outer = Object::new(10, "outer".into(), "outer container".into());
        outer.weight = 10;
        let outer = g.create_obj(outer);
        let mut inner = Object::new(11, "inner".into(), "inner container".into());
        inner.weight = 5;
        let inner = g.create_obj(inner);
        let mut gem = Object::new(12, "gem".into(), "a gem".into());
        gem.weight = 2;
        let gem = g.create_obj(gem);

        g.obj_to_char(outer, cid);
        assert_eq!(g.get_char(cid).unwrap().carry_weight, 10);

        g.obj_to_obj(inner, outer);
        assert_eq!(g.get_obj(outer).unwrap().weight, 15);
        assert_eq!(g.get_char(cid).unwrap().carry_weight, 15);

        g.obj_to_obj(gem, inner);
        assert_eq!(g.get_obj(inner).unwrap().weight, 7);
        assert_eq!(g.get_obj(outer).unwrap().weight, 17);
        assert_eq!(g.get_char(cid).unwrap().carry_weight, 17);

        g.obj_from_anywhere(gem);
        assert_eq!(g.get_obj(inner).unwrap().weight, 5);
        assert_eq!(g.get_obj(outer).unwrap().weight, 15);
        assert_eq!(g.get_char(cid).unwrap().carry_weight, 15);
    }

    #[test]
    fn obj_to_obj_rejects_direct_indirect_and_double_parenting_without_mutation() {
        let mut g = fresh_game();
        let mut a = Object::new(101, "a".into(), "container a".into());
        a.weight = 10;
        let a = g.create_obj(a);
        let mut b = Object::new(102, "b".into(), "container b".into());
        b.weight = 5;
        let b = g.create_obj(b);
        let child = g.create_obj(Object::new(103, "child".into(), "a child".into()));

        g.obj_to_obj(a, a);
        assert_eq!(g.get_obj(a).unwrap().loc, ObjLoc::Nowhere);
        assert!(g.get_obj(a).unwrap().contains.is_empty());

        g.obj_to_obj(b, a);
        let a_weight = g.get_obj(a).unwrap().weight;
        let b_weight = g.get_obj(b).unwrap().weight;
        g.obj_to_obj(a, b);
        assert_eq!(g.get_obj(a).unwrap().loc, ObjLoc::Nowhere);
        assert_eq!(g.get_obj(b).unwrap().loc, ObjLoc::Contained(a));
        assert_eq!(g.get_obj(a).unwrap().contains, vec![b]);
        assert!(g.get_obj(b).unwrap().contains.is_empty());
        assert_eq!(g.get_obj(a).unwrap().weight, a_weight);
        assert_eq!(g.get_obj(b).unwrap().weight, b_weight);

        g.obj_to_obj(child, a);
        let a_contents = g.get_obj(a).unwrap().contains.clone();
        let b_contents = g.get_obj(b).unwrap().contains.clone();
        g.obj_to_obj(child, b);
        assert_eq!(g.get_obj(child).unwrap().loc, ObjLoc::Contained(a));
        assert_eq!(g.get_obj(a).unwrap().contains, a_contents);
        assert_eq!(g.get_obj(b).unwrap().contains, b_contents);

        g.obj_to_obj(ObjId(u64::MAX), a);
        g.obj_to_obj(child, ObjId(u64::MAX));
        assert_eq!(g.get_obj(child).unwrap().loc, ObjLoc::Contained(a));
    }

    #[test]
    fn placement_mutators_reject_reverse_double_attachments() {
        let mut g = fresh_game();
        let first = g.add_room(crate::room::Room::new(
            9100,
            0,
            "First".into(),
            String::new(),
        ));
        let second = g.add_room(crate::room::Room::new(
            9101,
            0,
            "Second".into(),
            String::new(),
        ));
        let character = g.create_char(Character::new_player(
            "Carrier".into(),
            Class::Warrior,
            Race::Human,
        ));
        let object = g.create_obj(Object::new(104, "token".into(), "a token".into()));

        g.obj_to_room(object, first);
        g.obj_to_room(object, second);
        g.obj_to_char(object, character);
        g.equip_char(character, object, WEAR_BODY);
        assert_eq!(g.get_obj(object).unwrap().loc, ObjLoc::Room(first));
        assert_eq!(g.room(first).contents, vec![object]);
        assert!(g.room(second).contents.is_empty());
        assert!(g.get_char(character).unwrap().carrying.is_empty());
        assert_eq!(g.get_char(character).unwrap().equipment[WEAR_BODY], None);

        g.obj_from_anywhere(object);
        g.obj_to_char(object, character);
        g.obj_to_room(object, second);
        g.equip_char(character, object, WEAR_BODY);
        assert_eq!(g.get_obj(object).unwrap().loc, ObjLoc::Carried(character));
        assert_eq!(g.get_char(character).unwrap().carrying, vec![object]);
        assert_eq!(g.get_char(character).unwrap().carry_items, 1);
        assert!(g.room(second).contents.is_empty());
        assert_eq!(g.get_char(character).unwrap().equipment[WEAR_BODY], None);

        g.obj_from_anywhere(object);
        g.equip_char(character, object, WEAR_BODY);
        g.obj_to_room(object, second);
        g.obj_to_char(object, character);
        assert_eq!(
            g.get_obj(object).unwrap().loc,
            ObjLoc::Worn(character, WEAR_BODY)
        );
        assert_eq!(
            g.get_char(character).unwrap().equipment[WEAR_BODY],
            Some(object)
        );
        assert!(g.get_char(character).unwrap().carrying.is_empty());
        assert!(g.room(second).contents.is_empty());
    }

    #[test]
    fn total_weight_is_bounded_and_identity_safe_on_a_corrupt_cycle() {
        let mut g = fresh_game();
        let mut a = Object::new(201, "a".into(), "a".into());
        a.weight = 3;
        let a = g.create_obj(a);
        let mut b = Object::new(202, "b".into(), "b".into());
        b.weight = 7;
        let b = g.create_obj(b);
        g.get_obj_mut(a).unwrap().contains = vec![b];
        g.get_obj_mut(b).unwrap().contains = vec![a];

        assert_eq!(obj_total_weight(&g, a), 10);
    }

    #[test]
    fn snoop_fanout_is_utf8_safe_and_obeys_each_descriptor_output_ceiling() {
        let mut g = fresh_game();
        let room = g.add_room(crate::room::Room::new(
            9991,
            0,
            "Fanout".into(),
            String::new(),
        ));
        let victim_conn = ConnId(81);
        let snooper_conn = ConnId(82);
        g.descriptors.insert(
            victim_conn,
            Descriptor::new(victim_conn, "victim.test".into()),
        );
        g.descriptors.insert(
            snooper_conn,
            Descriptor::new(snooper_conn, "snooper.test".into()),
        );
        g.descriptors.get_mut(&victim_conn).unwrap().state = ConState::Playing;
        g.descriptors.get_mut(&snooper_conn).unwrap().state = ConState::Playing;
        let mut victim = Character::new_player("Victim".into(), Class::Warrior, Race::Human);
        victim.desc = Some(victim_conn);
        victim.trust = 1;
        let victim = g.create_char(victim);
        let mut snooper = Character::new_player("Snooper".into(), Class::Warrior, Race::Human);
        snooper.desc = Some(snooper_conn);
        snooper.trust = i32::from(LVL_IMPL);
        snooper.godcmds1 |= crate::gcmd::GCMD_SNOOP;
        let snooper = g.create_char(snooper);
        g.descriptors.get_mut(&victim_conn).unwrap().character = Some(victim);
        g.descriptors.get_mut(&snooper_conn).unwrap().character = Some(snooper);
        g.get_char_mut(victim).unwrap().snoop_by = Some(snooper);
        g.get_char_mut(snooper).unwrap().snooping = Some(victim);
        g.char_to_room(victim, room);
        g.char_to_room(snooper, room);

        // One direct delivery must independently cap both the victim and the
        // snooper relay. A multibyte payload exercises the UTF-8 truncation
        // boundary; avoiding room fan-out here proves the snooper did not only
        // overflow because it happened to be another room recipient (#417).
        let huge = "🦀".repeat(crate::connection::DESCRIPTOR_OUTPUT_LIMIT);
        g.send_to_char(victim, &huge);

        for conn in [victim_conn, snooper_conn] {
            let (output, overflowed) = g.descriptors.get_mut(&conn).unwrap().take_output_status();
            assert!(overflowed);
            assert!(output.len() <= crate::connection::DESCRIPTOR_OUTPUT_LIMIT);
            assert!(std::str::from_utf8(output.as_bytes()).is_ok());
            assert!(output.ends_with(crate::connection::OUTPUT_OVERFLOW_MARKER));
            assert_eq!(
                output
                    .matches(crate::connection::OUTPUT_OVERFLOW_MARKER)
                    .count(),
                1
            );
        }
    }

    fn lit_light(g: &mut GameState) -> ObjId {
        let mut light = Object::new(20, "torch".into(), "a torch".into());
        light.obj_type = ObjectType::Light;
        light.values[2] = 10;
        g.create_obj(light)
    }

    #[test]
    fn equip_and_unequip_lit_light_updates_room_light() {
        let mut g = fresh_game();
        let room = g.add_room(crate::room::Room::new(
            10,
            0,
            "Lit".into(),
            "A room.".into(),
        ));
        let cid = g.create_char(Character::new_player(
            "Torchbearer".into(),
            Class::Warrior,
            Race::Human,
        ));
        g.char_to_room(cid, room);
        let light = lit_light(&mut g);

        g.equip_char(cid, light, WEAR_LIGHT);
        assert_eq!(g.rooms[room].light, 1);

        assert_eq!(g.unequip_char(cid, WEAR_LIGHT), Some(light));
        assert_eq!(g.rooms[room].light, 0);
    }

    #[test]
    fn moving_with_lit_light_moves_room_light_count() {
        let mut g = fresh_game();
        let from = g.add_room(crate::room::Room::new(
            10,
            0,
            "From".into(),
            "A room.".into(),
        ));
        let to = g.add_room(crate::room::Room::new(11, 0, "To".into(), "A room.".into()));
        let cid = g.create_char(Character::new_player(
            "Torchbearer".into(),
            Class::Warrior,
            Race::Human,
        ));
        let light = lit_light(&mut g);
        g.equip_char(cid, light, WEAR_LIGHT);

        g.char_to_room(cid, from);
        assert_eq!(g.rooms[from].light, 1);

        g.char_from_room(cid);
        assert_eq!(g.rooms[from].light, 0);

        g.char_to_room(cid, to);
        assert_eq!(g.rooms[to].light, 1);
    }

    /// BUG 14: moving objects on/off a PC sets PLR_CRASH (so crash_save_all is
    /// no longer a no-op). NPCs are never flagged.
    #[test]
    fn plr_crash_set_on_object_movement() {
        let mut g = fresh_game();
        let pc = g.create_char(Character::new_player(
            "Crashy".into(),
            Class::Warrior,
            Race::Human,
        ));
        let mob = g.create_char(Character::new_npc(0));
        let oid = {
            let o = Object::new(3, "coin".into(), "a coin".into());
            g.create_obj(o)
        };
        g.obj_to_char(oid, pc);
        assert_ne!(
            g.get_char(pc).unwrap().act_flags & crate::objsave::PLR_CRASH,
            0
        );

        // NPC stays unflagged.
        let oid2 = g.create_obj(Object::new(4, "stick".into(), "a stick".into()));
        g.obj_to_char(oid2, mob);
        assert_eq!(
            g.get_char(mob).unwrap().act_flags & crate::objsave::PLR_CRASH,
            0
        );
    }

    /// BUG 22: extract_char clears master/followers links on both sides.
    #[test]
    fn extract_char_breaks_follow_links() {
        let mut g = fresh_game();
        let leader = g.create_char(Character::new_player(
            "Leader".into(),
            Class::Warrior,
            Race::Human,
        ));
        let follower = g.create_char(Character::new_player(
            "Pet".into(),
            Class::Warrior,
            Race::Human,
        ));
        // Wire a follow link by hand (add_follower normally does this).
        g.get_char_mut(follower).unwrap().master = Some(leader);
        g.get_char_mut(leader).unwrap().followers.push(follower);
        // Place both in a room so extract_char's char_from_room is well-defined.
        let rn = g.add_room(crate::room::Room::new(
            1,
            0,
            "Void".into(),
            "An empty void.".into(),
        ));
        g.char_to_room(leader, rn);
        g.char_to_room(follower, rn);

        g.extract_char(follower);
        // Leader must no longer list the extracted follower.
        assert!(!g.get_char(leader).unwrap().followers.contains(&follower));

        // Now extract the leader; its (already-empty) follower set is fine and the
        // leader had no master — just confirm it doesn't panic and removes cleanly.
        g.extract_char(leader);
        assert!(g.get_char(leader).is_none());
    }

    #[test]
    fn extract_char_notifies_snooper_when_victim_disappears() {
        let mut g = fresh_game();
        let snooper_conn = ConnId(1);
        g.descriptors.insert(
            snooper_conn,
            Descriptor::new(snooper_conn, "test".to_string()),
        );
        let mut snooper = Character::new_player("Snooper".into(), Class::Warrior, Race::Human);
        snooper.desc = Some(snooper_conn);
        let snooper = g.create_char(snooper);
        let victim = g.create_char(Character::new_player(
            "Victim".into(),
            Class::Warrior,
            Race::Human,
        ));
        let rn = g.add_room(crate::room::Room::new(
            1,
            0,
            "Void".into(),
            "An empty void.".into(),
        ));
        g.char_to_room(snooper, rn);
        g.char_to_room(victim, rn);
        g.get_char_mut(snooper).unwrap().snooping = Some(victim);
        g.get_char_mut(victim).unwrap().snoop_by = Some(snooper);

        g.extract_char(victim);

        assert_eq!(g.get_char(snooper).unwrap().snooping, None);
        assert!(
            g.descriptors
                .get(&snooper_conn)
                .unwrap()
                .outbuf
                .contains("Your victim is no longer among us.\r\n")
        );
    }

    #[test]
    fn extract_char_tears_down_arena_state_before_removal() {
        let mut g = fresh_game();
        let arena_room = g.add_room(crate::room::Room::new(
            4801,
            48,
            "Arena Prep".into(),
            String::new(),
        ));
        let mut player = Character::new_player("Extracted".into(), Class::Warrior, Race::Human);
        player.idnum = 77;
        player.wimp_level = 12;
        player.recall_level = 34;
        player.affect_flags = AFF_INVISIBLE;
        let player = g.create_char(player);
        g.char_to_room(player, arena_room);
        crate::arena::set_stat_for_test(&mut g, player, crate::arena::ARENA_COMBATANT1);
        crate::arena::bup_affects(&mut g, player);

        g.extract_char(player);

        assert!(!g.char_exists(player));
        assert_eq!(
            crate::arena::arena_stat(&g, player),
            crate::arena::ARENA_NOT
        );
        assert_eq!(g.player_save_requests, vec![player]);
    }
}
