// maputils.c — the DeltaMUD surface ("outside") world-map renderer.
//
// Port scope: the ASCII overworld map engine and its player commands.
//   * do_map        ("map [world|weather] [x1 y1 x2 y2]")  — the scrolling
//                     world / weather map (immortal "map" command).
//   * do_togglemap  ("togglemap on|off")                   — load/unload the
//                     surface map (a wizard toggle in C; here it flips the
//                     module-static MAP_ACTIVE state).
//   * pweather      ("weather")                            — the player-centred
//                     weather map (printweather).
//   * lweather      (immortal "lweather")                  — in production this
//                     command does nothing but echo a hex parse of its first
//                     argument and `return` before any weather editing; we port
//                     that exact early-return behaviour.
//   * printmap      — the player-centred surface map drawn by look_at_room when
//                     a PC stands on a map room (exported for cmd_informative).
//
// The C map system loads `world/worldmap` into a block of extra `world[]` rooms
// (indices map_start_room..top_of_world) whose `id` field is the rendered glyph
// string ("&G+", "&B~", " ", ...) taken from a per-glyph SectShow definition.
// `find_room_by_coords()` maps 1-based (x,y) onto that block with the world
// wrapping around (it is "ROUND!"). The Rust world loader does not splice those
// map rooms into GameState.rooms, so we instead parse `world/worldmap` once into
// a cached MapData (sector table + glyph grid + the precomputed per-cell render
// string) and render straight off the grid — the visible output is identical.
//
// The live weather subsystem (spawn/activity/collision storms keyed off the
// heartbeat) IS ported here (W7). On map load the storm list is seeded by
// init_weather() exactly as C does (MAX_WEATHER random storms), and the
// heartbeat calls weather_activity() every 30 RL-seconds (300 pulses) to age,
// move, collide, respawn and re-render the storms. The derived weather_map glyph
// grid + per-cell room weather live alongside MapData and feed the renderer, so
// `map weather`, `pweather` and the advanced overlay show live storm glyphs.
// (The unit_activity damage path that hurls/fries PCs requires players standing
// in map cells; the Rust world does not splice the map-room block into
// GameState.rooms, so no PC ever occupies a map cell and that path finds nobody
// — same observable result as a quiet map with no one outside.)
//
// Module-static state: the parsed map (keyed by lib_path) plus the MAP_ACTIVE
// load toggle and the live storm list, behind a Mutex/OnceLock like the other
// runtime tables.

use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ---- weather constants (maputils.h) ---------------------------------------

const WEATHER_TOTAL: usize = 10;

// WEATHER_* indices (maputils.h). Used by the storm spawn/collision logic.
const WEATHER_NONE: i32 = -1;
const WEATHER_RAINSTORM: usize = 0;
const WEATHER_SNOWSTORM: usize = 1;
const WEATHER_THUNDERSTORM: usize = 2;
const WEATHER_FIRESTORM: usize = 3;
const WEATHER_FOG: usize = 4;
const WEATHER_MAGICFOG: usize = 5;
const WEATHER_HURRICANE: usize = 6;
const WEATHER_TORNADO: usize = 7;
const WEATHER_BLIZZARD: usize = 8;

// MAX_WEATHER (maputils.h): how many storms the map keeps spawned.
const MAX_WEATHER: i32 = 4;

// AVOID_WEATHER(x) (maputils.h): weather types the advanced map overlays.
fn avoid_weather(x: i32) -> bool {
    matches!(
        x as usize,
        WEATHER_THUNDERSTORM
            | WEATHER_FIRESTORM
            | WEATHER_MAGICFOG
            | WEATHER_HURRICANE
            | WEATHER_TORNADO
            | WEATHER_BLIZZARD
    ) || x as usize == 9 /* WEATHER_DEATH */
}

// CircleMUD NORTH/SOUTH/EAST/WEST direction indices (structs.h) used by storms.
const NORTH: i32 = 0;
const EAST: i32 = 1;
const SOUTH: i32 = 2;
const WEST: i32 = 3;

// weather_data[type][...] (maputils.c): {speed, radius, damage, dir-change%,
// lifetime-in-half-minutes}. Only speed/radius/damage/dir%/lifetime are needed
// for the render-facing spawn/move/collide/expire logic.
const WEATHER_DATA: [[i32; 5]; WEATHER_TOTAL] = [
    [1, 3, 0, 5, 20],      // rain
    [1, 4, 0, 9, 48],      // snow
    [2, 4, 0, 13, 14],     // thunder
    [4, 2, 70, 20, 10],    // fire
    [0, 5, 0, 0, 16],      // fog
    [0, 5, 0, 0, 24],      // mfog
    [2, 5, 20, 15, 14],    // hurricane
    [3, 3, 10, 25, 12],    // tornado
    [1, 6, 7, 5, 96],      // blizzard
    [0, 10, 9999, 0, 96],  // DEATH
];

// Per-weather glyph / colour / name tables (maputils.c top of file).
const WEATHER_CHARS: [char; WEATHER_TOTAL] =
    ['R', 'S', 't', 'F', 'f', 'M', 'H', 'T', 'B', 'D'];

const WEATHER_NAMES: [&str; WEATHER_TOTAL] = [
    "rain storm",
    "snow storm",
    "thunder storm",
    "fire storm",
    "fog",
    "magical fog",
    "hurricane",
    "tornado",
    "blizzard",
    "DEATH STORM",
];

const WEATHER_COLORS: [&str; WEATHER_TOTAL] =
    ["&B", "&W", "&g", "&r", "&c", "&C", "&G", "&R", "&w", "&M"];

// Direction / filler glyphs used by the weather map (maputils.h).
const DIRECTION_NORTH: char = '^';
const DIRECTION_SOUTH: char = 'v';
const DIRECTION_WEST: char = '<';
const DIRECTION_EAST: char = '>';
const DIRECTION_STATIONARY: char = '-';
const FILLER_CHAR: char = '+';

// printmap vision radii (maputils.c #defines).
const MAP_VISION_RADIUS_X: i32 = 3;
const MAP_VISION_RADIUS_Y: i32 = 3;
const MAP_INDENT: &str = " ";

// printweather vision radii.
const WEATHER_VISION_RADIUS_X: i32 = 20;
const WEATHER_VISION_RADIUS_Y: i32 = 8;

// PRF2_* (structs.h). The advanced map overlays weather glyphs onto the terrain
// in check_noroom when the cell carries an AVOID_WEATHER storm and the viewer has
// PRF2_ADVANCEDMAP set.
const PRF2_ADVANCEDMAP: i64 = 1 << 8;

