// town_life.rs — the Deltania Breathes living-world driver: day/night NPC
// schedules, street barks, and the cross-town caravans.
//
// The design is STATELESS with respect to the world: every PULSE_MOBILE each
// directed npc re-evaluates "where should I be right now?" (a day post or a
// night post from the static SCHEDULES table, or a caravan's far-end by mud
// hour) and takes one step toward it. Nothing is written to disk, nothing
// survives a reboot, and copyover needs no special handling — a fresh boot
// simply re-derives everything from the tables and the mud clock. This is the
// same trick castle.rs's King uses (hours-gated walks), generalised:
//
//   * SCHEDULES   — in-town commuters. They walk real-room paths with
//                   graph::find_first_step (their posts are all interior
//                   rooms), one room per mobile pulse (10 RL-seconds).
//   * CARAVANS    — cross-town couriers. The surface map has no exits (map
//                   movement is coordinate-based), so find_first_step cannot
//                   route them. Instead boot_town_life() computes each
//                   caravan's route ONCE with a mixed-graph BFS (real exits +
//                   map-cell neighbours + EntryPoint links) and the caravan
//                   walks that precomputed path cell by cell, one cell per
//                   PULSE_MOBILE (a river crossing takes a few mud-hours of
//                   uneventful travel, exactly like it should).
//
// mobile_activity consults is_directed() and hands these mobs to drive()
// BEFORE scavenging/wandering/aggression, so the tables own their entire day.

use crate::act::{ActArg, To, act};
use crate::state::GameState;
use crate::types::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

/// Mud hours with daylight (weather.rs SUN_LIGHT 6..=20).
const DAY_START: i32 = 6;
const DAY_END: i32 = 20;

/// Caravans are away from their home post during these hours.
const CARAVAN_DEPART_HOUR: i32 = 8;
const CARAVAN_RETURN_HOUR: i32 = 16;

/// One scheduled townsfolk: day post and night post (room vnums, interior).
pub struct ScheduleEntry {
    pub mob_vnum: MobVnum,
    pub day_post: RoomVnum,
    pub night_post: RoomVnum,
    /// Street barks, told at random while standing at a post. `$n` is the
    /// speaker; act() renders them like any room message.
    pub barks: &'static [&'static str],
}

const fn sched(
    mob_vnum: MobVnum,
    day_post: RoomVnum,
    night_post: RoomVnum,
    barks: &'static [&'static str],
) -> ScheduleEntry {
    ScheduleEntry {
        mob_vnum,
        day_post,
        night_post,
        barks,
    }
}

/// The Newbie School faculty walks from the square to its classroom every
/// morning and home again every night; the traders open their posts by day
/// and close them by night (their shop rooms already refuse service after
/// hours — the walk just makes it visible).
static SCHEDULES: &[ScheduleEntry] = &[
    sched(
        200,
        201,
        210,
        &[
            "$n says, \"Class starts soon - no running in the halls.\"",
            "$n says, \"Another day, another dozen wide eyes to teach.\"",
        ],
    ),
    sched(201, 201, 210, &["$n mutters star charts under $s breath."]),
    sched(
        205,
        210,
        210,
        &["$n says, \"Mind the craft benches, they mind you back.\""],
    ),
    sched(
        206,
        212,
        210,
        &[
            "$n says, \"Fresh stock for fresh graduates!\"",
            "$n says, \"Back home before the lanterns burn low.\"",
        ],
    ),
    sched(
        301,
        311,
        310,
        &[
            "$n says, \"River prices are up, river luck is down.\"",
            "$n says, \"Buying by the crate, selling by the story!\"",
        ],
    ),
];

/// One cross-town caravan: home post, far post, and the bark it offers while
/// parked. The route between them is computed at boot.
pub struct CaravanEntry {
    pub mob_vnum: MobVnum,
    pub home: RoomVnum,
    pub far: RoomVnum,
    pub arrive_msg: &'static str,
    pub barks: &'static [&'static str],
}

