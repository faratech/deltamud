// handler.rs — the shared mutators and finders every command relies on
// (CircleMUD handler.c), ported to the id-indexed GameState. Adds them as
// inherent `impl GameState` methods so commands call one canonical version.

use crate::character::Character;
use crate::flags::*;
use crate::object::ObjLoc;
use crate::state::GameState;
use crate::types::*;

/// Port of CircleMUD isname(): true if `arg` is a whole keyword in `namelist`
/// (case-insensitive, whole-word — NOT prefix, matching C exactly).
pub fn isname(arg: &str, namelist: &str) -> bool {
    if arg.is_empty() {
        return false;
    }
    let arg = arg.to_lowercase();
    namelist
        .split_whitespace()
        .any(|word| word.to_lowercase() == arg)
}

/// Parse "3.sword" -> (3, "sword"); "sword" -> (1, "sword").
/// "all.coin" -> (i32::MAX, "coin"); "all" -> (i32::MAX, "all" w/ all-flag).
pub fn get_number(arg: &str) -> (i32, String) {
    if let Some((num, name)) = arg.split_once('.') {
        if num.eq_ignore_ascii_case("all") {
            return (i32::MAX, name.to_string());
        }
        if let Ok(n) = num.parse::<i32>() {
            return (n, name.to_string());
        }
    }
    (1, arg.to_string())
}