// ---------------------------------------------------------------------------
// Parsed map data
// ---------------------------------------------------------------------------

/// One sector definition (`NewSector:` block in world/worldmap).
#[derive(Clone)]
struct Sector {
    /// SectShow — the rendered glyph string ("&G+", " ", ...). C: s->show.
    show: String,
}

/// One live storm (C: struct w_index). The C `in_room` is a map rnum; we store
/// it as the storm centre's 1-based (x,y) instead, since the Rust map has no
/// world-room block. left/dir/radius mirror the C fields.
#[derive(Clone)]
struct Storm {
    /// weather type index (WEATHER_* / weather_data row).
    wtype: usize,
    /// centre cell, 1-based (x,y) (C rm2x/rm2y of w->in_room).
    x: i32,
    y: i32,
    /// half-minutes until expiry (C: w->left, decremented per weather_activity).
    left: i32,
    /// movement direction NORTH/EAST/SOUTH/WEST (C: w->dir).
    dir: i32,
    /// storm radius (C: w->radius).
    radius: i32,
}

struct MapData {
    /// Whether the map is currently "loaded" (C: MAP_ACTIVE). togglemap flips it.
    active: bool,
    max_x: i32,
    max_y: i32,
    /// Grid of glyph ids, row-major, grid[y0][x0] for 0-based y0/x0. Empty when
    /// the file was missing/unparsable (map never becomes active then).
    grid: Vec<Vec<char>>,
    /// glyph id -> rendered SectShow string. C: find_sect_by_id()->show.
    sectors: HashMap<char, Sector>,

    // ---- live weather state (W7) ----
    /// The active storms (C: the weather_index linked list).
    storms: Vec<Storm>,
    /// num_weather (C global): storm "weight" used to gate respawns.
    num_weather: i32,
    /// Whether init_weather() has run for this map yet (storms seeded once).
    weather_inited: bool,
    /// weather_map[y0][x0] render string per cell (C: char ***weather_map).
    /// Rebuilt by update_weather_map(); empty until the map is active.
    weather_map: Vec<Vec<String>>,
    /// Per-cell room weather type (C: world[i].weather), set by swc(); read by
    /// check_noroom / printmap fog logic. -1 == WEATHER_NONE.
    room_weather: Vec<Vec<i32>>,
}

impl MapData {
    fn empty() -> MapData {
        MapData {
            active: false,
            max_x: 0,
            max_y: 0,
            grid: Vec::new(),
            sectors: HashMap::new(),
            storms: Vec::new(),
            num_weather: 0,
            weather_inited: false,
            weather_map: Vec::new(),
            room_weather: Vec::new(),
        }
    }

    /// MAP_ACTIVE (maputils.h): map loaded *and* toggled on.
    fn is_active(&self) -> bool {
        self.active && self.max_x > 0 && self.max_y > 0 && !self.sectors.is_empty()
    }

    /// The render string for a 1-based (x,y) grid cell — the C `world[m].id`.
    /// Unknown glyphs fall back to a single space (the inaccessible-indoors
    /// look); C would have aborted at load on a missing sector, but a quiet
    /// fallback keeps the renderer total here.
    fn cell_id(&self, x: i32, y: i32) -> &str {
        let (nx, ny) = self.wrap(x, y);
        let glyph = self.grid[(ny - 1) as usize][(nx - 1) as usize];
        match self.sectors.get(&glyph) {
            Some(s) => &s.show,
            None => " ",
        }
    }

    /// C WRAPX/WRAPY macros: the world is round, so coordinates wrap into 1..=max.
    fn wrap(&self, mut x: i32, mut y: i32) -> (i32, i32) {
        while x > self.max_x {
            x -= self.max_x;
        }
        while x < 1 {
            x += self.max_x;
        }
        while y > self.max_y {
            y -= self.max_y;
        }
        while y < 1 {
            y += self.max_y;
        }
        (x, y)
    }

    // =====================================================================
    // Live weather subsystem (maputils.c / W7). Storm spawn, move, collide,
    // expire, respawn, and the weather_map glyph grid the renderer reads.
    // All RNG goes through the passed Rng so seed-pinned runs reproduce.
    // =====================================================================

    /// w_cchars index for a given weather type + glyph slot (C's flat w_cchars
    /// array, built in weather_alloc). The C layout is:
    ///   w_cchars[0]            = "&n+"          (filler)
    ///   w_cchars[type*6 + 1]   = colour + NORTH dir glyph
    ///   w_cchars[type*6 + 2]   = colour + SOUTH
    ///   w_cchars[type*6 + 3]   = colour + EAST
    ///   w_cchars[type*6 + 4]   = colour + WEST
    ///   w_cchars[type*6 + 5]   = colour + STATIONARY
    ///   w_cchars[type*6 + 6]   = colour + the weather's own char
    /// We build the string directly rather than caching the flat array.
    fn w_cchar(wtype: usize, slot: i32) -> String {
        // slot: 1=N 2=S 3=E 4=W 5=stationary 6=weatherchar
        let color = WEATHER_COLORS[wtype];
        let g = match slot {
            1 => DIRECTION_NORTH,
            2 => DIRECTION_SOUTH,
            3 => DIRECTION_EAST,
            4 => DIRECTION_WEST,
            5 => DIRECTION_STATIONARY,
            _ => WEATHER_CHARS[wtype],
        };
        format!("{}{}", color, g)
    }

    /// isinradius_bycoords (maputils.c): point (x1,y1) within `radius` of (x2,y2)
    /// using the squared-distance circle test.
    fn isinradius_bycoords(x1: i32, y1: i32, x2: i32, y2: i32, radius: i32) -> bool {
        (x1 - x2) * (x1 - x2) + (y1 - y2) * (y1 - y2) <= radius * radius
    }

    /// isinradius_wrap (maputils.c): try the nine map-wrapped placements of
    /// (x1,y1); returns the WRAP_* method that matched, or 0 if none.
    fn isinradius_wrap(&self, x1: i32, y1: i32, x2: i32, y2: i32, radius: i32) -> i32 {
        let mx = self.max_x;
        let my = self.max_y;
        if Self::isinradius_bycoords(x1, y1, x2, y2, radius) {
            return 1; // WRAP_NORM_NORM
        }
        if Self::isinradius_bycoords(x1 + mx, y1, x2, y2, radius) {
            return 4; // WRAP_PLUS_NORM
        }
        if Self::isinradius_bycoords(x1, y1 + my, x2, y2, radius) {
            return 3; // WRAP_NORM_PLUS
        }
        if Self::isinradius_bycoords(x1 + mx, y1 + my, x2, y2, radius) {
            return 6; // WRAP_PLUS_PLUS
        }
        if Self::isinradius_bycoords(x1 - mx, y1, x2, y2, radius) {
            return 7; // WRAP_LESS_NORM
        }
        if Self::isinradius_bycoords(x1, y1 - my, x2, y2, radius) {
            return 2; // WRAP_NORM_LESS
        }
        if Self::isinradius_bycoords(x1 - mx, y1 - my, x2, y2, radius) {
            return 8; // WRAP_LESS_LESS
        }
        if Self::isinradius_bycoords(x1 + mx, y1 - my, x2, y2, radius) {
            return 5; // WRAP_PLUS_LESS
        }
        if Self::isinradius_bycoords(x1 - mx, y1 + my, x2, y2, radius) {
            return 9; // WRAP_LESS_PLUS
        }
        0
    }