static CARAVANS: &[CaravanEntry] = &[
    CaravanEntry {
        mob_vnum: 304, // the Locris river courier
        home: 310,     // Locris Square
        far: 139,      // Inside The West Gate (Itrius)
        arrive_msg: "The Locris courier shoulders $s satchel and takes the river road.",
        barks: &[
            "$n says, \"Word from the capitol, fresh as the ferry wake.\"",
            "$n says, \"Two days on the road beats two days digging it.\"",
        ],
    },
    CaravanEntry {
        mob_vnum: 209, // the Newhaven carter
        home: 210,     // Newhaven Town Square
        far: 139,      // Inside The West Gate (Itrius)
        arrive_msg: "The Newhaven carter checks $s cart ties before the long road.",
        barks: &[
            "$n says, \"Schooltown goods for the big city!\"",
            "$n says, \"Roads are quiet - too quiet, if you ask me.\"",
        ],
    },
];

// ---------------------------------------------------------------------------
// Route table (computed at boot, keyed by the caravan's mob vnum).
// ---------------------------------------------------------------------------

fn routes() -> &'static Mutex<HashMap<MobVnum, Vec<RoomRnum>>> {
    static ROUTES: OnceLock<Mutex<HashMap<MobVnum, Vec<RoomRnum>>>> = OnceLock::new();
    ROUTES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compute every caravan route. Called from main.rs AFTER integrate_map_rooms
/// (the map cells must exist for the mixed-graph BFS). Idempotent: a second
/// call (copyover recovery) simply recomputes the same routes.
pub fn boot_town_life(g: &GameState) {
    let mut table = crate::lock_ok::lock(&routes());
    table.clear();
    for car in CARAVANS {
        let (Some(src), Some(dst)) = (g.real_room(car.home), g.real_room(car.far)) else {
            log::warn!(
                "SYSERR: town_life caravan {} route endpoints missing ({} -> {})",
                car.mob_vnum,
                car.home,
                car.far
            );
            continue;
        };
        match route_bfs(g, src, dst) {
            Some(path) => {
                table.insert(car.mob_vnum, path);
            }
            None => log::warn!(
                "SYSERR: town_life caravan {} has no walkable route {} -> {}",
                car.mob_vnum,
                car.home,
                car.far
            ),
        }
    }
}

/// Mixed-graph BFS: real-room exits (non-closed), map-cell 4-neighbours
/// (passable, not unswimmable water), and the EntryPoint enter/leave links.
/// Returns the full room-rnum path INCLUDING both endpoints.
fn route_bfs(g: &GameState, src: RoomRnum, dst: RoomRnum) -> Option<Vec<RoomRnum>> {
    if src == dst {
        return Some(vec![src]);
    }
    let mut prev: HashMap<RoomRnum, RoomRnum> = HashMap::new();
    let mut queue = VecDeque::from([src]);
    let mut visited: HashMap<RoomRnum, ()> = HashMap::from([(src, ())]);

    while let Some(cur) = queue.pop_front() {
        for next in neighbours(g, cur) {
            if visited.contains_key(&next) {
                continue;
            }
            visited.insert(next, ());
            prev.insert(next, cur);
            if next == dst {
                let mut path = vec![dst];
                let mut walk = dst;
                while let Some(&p) = prev.get(&walk) {
                    path.push(p);
                    walk = p;
                    if walk == src {
                        break;
                    }
                }
                path.reverse();
                return Some(path);
            }
            queue.push_back(next);
        }
    }
    None
}

