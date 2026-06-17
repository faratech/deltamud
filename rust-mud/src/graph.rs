// graph.rs — port of CircleMUD/DeltaMUD `src/graph.c`: breadth-first
// pathfinding plus the consumers that use it (the `track` skill and the mob
// `hunt_victim` driver).
//
// The C original threads a hand-rolled linked-list FIFO and marks visited
// rooms by setting the ROOM_BFS_MARK flag bit (1<<15) directly on each room.
// In the id-indexed GameState that global mutation is both unnecessary and
// borrow-hostile, so the port instead drives the BFS off a local
// `VecDeque<(RoomRnum, dir)>` frontier and a per-call `visited: Vec<bool>`
// keyed by rnum. The traversal order, edge predicate, and returned values are
// otherwise byte-for-byte identical to graph.c.
//
// graph.c compiles with TRACK_THROUGH_DOORS *defined*, so the edge predicate
// deliberately walks through closed/hidden doors (a closed door does NOT block
// the BFS); it only refuses NOWHERE destinations and ROOM_NOTRACK rooms. The
// closed-door check from the non-TRACK_THROUGH_DOORS branch is preserved in
// commented form for fidelity but is not active, matching the live build.
//
// Borrow discipline matches the rest of the command modules: read needed
// values into locals first, then mutate / send; entities are looked up by id
// every time.

use crate::cmd_movement::perform_move;
use crate::combat::hit;
use crate::interpreter::one_argument;
use crate::room::{RoomFlags, EX_CLOSED};
use crate::spell_parser::{get_char_world_vis, SKILL_TRACK, TYPE_UNDEFINED};
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// BFS result codes (utils.h). find_first_step returns either one of these
// negatives or a non-negative direction index (0..NUM_OF_DIRS-1).
// ---------------------------------------------------------------------------
pub const BFS_ERROR: i32 = -1;
pub const BFS_ALREADY_THERE: i32 = -2;
pub const BFS_NO_PATH: i32 = -3;

// AFF_NOTRACK (structs.h 1<<15): a character carrying this affect cannot be
// tracked. Not surfaced in flags.rs, transcribed here to the exact C value.
const AFF_NOTRACK: i64 = 1 << 15;

/// dirs[] — full direction names for the trail message (graph.c uses the same
/// `dirs[]` table act.movement.c uses). Mirrors types::DIR_NAMES.
const DIRS: [&str; NUM_OF_DIRS] = DIR_NAMES;

// ---------------------------------------------------------------------------
// VALID_EDGE — graph.c's edge predicate, with TRACK_THROUGH_DOORS defined.
//
// In C this is a macro over `world[x].dir_option[y]`; here `x` is the source
// rnum and `y` the direction. The Rust `Exit::to_room` is a *vnum*, so the
// destination must be resolved through `real_room()` (C stored a pre-resolved
// rnum in `to_room`). An edge is valid iff:
//   * the exit exists,
//   * its destination resolves to a real room (not NOWHERE),
//   * that room is not ROOM_NOTRACK,
//   * and that room has not already been visited this BFS.
// (When TRACK_THROUGH_DOORS is *not* defined the C code additionally requires
//  `!IS_SET(exit_info, EX_CLOSED)`; the live build defines it, so closed doors
//  are traversable. The constant is imported so the intent stays documented.)
// ---------------------------------------------------------------------------
fn valid_edge(g: &GameState, src: RoomRnum, dir: usize, visited: &[bool]) -> Option<RoomRnum> {
    let _ = EX_CLOSED; // documented-but-inactive in the TRACK_THROUGH_DOORS build
    let exit = g.room_opt(src)?.exits[dir].as_ref()?;
    let dest = g.real_room(exit.to_room)?;
    if g.room_opt(dest)?.room_flags.contains(RoomFlags::NO_TRACK) {
        return None;
    }
    if visited.get(dest).copied().unwrap_or(true) {
        return None;
    }
    Some(dest)
}

/// find_first_step (graph.c): given a source room and a target room, find the
/// first step on the shortest path from src to target. Returns the direction
/// index (0..NUM_OF_DIRS-1) of that first step, or a BFS_* error code.
///
/// Intended usage: in mobile_activity, hand a mob a direction to walk when it
/// is hunting another mob or PC; or back a player `track` skill.
pub fn find_first_step(g: &GameState, src: RoomRnum, target: RoomRnum) -> i32 {
    let top = g.rooms.len();
    if src >= top || target >= top {
        // log("Illegal value passed to find_first_step (graph.c)")
        eprintln!("SYSERR: Illegal value passed to find_first_step (graph.c)");
        return BFS_ERROR;
    }
    if src == target {
        return BFS_ALREADY_THERE;
    }

    // visited[] replaces the C ROOM_BFS_MARK flag sweep (all rooms start clear).
    let mut visited = vec![false; top];
    visited[src] = true;

    // FIFO of (room, first-step-direction). The C linked-list queue stores the
    // ORIGINATING direction on every node, so the answer is simply the saved
    // direction of whichever node first equals the target.
    let mut queue: std::collections::VecDeque<(RoomRnum, usize)> = std::collections::VecDeque::new();

    // First, enqueue the first steps, recording which direction we're going.
    for curr_dir in 0..NUM_OF_DIRS {
        if let Some(dest) = valid_edge(g, src, curr_dir, &visited) {
            visited[dest] = true;
            queue.push_back((dest, curr_dir));
        }
    }

    // Now the classic BFS.
    while let Some(&(room, first_dir)) = queue.front() {
        if room == target {
            return first_dir as i32;
        }
        for curr_dir in 0..NUM_OF_DIRS {
            if let Some(dest) = valid_edge(g, room, curr_dir, &visited) {
                visited[dest] = true;
                // Propagate the ORIGINAL first-step direction, not curr_dir.
                queue.push_back((dest, first_dir));
            }
        }
        queue.pop_front();
    }

    BFS_NO_PATH
}