    /// wrap_method_x / wrap_method_y (maputils.c): apply a WRAP_* x/y offset.
    fn wrap_method_x(&self, x: i32, method: i32) -> i32 {
        match method {
            1 | 2 | 3 => x,           // NORM_*
            4 | 5 | 6 => x + self.max_x, // PLUS_*
            7 | 8 | 9 => x - self.max_x, // LESS_*
            _ => 0,
        }
    }
    fn wrap_method_y(&self, y: i32, method: i32) -> i32 {
        match method {
            1 | 4 | 7 => y,           // *_NORM
            3 | 6 | 9 => y + self.max_y, // *_PLUS
            2 | 5 | 8 => y - self.max_y, // *_LESS
            _ => 0,
        }
    }

    /// rand_weather (maputils.c): weighted random weather type for a new storm.
    fn rand_weather(rng: &mut crate::rng::Rng) -> usize {
        let i = rng.number(1, 100);
        if i >= 80 {
            return WEATHER_RAINSTORM;
        }
        if i >= 60 {
            return WEATHER_THUNDERSTORM;
        }
        if i >= 40 {
            return WEATHER_FOG;
        }
        if i >= 30 {
            return WEATHER_MAGICFOG;
        }
        if i >= 20 {
            return WEATHER_SNOWSTORM;
        }
        match rng.number(1, 4) {
            1 => WEATHER_BLIZZARD,
            2 => WEATHER_FIRESTORM,
            3 => WEATHER_HURRICANE,
            4 => WEATHER_TORNADO,
            _ => WEATHER_RAINSTORM,
        }
    }

    /// reset_num_weather (maputils.c): recount num_weather, where a storm bigger
    /// than its base radius counts as multiple occurrences.
    fn reset_num_weather(&mut self) {
        self.num_weather = 0;
        for w in &self.storms {
            let base = WEATHER_DATA[w.wtype][1];
            if w.radius != base && base != 0 {
                self.num_weather += w.radius / base;
            } else {
                self.num_weather += 1;
            }
        }
    }

    /// spawn_weather (maputils.c): add a new storm centred on a random map cell.
    /// `dir` < 0 picks a random direction. C seeds the centre from a random map
    /// rnum; we pass a random (x,y) cell instead (any cell is a valid centre).
    fn spawn_weather(&mut self, wtype: usize, dir: i32, x: i32, y: i32, rng: &mut crate::rng::Rng) {
        if wtype >= WEATHER_TOTAL {
            return;
        }
        let dir = if !(0..=3).contains(&dir) { rng.number(0, 3) } else { dir };
        self.num_weather += 1;
        let storm = Storm {
            wtype,
            x,
            y,
            left: WEATHER_DATA[wtype][4],
            dir,
            radius: WEATHER_DATA[wtype][1],
        };
        self.storms.push(storm);
        // C send_weather_messages(WEATHER_MSG_FORM) needs players in map cells;
        // the Rust map has none, so the form announcement reaches nobody.
    }

    /// A random 1-based map cell. C draws a single random map rnum
    /// (number(map_start_room, top_of_world)) and derives (x,y) via rm2x/rm2y;
    /// we mirror that with one draw over the grid's linear index so the storm
    /// centre distribution (and RNG draw count) matches C.
    fn random_cell(&self, rng: &mut crate::rng::Rng) -> (i32, i32) {
        let cells = self.max_x * self.max_y;
        let r = rng.number(0, cells - 1); // 0-based linear cell index (C: r=room-map_start_room)
        let x = (r % self.max_x) + 1; // rm2x
        let y = (r - (r % self.max_x)) / self.max_x + 1; // rm2y
        (x, y)
    }

    /// init_weather (maputils.c): seed MAX_WEATHER random storms, then build the
    /// weather_map and resolve collisions. Runs once per map load.
    fn init_weather(&mut self, rng: &mut crate::rng::Rng) {
        if self.weather_inited {
            return;
        }
        self.weather_inited = true;
        for _ in 1..=MAX_WEATHER {
            let wtype = Self::rand_weather(rng);
            let (x, y) = self.random_cell(rng);
            self.spawn_weather(wtype, -1, x, y, rng);
        }
        self.update_weather_map();
        self.check_weather_collisions(rng);
    }