/// Every room reachable in ONE step from `r` under the mixed graph.
fn neighbours(g: &GameState, r: RoomRnum) -> Vec<RoomRnum> {
    let mut out = Vec::new();
    let Some(room) = g.room_opt(r) else {
        return out;
    };

    // Real exits, skipping closed doors.
    for e in room.exits.iter().flatten() {
        if e.exit_info & crate::room::EX_CLOSED != 0 {
            continue;
        }
        if let Some(to) = g.real_room(e.to_room) {
            out.push(to);
        }
    }

    // EntryPoint enter/leave links.
    if let Some(link) = room.linkrnum {
        out.push(link);
    }
    if let Some(link) = room.linkmapnum {
        out.push(link);
    }

    // Surface-map neighbours (map movement is coordinate-based; the map rooms
    // have no exits). Impassable cells and unswimmable water are barriers.
    if room.map_x.is_some() {
        if let (Some(x), Some(y)) = (room.map_x, room.map_y) {
            for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                if let Some(n) = g.map_coords_to_rnum(x + dx, y + dy) {
                    if let Some(nr) = g.room_opt(n) {
                        if nr.mapmv > 0 && nr.sector_type != crate::room::SectorType::WaterNoSwim {
                            out.push(n);
                        }
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The per-pulse driver.
// ---------------------------------------------------------------------------

/// True when `ch`'s prototype appears in the schedules or caravan tables —
/// mobile_activity hands these mobs to drive() and skips its own AI.
pub fn is_directed(g: &GameState, ch: CharId) -> bool {
    let Some(c) = g.get_char(ch) else {
        return false;
    };
    if !c.is_npc {
        return false;
    }
    lookup_schedule(c.nr).is_some() || lookup_caravan(c.nr).is_some()
}

fn lookup_schedule(vnum: MobVnum) -> Option<&'static ScheduleEntry> {
    SCHEDULES.iter().find(|s| s.mob_vnum == vnum)
}

fn lookup_caravan(vnum: MobVnum) -> Option<&'static CaravanEntry> {
    CARAVANS.iter().find(|c| c.mob_vnum == vnum)
}

/// One mobile pulse for a scheduled townsfolk or caravan. Always returns
/// having consumed the mob's turn.
pub fn drive(g: &mut GameState, ch: CharId) {
    let nr = match g.get_char(ch) {
        Some(c) => c.nr,
        None => return,
    };
    if lookup_caravan(nr).is_some() {
        drive_caravan(g, ch, nr);
    } else if let Some(entry) = lookup_schedule(nr) {
        drive_commuter(g, ch, entry);
    }
}

fn in_room_vnum(g: &GameState, ch: CharId) -> Option<RoomVnum> {
    g.get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.room_opt(r))
        .map(|r| r.number)
}

/// Walk toward the day/night post; bark while standing at it.
fn drive_commuter(g: &mut GameState, ch: CharId, entry: &'static ScheduleEntry) {
    let hours = crate::weather::time_now().0;
    let post = if (DAY_START..=DAY_END).contains(&hours) {
        entry.day_post
    } else {
        entry.night_post
    };

    if in_room_vnum(g, ch) == Some(post) {
        maybe_bark(g, ch, entry.barks);
        return;
    }
    step_toward(g, ch, post);
}

/// Walk the precomputed route toward the far post (day hours) or home; bark
/// while parked at either end.
fn drive_caravan(g: &mut GameState, ch: CharId, nr: MobVnum) {
    let Some(entry) = lookup_caravan(nr) else {
        return;
    };
    let path = crate::lock_ok::lock(&routes()).get(&nr).cloned();
    let Some(path) = path else { return };
    if path.len() < 2 {
        return;
    }

    let hours = crate::weather::time_now().0;
    let away = (CARAVAN_DEPART_HOUR..CARAVAN_RETURN_HOUR).contains(&hours);
    let target_rnum = if away { path[path.len() - 1] } else { path[0] };

    let here = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    if here == target_rnum {
        maybe_bark(g, ch, entry.barks);
        return;
    }

    // Which cell of the route are we on? (Stateless position lookup; the
    // zone-reset reloads the mob at its home room, which is path[0].)
    let idx = match path.iter().position(|&r| r == here) {
        Some(i) => i,
        None => return, // off-route (dragged, summoned): wait for a reset
    };
    let target_idx = if away { path.len() - 1 } else { 0 };
    let next_idx = if target_idx > idx { idx + 1 } else { idx - 1 };
    let next = path[next_idx];

    // Announce, transfer, announce. The caravan steps one cell per mobile
    // pulse; the routes run 60-80 cells, so a crossing takes a couple of mud
    // hours of steady travel.
    act(
        g,
        "$n continues on $s way.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(ch);
    g.char_to_room(ch, next);
    act(
        g,
        "$n arrives, road-worn and busy.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
}

/// find_first_step + perform_move: one room per mobile pulse. Falls silent
/// (no move) when the BFS finds no route — commuters only ever walk interior
/// room graphs, where a route always exists.
fn step_toward(g: &mut GameState, ch: CharId, post: RoomVnum) {
    let (here, target) = match (g.get_char(ch).and_then(|c| c.in_room), g.real_room(post)) {
        (Some(h), Some(t)) => (h, t),
        _ => return,
    };
    let dir = crate::graph::find_first_step(g, here, target);
    if (0..NUM_OF_DIRS as i32).contains(&dir) {
        crate::cmd_movement::perform_move(g, ch, dir, true);
    }
}

/// Roughly one bark every couple of mud hours while at post (a 1-in-30
/// chance per mobile pulse).
fn maybe_bark(g: &mut GameState, ch: CharId, barks: &'static [&'static str]) {
    if barks.is_empty() {
        return;
    }
    if g.rng.number(1, 30) != 1 {
        return;
    }
    let line = barks[g.rng.number(0, (barks.len() - 1) as i32) as usize];
    act(g, line, false, ch, None, ActArg::None, To::Room);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::room::{Exit, Room};

    /// The mud CLOCK is a process global (weather.rs module static): every
    /// test that drives hour-gated behaviour takes this lock or the tests
    /// race each other's set_hour calls.
    fn clock_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap()
    }

    fn npc(g: &mut GameState, vnum: MobVnum, room: RoomRnum) -> CharId {
        let mut m = Character::new_npc(vnum);
        m.player.level = 10;
        m.position = Position::Standing;
        m.points.move_points = 100;
        let cid = g.create_char(m);
        g.char_to_room(cid, room);
        cid
    }

    #[test]
    fn commuter_walks_to_day_post_and_home_at_night() {
        let _guard = clock_lock();
        crate::weather::test_clock::set_hour(12); // day
        let mut g = GameState::new(Config::default());
        let square = g.add_room(Room::new(210, 2, "Square".into(), String::new()));
        let hall = g.add_room(Room::new(201, 2, "Hall".into(), String::new()));
        // One-step commute each way.
        g.rooms[hall].exits[WEST] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: -1,
            to_room: 210,
        });
        g.rooms[square].exits[EAST] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: -1,
            to_room: 201,
        });

        // The schoolmaster (mob 200: day 201 / night 210) starts at the square.
        let teacher = npc(&mut g, 200, square);
        assert!(is_directed(&g, teacher));

        drive(&mut g, teacher);
        assert_eq!(
            g.get_char(teacher).unwrap().in_room,
            Some(hall),
            "day commute must head for the day post"
        );

        // Night falls: the next drive steps back toward the night post.
        crate::weather::test_clock::set_hour(23);
        drive(&mut g, teacher);
        assert_eq!(g.get_char(teacher).unwrap().in_room, Some(square));
    }

    #[test]
    fn undirected_mobs_are_not_claimed_by_town_life() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(100, 1, "Nowhere Much".into(), String::new()));
        let plain = npc(&mut g, 5000, room);
        assert!(!is_directed(&g, plain));
    }

    #[tokio::test]
    async fn real_lib_caravan_routes_exist_and_are_walkable() {
        let _guard = clock_lock();
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        if !std::path::Path::new(&format!("{}/world/worldmap", lib)).exists() {
            return;
        }
        let mut g = crate::state::GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib)
            .await
            .unwrap();
        crate::maputils::integrate_map_rooms(&mut g);
        boot_town_life(&g);

        let table = crate::lock_ok::lock(&routes());
        for car in CARAVANS {
            let path = table
                .get(&car.mob_vnum)
                .unwrap_or_else(|| panic!("caravan {} route missing", car.mob_vnum));
            assert_eq!(g.rooms[path[0]].number, car.home, "route starts at home");
            assert_eq!(
                g.rooms[path[path.len() - 1]].number,
                car.far,
                "route ends at the far post"
            );
            // Every cell must be a room, and the route must not revisit any
            // room (BFS simple path).
            let mut seen = std::collections::HashSet::new();
            for &r in path {
                assert!(g.room_opt(r).is_some(), "route room must exist");
                assert!(
                    seen.insert(r),
                    "route must not revisit room {}",
                    g.rooms[r].number
                );
            }
        }
    }

    #[tokio::test]
    async fn real_lib_caravan_follows_route_by_mud_hour() {
        let _guard = clock_lock();
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        if !std::path::Path::new(&format!("{}/world/worldmap", lib)).exists() {
            return;
        }
        crate::weather::test_clock::set_hour(12); // away hours
        let mut g = crate::state::GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib)
            .await
            .unwrap();
        crate::maputils::integrate_map_rooms(&mut g);
        boot_town_life(&g);

        let path = crate::lock_ok::lock(&routes()).get(&304).cloned().unwrap();
        let home = path[0];
        let courier = npc(&mut g, 304, home);

        // One drive = one cell toward the far post.
        drive(&mut g, courier);
        assert_eq!(
            g.get_char(courier).unwrap().in_room,
            Some(path[1]),
            "caravan must advance one cell per drive"
        );

        // After the return hour, the same courier heads home.
        crate::weather::test_clock::set_hour(17);
        drive(&mut g, courier);
        assert_eq!(g.get_char(courier).unwrap().in_room, Some(home));
    }
}