impl GameState {
    // ---- Character placement -------------------------------------------
    /// CircleMUD char_to_room: prepend to room.people (newest first).
    pub fn char_to_room(&mut self, cid: CharId, rnum: RoomRnum) {
        if rnum >= self.rooms.len() {
            return;
        }
        self.rooms[rnum].people.insert(0, cid);
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.in_room = Some(rnum);
        }
    }

    pub fn char_from_room(&mut self, cid: CharId) {
        let rnum = match self.chars.get(&cid).and_then(|c| c.in_room) {
            Some(r) => r,
            None => return,
        };
        if let Some(room) = self.rooms.get_mut(rnum) {
            room.people.retain(|&c| c != cid);
        }
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.in_room = None;
        }
    }

    // ---- Object placement ----------------------------------------------
    pub fn obj_to_room(&mut self, oid: ObjId, rnum: RoomRnum) {
        if rnum >= self.rooms.len() {
            return;
        }
        self.rooms[rnum].contents.insert(0, oid);
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Room(rnum);
        }
    }

    pub fn obj_to_char(&mut self, oid: ObjId, cid: CharId) {
        let weight = self.objs.get(&oid).map(|o| o.weight).unwrap_or(0);
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.carrying.insert(0, oid);
            ch.carry_weight += weight;
            ch.carry_items = ch.carry_items.saturating_add(1);
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Carried(cid);
        }
    }

    pub fn obj_to_obj(&mut self, oid: ObjId, container: ObjId) {
        if oid == container {
            return;
        }
        if let Some(c) = self.objs.get_mut(&container) {
            c.contains.insert(0, oid);
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Contained(container);
        }
    }

    /// Equip a worn item. Caller guarantees the slot is empty.
    pub fn equip_char(&mut self, cid: CharId, oid: ObjId, pos: usize) {
        if pos >= NUM_WEARS {
            return;
        }
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.equipment[pos] = Some(oid);
        }
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Worn(cid, pos);
        }
        self.affect_total(cid);
    }

    pub fn unequip_char(&mut self, cid: CharId, pos: usize) -> Option<ObjId> {
        if pos >= NUM_WEARS {
            return None;
        }
        let oid = self.chars.get_mut(&cid).and_then(|ch| ch.equipment[pos].take())?;
        if let Some(o) = self.objs.get_mut(&oid) {
            o.loc = ObjLoc::Nowhere;
        }
        self.affect_total(cid);
        Some(oid)
    }

    /// Remove a character from the world. Detaches from room, drops fighters,
    /// and extracts inventory/equipment. PCs are normally respawned instead.
    pub fn extract_char(&mut self, cid: CharId) {
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
            if let Some(sc) = self.chars.get_mut(&s) {
                sc.snooping = None;
            }
        }

        self.char_from_room(cid);

        // Extract carried + worn objects.
        let (carried, worn): (Vec<ObjId>, Vec<ObjId>) = match self.chars.get(&cid) {
            Some(ch) => (
                ch.carrying.clone(),
                ch.equipment.iter().flatten().copied().collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        for o in carried.into_iter().chain(worn) {
            self.extract_obj(o);
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
        // swap_remove: O(1) removal of the dead/extracted character from the
        // ordered arena (replaces the old char_list.retain + chars.remove pair).
        self.chars.swap_remove(&cid);
    }

    // ---- Visibility -----------------------------------------------------
    /// Simplified can_see: handles self, immortal holylight, invis-level and
    /// AFF_INVISIBLE+detect. Light/dark gating is handled at the look site.
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
        if v.is_immortal() && v.prf_flags & PRF_HOLYLIGHT != 0 {
            return true;
        }
        // Higher-invis-level immortals are hidden from lower-level viewers.
        if t.invis_level > v.player.level as i32 {
            return false;
        }
        if t.affect_flags & AFF_INVISIBLE != 0 && v.affect_flags & AFF_DETECT_INVIS == 0 {
            return false;
        }
        true
    }

    // ---- Finders --------------------------------------------------------
    /// Find a visible character in `observer`'s room by keyword (+ optional
    /// N.name ordinal). Mirrors get_char_room_vis.
    pub fn get_char_room_vis(&self, observer: CharId, arg: &str) -> Option<CharId> {
        let rnum = self.chars.get(&observer)?.in_room?;
        let (mut count, name) = get_number(arg);
        if count == 0 {
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
    pub fn get_obj_in_list_vis(&self, _observer: CharId, arg: &str, list: &[ObjId]) -> Option<ObjId> {
        let (mut count, name) = get_number(arg);
        if count == 0 {
            return None;
        }
        for &oid in list {
            let obj = match self.objs.get(&oid) {
                Some(o) => o,
                None => continue,
            };
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
        // Collect equipment object affects.
        let mut mods: Vec<(i32, i32)> = Vec::new();
        let mut flagbits: i64 = 0;
        if let Some(ch) = self.chars.get(&cid) {
            for slot in ch.equipment.iter().flatten() {
                if let Some(obj) = self.objs.get(slot) {
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
        if let Some(ch) = self.chars.get_mut(&cid) {
            ch.aff_abils = ch.real_abils;
            ch.affect_flags = flagbits;
            for (loc, m) in mods {
                apply_location(ch, loc, m);
            }
        }
    }
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
            c.affected
                .iter()
                .any(|af| (af.bitvector & bitvector) != 0 && af.duration == -1 && af.spell_type == -1)
        })
        .unwrap_or(false)
}

/// Apply one affect modifier to a character's affected fields (Tier-0:
/// abilities; the DeltaMUD combat mods are recomputed in Batch 5).
pub fn apply_location(ch: &mut Character, location: i32, modifier: i32) {
    let m = modifier as i8;
    match location {
        APPLY_STR => ch.aff_abils.str += m,
        APPLY_DEX => ch.aff_abils.dex += m,
        APPLY_INT => ch.aff_abils.intel += m,
        APPLY_WIS => ch.aff_abils.wis += m,
        APPLY_CON => ch.aff_abils.con += m,
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

/// Helper used by Object containment weight (recursive total weight).
pub fn obj_total_weight(state: &GameState, oid: ObjId) -> i32 {
    let obj = match state.objs.get(&oid) {
        Some(o) => o,
        None => return 0,
    };
    let mut total = obj.weight;
    for &c in &obj.contains {
        total += obj_total_weight(state, c);
    }
    total
}