    /// check_weather_collisions (maputils.c): merge storms whose centres fall
    /// within each other's radius into a single combined storm, repeating until
    /// no pair collides.
    fn check_weather_collisions(&mut self, rng: &mut crate::rng::Rng) {
        loop {
            let n = self.storms.len();
            let mut merged = false;
            'outer: for wi in 0..n {
                for ti in 0..n {
                    if wi == ti {
                        continue;
                    }
                    let (wx, wy, wrad, wtype, wleft) = {
                        let w = &self.storms[wi];
                        (w.x, w.y, w.radius, w.wtype, w.left)
                    };
                    let (tx, ty, trad, ttype, tleft) = {
                        let t = &self.storms[ti];
                        (t.x, t.y, t.radius, t.wtype, t.left)
                    };
                    // C tests w's centre against tw's radius.
                    let smode = self.isinradius_wrap(wx, wy, tx, ty, trad);
                    if smode != 0 {
                        // Midpoint of the two centres (wrapping w's into range).
                        let x = (self.wrap_method_x(wx, smode) + tx) / 2;
                        let y = (self.wrap_method_y(wy, smode) + ty) / 2;
                        // Resulting type weighted by the two radii.
                        let pick = rng.number(1, wrad + trad);
                        let newtype = if pick <= wrad { wtype } else { ttype };
                        // Collective radius, capped at the map's average dimension.
                        let rad = (wrad + trad).min((self.max_x + self.max_y) / 2);
                        // Collective remaining lifetime.
                        let left = wleft + tleft;
                        let newdir = rng.number(0, 3);
                        // Mutate tw into the merged storm, drop w.
                        let t = &mut self.storms[ti];
                        t.wtype = newtype;
                        t.dir = newdir;
                        t.x = x;
                        t.y = y;
                        t.radius = rad;
                        t.left = left;
                        self.storms.remove(wi);
                        self.reset_num_weather();
                        merged = true;
                        break 'outer;
                    }
                }
            }
            if !merged {
                break;
            }
        }
    }

    /// weather_activity (maputils.c): age each storm, move it, run radial activity
    /// (no PCs in map cells, so no damage), randomly shift direction, expire dead
    /// storms, respawn up to MAX_WEATHER, resolve collisions, and re-render.
    fn weather_activity(&mut self, rng: &mut crate::rng::Rng) {
        let mut i = 0usize;
        while i < self.storms.len() {
            self.storms[i].left -= 1;
            if self.storms[i].left <= 0 {
                // send_weather_messages(WEATHER_MSG_STOP) — nobody in map cells.
                self.storms.remove(i);
                self.reset_num_weather();
                continue;
            }

            let (wtype, dir) = {
                let w = &self.storms[i];
                (w.wtype, w.dir)
            };
            let speed = WEATHER_DATA[wtype][0];

            // Only non-negative-damage storms move (C guards weather_data[..][2]
            // >= 0; every entry is, so this always runs). Move `speed` cells in
            // `dir`, wrapping each step.
            if WEATHER_DATA[wtype][2] >= 0 && speed > 0 {
                for _ in 1..=speed {
                    let (nx, ny) = {
                        let w = &self.storms[i];
                        match dir {
                            NORTH => self.wrap(w.x, w.y - 1),
                            SOUTH => self.wrap(w.x, w.y + 1),
                            EAST => self.wrap(w.x + 1, w.y),
                            WEST => self.wrap(w.x - 1, w.y),
                            _ => (w.x, w.y),
                        }
                    };
                    let w = &mut self.storms[i];
                    w.x = nx;
                    w.y = ny;
                    // radial_activity(w): no PCs in map cells -> no effect.
                }
            }
            // Stationary storms call radial_activity once (also a no-op here).

            // Randomly shift direction.
            if rng.number(1, 100) <= WEATHER_DATA[wtype][3] {
                self.storms[i].dir = rng.number(0, 3);
            }
            // send_weather_messages(WEATHER_MSG_ACT) — nobody in map cells.
            i += 1;
        }

        // Respawn weather one at a time until back up to MAX_WEATHER.
        if self.num_weather < MAX_WEATHER {
            let wtype = Self::rand_weather(rng);
            let (x, y) = self.random_cell(rng);
            self.spawn_weather(wtype, -1, x, y, rng);
        }
        self.check_weather_collisions(rng);
        self.update_weather_map();
    }

    /// swc (maputils.c): set a weather_map cell and the per-cell room weather,
    /// wrapping the coordinates first.
    fn swc(&mut self, x: i32, y: i32, s: String, wtype: i32) {
        let (nx, ny) = self.wrap(x, y);
        let xi = (nx - 1) as usize;
        let yi = (ny - 1) as usize;
        self.weather_map[yi][xi] = s;
        self.room_weather[yi][xi] = wtype;
    }

    /// update_weather_map (maputils.c): clear the grid to filler, then paint each
    /// storm's diamond (checkerboard of dir-glyph / weather-char cells).
    fn update_weather_map(&mut self) {
        let filler = format!("&n{}", FILLER_CHAR);
        for y in 1..=self.max_y {
            for x in 1..=self.max_x {
                self.swc(x, y, filler.clone(), WEATHER_NONE);
            }
        }
        // Snapshot storm fields (avoid borrow conflict with swc's &mut self).
        let storms: Vec<(usize, i32, i32, i32, i32)> = self
            .storms
            .iter()
            .map(|w| (w.wtype, w.x, w.y, w.dir, w.radius))
            .collect();
        for (wtype, cx, cy, dir, radius) in storms {
            // Directional glyph (or the storm char for stationary storms).
            let dirchar = if WEATHER_DATA[wtype][0] <= 0 {
                Self::w_cchar(wtype, 5) // stationary
            } else {
                match dir {
                    NORTH => Self::w_cchar(wtype, 1),
                    SOUTH => Self::w_cchar(wtype, 2),
                    EAST => Self::w_cchar(wtype, 3),
                    WEST => Self::w_cchar(wtype, 4),
                    _ => Self::w_cchar(wtype, 5),
                }
            };
            let wchar = Self::w_cchar(wtype, 6);
            for y in (cy - radius)..=(cy + radius) {
                for x in (cx - radius)..=(cx + radius) {
                    if Self::isinradius_bycoords(x, y, cx, cy, radius) {
                        // Checkerboard: odd (x+y) => dir glyph, even => weather char.
                        if (x + y) % 2 != 0 {
                            self.swc(x, y, dirchar.clone(), wtype as i32);
                        } else {
                            self.swc(x, y, wchar.clone(), wtype as i32);
                        }
                    }
                }
            }
        }
    }

    /// weather_map[y-1][x-1] render string, wrapping coords (1-based input).
    fn wmap_cell(&self, x: i32, y: i32) -> &str {
        let (nx, ny) = self.wrap(x, y);
        &self.weather_map[(ny - 1) as usize][(nx - 1) as usize]
    }

    /// Per-cell room weather type (C: world[i].weather), wrapping coords.
    fn cell_weather(&self, x: i32, y: i32) -> i32 {
        let (nx, ny) = self.wrap(x, y);
        self.room_weather[(ny - 1) as usize][(nx - 1) as usize]
    }
}

static MAP: OnceLock<Mutex<HashMap<String, MapData>>> = OnceLock::new();

