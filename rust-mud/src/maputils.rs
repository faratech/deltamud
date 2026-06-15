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
// heartbeat) is not part of this command-facing port; with no active storms the
// weather map renders as the empty filler field exactly like a freshly
// initialised `update_weather_map()`, which is what `printweather` shows on a
// quiet day. The damaging-weather mechanics live with the heartbeat batch.
//
// Module-static state: the parsed map (keyed by lib_path) plus the MAP_ACTIVE
// load toggle, behind a Mutex/OnceLock like the other runtime tables.

use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ---- weather constants (maputils.h) ---------------------------------------

const WEATHER_TOTAL: usize = 10;

// Per-weather glyph / colour / name tables (maputils.c top of file). Only the
// rendering tables are needed here (the damage/behaviour table lives with the
// heartbeat weather batch).
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
// in check_noroom; that overlay path activates with the heartbeat weather batch.
#[allow(dead_code)]
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
}

impl MapData {
    fn empty() -> MapData {
        MapData { active: false, max_x: 0, max_y: 0, grid: Vec::new(), sectors: HashMap::new() }
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

    MapData { active: true, max_x, max_y, grid, sectors }
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

/// "Map of the World's Weather" (do_map weather mode). With no active storms the
/// weather field is uniform filler; the elision/player-marker logic still runs.
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
        let wmap = build_weather_map(m);
        for y in ym..=yl {
            for x in xm..=xl {
                if player == Some((x, y)) {
                    out.push_str("&n#");
                    continue;
                }
                let cur = wcell(&wmap, m, x, y);
                let cur_b = cur.as_bytes();
                if x > xm {
                    // C indexes weather_map[y-1][x-2] (raw, with the < 0 wrap).
                    let left = wcell(&wmap, m, x - 1, y);
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
// Weather map buffer. With no live storms this is a uniform filler field
// ("&n+"), exactly as update_weather_map() leaves it after clearing.
// ---------------------------------------------------------------------------

/// A weather_map equivalent: one render string per cell (1-based access via
/// wcell). Currently every cell is the filler glyph; the storm overlay belongs
/// to the heartbeat weather batch.
fn build_weather_map(_m: &MapData) -> String {
    format!("&n{}", FILLER_CHAR)
}

/// weather_map[y-1][x-1] lookup; uniform filler today (storm-free).
fn wcell<'a>(wmap: &'a str, m: &MapData, x: i32, y: i32) -> &'a str {
    let _ = m.wrap(x, y);
    wmap
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

    let mut sightradmult: i32 = 2;
    if level >= LVL_IMMORT {
        sightradmult += 1;
    }
    // Surface-room fog would shrink the view (WEATHER_FOG) or invert it
    // (WEATHER_MAGICFOG); with no live storms the standing room's weather is
    // "none", so neither modifier fires. The fog branches land with the
    // heartbeat weather batch.
    let invert = false;
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
        // !invert path (the only reachable one without live magic fog). The
        // inverted (mirror) path is the fog variant deferred to the weather
        // batch; it draws the same cells in reverse order.
        let _ = invert;
        for j in -ry..=ry {
            buf.push_str("&c|");
            for k in -radius..=radius {
                if k == 0 && j == 0 {
                    buf.push_str("&w#");
                } else {
                    buf.push_str(check_noroom(m, cx, cy, cx + k, cy + j, radius, 0));
                }
            }
            buf.push_str("&c|\r\n");
        }
    });

    buf.push_str("`&c");
    for _ in (-MAP_VISION_RADIUS_X * sightradmult)..=(MAP_VISION_RADIUS_X * sightradmult) {
        buf.push('-');
    }
    buf.push_str("&n'\r\n");

    g.send_to_char(ch, &buf);
}

/// check_noroom (maputils.c, modifier==0 / terrain path): the glyph for a cell,
/// applying the same run-length colour elision as the world map. A cell that
/// shares the leading 2-char "&X" colour of the cell on its left drops that
/// code — UNLESS that left cell is the player's own cell, or this cell sits at
/// the left edge of the visible window (so the window's first column always
/// carries its colour). `(px,py)` is the player's centre cell; `radius` is the
/// half-width of the window (MAP_VISION_RADIUS_X * sightradmult).
fn check_noroom<'a>(m: &'a MapData, px: i32, py: i32, x: i32, y: i32, radius: i32, _modifier: i32) -> &'a str {
    let tmp = m.cell_id(x, y);
    let left = m.cell_id(x - 1, y); // modifier==0 path: rm2x(i)-1
    let tmp_b = tmp.as_bytes();

    // j = rm2x(ch->in_room) - radius, wrapped into 1..=max_x: the x of the
    // window's left column. Elision is suppressed when this cell is that column.
    let mut j = px - radius;
    if j < 1 {
        j += m.max_x;
    }
    let (cur_x, _) = m.wrap(x, y);
    // The player's cell sits at (px,py); "left is the player" means (px,py)==(x-1,y).
    let left_is_player = px == x - 1 && py == y;

    if tmp_b.first() == Some(&b'&')
        && tmp.len() >= 3
        && left.len() >= 2
        && tmp_b.get(..2) == left.as_bytes().get(..2)
        && !left_is_player
        && j != cur_x
    {
        &tmp[2..]
    } else {
        tmp
    }
}

// ---------------------------------------------------------------------------
// pweather — the player weather map (printweather), and lweather.
// ---------------------------------------------------------------------------

pub fn pweather(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // C: fog blocks reading the weather; indoors is impossible; otherwise draw
    // the weather map if the room is a map room or its zone has a ZWeatherPoint.
    // With no live storms, room weather is "none" and the fog/indoor guards do
    // not fire for an ordinary outdoor room, so we render the (empty) map.
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

    let wmap = with_map(g, |m| build_weather_map(m));

    for j in -WEATHER_VISION_RADIUS_Y..=WEATHER_VISION_RADIUS_Y {
        let mut line = String::from("&c|");
        with_map(g, |m| {
            for k in -WEATHER_VISION_RADIUS_X..=WEATHER_VISION_RADIUS_X {
                if k == 0 && j == 0 {
                    line.push_str("&w#");
                } else {
                    line.push_str(weatherchar(&wmap, m, x + k, y + j, x, y));
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

/// weatherchar (maputils.c): weather glyph for a cell with colour elision vs the
/// cell to its left, suppressed at the left vision threshold. Storm-free today,
/// so every cell is the filler; the elision logic still runs for byte parity.
fn weatherchar<'a>(
    wmap: &'a str,
    m: &MapData,
    x: i32,
    y: i32,
    inx: i32,
    _iny: i32,
) -> &'a str {
    let tmp = wcell(wmap, m, x, y);
    let left = wcell(wmap, m, x - 1, y);
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
