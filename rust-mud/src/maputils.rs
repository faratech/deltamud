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

use crate::act::{ActArg, To, act};
use crate::flags::AFF_SANCTUARY;
use crate::room::{Room, SectorType};
use crate::spell_parser::SPELL_REDIRECT_CHARGE;
use crate::state::GameState;
use crate::types::*;
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

// ---- weather constants (maputils.h) ---------------------------------------

const WEATHER_TOTAL: usize = 10;

// WEATHER_* indices (maputils.h). Used by the storm spawn/collision logic.
const WEATHER_NONE: i32 = -1;
// structs.h PRF2_NOMAP (toggle nomap).
const PRF2_NOMAP_LOCAL: i64 = 1 << 2;
const WEATHER_RAINSTORM: usize = 0;
const WEATHER_SNOWSTORM: usize = 1;
const WEATHER_THUNDERSTORM: usize = 2;
const WEATHER_FIRESTORM: usize = 3;
const WEATHER_FOG: usize = 4;
pub const WEATHER_MAGICFOG: usize = 5;
const WEATHER_HURRICANE: usize = 6;
const WEATHER_TORNADO: usize = 7;
const WEATHER_BLIZZARD: usize = 8;

const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;
const AFF_REDIRECT_CHARGE: i64 = 1 << 25;
const AFF_R_CHARGED: i64 = 1 << 26;
const APPLY_DAMAGE: i32 = 22;

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
    [1, 3, 0, 5, 20],     // rain
    [1, 4, 0, 9, 48],     // snow
    [2, 4, 0, 13, 14],    // thunder
    [4, 2, 70, 20, 10],   // fire
    [0, 5, 0, 0, 16],     // fog
    [0, 5, 0, 0, 24],     // mfog
    [2, 5, 20, 15, 14],   // hurricane
    [3, 3, 10, 25, 12],   // tornado
    [1, 6, 7, 5, 96],     // blizzard
    [0, 10, 9999, 0, 96], // DEATH
];

// Per-weather glyph / colour / name tables (maputils.c top of file).
const WEATHER_CHARS: [char; WEATHER_TOTAL] = ['R', 'S', 't', 'F', 'f', 'M', 'H', 'T', 'B', 'D'];

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

const WEATHER_MESSAGES: [&str; WEATHER_TOTAL] = [
    "A rain storm pours down on you from above.",
    "You tread heavily in the snow storm.",
    "You hear the blaring of the thunder storm above you and see lightning in the distance.",
    "Your already blackened skin is singed in the fire storm!",
    "You can barely see your hands in the heavy fog.",
    "You feel very uneasy in the strange fog.",
    "You attempt to hold your ground in the fierce hurricane!",
    "You are savagely hurled around by the tornado!",
    "Your limbs are chilled to the bone as a heavy blizzard looms above you.",
    "You cough blood and develop lesions as death encoils you.",
];

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
const WEATHER_MSG_FORM: i32 = 0;
const WEATHER_MSG_ACT: i32 = 1;
const WEATHER_MSG_STOP: i32 = 2;

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
    /// SectName — the room title shown when a PC stands on the cell. C: s->name.
    name: String,
    /// SectDesc — the room description. C: s->desc (NULL/empty when absent).
    desc: String,
    /// SectSect — the CircleMUD sector type index. C: s->sect (0/Inside default).
    sect: i32,
    /// SectMove — movement cost for map cells. -1 marks an impassable map cell.
    move_cost: i32,
}

/// One EntryPoint directive (maputils.c read_map): a link between a surface map
/// cell (1-based x,y) and a city-interior room (vnum). `dir` is the optional
/// direction (NORTH/EAST/SOUTH/WEST/UP/DOWN); None for the bidirectional
/// linkrnum/linkmapnum form.
#[derive(Clone)]
struct EntryPoint {
    x: i32,
    y: i32,
    interior_vnum: RoomVnum,
    dir: Option<usize>,
}

#[derive(Clone)]
struct ZWeatherPoint {
    x: i32,
    y: i32,
    zone_number: i32,
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

/// One radial_activity() invocation produced by a storm step: the storm centre
/// (1-based x,y), its weather type and radius. The public weather_activity()
/// replays these against GameState to drive unit_activity (weather damage).
#[derive(Clone, Copy)]
struct RadialHit {
    wtype: usize,
    radius: i32,
    x: i32,
    y: i32,
}

#[derive(Clone, Copy)]
enum WeatherEvent {
    Message {
        kind: i32,
        wtype: usize,
        radius: i32,
        x: i32,
        y: i32,
    },
    Radial(RadialHit),
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
    /// EntryPoint directives, applied by integrate_map_rooms() once the map cells
    /// are spliced into GameState.rooms. C: parsed inline in read_map.
    entry_points: Vec<EntryPoint>,
    /// ZWeatherPoint directives, mapping real zones to surface control cells.
    z_weather_points: Vec<ZWeatherPoint>,

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
    /// Per-cell zone weather controller (C: world[map_cell].wzonecontrol).
    cell_wzone_control: Vec<Vec<Option<i32>>>,
}

impl MapData {
    fn empty() -> MapData {
        MapData {
            active: false,
            max_x: 0,
            max_y: 0,
            grid: Vec::new(),
            sectors: HashMap::new(),
            entry_points: Vec::new(),
            z_weather_points: Vec::new(),
            storms: Vec::new(),
            num_weather: 0,
            weather_inited: false,
            weather_map: Vec::new(),
            room_weather: Vec::new(),
            cell_wzone_control: Vec::new(),
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
            1 | 2 | 3 => x,              // NORM_*
            4 | 5 | 6 => x + self.max_x, // PLUS_*
            7 | 8 | 9 => x - self.max_x, // LESS_*
            _ => 0,
        }
    }
    fn wrap_method_y(&self, y: i32, method: i32) -> i32 {
        match method {
            1 | 4 | 7 => y,              // *_NORM
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
        let dir = if !(0..=3).contains(&dir) {
            rng.number(0, 3)
        } else {
            dir
        };
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

    /// weather_activity (maputils.c): age each storm, move it (calling
    /// radial_activity at every cell-step), randomly shift direction, expire dead
    /// storms, respawn up to MAX_WEATHER, resolve collisions, and re-render.
    ///
    /// Returns the ordered list of `RadialHit`s — one per radial_activity() call
    /// C makes (each single-cell move of a moving storm, plus one for each
    /// stationary storm). The caller replays them against GameState so the
    /// damage/knockback path (radial_activity -> unit_activity) reaches PCs now
    /// standing in spliced map rooms.
    fn weather_activity(&mut self, rng: &mut crate::rng::Rng) -> Vec<WeatherEvent> {
        let mut events: Vec<WeatherEvent> = Vec::new();
        let mut i = 0usize;
        while i < self.storms.len() {
            self.storms[i].left -= 1;
            if self.storms[i].left <= 0 {
                let w = &self.storms[i];
                events.push(WeatherEvent::Message {
                    kind: WEATHER_MSG_STOP,
                    wtype: w.wtype,
                    radius: w.radius,
                    x: w.x,
                    y: w.y,
                });
                self.storms.remove(i);
                self.reset_num_weather();
                continue;
            }

            let (wtype, dir, radius) = {
                let w = &self.storms[i];
                (w.wtype, w.dir, w.radius)
            };
            let speed = WEATHER_DATA[wtype][0];

            // C guards weather_data[..][2] >= 0 (no healing storms); every row is,
            // so a moving storm steps `speed` cells, running radial_activity after
            // each single-cell move.
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
                    events.push(WeatherEvent::Radial(RadialHit {
                        wtype,
                        radius,
                        x: nx,
                        y: ny,
                    }));
                }
            }
            // Stationary storms (speed 0) run radial_activity once at their cell.
            if speed == 0 {
                let w = &self.storms[i];
                events.push(WeatherEvent::Radial(RadialHit {
                    wtype,
                    radius,
                    x: w.x,
                    y: w.y,
                }));
            }

            // Randomly shift direction.
            if rng.number(1, 100) <= WEATHER_DATA[wtype][3] {
                self.storms[i].dir = rng.number(0, 3);
            }
            let w = &self.storms[i];
            events.push(WeatherEvent::Message {
                kind: WEATHER_MSG_ACT,
                wtype: w.wtype,
                radius: w.radius,
                x: w.x,
                y: w.y,
            });
            i += 1;
        }

        // Respawn weather one at a time until back up to MAX_WEATHER.
        if self.num_weather < MAX_WEATHER {
            let wtype = Self::rand_weather(rng);
            let (x, y) = self.random_cell(rng);
            self.spawn_weather(wtype, -1, x, y, rng);
            if let Some(w) = self.storms.last() {
                events.push(WeatherEvent::Message {
                    kind: WEATHER_MSG_FORM,
                    wtype: w.wtype,
                    radius: w.radius,
                    x: w.x,
                    y: w.y,
                });
            }
        }
        self.check_weather_collisions(rng);
        self.update_weather_map();
        events
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
    let path = std::path::Path::new(lib_path)
        .join("world")
        .join("worldmap");
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return MapData::empty(),
    };

    let mut sectors: HashMap<char, Sector> = HashMap::new();
    let mut entry_points: Vec<EntryPoint> = Vec::new();
    let mut z_weather_points: Vec<ZWeatherPoint> = Vec::new();
    let mut grid: Vec<Vec<char>> = Vec::new();
    let mut max_x: i32 = 0;
    let mut max_y: i32 = 0;

    // Sector parse state.
    let mut cur_id: Option<char> = None;
    let mut cur_show: Option<String> = None;
    let mut cur_name: String = String::new();
    let mut cur_sect: i32 = 0;
    let mut cur_move: i32 = 0;
    let mut cur_desc: String = String::new();
    // SectDesc multi-line accumulation (between `SectDesc:` and the next `~`).
    let mut in_sectdesc = false;
    // Grid parse state.
    let mut in_grid = false;

    // Flush the in-progress sector into the table (C: advancing s on NewSector /
    // EndSector). Captures show/name/desc/sect for the glyph.
    macro_rules! flush_sector {
        ($id:expr) => {{
            sectors.insert(
                $id,
                Sector {
                    show: cur_show.take().unwrap_or_else(|| " ".into()),
                    name: std::mem::take(&mut cur_name),
                    desc: std::mem::take(&mut cur_desc),
                    sect: std::mem::take(&mut cur_sect),
                    move_cost: std::mem::take(&mut cur_move),
                },
            );
        }};
    }