// ---------------------------------------------------------------------------
// CAN_GO (utils.h): an exit the char may actually walk through — exists, leads
// somewhere real, is not closed, and does not lead to an impassable map cell.
// Used by do_track's random-stumble branch.
// ---------------------------------------------------------------------------
fn can_go(g: &GameState, ch: CharId, dir: usize) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };
    let exit = match g.room_opt(rnum).and_then(|r| r.exits[dir].as_ref()) {
        Some(e) => e,
        None => return false,
    };
    let dest = match g.real_room(exit.to_room) {
        Some(r) => r,
        None => return false,
    };
    if exit.exit_info & EX_CLOSED != 0 {
        return false;
    }
    let dest_room = g.room(dest);
    if dest_room.map_x.is_some() && dest_room.map_y.is_some() && dest_room.mapmv == -1 {
        return false;
    }
    true
}

/// him/her/it for a character (graph.c HMHR). act.rs keeps its hssh/hmhr
/// helpers private, and do_track / hunt_victim compose their strings by hand
/// just like the C, so a local copy is supplied here.
fn hmhr(g: &GameState, cid: CharId) -> &'static str {
    match g.get_char(cid).map(|c| c.player.sex) {
        Some(Gender::Male) => "him",
        Some(Gender::Female) => "her",
        _ => "it",
    }
}

// ===========================================================================
//  Functions and commands which use the above.
// ===========================================================================

/// do_track (graph.c): the player `track` skill. Finds a visible target and
/// reports the first direction of the shortest path to them — with a
/// skill-roll chance of pointing the wrong (but walkable) way.
pub fn do_track(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // Must know the skill.
    if g.get_char(ch).map(|c| c.skill(SKILL_TRACK as u16)).unwrap_or(0) == 0 {
        g.send_to_char(ch, "You have no idea how.\r\n");
        return;
    }

    let (name, _) = one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "Whom are you trying to track?\r\n");
        return;
    }

    let vict = match get_char_world_vis(g, ch, &name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No-one around by that name.\r\n");
            return;
        }
    };

    // AFF_NOTRACK targets leave no trail.
    if g.get_char(vict).map(|c| c.affect_flags & AFF_NOTRACK != 0).unwrap_or(false) {
        g.send_to_char(ch, "You sense no trail.\r\n");
        return;
    }

    let src = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let dst = match g.get_char(vict).and_then(|c| c.in_room) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "You sense no trail.\r\n");
            return;
        }
    };

    let mut dir = find_first_step(g, src, dst);

    match dir {
        BFS_ERROR => g.send_to_char(ch, "Hmm.. something seems to be wrong.\r\n"),
        BFS_ALREADY_THERE => g.send_to_char(ch, "You're already in the same room!!\r\n"),
        BFS_NO_PATH => {
            let buf = format!("You can't sense a trail to {} from here.\r\n", hmhr(g, vict));
            g.send_to_char(ch, &buf);
        }
        _ => {
            // 101% is a guaranteed failure; on a failed roll, stumble onto a
            // random *walkable* direction instead of the true one.
            let num = g.rng.number(0, 101);
            let skill = g.get_char(ch).map(|c| c.skill(SKILL_TRACK as u16) as i32).unwrap_or(0);
            if skill < num {
                // do { dir = number(0, NUM_OF_DIRS-1); } while (!CAN_GO(ch,dir));
                // Guard against a room with no walkable exits at all so the loop
                // can't spin forever (the C version assumes at least one exists).
                if (0..NUM_OF_DIRS).any(|d| can_go(g, ch, d)) {
                    loop {
                        dir = g.rng.number(0, NUM_OF_DIRS as i32 - 1);
                        if can_go(g, ch, dir as usize) {
                            break;
                        }
                    }
                }
            }
            let buf = format!("You sense a trail {} from here!\r\n", DIRS[dir as usize]);
            g.send_to_char(ch, &buf);
        }
    }
}