fn map_table() -> &'static Mutex<HashMap<String, MapData>> {
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parse `<lib>/world/worldmap` into a MapData. On any structural problem the
/// returned data is inactive (empty grid), mirroring "map not loaded".
fn parse_worldmap(lib_path: &str) -> MapData {
    let path = std::path::Path::new(lib_path).join("world").join("worldmap");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return MapData::empty(),
    };

    let mut sectors: HashMap<char, Sector> = HashMap::new();
    let mut grid: Vec<Vec<char>> = Vec::new();
    let mut max_x: i32 = 0;
    let mut max_y: i32 = 0;

    // Sector parse state.
    let mut cur_id: Option<char> = None;
    let mut cur_show: Option<String> = None;
    // Grid parse state.
    let mut in_grid = false;

    for raw in contents.lines() {
        // JUDOCHOP already done by lines(); also drop any stray CR.
        let line = raw.trim_end_matches(['\r', '\n']);

        if in_grid {
            if line.starts_with('~') {
                in_grid = false;
                continue;
            }
            // Each grid row is a string of glyph ids; width must be consistent.
            let row: Vec<char> = line.chars().collect();
            if row.is_empty() {
                continue;
            }
            if max_x == 0 {
                max_x = row.len() as i32;
            }
            // Inconsistent width => corrupt map; bail to inactive (C exit(0)).
            if row.len() as i32 != max_x {
                return MapData::empty();
            }
            grid.push(row);
            max_y += 1;
            continue;
        }

        let arg1 = get_arg(line, 1);

        if compare(&arg1, "NewSector:") {
            // Flush any previous sector that lacked an explicit EndSector.
            if let Some(id) = cur_id.take() {
                sectors.insert(id, Sector { show: cur_show.take().unwrap_or_else(|| " ".into()) });
            }
            let idarg = get_arg(line, 2);
            cur_id = idarg.chars().next();
            cur_show = None;
            continue;
        }
        if compare(&arg1, "SectShow:") {
            // C: if buf[10]!=' ' take the 2nd token, else show is a lone space.
            // get_arg(buf,2) yields the token (empty => single space).
            let tok = get_arg(line, 2);
            cur_show = Some(if tok.is_empty() { " ".to_string() } else { tok });
            continue;
        }
        if compare(&arg1, "EndSector") {
            if let Some(id) = cur_id.take() {
                sectors.insert(id, Sector { show: cur_show.take().unwrap_or_else(|| " ".into()) });
            }
            continue;
        }
        if compare(&arg1, "WorldMap:") {
            max_x = 0;
            max_y = 0;
            grid.clear();
            in_grid = true;
            continue;
        }
        // SectName/SectMove/SectDesc/SectSect/EntryPoint/ZWeatherPoint/etc are
        // not needed for rendering; ignore them.
    }

    // Flush a trailing sector with no EndSector.
    if let Some(id) = cur_id.take() {
        sectors.insert(id, Sector { show: cur_show.take().unwrap_or_else(|| " ".into()) });
    }

    if max_x == 0 || max_y == 0 || sectors.is_empty() {
        return MapData::empty();
    }

    // weather_alloc(): allocate the weather_map / room_weather grids. The actual
    // glyph strings are filled by update_weather_map() once storms are seeded.
    let filler = format!("&n{}", FILLER_CHAR);
    let weather_map = vec![vec![filler.clone(); max_x as usize]; max_y as usize];
    let room_weather = vec![vec![WEATHER_NONE; max_x as usize]; max_y as usize];

    MapData {
        active: true,
        max_x,
        max_y,
        grid,
        sectors,
        storms: Vec::new(),
        num_weather: 0,
        weather_inited: false,
        weather_map,
        room_weather,
    }
}

/// Run `f` against the (lazily parsed, per-lib_path cached) map data.
fn with_map<R>(g: &GameState, f: impl FnOnce(&MapData) -> R) -> R {
    let lib = g.config.lib_path.clone();
    let mut tbl = map_table().lock().unwrap();
    if !tbl.contains_key(&lib) {
        let data = parse_worldmap(&lib);
        tbl.insert(lib.clone(), data);
    }
    f(tbl.get(&lib).unwrap())
}

/// Mutable variant for togglemap's active flag.
fn with_map_mut<R>(g: &GameState, f: impl FnOnce(&mut MapData) -> R) -> R {
    let lib = g.config.lib_path.clone();
    let mut tbl = map_table().lock().unwrap();
    if !tbl.contains_key(&lib) {
        let data = parse_worldmap(&lib);
        tbl.insert(lib.clone(), data);
    }
    f(tbl.get_mut(&lib).unwrap())
}

// ---------------------------------------------------------------------------
// C string helpers (get_arg / compare) — kept local to mirror maputils.c.
// ---------------------------------------------------------------------------

/// get_arg(string, argnum): the argnum-th (1-based) space-delimited token.
/// Leading spaces are skipped; multiple spaces collapse like the C version.
fn get_arg(string: &str, argnum: usize) -> String {
    string.split(' ').filter(|t| !t.is_empty()).nth(argnum.saturating_sub(1)).unwrap_or("").to_string()
}