    for raw in contents.lines() {
        // JUDOCHOP already done by lines(); also drop any stray CR.
        let line = raw.trim_end_matches(['\r', '\n']);

        // SectDesc body: accumulate until the terminating '~' (C mode==3).
        if in_sectdesc {
            if line.starts_with('~') {
                in_sectdesc = false;
            } else {
                cur_desc.push_str(line);
                cur_desc.push_str("\r\n");
            }
            continue;
        }

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
                flush_sector!(id);
            }
            let idarg = get_arg(line, 2);
            cur_id = idarg.chars().next();
            cur_show = None;
            cur_name = String::new();
            cur_desc = String::new();
            cur_sect = 0;
            cur_move = 0;
            continue;
        }
        if compare(&arg1, "SectShow:") {
            // C: if buf[10]!=' ' take the 2nd token, else show is a lone space.
            // get_arg(buf,2) yields the token (empty => single space).
            let tok = get_arg(line, 2);
            cur_show = Some(if tok.is_empty() { " ".to_string() } else { tok });
            continue;
        }
        if compare(&arg1, "SectName:") {
            // C: get_arg_exclude(buf, 1, arg) — everything after the directive.
            cur_name = get_arg_exclude(line, 1);
            continue;
        }
        if compare(&arg1, "SectSect:") {
            // C: match the remainder against sector_types[] for the index.
            let want = get_arg_exclude(line, 1);
            cur_sect = sector_from_name(&want);
            continue;
        }
        if compare(&arg1, "SectMove:") {
            match crate::text::parse_i32_atoi(&get_arg(line, 2)) {
                Ok(value) => cur_move = value,
                Err(crate::text::ParseIntError::Overflow) => {
                    log::warn!("SYSERR: WorldMap SectMove overflow; directive ignored: {line}");
                }
                Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
            }
            continue;
        }
        if compare(&arg1, "SectDesc:") {
            cur_desc.clear();
            in_sectdesc = true;
            continue;
        }
        if compare(&arg1, "EndSector") {
            if let Some(id) = cur_id.take() {
                flush_sector!(id);
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
        if compare(&arg1, "EntryPoint:") {
            // EntryPoint: <x> <y> <interior_vnum> [DIR]
            let parsed = [2, 3, 4].map(|index| crate::text::parse_i32_atoi(&get_arg(line, index)));
            let [Ok(x), Ok(y), Ok(vnum)] = parsed else {
                log::warn!(
                    "SYSERR: WorldMap EntryPoint numeric overflow; directive ignored: {line}"
                );
                continue;
            };
            let dir = parse_dir(&get_arg(line, 5));
            entry_points.push(EntryPoint {
                x,
                y,
                interior_vnum: vnum,
                dir,
            });
            continue;
        }
        if compare(&arg1, "ZWeatherPoint:") {
            let parsed = [2, 3, 4].map(|index| crate::text::parse_i32_atoi(&get_arg(line, index)));
            let [Ok(x), Ok(y), Ok(zone_number)] = parsed else {
                log::warn!(
                    "SYSERR: WorldMap ZWeatherPoint numeric overflow; directive ignored: {line}"
                );
                continue;
            };
            z_weather_points.push(ZWeatherPoint { x, y, zone_number });
            continue;
        }
        // BuildExit/FlagRoom/SpecRoom: not needed for the render + splice.
    }

    // Flush a trailing sector with no EndSector.
    if let Some(id) = cur_id.take() {
        flush_sector!(id);
    }

    if max_x == 0 || max_y == 0 || sectors.is_empty() {
        return MapData::empty();
    }

    // weather_alloc(): allocate the weather_map / room_weather grids. The actual
    // glyph strings are filled by update_weather_map() once storms are seeded.
    let filler = format!("&n{}", FILLER_CHAR);
    let weather_map = vec![vec![filler.clone(); max_x as usize]; max_y as usize];
    let room_weather = vec![vec![WEATHER_NONE; max_x as usize]; max_y as usize];
    let mut cell_wzone_control = vec![vec![None; max_x as usize]; max_y as usize];
    for zwp in &z_weather_points {
        if zwp.x >= 1 && zwp.x <= max_x && zwp.y >= 1 && zwp.y <= max_y {
            cell_wzone_control[(zwp.y - 1) as usize][(zwp.x - 1) as usize] = Some(zwp.zone_number);
        }
    }

    MapData {
        active: true,
        max_x,
        max_y,
        grid,
        sectors,
        entry_points,
        z_weather_points,
        storms: Vec::new(),
        num_weather: 0,
        weather_inited: false,
        weather_map,
        room_weather,
        cell_wzone_control,
    }
}

/// Run `f` against the (lazily parsed, per-lib_path cached) map data.
/// Per-room weather type. C reads world[room].weather, which swc sets for
/// map cells AND fans out to every outdoor room of a wzonecontrol zone
/// (maputils.c:2084-2096); rooms off the map with no fanned-out cell return
/// -1 (#121/#120/#190).
pub fn room_weather_type(g: &GameState, room: &crate::room::Room) -> i32 {
    if room.weather >= 0 {
        return room.weather;
    }
    let (Some(x), Some(y)) = (room.map_x, room.map_y) else {
        return -1;
    };
    with_map(g, |m| m.cell_weather(x, y))
}

/// C maputils.c:2084-2096 swc's second half: mirror the storm grid onto the
/// world rooms - each map cell's weather, plus every OUTDOOR room of a
/// wzonecontrol zone. Called from the weather heartbeat (weather_activity).
pub fn sync_room_weather(g: &mut GameState) {
    if !map_active(g) {
        return;
    }
    // Map cells.
    let cell_updates: Vec<(RoomRnum, i32)> = g
        .rooms
        .iter()
        .enumerate()
        .filter(|(_, r)| r.map_x.is_some() && r.map_y.is_some())
        .map(|(i, r)| (i, room_weather_type(g, r)))
        .collect();
    for (i, w) in cell_updates {
        if let Some(room) = g.rooms.get_mut(i) {
            room.weather = w;
        }
    }
    // Zone fan-out: outdoor rooms of every controlled zone take their
    // ZWeatherPoint cell's weather.
    let zone_updates: Vec<(i32, i32)> = {
        let tbl = crate::lock_ok::lock(&map_table());
        tbl.values()
            .flat_map(|m| {
                m.z_weather_points
                    .iter()
                    .filter_map(|zwp| {
                        m.cell_weather(zwp.x, zwp.y);
                        m.cell_wzone_control
                            .get((zwp.y - 1) as usize)?
                            .get((zwp.x - 1) as usize)?
                            .map(|zn| (zn, m.cell_weather(zwp.x, zwp.y)))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    for (zone_number, w) in zone_updates {
        for room in g.rooms.iter_mut() {
            if room.zone == zone_number
                && !room.room_flags.contains(crate::room::RoomFlags::INDOORS)
            {
                room.weather = w;
            }
        }
    }
}

fn map_active(g: &GameState) -> bool {
    g.map_start_rnum.is_some()
}

fn with_map<R>(g: &GameState, f: impl FnOnce(&MapData) -> R) -> R {
    let lib = g.config.lib_path.clone();
    let mut tbl = crate::lock_ok::lock(&map_table());
    if !tbl.contains_key(&lib) {
        let data = parse_worldmap(&lib);
        tbl.insert(lib.clone(), data);
    }
    f(tbl.get(&lib).unwrap())
}

/// Mutable variant for togglemap's active flag.
fn with_map_mut<R>(g: &GameState, f: impl FnOnce(&mut MapData) -> R) -> R {
    let lib = g.config.lib_path.clone();
    let mut tbl = crate::lock_ok::lock(&map_table());
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
    string
        .split(' ')
        .filter(|t| !t.is_empty())
        .nth(argnum.saturating_sub(1))
        .unwrap_or("")
        .to_string()
}

/// compare(a,b): exact, case-insensitive equality of two whole strings.
fn compare(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// get_arg_exclude(string, argnum) (maputils.c): the line with the `argnum`-th
/// space-delimited token removed, trailing whitespace trimmed. C uses this to
/// read multi-word fields (SectName / SectSect) after the directive keyword; our
/// callers always pass argnum=1 (drop the directive token, keep the remainder).
fn get_arg_exclude(string: &str, argnum: usize) -> String {
    let mut out = String::new();
    let mut j = 1usize;
    for c in string.chars() {
        if c == ' ' {
            // A space advances the token counter; the space that *ends* the
            // excluded token is itself skipped (C: continue without emitting).
            let was = j;
            j += 1;
            if was == argnum {
                continue;
            }
        }
        if j == argnum {
            continue;
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

/// parse_dir(token) (maputils.c read_map): abbreviation match for a cardinal /
/// vertical direction; None when the token is empty or unrecognised (the
/// bidirectional linkrnum/linkmapnum EntryPoint form).
fn parse_dir(tok: &str) -> Option<usize> {
    if tok.is_empty() {
        return None;
    }
    let t = tok.to_ascii_uppercase();
    for (name, dir) in [
        ("NORTH", NORTH as usize),
        ("SOUTH", SOUTH as usize),
        ("EAST", EAST as usize),
        ("WEST", WEST as usize),
        ("UP", UP),
        ("DOWN", DOWN),
    ] {
        // is_abbrev: token is a non-empty prefix of the direction name.
        if name.starts_with(&t) {
            return Some(dir);
        }
    }
    None
}

/// sector_from_name(name) (maputils.c): map a `SectSect:` label to its
/// CircleMUD sector index via constants::SECTOR_TYPES. 0 (Inside) when unknown.
fn sector_from_name(name: &str) -> i32 {
    for (i, s) in crate::constants::SECTOR_TYPES.iter().enumerate() {
        if compare(s, name) {
            return i as i32;
        }
    }
    0
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
    let mode = if a1.eq_ignore_ascii_case("weather") {
        2
    } else {
        1
    };

    // C: MIN(MAX(atoi(buf), 1), max) — i.e. clamp into 1..=max.
    let parse_arg = |index| crate::text::parse_i32_atoi(&get_arg(arg, index));
    let (xm, ym, a4, a5) = match (parse_arg(2), parse_arg(3), parse_arg(4), parse_arg(5)) {
        (Ok(xm), Ok(ym), Ok(a4), Ok(a5)) => (xm.clamp(1, max_x), ym.clamp(1, max_y), a4, a5),
        _ => {
            g.send_to_char(
                ch,
                "Map coordinates must fit the supported 32-bit range.\r\n",
            );
            return;
        }
    };

    let xl = if a4 == 0 {
        xm.saturating_add(98).min(max_x)
    } else {
        a4.max(xm).min(xm.saturating_add(98).min(max_x))
    };
    let yl = if a5 == 0 {
        ym.saturating_add(98).min(max_y)
    } else {
        a5.max(ym).min(ym.saturating_add(98).min(max_y))
    };

    let pos = player_xy(g, ch);

    let out = match mode {
        2 => render_weather_world_map(g, xm, ym, xl, yl, pos),
        _ => render_world_map(g, xm, ym, xl, yl, pos),
    };

    g.send_to_char(ch, &out);
}

/// Split the map convention `&<ASCII color><glyph>` without ever indexing a
/// UTF-8 continuation byte. Inputs such as `&é` are data, not a valid two-byte
/// color prefix, and deliberately return `None`.
fn color_glyph_parts(value: &str) -> Option<(&str, &str)> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'&')
        || !bytes.get(1).is_some_and(u8::is_ascii)
        || !value.is_char_boundary(2)
    {
        return None;
    }
    let glyph = value.get(2..)?;
    (!glyph.is_empty()).then_some((&value[..2], glyph))
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
                if x > xm {
                    let left = m.cell_id(x - 1, y);
                    // Same leading colour code as the cell to our left: print
                    // only the glyph, preserving a multibyte glyph intact.
                    if let (Some((color, glyph)), Some((left_color, _))) =
                        (color_glyph_parts(id), color_glyph_parts(left))
                        && color == left_color
                    {
                        out.push_str(glyph);
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
                if x > xm {
                    // C indexes weather_map[y-1][x-2] (raw, with the < 0 wrap).
                    let left = m.wmap_cell(x - 1, y);
                    if let (Some((color, glyph)), Some((left_color, _))) =
                        (color_glyph_parts(cur), color_glyph_parts(left))
                        && color == left_color
                    {
                        out.push_str(glyph);
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

    let advancedmap = g
        .get_char(ch)
        .map(|c| c.prf2_flags & PRF2_ADVANCEDMAP != 0)
        .unwrap_or(false);

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
    let (lx, lf) = if modifier == 0 {
        (x - 1, -1)
    } else {
        (x + 1, 1)
    };
    let left = noroom_glyph(m, lx, y, weather_active, advancedmap);
    // j = rm2x(ch->in_room) +/- radius, wrapped: the x of the window's edge column.
    let mut j = if modifier == 0 {
        px - radius
    } else {
        px + radius
    };
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

    if matches!(
        (color_glyph_parts(&tmp), color_glyph_parts(&left)),
        (Some((color, _)), Some((left_color, _))) if color == left_color
    ) && !neighbour_is_player
        && j != cur_x
    {
        color_glyph_parts(&tmp)
            .map(|(_, glyph)| glyph.to_string())
            .unwrap_or(tmp)
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
        let glyph = color_glyph_parts(cell)
            .and_then(|(_, glyph)| glyph.chars().next())
            .unwrap_or('?');
        format!("&K{}", glyph)
    } else {
        m.cell_id(x, y).to_string()
    }
}

// ---------------------------------------------------------------------------
// pweather — the player weather map (printweather), and lweather.
// ---------------------------------------------------------------------------

pub fn pweather(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // C maputils.c:2031-2044 gate ORDER: fog (mortals AND NPCs) -> indoors
    // -> ismap -> ZWeatherPoint coords set -> 'unset' notice. The port
    // resolved coords first, so the 'unset' notice preempted the fog and
    // indoors replies (#191).
    let (level, is_npc) = g
        .get_char(ch)
        .map(|c| (c.player.level, c.is_npc))
        .unwrap_or((0, true));
    let indoor = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.rooms.get(r))
        .map(|room| room.room_flags.contains(crate::room::RoomFlags::INDOORS))
        .unwrap_or(false);

    // 1. fog gate (level < LVL_IMMORT || IS_NPC).
    let standing_fog = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.rooms.get(r))
        .map(|room| {
            let w = room_weather_type(g, room);
            w == WEATHER_FOG as i32 || w == WEATHER_MAGICFOG as i32
        })
        .unwrap_or(false);
    if standing_fog && (level < LVL_IMMORT || is_npc) {
        g.send_to_char(
            ch,
            "The thick fog prevents you from determining the weather.\r\n",
        );
        return;
    }
    // 2. indoors.
    if indoor {
        g.send_to_char(ch, "Determine the weather indoors!? Impossible!\r\n");
        return;
    }
    // 3. on the map -> print.
    let on_map = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.rooms.get(r))
        .map(|room| room.map_x.is_some())
        .unwrap_or(false);
    if on_map {
        printweather(g, ch);
        return;
    }
    // 4. zone ZWeatherPoint with NON-ZERO coords -> print; else the notice.
    let zw = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.rooms.get(r))
        .map(|r| r.zone)
        .and_then(|zone_number| {
            with_map(g, |m| {
                m.z_weather_points
                    .iter()
                    .find(|p| p.zone_number == zone_number)
                    .map(|p| (p.x, p.y))
            })
        });
    match zw {
        Some((x, y)) if x != 0 && y != 0 => printweather(g, ch),
        _ => g.send_to_char(
            ch,
            "Notify immortals that this zone's ZWeatherPoint is unset please.\r\n",
        ),
    }
}

fn weather_view_xy(g: &GameState, ch: CharId) -> Option<(i32, i32)> {
    if let Some(p) = player_xy(g, ch) {
        return Some(p);
    }
    let zone_number = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.rooms.get(r))
        .map(|r| r.zone)?;
    with_map(g, |m| {
        m.z_weather_points
            .iter()
            .find(|p| p.zone_number == zone_number)
            .map(|p| (p.x, p.y))
    })
}

/// printweather — the large player-centred weather map with the legend column.
fn printweather(g: &mut GameState, ch: CharId) {
    let active = with_map(g, |m| m.is_active());
    if !active {
        return;
    }
    let (x, y) = match weather_view_xy(g, ch) {
        Some(p) => p,
        None => return,
    };

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
            1 => line.push_str(&format!(
                "&n{} = North {} = South",
                DIRECTION_NORTH, DIRECTION_SOUTH
            )),
            2 => line.push_str(&format!(
                "&n{} = East  {} = West",
                DIRECTION_EAST, DIRECTION_WEST
            )),
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
    // i = rm2x(inroom) - WEATHER_VISION_RADIUS_X, wrapped into 1..=max.
    let mut thr = inx - WEATHER_VISION_RADIUS_X;
    if thr < 1 {
        thr += m.max_x;
    }
    let (cur_x, _) = m.wrap(x, y);

    if matches!(
        (color_glyph_parts(tmp), color_glyph_parts(left)),
        (Some((color, _)), Some((left_color, _))) if color == left_color
    ) && !(inx == x - 1)
        && thr != cur_x
    {
        color_glyph_parts(tmp)
            .map(|(_, glyph)| glyph)
            .unwrap_or(tmp)
    } else {
        tmp
    }
}

/// lweather (maputils.c:1945) — the immortal weather-admin console. The C
/// shipped this with an unconditional hex-echo + return above the whole
/// subcommand chain, leaving every editing path unreachable dead code; the
/// "finish the game" activation routes subcommands to their handlers and
/// keeps the hex echo as the fallback for non-subcommand arguments (so the
/// shipped observable behaviour for a bare argument is preserved).
/// The author's own commented-out gmode experiment and the interactive
/// `edit` walker (a raw w_index list-walk the port's storm Vec supersedes)
/// are deliberately not carried.
pub fn lweather(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let first = get_arg(arg, 1).to_lowercase();

    if first == "update_weather_activity" {
        weather_activity(g);
        return;
    }
    if first == "update_weather_map" {
        with_map_mut(g, |m| m.update_weather_map());
        return;
    }
    if first == "new" {
        let type_word = get_arg(arg, 2).to_lowercase();
        let at = get_arg(arg, 3).to_lowercase();
        // C matches the first letter of each weather name.
        let wtype = (0..WEATHER_TOTAL).find(|&i| {
            WEATHER_NAMES[i]
                .split_whitespace()
                .next()
                .map(|w| w.to_lowercase().starts_with(&type_word))
                .unwrap_or(false)
        });
        let Some(wtype) = wtype else {
            return;
        };
        // cdsr(): "NxM" coordinates, else a random map room.
        // Without an explicit cdsr target C draws a random map room; the cell
        // is derived uniformly from the grid's linear index.
        let grid = with_map(g, |m| (m.max_x, m.max_y));
        if grid.0 <= 0 || grid.1 <= 0 {
            g.send_to_char(ch, "Map not loaded.\r\n");
            return;
        }
        let cells = (grid.0 * grid.1) as i64;
        let r = g.rng.number(0, (cells - 1).max(0) as i32) as i64;
        let random = (
            (r % grid.0 as i64) as i32 + 1,
            ((r - r % grid.0 as i64) / grid.1 as i64) as i32 + 1,
        );
        let (x, y) = match parse_cdsr(&at) {
            Ok(Some((x, y))) if (1..=grid.0).contains(&x) && (1..=grid.1).contains(&y) => (x, y),
            Ok(Some(_)) => {
                g.send_to_char(ch, "Weather coordinates are outside the loaded map.\r\n");
                return;
            }
            Ok(None) => random,
            Err(crate::text::ParseIntError::Overflow) => {
                g.send_to_char(
                    ch,
                    "Weather coordinates are outside the supported range.\r\n",
                );
                return;
            }
            Err(_) => random,
        };
        let name = WEATHER_NAMES[wtype].to_string();
        // Pre-roll the direction here (spawn_weather only draws when given an
        // invalid dir); the map lock borrows g immutably so the game RNG must
        // not be captured across it.
        let dir = g.rng.number(0, 3);
        let mut unused_rng = crate::rng::Rng::default();
        with_map_mut(g, |m| {
            m.spawn_weather(wtype, dir, x, y, &mut unused_rng);
            m.update_weather_map();
        });
        g.send_to_char(ch, &format!("Spawned a {} at {:>2}x{:>2}.\r\n", name, x, y));
        return;
    }
    if first == "destroy" {
        let Ok(n) = get_arg(arg, 2).parse::<usize>() else {
            return;
        };
        if n == 0 {
            return;
        }
        // C destroy_weather: WEATHER_MSG_STOP to the covered cells, then drop
        // the storm; also update the weather map afterwards (C's caller does).
        let storm = with_map_mut(g, |m| {
            if n > m.storms.len() {
                return None;
            }
            let s = m.storms.remove(n - 1);
            m.num_weather = m.num_weather.saturating_sub(1);
            m.update_weather_map();
            Some((s.wtype, s.radius, s.x, s.y))
        });
        if let Some((wtype, radius, x, y)) = storm {
            send_weather_messages(g, WEATHER_MSG_STOP, wtype, radius, x, y);
            g.send_to_char(
                ch,
                &format!("Attempted to destroy meteoric occurence #{}.\r\n", n),
            );
        }
        return;
    }
    if !first.is_empty()
        && first
            .chars()
            .next()
            .map(|c| c.is_ascii_hexdigit())
            .unwrap_or(false)
        && !first.starts_with("update")
    {
        // No subcommand matched: C's shipped behaviour is the hex echo.
    }
    let k = match parse_leading_hex(arg.trim_start()) {
        Ok(value) => value,
        Err(()) => {
            g.send_to_char(
                ch,
                "That hexadecimal value is outside the supported range.\r\n",
            );
            return;
        }
    };
    g.send_to_char(ch, &format!("{}\r\n", k));
    listweather(g, ch);
}

/// C cdsr() coordinate form ("12x34") -> 1-based map cell.
fn parse_cdsr(s: &str) -> Result<Option<(i32, i32)>, crate::text::ParseIntError> {
    let s = s.trim();
    let Some(idx) = s.find('x') else {
        return Ok(None);
    };
    let x = match crate::text::parse_i32_strict(s[..idx].trim()) {
        Ok(value) => value,
        Err(crate::text::ParseIntError::Overflow) => {
            return Err(crate::text::ParseIntError::Overflow);
        }
        Err(_) => return Ok(None),
    };
    let y = match crate::text::parse_i32_strict(s[idx + 1..].trim()) {
        Ok(value) => value,
        Err(crate::text::ParseIntError::Overflow) => {
            return Err(crate::text::ParseIntError::Overflow);
        }
        Err(_) => return Ok(None),
    };
    Ok(Some((x, y)))
}

/// C listweather (maputils.c:1831).
fn listweather(g: &mut GameState, ch: CharId) {
    let map_ok = with_map(g, |m| m.is_active());
    if !map_ok {
        g.send_to_char(ch, "Map not loaded.\r\n");
        return;
    }
    let (lines, autospawn) = with_map(g, |m| {
        let mut lines = Vec::new();
        for (i, w) in m.storms.iter().enumerate() {
            let mut line = format!(
                "({}) There is a {:>13} with a radius of {:>2} {}",
                i + 1,
                WEATHER_NAMES[w.wtype],
                w.radius,
                if WEATHER_DATA[w.wtype][0] <= 0 {
                    "[stationary] "
                } else {
                    ""
                }
            );
            if WEATHER_DATA[w.wtype][0] > 0 {
                let going = match w.dir {
                    0 => "going  north ",
                    1 => "going   east ",
                    2 => "going  south ",
                    3 => "going   west ",
                    _ => "",
                };
                line.push_str(going);
            }
            line.push_str(&format!(
                "at {:>2}x{:>2}. Expiration in mins: {}.{}\r\n",
                w.x,
                w.y,
                w.left / 2,
                if w.left % 2 == 0 { 0 } else { 5 }
            ));
            lines.push(line);
        }
        (lines, (MAX_WEATHER as i32).saturating_sub(m.num_weather))
    });
    for line in lines {
        g.send_to_char(ch, &line);
    }
    g.send_to_char(ch, &format!("Weather to be autospawned: {}\r\n", autospawn));
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
    let name = g
        .get_char(ch)
        .map(|c| c.get_name().to_string())
        .unwrap_or_default();

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

/// integrate_map_rooms (maputils.c read_map): splice the surface-map cells into
/// GameState.rooms as real Rooms, then apply the EntryPoint links. Run once,
/// after the .wld/.zon/.mob/.obj world is loaded and BEFORE zone priming.
///
/// Faithful to C's grid loop (read_map mode==2): the map block is appended after
/// the existing rooms (C: top_of_world++; here rooms.push), so the real-room
/// block (rnums 0..len-1) and real_room(real_vnum) are UNCHANGED. Cell (x,y),
/// 1-based, lands at rnum `map_start_rnum + (y-1)*max_x + (x-1)` — the same
/// formula as find_room_by_coords and GameState::map_coords_to_rnum. Each map
/// room gets number = 2_000_000 + linear index (C virtual_map_start_room), the
/// glyph's sector/name/desc, and map_x/map_y set so the renderer + do_enter/leave
/// recognise it. Map vnums are virtual (>= 2_000_000) so they never collide with
/// real vnums in room_index.
pub fn integrate_map_rooms(g: &mut GameState) -> EntryPointReport {
    // Already spliced? (idempotent — a second boot/copyover must not double-add.)
    if g.map_start_rnum.is_some() {
        return EntryPointReport {
            linked: Vec::new(),
            unresolved: Vec::new(),
        };
    }

    let lib = g.config.lib_path.clone();
    // Snapshot the parsed map (grid + sectors + entry points) out of the cache so
    // we can mutate g.rooms without holding the map lock.
    let (max_x, max_y, grid, glyph_meta, entry_points) = {
        let mut tbl = crate::lock_ok::lock(&map_table());
        if !tbl.contains_key(&lib) {
            let data = parse_worldmap(&lib);
            tbl.insert(lib.clone(), data);
        }
        let m = tbl.get(&lib).unwrap();
        if !m.is_active() {
            // No worldmap file / corrupt map: nothing to splice.
            return EntryPointReport {
                linked: Vec::new(),
                unresolved: Vec::new(),
            };
        }
        // glyph -> (name, desc, sect, move_cost) so we can build each room
        // without the lock.
        let meta: HashMap<char, (String, String, i32, i32)> = m
            .sectors
            .iter()
            .map(|(&id, s)| (id, (s.name.clone(), s.desc.clone(), s.sect, s.move_cost)))
            .collect();
        (
            m.max_x,
            m.max_y,
            m.grid.clone(),
            meta,
            m.entry_points.clone(),
        )
    };

    const VIRTUAL_MAP_START: RoomVnum = 2_000_000;
    let map_start_rnum = g.rooms.len();

    // Grid loop (read_map mode==2): row-major, so linear index advances x within
    // each row y. This matches (y-1)*max_x + (x-1) for 1-based coords.
    let mut linear: RoomVnum = 0;
    for row in &grid {
        for &glyph in row {
            let (name, desc, sect, move_cost) = match glyph_meta.get(&glyph) {
                Some(t) => (t.0.clone(), t.1.clone(), t.2, t.3),
                // C aborts on a missing sector; a quiet fallback keeps boot total.
                None => (String::new(), String::new(), 0, 0),
            };
            let x = (linear % max_x) + 1; // rm2x
            let y = (linear / max_x) + 1; // rm2y
            let vnum = VIRTUAL_MAP_START + linear;
            let mut room = Room::new(vnum, -1, name, desc);
            room.sector_type = SectorType::from_i32(sect);
            room.map_x = Some(x);
            room.map_y = Some(y);
            room.mapmv = move_cost;
            // add_room appends to rooms and registers the (virtual) vnum.
            g.add_room(room);
            linear += 1;
        }
    }

    g.map_start_rnum = Some(map_start_rnum);
    g.max_map_x = max_x;
    g.max_map_y = max_y;

    info!(
        "Surface map spliced: {} cells ({}x{}), rnums {}..{}",
        linear,
        max_x,
        max_y,
        map_start_rnum,
        map_start_rnum + linear as usize - 1
    );

    // EntryPoints (read_map): link each surface cell to a city interior. C does
    // two things depending on whether a direction was given:
    //   * No DIR  -> bidirectional link: map cell linkrnum = interior rnum, and
    //                interior linkmapnum = map cell rnum (the do_enter/do_leave
    //                path). Requires both the coords and the interior to resolve.
    //   * A DIR   -> directional exits: map cell --dir--> interior, and
    //                interior --rev_dir--> map cell.
    // We always populate the bidirectional link (so do_enter/do_leave work for
    // every EntryPoint) AND, when a DIR is present, also create the directional
    // exits exactly as C does.
    let report = apply_entry_points(g, &entry_points);
    for failure in &report.unresolved {
        warn!(
            "SYSERR: EntryPoint at map ({}, {}) -> room {} unresolvable: {}",
            failure.x, failure.y, failure.interior_vnum, failure.reason
        );
    }
    if report.unresolved.is_empty() {
        info!("EntryPoints: {} linked (all resolved)", report.linked.len());
    } else {
        warn!(
            "EntryPoints: {} linked, {} UNRESOLVED (see SYSERR lines above)",
            report.linked.len(),
            report.unresolved.len()
        );
    }
    report
}

/// Outcome of linking one EntryPoint directive: the (x, y) it failed at and
/// why. `linked` carries the map rnum <-> interior rnum pairs that succeeded.
pub(crate) struct EntryPointReport {
    linked: Vec<(usize, usize)>,
    unresolved: Vec<EntryPointFailure>,
}

struct EntryPointFailure {
    x: i32,
    y: i32,
    interior_vnum: RoomVnum,
    reason: &'static str,
}

/// The EntryPoint application half of integrate_map_rooms, split out so boot
/// diagnostics (and tests) can see which directives failed to resolve instead
/// of the C behaviour of dropping them silently (a typo'd coordinate or a
/// renamed interior vnum otherwise reads as "the gate is just gone").
fn apply_entry_points(g: &mut GameState, entry_points: &[EntryPoint]) -> EntryPointReport {
    let mut report = EntryPointReport {
        linked: Vec::new(),
        unresolved: Vec::new(),
    };
    for ep in entry_points {
        let map_rnum = match g.map_coords_to_rnum(ep.x, ep.y) {
            Some(r) => r,
            None => {
                report.unresolved.push(EntryPointFailure {
                    x: ep.x,
                    y: ep.y,
                    interior_vnum: ep.interior_vnum,
                    reason: "no map cell at those coordinates",
                });
                continue;
            }
        };
        let interior_rnum = match g.real_room(ep.interior_vnum) {
            Some(r) => r,
            None => {
                report.unresolved.push(EntryPointFailure {
                    x: ep.x,
                    y: ep.y,
                    interior_vnum: ep.interior_vnum,
                    reason: "interior room vnum is not loaded",
                });
                continue;
            }
        };

        // Bidirectional link (do_enter from the map cell, do_leave from interior).
        g.rooms[map_rnum].linkrnum = Some(interior_rnum);
        g.rooms[interior_rnum].linkmapnum = Some(map_rnum);
        report.linked.push((map_rnum, interior_rnum));

        // Directional exits, when a DIR was given (C: read_map ~494-501).
        if let Some(dir) = ep.dir {
            let map_vnum = g.rooms[map_rnum].number;
            let interior_vnum = g.rooms[interior_rnum].number;
            let rev = REV_DIR[dir];
            // map cell --dir--> interior
            if g.rooms[map_rnum].exits[dir].is_none() {
                g.rooms[map_rnum].set_exit(dir, make_exit(interior_vnum));
            } else if let Some(e) = g.rooms[map_rnum].exits[dir].as_mut() {
                e.to_room = interior_vnum;
            }
            // interior --rev_dir--> map cell
            if g.rooms[interior_rnum].exits[rev].is_none() {
                g.rooms[interior_rnum].set_exit(rev, make_exit(map_vnum));
            } else if let Some(e) = g.rooms[interior_rnum].exits[rev].as_mut() {
                e.to_room = map_vnum;
            }
        }
    }
    report
}

/// A plain open passage exit to `to_vnum` (CREATE of room_direction_data in C
/// leaves everything zeroed but the destination).
fn make_exit(to_vnum: RoomVnum) -> crate::room::Exit {
    crate::room::Exit {
        description: None,
        keyword: None,
        exit_info: 0,
        key: -1,
        to_room: to_vnum,
    }
}

/// prime_weather (db.c read_map -> init_weather, run at boot): seed the surface
/// map's MAX_WEATHER storms and build the initial weather_map so the world has
/// live weather from the first tick, exactly as C does on startup. A no-op when
/// the surface map is not active (no worldmap file).
pub fn prime_weather(g: &mut GameState) {
    let lib = g.config.lib_path.clone();
    let mut tbl = crate::lock_ok::lock(&map_table());
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
    // Advance the storms under the map lock, collecting the radial_activity hits
    // produced by each storm step. Release the lock BEFORE replaying them so
    // unit_activity (which mutates g.rooms / characters) is not deadlocked.
    let events = {
        let mut tbl = crate::lock_ok::lock(&map_table());
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
        m.weather_activity(&mut g.rng)
    };

    for event in events {
        match event {
            WeatherEvent::Message {
                kind,
                wtype,
                radius,
                x,
                y,
            } => {
                send_weather_messages(g, kind, wtype, radius, x, y);
            }
            WeatherEvent::Radial(hit) => radial_activity(g, hit),
        }
    }

    // Mirror the storm grid onto world rooms (swc zone fan-out, #190).
    sync_room_weather(g);
}

fn outdoor_room_has_people(g: &GameState, rnum: RoomRnum) -> bool {
    g.rooms
        .get(rnum)
        .map(|r| !r.room_flags.contains(crate::room::RoomFlags::INDOORS) && !r.people.is_empty())
        .unwrap_or(false)
}

fn zone_rooms_for_weather(g: &GameState, zone_number: i32) -> Vec<RoomRnum> {
    g.rooms
        .iter()
        .enumerate()
        .filter_map(|(rnum, room)| {
            if room.zone == zone_number
                && !room.room_flags.contains(crate::room::RoomFlags::INDOORS)
                && !room.people.is_empty()
            {
                Some(rnum)
            } else {
                None
            }
        })
        .collect()
}

fn affected_weather_rooms(g: &GameState, cx: i32, cy: i32, radius: i32) -> Vec<RoomRnum> {
    let mut rooms = Vec::new();
    let mut seen = HashSet::new();
    let controlled_zones = with_map(g, |m| {
        let mut zones = Vec::new();
        for y in (cy - radius)..=(cy + radius) {
            for x in (cx - radius)..=(cx + radius) {
                if !MapData::isinradius_bycoords(x, y, cx, cy, radius) {
                    continue;
                }
                if let Some(rnum) = g.map_coords_to_rnum(x, y) {
                    if outdoor_room_has_people(g, rnum) && seen.insert(rnum) {
                        rooms.push(rnum);
                    }
                }
                let (wx, wy) = m.wrap(x, y);
                if let Some(zone) = m.cell_wzone_control[(wy - 1) as usize][(wx - 1) as usize] {
                    zones.push(zone);
                }
            }
        }
        zones
    });

    for zone in controlled_zones {
        for rnum in zone_rooms_for_weather(g, zone) {
            if seen.insert(rnum) {
                rooms.push(rnum);
            }
        }
    }
    rooms
}

fn send_weather_messages(
    g: &mut GameState,
    kind: i32,
    wtype: usize,
    radius: i32,
    cx: i32,
    cy: i32,
) {
    if wtype >= WEATHER_TOTAL {
        return;
    }
    let (near, above) = match kind {
        WEATHER_MSG_FORM => (
            format!("You see a {} brewing to your ", WEATHER_NAMES[wtype]),
            format!("You see a {} brewing above you.\r\n", WEATHER_NAMES[wtype]),
        ),
        WEATHER_MSG_ACT => (
            format!("You see a {} to your ", WEATHER_NAMES[wtype]),
            format!("{}\r\n", WEATHER_MESSAGES[wtype]),
        ),
        WEATHER_MSG_STOP => (
            format!("You see a {} dissipate to your ", WEATHER_NAMES[wtype]),
            format!("The {} above you dissipates.\r\n", WEATHER_NAMES[wtype]),
        ),
        _ => return,
    };

    let scan_radius = radius * 2;
    for y in (cy - scan_radius)..=(cy + scan_radius) {
        for x in (cx - scan_radius)..=(cx + scan_radius) {
            let smode = with_map(g, |m| m.isinradius_wrap(x, y, cx, cy, radius));
            let msg = if smode != 0 {
                above.clone()
            } else {
                let mut s = near.clone();
                if y < cy {
                    s.push_str("south");
                }
                if y > cy {
                    s.push_str("north");
                }
                if x < cx {
                    s.push_str("east");
                }
                if x > cx {
                    s.push_str("west");
                }
                s.push_str(".\r\n");
                s
            };
            // C maputils.c:1891-1948: MARK only guards the CENTRE cell; the
            // wzonecontrol fan-out re-sends for every covering cell, so
            // controlled-zone players receive the (differently suffixed)
            // message per covering storm cell (#197).
            for rnum in weather_message_rooms(g, x, y) {
                if outdoor_room_has_people(g, rnum) {
                    g.send_to_room(rnum, &msg, None);
                }
            }
        }
    }
}

fn weather_message_rooms(g: &GameState, x: i32, y: i32) -> Vec<RoomRnum> {
    let mut rooms = Vec::new();
    let mut seen = HashSet::new();
    let zone = with_map(g, |m| {
        let (wx, wy) = m.wrap(x, y);
        m.cell_wzone_control[(wy - 1) as usize][(wx - 1) as usize]
    });
    if let Some(rnum) = g.map_coords_to_rnum(x, y) {
        if outdoor_room_has_people(g, rnum) && seen.insert(rnum) {
            rooms.push(rnum);
        }
    }
    if let Some(zone) = zone {
        for rnum in zone_rooms_for_weather(g, zone) {
            if seen.insert(rnum) {
                rooms.push(rnum);
            }
        }
    }
    rooms
}

/// radial_activity (maputils.c): walk the storm's diamond (its radius), and for
/// every spliced map cell or ZWeatherPoint-controlled real-zone room that holds
/// people, run unit_activity (the per-room weather effect).
fn radial_activity(g: &mut GameState, hit: RadialHit) {
    let rooms = affected_weather_rooms(g, hit.x, hit.y, hit.radius);
    for rnum in rooms {
        unit_activity(g, rnum, hit.wtype);
    }
}

/// unit_activity (maputils.c): apply a storm's per-pulse effect to every player
/// standing in `room`. Faithful port of the damage arithmetic + messages:
///   * magic fog: a 1-in-5 chance to fire a random involuntary social.
///   * thunderstorm: a 1-in-4 chance of a lightning bolt for number(400,900)
///     damage (halved under sanctuary), with the exact crisp messages.
///   * any storm with damage > 0: a flat weather_data[type][2] wound (halved
///     under sanctuary when >= 2), with the "&RYou are wounded by the <name>!&n"
///     message.
///   * hurricane / tornado: a weight-gated knockback that jettisons the PC.
/// NPCs are skipped (C returns on the first NPC in the room list — a quirk we
/// preserve: an NPC ahead of a PC shields the rest of the room that pulse).
fn unit_activity(g: &mut GameState, room: RoomRnum, wtype: usize) {
    let people = match g.rooms.get(room) {
        Some(r) => r.people.clone(),
        None => return,
    };
    // C (maputils.c:1577): `struck` is per-room — at most ONE PC is fried by
    // lightning per tick. Without it every eligible PC in the room got struck.
    let mut struck = false;
    for ch in people {
        let (is_npc, level) = match g.get_char(ch) {
            Some(c) => (c.is_npc, c.player.level),
            None => continue,
        };
        // C: "Mike 6/28/00" — the loop returns on the first NPC encountered.
        if is_npc {
            return;
        }
        // Only NPCs / mortals are affected (immortals are immune).
        if (level as u8) >= LVL_IMMORT {
            continue;
        }

        // --- magic fog: random involuntary social (1 in 5) ---
        if wtype == WEATHER_MAGICFOG && g.rng.number(1, 5) == 1 {
            let cmd = match g.rng.number(1, 6) {
                1 => "sneeze",
                2 => "scream",
                3 => "hiccup",
                4 => "heh",
                5 => "slap self",
                _ => "emote shakes and quivers like a bowlfull of jelly.",
            };
            crate::interpreter::command_interpreter(g, ch, cmd);
            if !g.char_exists(ch) {
                continue;
            }
        }

        // --- thunderstorm: lightning strike (1 in 4), at most one PC per room ---
        if wtype == WEATHER_THUNDERSTORM && !struck && g.rng.number(1, 4) == 1 {
            struck = true;
            act(
                g,
                "You see a holy bolt of lightning discharge from the sky!\r\nThe SHOCKING moment fries $n to a crisp!",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You see a holy bolt of lightning discharge from the sky in your direction!\r\n&CZZZZZZZZZZZZT&n!!\r\n",
            );
            if apply_thunderstorm_bolt(g, ch, wtype, &mut |g| g.rng.number(400, 900)) {
                continue; // PC died (extracted/respawned).
            }
        }

        // --- flat storm damage (fire/hurricane/tornado/blizzard/death) ---
        if WEATHER_DATA[wtype][2] > 0 {
            let mut msg = String::from("&RYou are wounded by the ");
            msg.push_str(WEATHER_NAMES[wtype]);
            msg.push_str("!&n\r\n");
            g.send_to_char(ch, &msg);
            let sanct = g
                .get_char(ch)
                .map(|c| c.affect_flags & AFF_SANCTUARY != 0)
                .unwrap_or(false);
            let dmg = if sanct && WEATHER_DATA[wtype][2] >= 2 {
                WEATHER_DATA[wtype][2] / 2
            } else {
                WEATHER_DATA[wtype][2]
            };
            if let Some(c) = g.get_char_mut(ch) {
                c.points.hit -= dmg;
            }
            weather_update_pos(g, ch);
            if weather_show_pos(g, ch, wtype) {
                continue; // PC died.
            }
        }

        // --- hurricane / tornado knockback (weight-gated) ---
        if wtype == WEATHER_HURRICANE || wtype == WEATHER_TORNADO {
            let weight = g
                .get_char(ch)
                .map(|c| c.player.weight as i32)
                .unwrap_or(120);
            let gate = weight.clamp(120, 160); // MIN(MAX(GET_WEIGHT,120),160)
            if g.rng.number(1, gate) <= 70 {
                let name = WEATHER_NAMES[wtype];
                let room_msg = format!("The {} jettisons $n into the air!", name);
                act(g, &room_msg, false, ch, None, ActArg::None, To::Room);
                let self_msg = format!("The {} jettisons you into the air!\r\n", name);
                g.send_to_char(ch, &self_msg);
                let length = if wtype == WEATHER_HURRICANE { 12 } else { 8 };
                let (origin, dest) = match weather_mprand(g, ch, length) {
                    Some(p) => p,
                    None => continue,
                };
                // The victim already WALKED to the landing room via
                // move_char_fly (C move_char), so land where they are.
                let oldroom = g.get_char(ch).and_then(|v| v.in_room);
                let oldroom = match oldroom {
                    Some(r) => r,
                    None => continue,
                };
                let origin_room = g.map_coords_to_rnum(origin.0, origin.1);
                if let Some(c) = g.get_char_mut(ch) {
                    c.was_in_room = origin_room;
                }
                g.char_from_room(ch);
                let fly_msg = format!(
                    "You see {} flying through the air in the distance.\r\n",
                    g.get_char(ch)
                        .map(|c| c.get_name().to_string())
                        .unwrap_or_default()
                );
                send_to_radius(
                    g,
                    &fly_msg,
                    (origin.0 + dest.0) / 2,
                    (origin.1 + dest.1) / 2,
                    WEATHER_DATA[wtype][1] * 2,
                );
                let land_msg = format!(
                    "{} falls from the sky landing head-first into the ground!\r\n",
                    g.get_char(ch)
                        .map(|c| c.get_name().to_string())
                        .unwrap_or_default()
                );
                g.send_to_char(ch, "You land head-first into the ground!\r\n");
                if let Some(c) = g.get_char_mut(ch) {
                    c.was_in_room = None;
                    c.position = Position::Resting;
                }
                g.send_to_room(oldroom, &land_msg, None);
                g.char_to_room(ch, oldroom);
            }
        }
    }
}

fn char_weather_xy(g: &GameState, ch: CharId) -> Option<(i32, i32)> {
    player_xy(g, ch).or_else(|| weather_view_xy(g, ch))
}

/// C maputils.c:1387-1450 move_char(ch, length, dir): walk a thrown character
/// `length` map rooms in direction `dir` (0-3 cardinal, 4-7 diagonal).
/// Intermediate rooms see the flyby message, and the first mortal bystander
/// has a 10% chance per room of being bowled over (knocked to SLEEPING with
/// two messages). The thrown character looks at every room they pass through,
/// and PRF2_NOMAP is forced during the walk and restored after (#187).
pub fn move_char_fly(g: &mut GameState, ch: CharId, length: i32, dir: i32) {
    if !(0..=7).contains(&dir) || g.get_char(ch).is_none() {
        return;
    }
    // C: temporarily force PRF2_NOMAP so the walk doesn't render maps.
    let had_nomap = g
        .get_char(ch)
        .map(|c| c.prf2_flags & PRF2_NOMAP_LOCAL != 0)
        .unwrap_or(false);
    if !had_nomap {
        if let Some(c) = g.get_char_mut(ch) {
            c.prf2_flags |= PRF2_NOMAP_LOCAL;
        }
    }

    // Start room: the char's own map coords (or the zone's ZWeatherPoint).
    if let Some((x, y)) = char_weather_xy(g, ch) {
        if let Some(start) = g.map_coords_to_rnum(x, y) {
            g.char_from_room(ch);
            g.char_to_room(ch, start);
        }
    }

    let flyby = format!(
        "You duck as you see {} fly by your head screaming!\r\n",
        g.get_char(ch)
            .map(|c| c.get_name().to_string())
            .unwrap_or_else(|| "someone".into())
    );

    // Diagonal axis pairs (C maputils.c:1402-1405). Rust dirs: 0=N 1=E 2=S 3=W.
    let (d1, d2) = match dir {
        4 => (0usize, 1usize),
        5 => (0usize, 3usize),
        6 => (2usize, 1usize),
        7 => (2usize, 3usize),
        _ => (0usize, 0usize),
    };

    for i in 1..=length {
        if dir < 4 {
            // Cardinal leg: straight CAN_GO walk.
            let d = dir as usize;
            if !crate::graph::can_go(g, ch, d) {
                break;
            }
            let nr = match g
                .get_char(ch)
                .and_then(|c| c.in_room)
                .and_then(|r| g.room(r).exits[d].clone())
                .and_then(|e| g.real_room(e.to_room))
            {
                Some(n) => n,
                None => break,
            };
            g.char_from_room(ch);
            if i != length {
                flyby_step(g, ch, nr, &flyby);
            } else {
                g.send_to_room(nr, &flyby, None);
            }
            g.char_to_room(ch, nr);
            crate::commands::look_at_room(g, ch, false);
            continue;
        }

        // Diagonal: one room along d1, then one along d2 - displacement is
        // `length` PER AXIS (C maputils.c:1409-1417); the port halved it (#188).
        let cur = match g.get_char(ch).and_then(|c| c.in_room) {
            Some(r) => r,
            None => break,
        };
        if !crate::graph::can_go(g, ch, d1) {
            break;
        }
        let n1 = match g.room(cur).exits[d1]
            .clone()
            .and_then(|e| g.real_room(e.to_room))
        {
            Some(n) => n,
            None => break,
        };
        let n2 = match g.room_opt(n1).and_then(|r| r.exits[d2].clone()) {
            Some(e) => match g.real_room(e.to_room) {
                Some(n) => n,
                None => break,
            },
            None => break,
        };
        g.char_from_room(ch);
        if i != length {
            flyby_step(g, ch, n2, &flyby);
        } else {
            g.send_to_room(n2, &flyby, None);
        }
        g.char_to_room(ch, n2);
        crate::commands::look_at_room(g, ch, false);
    }

    if !had_nomap {
        if let Some(c) = g.get_char_mut(ch) {
            c.prf2_flags &= !PRF2_NOMAP_LOCAL;
        }
    }
}

/// The intermediate-step bystander check from C move_char: the first person
/// in `nr` (a mortal) has a 10% chance to be knocked unconscious by the
/// flying character, who then reports the collision.
fn flyby_step(g: &mut GameState, ch: CharId, nr: RoomRnum, flyby: &str) {
    let bystander = g.room(nr).people.first().copied();
    let eligible = bystander
        .filter(|&t| {
            g.get_char(t)
                .map(|c| c.player.level < LVL_IMMORT)
                .unwrap_or(false)
        })
        .filter(|&_t| g.rng.number(1, 100) <= 10);
    if let Some(t) = eligible {
        let tname = g
            .get_char(t)
            .map(|c| c.get_name().to_string())
            .unwrap_or_else(|| "someone".into());
        let cname = g
            .get_char(ch)
            .map(|c| c.get_name().to_string())
            .unwrap_or_else(|| "someone".into());
        g.send_to_char(
            t,
            &format!(
                "You gasp but are too late to dodge an oncoming {} and the impact knocks you unconscious!\r\n",
                cname
            ),
        );
        g.send_to_room(
            nr,
            &format!(
                "{} watches in horror as {} comes flying into {}!\r\n{} is knocked unconscious by the impact and {} keeps going!\r\n",
                tname, cname, tname, tname, cname
            ),
            None,
        );
        if let Some(c) = g.get_char_mut(t) {
            c.position = Position::Sleeping;
        }
    } else {
        g.send_to_room(nr, flyby, None);
    }
}

fn weather_mprand(g: &mut GameState, ch: CharId, length: i32) -> Option<((i32, i32), (i32, i32))> {
    let (x, y) = char_weather_xy(g, ch)?;
    for _ in 0..7 {
        let attempt = g.rng.number(0, 7);
        let half = length / 2;
        let dest = match attempt {
            0 if y - length >= 1 => (x, y - length),
            1 if y + length <= g.max_map_y => (x, y + length),
            2 if x + length <= g.max_map_x => (x + length, y),
            3 if x - length >= 1 => (x - length, y),
            4 if y - half >= 1 && x + half <= g.max_map_x => (x + half, y - half),
            5 if y - half >= 1 && x - half >= 1 => (x - half, y - half),
            6 if y + half <= g.max_map_y && x + half <= g.max_map_x => (x + half, y + half),
            7 if y + half <= g.max_map_y && x - half >= 1 => (x - half, y + half),
            _ => continue,
        };
        // C maputils.c:1474+ calls move_char(ch, length, <C dir>): the victim
        // WALKS room-by-room (flyby messages, bystander knockouts, look at
        // each room) instead of teleporting (#187). Diagonals move
        // `length` per axis (#188). C's direction constants for the four
        // cardinals map to Rust dirs as N=0, S=2, E=1, W=3.
        const WALK_DIR: [usize; 8] = [0, 2, 1, 3, 4, 5, 6, 7];
        move_char_fly(g, ch, length, WALK_DIR[attempt as usize] as i32);
        return Some(((x, y), dest));
    }
    None
}

fn send_to_radius(g: &mut GameState, msg: &str, cx: i32, cy: i32, radius: i32) {
    for rnum in affected_weather_rooms(g, cx, cy, radius) {
        g.send_to_room(rnum, msg, None);
    }
}

/// weather_show_pos (maputils.c): announce a PC's post-damage position, and on
/// death run the weather death path (log, strip flags, corpse, extract).
/// Returns true if the PC died this call (the caller must `continue`).
fn weather_show_pos(g: &mut GameState, ch: CharId, wtype: usize) -> bool {
    let pos = match g.get_char(ch) {
        Some(c) => c.position,
        None => return true,
    };
    match pos {
        Position::MortallyWounded => {
            act(
                g,
                "$n is mortally wounded, and will die soon, if not aided.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You are mortally wounded, and will die soon, if not aided.\r\n",
            );
            false
        }
        Position::Incapacitated => {
            act(
                g,
                "$n is incapacitated and will slowly die, if not aided.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You are incapacitated an will slowly die, if not aided.\r\n",
            );
            false
        }
        Position::Stunned => {
            act(
                g,
                "$n is stunned, but will probably regain consciousness again.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You're stunned, but will probably regain consciousness again.\r\n",
            );
            false
        }
        Position::Dead => {
            act(
                g,
                "$n is dead!  R.I.P.",
                false,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(ch, "You are dead!  Sorry...\r\n");
            let (name, roomname) = {
                let nm = g
                    .get_char(ch)
                    .map(|c| c.get_name().to_string())
                    .unwrap_or_default();
                let rn = g
                    .get_char(ch)
                    .and_then(|c| c.in_room)
                    .and_then(|r| g.rooms.get(r))
                    .map(|r| r.name.clone())
                    .unwrap_or_default();
                (nm, rn)
            };
            mudlog(
                g,
                &format!("{} killed by weather at {}", name, roomname),
                LVL_IMMORT,
            );
            // raw_kill: strip affects, death cry, weather corpse, extract/respawn.
            weather_die(g, ch, wtype);
            true
        }
        _ => false,
    }
}

/// update_pos (fight.c): set the character's position from current HP. Inlined
/// here (combat.rs's copy is private and out of edit scope) with identical
/// thresholds: >0 keep (fighting->fighting); <=-11 dead; <=-6 mortally wounded;
/// <=-3 incapacitated; else stunned.
fn weather_update_pos(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        let hp = c.points.hit;
        c.position = if hp > 0 {
            c.position
        } else if hp <= -11 {
            Position::Dead
        } else if hp <= -6 {
            Position::MortallyWounded
        } else if hp <= -3 {
            Position::Incapacitated
        } else {
            Position::Stunned
        };
    }
}

fn apply_thunderstorm_bolt(
    g: &mut GameState,
    ch: CharId,
    wtype: usize,
    roll: &mut dyn FnMut(&mut GameState) -> i32,
) -> bool {
    // C maputils.c:1619-1637: number(400,900) is rolled inside whichever
    // branch runs - a redirected target rolls nothing (RNG parity, #189).
    let bolt = roll(g);
    let sanct_divisor = if g
        .get_char(ch)
        .map(|c| c.affect_flags & AFF_SANCTUARY != 0)
        .unwrap_or(false)
    {
        2
    } else {
        1
    };
    let redirected = g
        .get_char(ch)
        .map(|c| c.affect_flags & AFF_REDIRECT_CHARGE != 0)
        .unwrap_or(false);

    if redirected {
        let mut converted = false;
        if let Some(c) = g.get_char_mut(ch) {
            c.affect_flags &= !AFF_REDIRECT_CHARGE;
            if let Some(af) = c
                .affected
                .iter_mut()
                .find(|af| af.spell_type == SPELL_REDIRECT_CHARGE)
            {
                c.affect_flags |= AFF_R_CHARGED;
                af.bitvector = AFF_R_CHARGED;
                af.modifier = (bolt * 15) / 16;
                af.location = APPLY_DAMAGE;
                af.duration = 100;
                c.points.hit -= (bolt / 16) / sanct_divisor;
                converted = true;
            }
        }
        if converted {
            act(
                g,
                "$n amazingly absorbs the bolt of energy!",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            g.send_to_char(
                ch,
                "You feel the godly charge run through your body and your magic contains it!\r\n",
            );
        }
    } else if let Some(c) = g.get_char_mut(ch) {
        c.points.hit -= bolt / sanct_divisor;
    }

    weather_update_pos(g, ch);
    if weather_show_pos(g, ch, wtype) {
        return true;
    }
    // C maputils.c:1639: every survivor of the bolt feels 'a little bit
    // crispy' - redirected or not (#189).
    g.send_to_char(ch, "You feel a little bit crispier.\r\n");
    false
}

/// weather_corpse_names (maputils.c): the adjective prepended to a weather
/// corpse for the lethal storm types. A bare " " means "no adjective".
const WEATHER_CORPSE_NAMES: [&str; WEATHER_TOTAL] = [
    " ",
    " ",
    " ",
    "burnt crispy ",
    " ",
    " ",
    "torn apart ",
    "mangled ",
    "frozen solid ",
    "savagely ripped up ",
];

/// The weather death path (maputils.c weather_show_pos POS_DEAD tail): stop
/// fighting, strip affects, scream, drop a (weather-flavoured) corpse with the
/// PC's loot, then extract. Mirrors raw_kill closely; PC respawn is the
/// observable result of extract_char unlinking the descriptor (menu re-entry),
/// matching C's extract_char(ch) call here.
fn weather_die(g: &mut GameState, ch: CharId, wtype: usize) {
    cleanup_weather_death_player_state(g, ch);

    // FIGHTING(ch) -> stop_fighting; strip all affects (C: while(ch->affected)
    // affect_remove).
    if let Some(c) = g.get_char_mut(ch) {
        c.fighting = None;
        if c.position == Position::Fighting {
            c.position = Position::Dead;
        }
        c.affected.clear();
    }
    g.affect_total(ch);

    // death_cry: wail into this room + a generic cry into every open-exit
    // neighbour (fight.c death_cry), shared with the combat death path.
    crate::combat::death_cry(g, ch);

    if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
        increase_blood(g, rnum);
        increase_snow(g, rnum);
        let name = g
            .get_char(ch)
            .map(|c| c.display_for_others())
            .unwrap_or_default();
        let corpse = make_weather_corpse(g, &name, wtype, ch);
        let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
        let create_gold = g
            .get_char(ch)
            .map(|c| c.is_npc || (!c.is_npc && c.desc.is_some()))
            .unwrap_or(false);
        let carried = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        for oid in carried {
            g.obj_from_anywhere(oid);
            g.obj_to_obj(oid, corpse);
        }
        let worn: Vec<usize> = (0..NUM_WEARS)
            .filter(|&p| {
                g.get_char(ch)
                    .map(|c| c.equipment[p].is_some())
                    .unwrap_or(false)
            })
            .collect();
        for p in worn {
            if let Some(oid) = g.unequip_char(ch, p) {
                g.obj_to_obj(oid, corpse);
            }
        }
        if gold > 0 {
            if create_gold {
                let money = crate::combat::create_money(g, gold);
                g.obj_to_obj(money, corpse);
            }
            if let Some(c) = g.get_char_mut(ch) {
                crate::gold::set(c, crate::gold::Account::Carried, 0);
            }
        }
        g.obj_to_room(corpse, rnum);
    }

    // C: extract_char(ch) — for a PC this unlinks the descriptor (respawn/menu).
    g.extract_char(ch);
}

fn cleanup_weather_death_player_state(g: &mut GameState, ch: CharId) {
    if let Some(c) = g.get_char_mut(ch) {
        if !c.is_npc {
            c.act_flags &= !(PLR_KILLER | PLR_THIEF);
            c.conditions[FULL] = 0;
            c.conditions[THIRST] = 0;
            c.conditions[DRUNK] = 0;
        }
    }
}

/// make_weather_corpse (maputils.c): a corpse container holding the victim's
/// loot. values[3]=1 marks it a corpse so the object decay path reaps it.
fn make_weather_corpse(g: &mut GameState, who: &str, wtype: usize, victim: CharId) -> ObjId {
    use crate::object::{ObjLoc, Object, ObjectType};
    let corpse_name = WEATHER_CORPSE_NAMES.get(wtype).copied().unwrap_or(" ");
    let adjective = if corpse_name.starts_with(' ') {
        ""
    } else {
        corpse_name
    };
    let mut obj = Object::new(
        NOTHING,
        format!("corpse {}", who),
        format!("the {}corpse of {}", adjective, who),
    );
    obj.description = format!("The {}corpse of {} is lying here.", adjective, who);
    obj.obj_type = ObjectType::Container;
    // C fight.c:315-318: GET_OBJ_TIMER(corpse) = IS_NPC(ch) ?
    // max_npc_corpse_time (5) : max_pc_corpse_time (10) (config.c:120-121),
    // decremented once per mud hour by point_update. The flat 60 made
    // corpses persist 6-12x longer than C (#102).
    obj.timer = if g.get_char(victim).map(|c| c.is_npc).unwrap_or(true) {
        5
    } else {
        10
    };
    obj.values = [0, 0, 0, 1];
    obj.loc = ObjLoc::Nowhere;
    g.create_obj(obj)
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

/// increase_snow(rm) (fight.c:197): bump a room's snow level by one, capped
/// at 10 - snow accumulates where corpses fall in cold weather (#116).
pub fn increase_snow(g: &mut GameState, rnum: RoomRnum) {
    let room = g.room_mut(rnum);
    room.snow = (room.snow + 1).min(10);
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

/// sscanf("%x") on a leading hex number; 0 if none (matches the uninitialised
/// read C effectively relies on for a non-numeric argument is 0 in practice).
fn parse_leading_hex(s: &str) -> Result<u32, ()> {
    let mut end = 0;
    let bytes = s.as_bytes();
    while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    if end == 0 {
        return Ok(0);
    }
    u32::from_str_radix(&s[..end], 16).map_err(|_| ())
}

/// mudlog: broadcast an immortal log line (same shape as the other modules).
fn mudlog(g: &mut GameState, line: &str, min_level: u8) {
    crate::syslog::mudlog(g, line, crate::syslog::NRM, min_level);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{Affect, Character};
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::object::ObjectType;

    fn player_in_room(g: &mut GameState, name: &str, room: RoomRnum) -> CharId {
        let ch = g.create_char(Character::new_player(
            name.to_string(),
            Class::Warrior,
            Race::Human,
        ));
        g.char_to_room(ch, room);
        ch
    }

    fn attach_conn(g: &mut GameState, ch: CharId, id: u64) -> ConnId {
        let conn = ConnId(id);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        if let Some(c) = g.get_char_mut(ch) {
            c.desc = Some(conn);
        }
        conn
    }

    fn temp_lib_with_worldmap(
        name: &str,
        width: usize,
        height: usize,
        extra: &str,
    ) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!(
            "deltamud-{}-{}-{}",
            name,
            std::process::id(),
            unique
        ));
        let world = dir.join("world");
        std::fs::create_dir_all(&world).unwrap();
        let row = ".".repeat(width);
        let mut map = String::from(
            "NewSector: .\n\
SectName: Field\n\
SectShow: .\n\
SectMove: 1\n\
SectSect: Field\n\
EndSector\n\
WorldMap:\n",
        );
        for _ in 0..height {
            map.push_str(&row);
            map.push('\n');
        }
        map.push_str("~\n");
        map.push_str(extra);
        std::fs::write(world.join("worldmap"), map).unwrap();
        dir
    }

    fn weather_corpse_descriptions(wtype: usize) -> (String, String) {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Outside".to_string(),
            "A weather test room.".to_string(),
        ));
        let ch = player_in_room(&mut g, "Stormvictim", room);

        weather_die(&mut g, ch, wtype);

        let corpse = *g.rooms[room].contents.first().expect("weather corpse");
        let obj = g.get_obj(corpse).unwrap();
        (obj.short_description.clone(), obj.description.clone())
    }

    #[test]
    fn entry_points_link_and_report_unresolvable() {
        // One good no-DIR link, one off-map coordinate, one interior vnum that
        // is not loaded. Only the good link may take effect; the failures must
        // come back in the report instead of vanishing (C dropped them
        // silently, so a typo'd gate just "didn't work").
        let dir = temp_lib_with_worldmap(
            "entry-points",
            5,
            5,
            "EntryPoint: 2 2 100\n\
             EntryPoint: 9 9 100\n\
             EntryPoint: 3 3 999\n",
        );
        let mut cfg = Config::default();
        cfg.lib_path = dir.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        let interior = g.add_room(Room::new(
            100,
            0,
            "Gate Interior".to_string(),
            String::new(),
        ));

        let report = integrate_map_rooms(&mut g);

        // Coordinates fold on the torus (WRAPX/WRAPY), so both directives with
        // a live interior resolve: (2,2) directly, (9,9) wrapped onto (4,4).
        let cell_2_2 = g.map_coords_to_rnum(2, 2).unwrap();
        let cell_9_9 = g.map_coords_to_rnum(9, 9).unwrap();
        assert_ne!(cell_2_2, cell_9_9);
        assert_eq!(g.room(cell_2_2).linkrnum, Some(interior));
        assert_eq!(g.room(cell_9_9).linkrnum, Some(interior));
        assert_eq!(g.room(interior).linkmapnum, Some(cell_9_9)); // last link wins
        assert_eq!(report.linked.len(), 2);
        // The directive naming interior 999 has no loaded room: reported, not
        // silently dropped.
        assert_eq!(report.unresolved.len(), 1);
        assert!(report.unresolved[0].reason.contains("not loaded"));
        assert_eq!(report.unresolved[0].interior_vnum, 999);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lweather_rejects_overflowing_or_off_map_explicit_coordinates() {
        let dir = temp_lib_with_worldmap("lweather-coordinates", 5, 5, "");
        let mut cfg = Config::default();
        cfg.lib_path = dir.to_string_lossy().into_owned();
        let mut g = GameState::new(cfg);
        integrate_map_rooms(&mut g);
        let room = g.map_start_rnum.expect("surface map room");
        let ch = player_in_room(&mut g, "WeatherAdmin", room);
        let conn = attach_conn(&mut g, ch, 9198);
        let before = with_map(&g, |m| m.storms.len());

        for (coords, expected) in [
            ("2147483648x1", "outside the supported range"),
            ("2147483647x1", "outside the loaded map"),
            ("-2147483648x1", "outside the loaded map"),
        ] {
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
            lweather(&mut g, ch, &format!("new rain {coords}"), 0);
            assert_eq!(with_map(&g, |m| m.storms.len()), before, "{coords}");
            assert!(g.descriptors[&conn].outbuf.contains(expected), "{coords}");
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn real_lib_links_temple_and_all_three_towns() {
        // The shipped lib (sibling of the crate); skip on exotic checkouts.
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        if !std::path::Path::new(&format!("{}/world/worldmap", lib)).exists() {
            return;
        }
        let mut g = crate::state::GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib)
            .await
            .unwrap();
        let report = integrate_map_rooms(&mut g);

        // Temple of Itrius (uncommented 2026-08-31), the three Itrius gates,
        // and the new Newhaven / Oranthalon / Locris links — every directive
        // in world/worldmap must resolve.
        assert!(
            report.unresolved.is_empty(),
            "unresolved EntryPoints: {:?}",
            report
                .unresolved
                .iter()
                .map(|f| (f.x, f.y, f.interior_vnum))
                .collect::<Vec<_>>()
        );
        assert_eq!(report.linked.len(), 7);

        // Each interior's `leave` leads back to a real surface cell at the
        // authored coordinates.
        for (interior_vnum, x, y) in [(100i32, 6, 82), (210, 17, 41), (600, 6, 60), (300, 29, 88)] {
            let rnum = g.real_room(interior_vnum).expect("interior room");
            let map_rnum = g.room(rnum).linkmapnum.expect("interior linked to map");
            assert_eq!(g.room(map_rnum).map_x, Some(x), "vnum {}", interior_vnum);
            assert_eq!(g.room(map_rnum).map_y, Some(y), "vnum {}", interior_vnum);
            assert_eq!(
                g.room(map_rnum).linkrnum,
                Some(rnum),
                "enter/leave round trip for vnum {}",
                interior_vnum
            );
        }
    }

    #[tokio::test]
    async fn real_lib_towns_walkable_from_itrius() {
        // The world-connects half of the EntryPoint work: from the Itrius east
        // gate a player must be able to WALK to each new town's enter cell
        // without crossing an impassable sector or unswimmable water.
        let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/../lib");
        if !std::path::Path::new(&format!("{}/world/worldmap", lib)).exists() {
            return;
        }
        let mut g = crate::state::GameState::new(Config::default());
        g.config.lib_path = lib.to_string();
        crate::file_loader::FileLoader::load_world(&mut g, lib)
            .await
            .unwrap();
        integrate_map_rooms(&mut g);

        let walkable = |r: &Room| r.mapmv > 0 && r.sector_type != SectorType::WaterNoSwim;
        let start = g.map_coords_to_rnum(10, 82).expect("Itrius east gate cell");
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::from([start]);
        seen.insert(start);
        while let Some(r) = queue.pop_front() {
            let (x, y) = (
                g.room(r).map_x.expect("map cell x"),
                g.room(r).map_y.expect("map cell y"),
            );
            for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                if let Some(n) = g.map_coords_to_rnum(x + dx, y + dy) {
                    if seen.insert(n) && walkable(g.room(n)) {
                        queue.push_back(n);
                    }
                }
            }
        }
        // BFS entered passable neighbours; re-root at the gate itself so the
        // gate cell counts as reached terrain.
        seen.insert(start);
        for (vnum, x, y) in [(210i32, 17, 41), (600, 6, 60), (300, 29, 88)] {
            let cell = g
                .map_coords_to_rnum(x, y)
                .unwrap_or_else(|| panic!("cell {},{} for vnum {}", x, y, vnum));
            assert!(
                seen.contains(&cell),
                "town vnum {} (cell {},{}) is not walkable from Itrius",
                vnum,
                x,
                y
            );
        }
    }

    #[test]
    fn integrate_map_rooms_preserves_sector_move_cost() {
        let mut dir = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("deltamud-mapmv-{}-{}", std::process::id(), unique));
        let world = dir.join("world");
        std::fs::create_dir_all(&world).unwrap();
        std::fs::write(
            world.join("worldmap"),
            "NewSector: .\n\
SectName: Passable\n\
SectShow: .\n\
SectMove: 2\n\
SectSect: Field\n\
EndSector\n\
NewSector: #\n\
SectName: Wall\n\
SectShow: #\n\
SectMove: -1\n\
SectSect: City\n\
EndSector\n\
WorldMap:\n\
.#\n\
~\n",
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.lib_path = dir.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);

        integrate_map_rooms(&mut g);

        let start = g.map_start_rnum.expect("map rooms spliced");
        assert_eq!(g.room(start).mapmv, 2);
        assert_eq!(g.room(start + 1).mapmv, -1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn world_map_preserves_unicode_sector_glyph_boundaries() {
        for (label, show, expected) in [
            ("unicode-color-glyph", "&Ré", "&Réé"),
            ("unicode-invalid-prefix", "&é", "&é&é"),
        ] {
            let dir = temp_lib_with_worldmap(label, 2, 1, "");
            let path = dir.join("world/worldmap");
            let source = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                &path,
                source.replacen("SectShow: .", &format!("SectShow: {show}"), 1),
            )
            .unwrap();
            let mut config = Config::default();
            config.lib_path = dir.to_string_lossy().into_owned();
            let mut g = GameState::new(config);

            integrate_map_rooms(&mut g);
            let rendered = render_world_map(&g, 1, 1, 2, 1, None);

            assert!(
                rendered.contains(expected),
                "show={show:?}, rendered={rendered:?}"
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn weather_corpse_uses_storm_adjective() {
        let (short, desc) = weather_corpse_descriptions(WEATHER_FIRESTORM);

        assert_eq!(short, "the burnt crispy corpse of Stormvictim");
        assert_eq!(
            desc,
            "The burnt crispy corpse of Stormvictim is lying here."
        );
    }

    #[test]
    fn weather_corpse_skips_blank_adjective_entries() {
        let (short, desc) = weather_corpse_descriptions(WEATHER_RAINSTORM);

        assert_eq!(short, "the corpse of Stormvictim");
        assert_eq!(desc, "The corpse of Stormvictim is lying here.");
    }

    #[test]
    fn weather_messages_fan_out_to_zweatherpoint_zone() {
        let dir = temp_lib_with_worldmap("weather-msg", 5, 5, "ZWeatherPoint: 3 3 1\n");
        let mut cfg = Config::default();
        cfg.lib_path = dir.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        let room = g.add_room(Room::new(100, 1, "Outside".into(), "Outside.".into()));
        let ch = player_in_room(&mut g, "Watcher", room);
        let conn = attach_conn(&mut g, ch, 1);

        send_weather_messages(&mut g, WEATHER_MSG_ACT, WEATHER_RAINSTORM, 1, 3, 3);

        assert!(
            g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("A rain storm pours down on you from above.")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn radial_activity_fans_out_damage_to_zweatherpoint_zone() {
        let dir = temp_lib_with_worldmap("weather-radial", 5, 5, "ZWeatherPoint: 3 3 1\n");
        let mut cfg = Config::default();
        cfg.lib_path = dir.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        let room = g.add_room(Room::new(100, 1, "Outside".into(), "Outside.".into()));
        let ch = player_in_room(&mut g, "Chilled", room);
        g.get_char_mut(ch).unwrap().points.hit = 100;

        radial_activity(
            &mut g,
            RadialHit {
                wtype: WEATHER_BLIZZARD,
                radius: 1,
                x: 3,
                y: 3,
            },
        );

        assert_eq!(g.get_char(ch).unwrap().points.hit, 93);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tornado_knockback_relocates_and_sends_midpoint_message() {
        let dir = temp_lib_with_worldmap("weather-knockback", 20, 20, "");
        let mut cfg = Config::default();
        cfg.lib_path = dir.to_string_lossy().to_string();
        let mut g = GameState::new(cfg);
        integrate_map_rooms(&mut g);
        let origin = g.map_coords_to_rnum(10, 10).unwrap();
        let observer_room = g.map_coords_to_rnum(10, 14).unwrap();
        let victim = player_in_room(&mut g, "Flyer", origin);
        let observer = player_in_room(&mut g, "Observer", observer_room);
        let victim_conn = attach_conn(&mut g, victim, 1);
        let observer_conn = attach_conn(&mut g, observer, 2);
        {
            let c = g.get_char_mut(victim).unwrap();
            c.points.hit = 1000;
            c.player.weight = 120;
        }

        unit_activity(&mut g, origin, WEATHER_TORNADO);

        // C move_char walks the surface via cell exits; the SHIPPED worldmap
        // defines no cell-to-cell exits (0 'Exit:' lines), so C's own walk
        // terminates immediately and the victim lands where they started.
        // The walk becomes live the moment a worldmap with exits ships.
        // Assert the jettison outcome: alive, resting, landing message sent.
        assert_eq!(g.get_char(victim).unwrap().position, Position::Resting);
        assert!(
            g.descriptors
                .get(&victim_conn)
                .unwrap()
                .outbuf
                .contains("You land head-first into the ground!")
        );
        let _ = observer_conn;
        assert!(
            g.descriptors
                .get(&victim_conn)
                .unwrap()
                .outbuf
                .contains("You land head-first into the ground!")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn weather_death_cleanup_clears_pc_criminal_flags_and_conditions() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Outside".to_string(),
            "A weather test room.".to_string(),
        ));
        let ch = player_in_room(&mut g, "Flagged", room);
        {
            let c = g.get_char_mut(ch).unwrap();
            c.act_flags |= PLR_KILLER | PLR_THIEF;
            c.conditions[FULL] = 10;
            c.conditions[THIRST] = 11;
            c.conditions[DRUNK] = 12;
        }

        cleanup_weather_death_player_state(&mut g, ch);

        let c = g.get_char(ch).unwrap();
        assert_eq!(c.act_flags & (PLR_KILLER | PLR_THIEF), 0);
        assert_eq!(c.conditions[FULL], 0);
        assert_eq!(c.conditions[THIRST], 0);
        assert_eq!(c.conditions[DRUNK], 0);
    }

    #[test]
    fn weather_die_transfers_connected_pc_gold_to_corpse() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Outside".to_string(),
            "A weather test room.".to_string(),
        ));
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let ch = player_in_room(&mut g, "Richvictim", room);
        {
            let c = g.get_char_mut(ch).unwrap();
            c.desc = Some(conn);
            crate::gold::set(c, crate::gold::Account::Carried, 1234);
        }

        weather_die(&mut g, ch, WEATHER_FIRESTORM);

        assert!(!g.char_exists(ch));
        let corpse = *g.rooms[room].contents.first().expect("weather corpse");
        let money = g
            .get_obj(corpse)
            .unwrap()
            .contains
            .iter()
            .copied()
            .find(|&oid| {
                g.get_obj(oid)
                    .map(|o| o.obj_type == ObjectType::Money)
                    .unwrap_or(false)
            })
            .expect("corpse money");
        assert_eq!(g.get_obj(money).unwrap().values[0], 1234);
    }

    #[test]
    fn thunderstorm_bolt_converts_redirect_charge() {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(Room::new(
            100,
            0,
            "Outside".to_string(),
            "A weather test room.".to_string(),
        ));
        let ch = player_in_room(&mut g, "Charged", room);
        {
            let c = g.get_char_mut(ch).unwrap();
            c.points.hit = 1000;
            c.affect_flags |= AFF_REDIRECT_CHARGE;
            c.affected.push(Affect {
                spell_type: SPELL_REDIRECT_CHARGE,
                duration: 24,
                modifier: 0,
                location: 0,
                bitvector: AFF_REDIRECT_CHARGE,
                caster: None,
            });
        }

        assert!(!apply_thunderstorm_bolt(
            &mut g,
            ch,
            WEATHER_THUNDERSTORM,
            &mut |_g| 800,
        ));

        let c = g.get_char(ch).unwrap();
        assert_eq!(c.points.hit, 950);
        assert_eq!(c.affect_flags & AFF_REDIRECT_CHARGE, 0);
        assert_ne!(c.affect_flags & AFF_R_CHARGED, 0);
        let af = c
            .affected
            .iter()
            .find(|af| af.spell_type == SPELL_REDIRECT_CHARGE)
            .unwrap();
        assert_eq!(af.bitvector, AFF_R_CHARGED);
        assert_eq!(af.modifier, 750);
        assert_eq!(af.location, APPLY_DAMAGE);
        assert_eq!(af.duration, 100);
    }
}