/// hunt_victim (graph.c): the per-pulse driver that walks a hunting mob one
/// step toward its quarry, attacking on arrival; gives up (with a shout) if the
/// quarry has left the world or no path remains.
///
/// Called from mobile_activity for any mob whose `hunting` field is set.
pub fn hunt_victim(g: &mut GameState, ch: CharId) {
    // Nothing to do without a live char that is actually hunting.
    let prey = match g.get_char(ch).and_then(|c| c.hunting) {
        Some(p) => p,
        None => return,
    };

    // Make sure the prey still exists in the world (C scans character_list).
    if !g.char_exists(prey) {
        // C: do_say(ch, "Damn!  My prey is gone!!", 0, 0)
        crate::cmd_comm::do_say(g, ch, "Damn!  My prey is gone!!", 0);
        if let Some(c) = g.get_char_mut(ch) {
            c.hunting = None;
        }
        return;
    }

    let src = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let dst = match g.get_char(prey).and_then(|c| c.in_room) {
        Some(r) => r,
        None => {
            // Prey is roomless: treat as lost (find_first_step would error).
            lose_prey(g, ch, prey);
            return;
        }
    };

    let dir = find_first_step(g, src, dst);
    if dir < 0 {
        lose_prey(g, ch, prey);
        return;
    }

    // Walk one step; if that lands us in the prey's room, strike.
    perform_move(g, ch, dir, true);
    let now = g.get_char(ch).and_then(|c| c.in_room);
    let prey_room = g.get_char(prey).and_then(|c| c.in_room);
    if now.is_some() && now == prey_room {
        let _ = TYPE_UNDEFINED; // C passes TYPE_UNDEFINED; Rust hit() picks the type itself.
        hit(g, ch, prey);
    }
}

/// "Damn!  Lost <him>!" give-up path shared by hunt_victim's failure cases.
/// C: sprintf(buf,"Damn!  Lost %s!",HMHR(...)); do_say(ch, buf, 0, 0);
fn lose_prey(g: &mut GameState, ch: CharId, prey: CharId) {
    let buf = format!("Damn!  Lost {}!", hmhr(g, prey));
    crate::cmd_comm::do_say(g, ch, &buf, 0);
    if let Some(c) = g.get_char_mut(ch) {
        c.hunting = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::room::{Exit, Room};

    fn add_char(g: &mut GameState, room: RoomRnum) -> CharId {
        let ch = g.create_char(Character::new_player(
            "Tracker".to_string(),
            Class::Warrior,
            Race::Human,
        ));
        g.char_to_room(ch, room);
        ch
    }

    fn open_exit(to_room: RoomVnum) -> Exit {
        Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: NOTHING,
            to_room,
        }
    }

    #[test]
    fn can_go_allows_non_map_room_with_default_mapmv() {
        let mut g = GameState::new(Config::default());
        let src = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        let dest = g.add_room(Room::new(101, 0, "Normal".to_string(), "Normal.".to_string()));
        g.room_mut(src).exits[EAST] = Some(open_exit(101));
        let ch = add_char(&mut g, src);

        assert_eq!(g.room(dest).mapmv, -1);
        assert!(can_go(&g, ch, EAST));
    }

    #[test]
    fn can_go_blocks_impassable_map_destination() {
        let mut g = GameState::new(Config::default());
        let src = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        let dest = g.add_room(Room::new(101, 0, "Wall".to_string(), "Wall.".to_string()));
        {
            let room = g.room_mut(dest);
            room.map_x = Some(1);
            room.map_y = Some(1);
            room.mapmv = -1;
        }
        g.room_mut(src).exits[EAST] = Some(open_exit(101));
        let ch = add_char(&mut g, src);

        assert!(!can_go(&g, ch, EAST));
    }

    #[test]
    fn can_go_allows_passable_map_destination() {
        let mut g = GameState::new(Config::default());
        let src = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        let dest = g.add_room(Room::new(101, 0, "Road".to_string(), "Road.".to_string()));
        {
            let room = g.room_mut(dest);
            room.map_x = Some(1);
            room.map_y = Some(1);
            room.mapmv = 1;
        }
        g.room_mut(src).exits[EAST] = Some(open_exit(101));
        let ch = add_char(&mut g, src);

        assert!(can_go(&g, ch, EAST));
    }

    #[test]
    fn bfs_still_routes_through_impassable_map_destination() {
        let mut g = GameState::new(Config::default());
        let src = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        let mid = g.add_room(Room::new(101, 0, "Wall".to_string(), "Wall.".to_string()));
        let target = g.add_room(Room::new(102, 0, "Target".to_string(), "Target.".to_string()));
        {
            let room = g.room_mut(mid);
            room.map_x = Some(1);
            room.map_y = Some(1);
            room.mapmv = -1;
        }
        g.room_mut(src).exits[EAST] = Some(open_exit(101));
        g.room_mut(mid).exits[EAST] = Some(open_exit(102));

        assert_eq!(find_first_step(&g, src, target), EAST as i32);
    }
}