/// compare(a,b): exact, case-insensitive equality of two whole strings.
fn compare(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ---------------------------------------------------------------------------
// Coordinate helpers. We work directly in (x,y) since the Rust world has no
// map-room block; the player marker uses the room's map_x/map_y instead of the
// C `m == ch->in_room` room-identity test.
// ---------------------------------------------------------------------------

/// The player's 1-based (x,y) map cell, if the room they stand in is a map room
/// (its map_x/map_y are set). None => no `#` marker is drawn (the player is not
/// on the surface map).
fn player_xy(g: &GameState, ch: CharId) -> Option<(i32, i32)> {
    let c = g.get_char(ch)?;
    let rnum = c.in_room?;
    let room = g.rooms.get(rnum)?;
    match (room.map_x, room.map_y) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// do_map — the scrolling world / weather map.
// ---------------------------------------------------------------------------

pub fn do_map(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // Snapshot everything we need before building the (large) output buffer.
    let (active, max_x, max_y) = with_map(g, |m| (m.is_active(), m.max_x, m.max_y));
    if !active {
        return; // C: if (!MAP_ACTIVE) return;
    }

    // Args: map [world | weather] [[x1] [y1] [x2] [y2]]
    let a1 = get_arg(arg, 1);
    let mode = if a1.eq_ignore_ascii_case("weather") { 2 } else { 1 };

    // C: MIN(MAX(atoi(buf), 1), max) — i.e. clamp into 1..=max.
    let xm = atoi(&get_arg(arg, 2)).clamp(1, max_x);
    let ym = atoi(&get_arg(arg, 3)).clamp(1, max_y);

    let a4 = atoi(&get_arg(arg, 4));
    let xl = if a4 == 0 {
        (xm + 98).min(max_x)
    } else {
        a4.max(xm).min((xm + 98).min(max_x))
    };
    let a5 = atoi(&get_arg(arg, 5));
    let yl = if a5 == 0 {
        (ym + 98).min(max_y)
    } else {
        a5.max(ym).min((ym + 98).min(max_y))
    };

    let pos = player_xy(g, ch);

    let out = match mode {
        2 => render_weather_world_map(g, xm, ym, xl, yl, pos),
        _ => render_world_map(g, xm, ym, xl, yl, pos),
    };

    g.send_to_char(ch, &out);
}

/// "Map of the World" (do_map default mode). Renders terrain glyphs with C's
/// run-length colour-code elision (a cell reusing the previous cell's colour
/// prints only the bare glyph), then a right-margin y label per row.
fn render_world_map(
    g: &GameState,
    xm: i32,
    ym: i32,
    xl: i32,
    yl: i32,
    player: Option<(i32, i32)>,
) -> String {
    let mut out = String::from("Map of the World:\r\n");
    with_map(g, |m| {
        for y in ym..=yl {
            for x in xm..=xl {
                if player == Some((x, y)) {
                    out.push_str("&n#");
                    continue;
                }
                let id = m.cell_id(x, y);
                let id_bytes = id.as_bytes();
                if x > xm {
                    let left = m.cell_id(x - 1, y);
                    // Same leading 2-char colour code as the cell to our left,
                    // and the cell starts with '&': print only the glyph char.
                    if id_bytes.first() == Some(&b'&')
                        && id.len() >= 3
                        && left.len() >= 2
                        && id_bytes.get(..2) == left.as_bytes().get(..2)
                    {
                        out.push(id.as_bytes()[2] as char);
                        continue;
                    }
                }
                out.push_str(id);
            }
            out.push_str(&format!("&n{:3}", y));
            out.push_str("\r\n");
        }
    });
    out
}

/// "Map of the World's Weather" (do_map weather mode). Renders the live
/// weather_map glyph grid with C's colour-code elision against the cell to the
/// left and the player marker.
fn render_weather_world_map(
    g: &GameState,
    xm: i32,
    ym: i32,
    xl: i32,
    yl: i32,
    player: Option<(i32, i32)>,
) -> String {
    let mut out = String::from("Map of the World's Weather:\r\n");
    with_map(g, |m| {
        for y in ym..=yl {
            for x in xm..=xl {
                if player == Some((x, y)) {
                    out.push_str("&n#");
                    continue;
                }
                let cur = m.wmap_cell(x, y);
                let cur_b = cur.as_bytes();
                if x > xm {
                    // C indexes weather_map[y-1][x-2] (raw, with the < 0 wrap).
                    let left = m.wmap_cell(x - 1, y);
                    if cur_b.first() == Some(&b'&')
                        && cur.len() >= 3
                        && left.len() >= 2
                        && cur_b.get(..2) == left.as_bytes().get(..2)
                    {
                        out.push(cur.as_bytes()[2] as char);
                        continue;
                    }
                }
                out.push_str(cur);
            }
            out.push_str(&format!("&n{:3}", y));
            out.push_str("\r\n");
        }
    });
    out
}

// ---------------------------------------------------------------------------
// printmap — the player-centred surface map (look_at_room). Exported so the
// look/informative code can draw it when a PC stands on a map room.
// ---------------------------------------------------------------------------

/// Draw the small player-centred terrain map around the character. The centre
/// cell is the player ("&w#"); cells radiate out by the sight radius, which
/// immortals see further and fog shrinks. Wired into look_at_room (the C call
/// site) by the informative/look batch.
#[allow(dead_code)]
pub fn printmap(g: &mut GameState, ch: CharId) {
    let (cx, cy) = match player_xy(g, ch) {
        Some(p) => p,
        None => return,
    };
    let (active, level) = match g.get_char(ch) {
        Some(c) => (with_map(g, |m| m.is_active()), c.player.level),
        None => return,
    };
    if !active {
        return; // C: map_start_room==-1 || !sect_index
    }

    let advancedmap = g.get_char(ch).map(|c| c.prf2_flags & PRF2_ADVANCEDMAP != 0).unwrap_or(false);

    let mut sightradmult: i32 = 2;
    if level >= LVL_IMMORT {
        sightradmult += 1;
    }
    // Surface-room fog shrinks the view (WEATHER_FOG) for mortals and inverts it
    // (WEATHER_MAGICFOG). C reads world[ch->in_room].weather; with live storms
    // the standing cell may now carry fog/magic-fog.
    let standing_weather = with_map(g, |m| m.cell_weather(cx, cy));
    let mortal = level < LVL_IMMORT;
    if standing_weather == WEATHER_FOG as i32 && mortal {
        sightradmult -= 1;
    }
    let invert = standing_weather == WEATHER_MAGICFOG as i32 && mortal;
    let radius = MAP_VISION_RADIUS_X * sightradmult;

    let mut buf = String::new();
    buf.push_str(MAP_INDENT);
    buf.push_str("&y+&n Map of Deltania &y+\r\n");
    buf.push_str("&n.&c");
    for _ in (-MAP_VISION_RADIUS_X * sightradmult)..=(MAP_VISION_RADIUS_X * sightradmult) {
        buf.push('-');
    }
    buf.push_str("&n.\r\n");

    let ry = MAP_VISION_RADIUS_Y - if sightradmult == 1 { 2 } else { 0 };
    with_map(g, |m| {
        let weather_active = !m.storms.is_empty();
        if !invert {
            for j in -ry..=ry {
                buf.push_str("&c|");
                for k in -radius..=radius {
                    if k == 0 && j == 0 {
                        buf.push_str("&w#");
                    } else {
                        buf.push_str(&check_noroom(
                            m,
                            cx,
                            cy,
                            cx + k,
                            cy + j,
                            radius,
                            0,
                            weather_active,
                            advancedmap,
                        ));
                    }
                }
                buf.push_str("&c|\r\n");
            }
        } else {
            // WEATHER_MAGICFOG inversion: rows top-to-bottom and columns
            // right-to-left, modifier==1 (the C inverted branch).
            for j in (-MAP_VISION_RADIUS_Y..=MAP_VISION_RADIUS_Y).rev() {
                buf.push_str("&c|");
                for k in (-radius..=radius).rev() {
                    if k == 0 && j == 0 {
                        buf.push_str("&w#");
                    } else {
                        buf.push_str(&check_noroom(
                            m,
                            cx,
                            cy,
                            cx + k,
                            cy + j,
                            radius,
                            1,
                            weather_active,
                            advancedmap,
                        ));
                    }
                }
                buf.push_str("&c|\r\n");
            }
        }
    });

    buf.push_str("`&c");
    for _ in (-MAP_VISION_RADIUS_X * sightradmult)..=(MAP_VISION_RADIUS_X * sightradmult) {
        buf.push('-');
    }
    buf.push_str("&n'\r\n");

    g.send_to_char(ch, &buf);
}

/// check_noroom (maputils.c): the glyph for a cell, with the advanced-map weather
/// overlay and the same run-length colour elision as the world map. When the
/// viewer has PRF2_ADVANCEDMAP set, weather is active, and the cell carries an
/// AVOID_WEATHER storm, the glyph becomes "&K" + the storm's display char
/// (C: wmstr); otherwise it is the terrain glyph (world[i].id). A cell that
/// shares the leading 2-char "&X" colour of the cell on its left drops that code
/// — UNLESS that left cell is the player's own cell, or this cell sits at the
/// left edge of the visible window. `(px,py)` is the player centre; `radius` is
/// the window half-width; `modifier` 0 looks left, 1 looks right (the inverted
/// magic-fog path).
fn check_noroom(
    m: &MapData,
    px: i32,
    py: i32,
    x: i32,
    y: i32,
    radius: i32,
    modifier: i32,
    weather_active: bool,
    advancedmap: bool,
) -> String {
    // Overlay or terrain glyph for the target cell.
    let tmp = noroom_glyph(m, x, y, weather_active, advancedmap);
    // C: modifier==0 looks at rm2x(i)-1, modifier==1 at rm2x(i)+1.
    let (lx, lf) = if modifier == 0 { (x - 1, -1) } else { (x + 1, 1) };
    let left = noroom_glyph(m, lx, y, weather_active, advancedmap);
    let tmp_b = tmp.as_bytes();

    // j = rm2x(ch->in_room) +/- radius, wrapped: the x of the window's edge column.
    let mut j = if modifier == 0 { px - radius } else { px + radius };
    if modifier == 0 {
        if j < 1 {
            j += m.max_x;
        }
    } else if j > m.max_x {
        j -= m.max_x;
    }
    let (cur_x, _) = m.wrap(x, y);
    // "left is the player": (px,py) == the neighbour cell at (x+lf, y).
    let neighbour_is_player = px == x + lf && py == y;

    if tmp_b.first() == Some(&b'&')
        && tmp.len() >= 3
        && left.len() >= 2
        && tmp_b.get(..2) == left.as_bytes().get(..2)
        && !neighbour_is_player
        && j != cur_x
    {
        tmp[2..].to_string()
    } else {
        tmp
    }
}

/// The glyph string check_noroom resolves for one cell: the AVOID_WEATHER overlay
/// ("&K" + storm char) when active and the viewer has the advanced map, else the
/// terrain id.
fn noroom_glyph(m: &MapData, x: i32, y: i32, weather_active: bool, advancedmap: bool) -> String {
    if weather_active && advancedmap && avoid_weather(m.cell_weather(x, y)) {
        // C: wmstr = "&K" with wmstr[2] = weather_map[...][2] (the glyph char).
        let cell = m.wmap_cell(x, y);
        let glyph = cell.as_bytes().get(2).copied().unwrap_or(b'?') as char;
        format!("&K{}", glyph)
    } else {
        m.cell_id(x, y).to_string()
    }
}

// ---------------------------------------------------------------------------
// pweather — the player weather map (printweather), and lweather.
// ---------------------------------------------------------------------------

pub fn pweather(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // C: thick (magic) fog blocks reading the weather for mortals; indoors is
    // impossible; otherwise draw the weather map.
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(0);
    let mortal = level < LVL_IMMORT;
    let (cx, cy) = player_xy(g, ch).unwrap_or((1, 1));
    let standing_weather = with_map(g, |m| m.cell_weather(cx, cy));
    if mortal && (standing_weather == WEATHER_FOG as i32 || standing_weather == WEATHER_MAGICFOG as i32)
    {
        g.send_to_char(ch, "The thick fog prevents you from determining the weather.\r\n");
        return;
    }
    let indoors = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.rooms.get(r))
        .map(|room| room.room_flags.contains(crate::room::RoomFlags::INDOORS))
        .unwrap_or(false);
    if indoors {
        g.send_to_char(ch, "Determine the weather indoors!? Impossible!\r\n");
        return;
    }
    printweather(g, ch);
}

/// printweather — the large player-centred weather map with the legend column.
fn printweather(g: &mut GameState, ch: CharId) {
    let active = with_map(g, |m| m.is_active());
    if !active {
        return;
    }
    // Player centre: their map cell, else their zone's ZWeatherPoint (which the
    // Rust world does not carry yet), else fall back to (1,1).
    let (x, y) = player_xy(g, ch).unwrap_or((1, 1));

    let mut buf = String::new();
    buf.push_str(MAP_INDENT);
    buf.push_str("&y+&n Map of Deltania's Weather &y+&n\r\n");
    buf.push_str("&n.&c");
    for _ in -WEATHER_VISION_RADIUS_X..=WEATHER_VISION_RADIUS_X {
        buf.push('-');
    }
    buf.push_str("&n.\r\n");
    g.send_to_char(ch, &buf);

    for j in -WEATHER_VISION_RADIUS_Y..=WEATHER_VISION_RADIUS_Y {
        let mut line = String::from("&c|");
        with_map(g, |m| {
            for k in -WEATHER_VISION_RADIUS_X..=WEATHER_VISION_RADIUS_X {
                if k == 0 && j == 0 {
                    line.push_str("&w#");
                } else {
                    line.push_str(weatherchar(m, x + k, y + j, x));
                }
            }
        });
        line.push_str("&c| ");

        // Right-hand legend (printweather's per-row annotations).
        let row = j + WEATHER_VISION_RADIUS_Y;
        match row {
            0 => line.push_str("&nDirections:"),
            1 => line.push_str(&format!("&n{} = North {} = South", DIRECTION_NORTH, DIRECTION_SOUTH)),
            2 => line.push_str(&format!("&n{} = East  {} = West", DIRECTION_EAST, DIRECTION_WEST)),
            3 => line.push_str(&format!("&n{} = Stationary", DIRECTION_STATIONARY)),
            5 => line.push_str("&nWeather:"),
            r if r > 5 => {
                let idx = (r - 6) as usize;
                if idx < WEATHER_TOTAL {
                    line.push_str(&format!(
                        "{}{} = {}",
                        WEATHER_COLORS[idx], WEATHER_CHARS[idx], WEATHER_NAMES[idx]
                    ));
                }
            }
            _ => {}
        }
        line.push_str("\r\n");
        g.send_to_char(ch, &line);
    }

    let mut foot = String::from("&n`&c");
    for _ in -WEATHER_VISION_RADIUS_X..=WEATHER_VISION_RADIUS_X {
        foot.push('-');
    }
    foot.push_str("&n'\r\n");
    g.send_to_char(ch, &foot);
}

/// weatherchar (maputils.c): the live weather glyph for a cell with colour
/// elision vs the cell to its left, suppressed at the left vision threshold.
/// `inx` is the viewer's centre x.
fn weatherchar<'a>(m: &'a MapData, x: i32, y: i32, inx: i32) -> &'a str {
    let tmp = m.wmap_cell(x, y);
    let left = m.wmap_cell(x - 1, y);
    let tmp_b = tmp.as_bytes();

    // i = rm2x(inroom) - WEATHER_VISION_RADIUS_X, wrapped into 1..=max.
    let mut thr = inx - WEATHER_VISION_RADIUS_X;
    if thr < 1 {
        thr += m.max_x;
    }
    let (cur_x, _) = m.wrap(x, y);

    if tmp_b.first() == Some(&b'&')
        && tmp.len() >= 3
        && left.len() >= 2
        && tmp_b.get(..2) == left.as_bytes().get(..2)
        && !(inx == x - 1)
        && thr != cur_x
    {
        &tmp[2..]
    } else {
        tmp
    }
}

/// lweather (maputils.c): in production this command echoes a hex parse of its
/// first argument and returns immediately — every editing path below the early
/// `return` is dead code. We reproduce the observable behaviour exactly.
pub fn lweather(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let first = get_arg(arg, 1);
    // C: sscanf(argument, "%x", &k); sprintf(buf, "%i\r\n", k);
    // sscanf("%x") reads a leading hex number from the whole argument string.
    let k = parse_leading_hex(arg.trim_start());
    let _ = first;
    g.send_to_char(ch, &format!("{}\r\n", k));
}

/// do_togglemap (maputils.c): "togglemap on|off" loads/unloads the surface map.
/// The C "off" branch is intentionally inert (it sends a warning and returns
/// before unloading); we keep "on" loading the map and reproduce the "off"
/// warning. Both guard against a no-op (already-on / already-off).
///
/// In C this ACMD is only forward-declared and is never added to cmd_info[], so
/// it has no command dispatch arm; it is ported for completeness and to back any
/// future wizard "loadmap"-style wiring.
#[allow(dead_code)]
pub fn do_togglemap(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let argument = arg.trim();
    let name = g.get_char(ch).map(|c| c.get_name().to_string()).unwrap_or_default();

    if compare(argument, "on") {
        let already = with_map(g, |m| m.is_active());
        if already {
            g.send_to_char(ch, "No, no, no!\r\n");
            return;
        }
        // Load (re-parse) and activate the map.
        with_map_mut(g, |m| m.active = true);
        let active_now = with_map(g, |m| m.is_active());
        if !active_now {
            // Parse produced no usable map (missing/corrupt worldmap file).
            with_map_mut(g, |m| m.active = false);
            return;
        }
        mudlog(g, &format!("{} has loaded the surface map.", name), LVL_GOD);
        return;
    }
    if compare(argument, "off") {
        let active = with_map(g, |m| m.is_active());
        if !active {
            g.send_to_char(ch, "No, no, no!\r\n");
            return;
        }
        // C aborts the unload with a warning before doing anything destructive.
        g.send_to_char(ch, "NO! YOU'LL HURT SOMEONE!\r\n");
    }
}

// ---------------------------------------------------------------------------
// Heartbeat entry points (W7): the live weather pulse and blood decay.
// ---------------------------------------------------------------------------

/// prime_weather (db.c read_map -> init_weather, run at boot): seed the surface
/// map's MAX_WEATHER storms and build the initial weather_map so the world has
/// live weather from the first tick, exactly as C does on startup. A no-op when
/// the surface map is not active (no worldmap file).
pub fn prime_weather(g: &mut GameState) {
    let lib = g.config.lib_path.clone();
    let mut tbl = map_table().lock().unwrap();
    if !tbl.contains_key(&lib) {
        let data = parse_worldmap(&lib);
        tbl.insert(lib.clone(), data);
    }
    let m = tbl.get_mut(&lib).unwrap();
    if m.is_active() && !m.weather_inited {
        m.init_weather(&mut g.rng);
    }
}

/// weather_activity (comm.c heartbeat, every 30 RL-seconds = 300 pulses):
/// age/move/collide/respawn the storms and rebuild the weather_map glyph grid.
/// Storms are seeded at boot by prime_weather (the C read_map step); if that
/// somehow hasn't run (map activated later via togglemap), seed lazily here.
/// All RNG goes through g.rng so seed-pinned runs reproduce. A no-op when the
/// surface map is not active.
pub fn weather_activity(g: &mut GameState) {
    let lib = g.config.lib_path.clone();
    let mut tbl = map_table().lock().unwrap();
    if !tbl.contains_key(&lib) {
        let data = parse_worldmap(&lib);
        tbl.insert(lib.clone(), data);
    }
    let m = tbl.get_mut(&lib).unwrap();
    if !m.is_active() {
        return;
    }
    if !m.weather_inited {
        m.init_weather(&mut g.rng);
    }
    m.weather_activity(&mut g.rng);
}

/// blood_update (comm.c heartbeat, every 60 RL-seconds = 600 pulses): decay one
/// unit of blood from every bloodied room (handler.c blood_update).
pub fn blood_update(g: &mut GameState) {
    for room in g.rooms.iter_mut() {
        if room.blood > 0 {
            room.blood -= 1;
        }
    }
}

/// increase_blood(rm) (fight.c): bump a room's blood level by one, capped at 10.
/// Called from the death path so corpses leave a bloodstain that decays via
/// blood_update.
pub fn increase_blood(g: &mut GameState, rnum: RoomRnum) {
    if let Some(room) = g.rooms.get_mut(rnum) {
        room.blood = (room.blood + 1).min(10);
    }
}

// ---------------------------------------------------------------------------
// Small local utilities.
// ---------------------------------------------------------------------------

/// atoi: leading integer, else 0 (C atoi semantics for our arg parsing).
fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let mut end = 0;
    let bytes = s.as_bytes();
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    s[..end].parse::<i32>().unwrap_or(0)
}

/// sscanf("%x") on a leading hex number; 0 if none (matches the uninitialised
/// read C effectively relies on for a non-numeric argument is 0 in practice).
fn parse_leading_hex(s: &str) -> u32 {
    let mut end = 0;
    let bytes = s.as_bytes();
    while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    if end == 0 {
        return 0;
    }
    u32::from_str_radix(&s[..end], 16).unwrap_or(0)
}

/// mudlog: broadcast an immortal log line (same shape as the other modules).
fn mudlog(g: &mut GameState, line: &str, min_level: u8) {
    let formatted = format!("[ {} ]\r\n", line);
    let imms: Vec<CharId> = g
        .players_by_name
        .values()
        .copied()
        .filter(|&id| {
            g.get_char(id)
                .map(|c| c.player.level >= min_level && c.player.level >= LVL_IMMORT)
                .unwrap_or(false)
        })
        .collect();
    for id in imms {
        g.send_to_char(id, &formatted);
    }
}
