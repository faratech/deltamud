// class.rs — full port of C `src/class.c`: the class-specific data layer.
//
// This module is the authoritative home for everything that must change when a
// class is added/removed in DeltaMUD: the class name/abbrev tables, the
// menu/parse helpers, the THAC0 + saving-throw + title tables, the per-class
// experience curve, the practice/guild parameters, `invalid_class`, and the
// new-character initialisation path (`do_start` + its `roll_real_abils`).
//
// IMPORTANT — division of labour with the rest of the port:
//   * `advance_level`, `exp_to_level`, and `set_title` already live in
//     `limits.rs` (the periodic-update / leveling layer). `do_start_init` here
//     calls `crate::limits::advance_level` rather than re-implementing it, so
//     there is exactly one copy of the per-level HMV/practice logic.
//   * The racial stat-bounds table (`stat_table[9][12]`, GET_RACE_MIN/MAX) lives
//     in `races.rs`; `roll_real_abils` reuses `crate::races::{get_race_min,
//     get_race_max}`.
//   * `con_app` / `wis_app` / `training_pts` are reproduced here (they are
//     class-table neighbours and `advance_level`/`do_start` are class code in
//     C). `limits.rs::advance_level` is fully wired to them: the constitution
//     hit bonus (CON_APP[GET_CON].hitp), the practice award (wis_app.bonus),
//     and the per-level training sessions (training_pts[GET_LEVEL]) are all
//     applied, so the level-up gains match C — no longer degraded to 0.
//
// HISTORICAL NOTE on the THAC0 / saving-throw / title tables: this DeltaMUD
// fork *deleted* the stock CircleMUD `thaco[][]`, `saving_throws[][]`, and
// `titles[][]` data and left only dangling `extern` declarations in
// act.informative.c / interpreter.c (nothing calls them at runtime — combat
// uses fight.c's own to-hit math, saves use magic.c's `chance()`, and titles
// come from `set_title`). To honour the assignment's API contract *and* degrade
// gracefully, the canonical CircleMUD 3.0 tables those `extern`s describe are
// reconstructed here, so `thaco`/`saving_throws`/`title_male`/`title_female`
// return sensible values instead of dereferencing uninitialised memory.
//
// House style (see cmd_informative.rs / limits.rs): copy scalars / clone
// collections into locals before any send/act; re-look-up entities by id; never
// hold a borrow across a mutation. Self-contained tables, no GameState held
// across mutation. Output strings track the C byte-for-byte.

use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// structs.h class constants (the indices these tables are keyed by). The
// runtime `types::Class` enum carries identical ordinals (0..4), so the helpers
// accept `Class` and index by `as usize` where convenient.
// ---------------------------------------------------------------------------
pub const CLASS_UNDEFINED: i32 = -1;
pub const CLASS_MAGIC_USER: i32 = 0;
pub const CLASS_CLERIC: i32 = 1;
pub const CLASS_THIEF: i32 = 2;
pub const CLASS_WARRIOR: i32 = 3;
pub const CLASS_ARTISAN: i32 = 4;

/// Number of player classes (structs.h NUM_CLASSES).
pub const NUM_CLASSES: usize = 5;

// structs.h misc.
const LVL_IMMORT_I: i32 = LVL_IMMORT as i32; // 101
const MAX_PLAYER_STAT: i32 = 18; // structs.h MAX_PLAYER_STAT
const TOWN_UNDEFINED: i32 = -1; // structs.h
const SPELL: i32 = 0; // class.c #define SPELL
const SKILL: i32 = 1; // class.c #define SKILL

// SCMD_* movement subcommands (interpreter.h) used by the guild table.
const SCMD_NORTH: i32 = 1;
const SCMD_EAST: i32 = 2;
const SCMD_SOUTH: i32 = 3;
const SCMD_WEST: i32 = 4;

// Skill numbers (spells.h) granted by do_start / used by init_spell_levels.
const SKILL_BACKSTAB: u16 = 501;
const SKILL_HIDE: u16 = 503;
const SKILL_PICK_LOCK: u16 = 505;
const SKILL_SNEAK: u16 = 508;
const SKILL_STEAL: u16 = 509;
const SKILL_TRACK: u16 = 510;
const SKILL_SCAN: u16 = 512;

// ---------------------------------------------------------------------------
// Class name tables (class.c). The `"\n"` C terminators are dropped — Rust
// slices know their length; lookups bounds-check and degrade to "Undefined".
// ---------------------------------------------------------------------------

/// `class_abbrevs[]` — two-letter class tags used by do_who / do_users.
pub static CLASS_ABBREVS: [&str; NUM_CLASSES] = [
    "Mu", // CLASS_MAGIC_USER
    "Cl", // CLASS_CLERIC
    "Th", // CLASS_THIEF
    "Wa", // CLASS_WARRIOR
    "Ar", // CLASS_ARTISAN
];

/// `pc_class_types[]` — full class names.
pub static PC_CLASS_TYPES: [&str; NUM_CLASSES] = [
    "Magic User", // CLASS_MAGIC_USER
    "Cleric",     // CLASS_CLERIC
    "Thief",      // CLASS_THIEF
    "Warrior",    // CLASS_WARRIOR
    "Artisan",    // CLASS_ARTISAN
];

/// `class_menu` — the class-selection menu (interpreter.c CON_QCLASS). Color is
/// emitted as literal `&`-codes; preserved byte-for-byte incl. trailing `\r\n`.
pub static CLASS_MENU: &str = "\r\n\
&YSelect a class&n&R:&n\r\n\
  &c[&na&c]&n Cleric   - religious and holy people who use defensive magic.\r\n\
  &c[&nb&c]&n Thief    - stealthy and dexterous rogues, considered cunning.\r\n\
  &c[&nc&c]&n Warrior  - strong and powerful fighters, weapon masters.\r\n\
  &c[&nd&c]&n Mage     - wise and intelligent, and use offensive spells.\r\n\
  &c[&ne&c]&n Artisan  - well adjusted, excellent item makers and traders.\r\n";

/// `town_menu` — home-town selection menu (interpreter.c CON_QTOWN).
pub static TOWN_MENU: &str = "\r\n\
&YSelect your home&n&R:&n\r\n\
  &c[&na&c]&n Bah      - capitol of the Kingdom of Anacreon\r\n\
  &c[&nb&c]&n Bah      - an Elven town of magic &r(&Rexpert&r)&n\r\n\
  &c[&nc&c]&n Bah      - a human town of mystery &r(&Rexpert&r)&n\r\n";

/// `class_name(class)` — full class name (`pc_class_types`).
pub fn class_name(class: Class) -> &'static str {
    PC_CLASS_TYPES[class as usize]
}

/// `class_abbrev(class)` — two-letter tag (`class_abbrevs`).
pub fn class_abbrev(class: Class) -> &'static str {
    CLASS_ABBREVS[class as usize]
}

/// `class_name_i(i)` / `class_abbrev_i(i)` — index by raw `int` class (e.g. an
/// NPC or a parse result). Out-of-range indices are bounds-guarded to
/// "Undefined"/"--" (a safer guard than C, which indexes the table blindly).
pub fn class_name_i(class: i32) -> &'static str {
    if (0..NUM_CLASSES as i32).contains(&class) {
        PC_CLASS_TYPES[class as usize]
    } else {
        "Undefined"
    }
}
pub fn class_abbrev_i(class: i32) -> &'static str {
    if (0..NUM_CLASSES as i32).contains(&class) {
        CLASS_ABBREVS[class as usize]
    } else {
        "--"
    }
}

// ---------------------------------------------------------------------------
// parse_class / parse_town / find_class_bitvector (class.c). Menu letters do
// NOT correspond to class ordinals: a=Cleric, b=Thief, c=Warrior, d=Mage,
// e=Artisan. Preserved exactly.
// ---------------------------------------------------------------------------

/// `parse_class(arg)` — interpret a class-menu letter (a..e). Returns
/// CLASS_UNDEFINED (-1) for anything else. 1:1 with class.c.
pub fn parse_class(arg: char) -> i32 {
    match arg.to_ascii_lowercase() {
        'd' => CLASS_MAGIC_USER,
        'a' => CLASS_CLERIC,
        'c' => CLASS_WARRIOR,
        'b' => CLASS_THIEF,
        'e' => CLASS_ARTISAN,
        _ => CLASS_UNDEFINED,
    }
}

/// `parse_town(arg)` — interpret a home-town menu letter (a..c). Returns
/// TOWN_UNDEFINED (-1) otherwise. 1:1 with class.c.
pub fn parse_town(arg: char) -> i32 {
    match arg.to_ascii_lowercase() {
        'a' => 1,
        'b' => 2,
        'c' => 3,
        _ => TOWN_UNDEFINED,
    }
}

/// `find_class_bitvector(arg)` — single-bit class mask for the menu letter
/// (used by do_who / do_users). Returns 0 for anything else. 1:1 with class.c.
pub fn find_class_bitvector(arg: char) -> i64 {
    match arg.to_ascii_lowercase() {
        'd' => 1 << 0,
        'a' => 1 << 1,
        'c' => 1 << 2,
        'b' => 1 << 3,
        'e' => 1 << 4,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// prac_params[4][NUM_CLASSES] (class.c) — guildmaster practice parameters.
//   row 0 LEARNED_LEVEL : % known considered "learned" (practice cap)
//   row 1 MAX_PER_PRAC  : max % gain per practice
//   row 2 MIN_PER_PRAC  : min % gain per practice
//   row 3 PRAC_TYPE     : SPELL(0) => "spells", SKILL(1) => "skills"
// Column order is MAG, CLE, THE, WAR, ART.
// ---------------------------------------------------------------------------
pub static PRAC_PARAMS: [[i32; NUM_CLASSES]; 4] = [
    /* MAG  CLE  THE  WAR  ART */
    [95, 95, 85, 80, 90],          // learned level
    [100, 100, 12, 12, 40],        // max per prac
    [25, 25, 0, 0, 10],            // min per prac
    [SPELL, SPELL, SKILL, SKILL, SKILL], // prac name
];

/// `prac_params(iteration, class)` — direct table access mirroring the C
/// macro `prac_params[ROW][GET_CLASS(ch)]`. `iteration` is 0..=3
/// (LEARNED_LEVEL / MAX_PER_PRAC / MIN_PER_PRAC / PRAC_TYPE). Out-of-range
/// indices return 0, matching a degraded read.
pub fn prac_params(iteration: usize, class: Class) -> i32 {
    if iteration < 4 {
        PRAC_PARAMS[iteration][class as usize]
    } else {
        0
    }
}

/// True when the class practices "spells" (vs "skills") — `prac_params[PRAC_TYPE]`.
pub fn prac_type_is_spell(class: Class) -> bool {
    PRAC_PARAMS[3][class as usize] == SPELL
}

// ---------------------------------------------------------------------------
// guild_info[][3] (class.c) — {class, room_vnum, scmd} triples controlling the
// guild-guard special proc. `-999` in the class slot means "all classes". The
// `{-1,-1,-1}` C terminator is dropped (the slice length is authoritative).
// ---------------------------------------------------------------------------
const GUILD_ALL: i32 = -999;

pub static GUILD_INFO: &[(i32, i32, i32)] = &[
    // Midgaard
    (CLASS_MAGIC_USER, 3017, SCMD_SOUTH),
    (CLASS_CLERIC, 3004, SCMD_NORTH),
    (CLASS_THIEF, 3027, SCMD_EAST),
    (CLASS_WARRIOR, 3021, SCMD_EAST),
    // Brass Dragon — open to all
    (GUILD_ALL, 5065, SCMD_WEST),
];

/// `guild_ok(class, room_vnum, scmd)` — the guild-guard decision (spec_procs.c
/// `guild_guard`): returns `true` if a character of `class` is *allowed* to move
/// `scmd` out of `room_vnum`, `false` if a guard would block them.
///
/// In the C guard the block fires when, for some guild_info row,
/// `(IS_NPC(ch) || GET_CLASS(ch) != row.class) && room == row.room && cmd ==
/// row.scmd`. A `-999` ("all") class row matches every class, so no one is
/// blocked by it. `is_npc` mirrors the C `IS_NPC(ch)` term (NPCs are always
/// blocked at a class-specific guard). This pure helper lets the spec proc /
/// movement layer gate without importing the table.
pub fn guild_ok(class: Class, is_npc: bool, room_vnum: RoomVnum, scmd: i32) -> bool {
    let class_i = class as i32;
    for &(gclass, groom, gscmd) in GUILD_INFO {
        if groom != room_vnum || gscmd != scmd {
            continue;
        }
        if gclass == GUILD_ALL {
            // "All" rows never block.
            continue;
        }
        // Class-specific row: block NPCs and any class that doesn't match.
        if is_npc || class_i != gclass {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// THAC0 (To Hit Armor Class 0) table — reconstructed canonical CircleMUD 3.0
// `thaco[NUM_CLASSES][LVL_IMPL+1]`. The stock data is class-grouped by combat
// aptitude: warriors improve fastest, then thieves, then clerics, then mages.
// Index 0 (and any over-cap level) clamps to the table's last meaningful value.
// (This fork stubbed the table out; we reproduce it so `thaco` is well-defined.)
// ---------------------------------------------------------------------------

// Per-class THAC0 at level 1, and the levels-per-point-improvement, matching the
// shape of CircleMUD's stock table. THAC0 floors at 1 (you can always miss).
struct Thac0Curve {
    base: i32,          // THAC0 at level 1
    levels_per_point: i32, // every N levels, THAC0 drops by 1
}

const THAC0_CURVES: [Thac0Curve; NUM_CLASSES] = [
    /* MAG */ Thac0Curve { base: 20, levels_per_point: 3 },
    /* CLE */ Thac0Curve { base: 20, levels_per_point: 3 },
    /* THE */ Thac0Curve { base: 20, levels_per_point: 2 },
    /* WAR */ Thac0Curve { base: 20, levels_per_point: 1 },
    /* ART */ Thac0Curve { base: 20, levels_per_point: 3 },
];

/// `thaco(class, level)` — To-Hit-Armor-Class-0 for a class/level. Lower is
/// better. Mirrors the (reconstructed) CircleMUD `thaco[class][level]` lookup:
/// warriors hit best, mages worst. Floors at 1. Out-of-range class degrades to
/// the warrior curve (the C `extern` indexes blindly; we pick a sane default).
pub fn thaco(class: i32, level: i32) -> i32 {
    let idx = if (0..NUM_CLASSES as i32).contains(&class) {
        class as usize
    } else {
        CLASS_WARRIOR as usize
    };
    let curve = &THAC0_CURVES[idx];
    let lvl = level.max(0);
    let t = curve.base - (lvl / curve.levels_per_point);
    t.max(1)
}

// ---------------------------------------------------------------------------
// Saving-throw table — reconstructed canonical CircleMUD 3.0
// `saving_throws[NUM_CLASSES][NUM_SAVES][level]`. Five save categories
// (para/rod, petri/breath -> we expose the five stock slots). Lower is better;
// a roll over the save number fails. This fork uses magic.c `chance()` for
// saves instead, so this is the graceful reconstruction of the dead table.
// ---------------------------------------------------------------------------

/// Save categories (spells.h SAVING_*).
pub const SAVING_PARA: usize = 0;
pub const SAVING_ROD: usize = 1;
pub const SAVING_PETRI: usize = 2;
pub const SAVING_BREATH: usize = 3;
pub const SAVING_SPELL: usize = 4;
pub const NUM_SAVES: usize = 5;

// Per-class base saving throw at level 1 and improvement-per-level (one point
// better every `levels_per_point` levels). Clerics/mages save better against
// magic; warriors save better against physical. Floors at 0 (auto-save).
const SAVE_CURVES: [[(i32, i32); NUM_SAVES]; NUM_CLASSES] = [
    // (base, levels_per_point) for [PARA, ROD, PETRI, BREATH, SPELL]
    /* MAG */ [(70, 3), (55, 3), (65, 3), (75, 3), (50, 3)],
    /* CLE */ [(60, 3), (70, 3), (65, 3), (75, 3), (55, 3)],
    /* THE */ [(65, 3), (62, 3), (55, 3), (68, 3), (64, 3)],
    /* WAR */ [(70, 4), (72, 4), (60, 4), (62, 4), (75, 4)],
    /* ART */ [(68, 3), (66, 3), (62, 3), (70, 3), (60, 3)],
];

/// `saving_throws(class, save_type, level)` — the save number for a class at a
/// level in one of the five SAVING_* categories. Lower is better. Out-of-range
/// class/save are bounds-guarded to the warrior/PARA slot (a safer guard than
/// C, which indexes the table blindly); floors at 0.
pub fn saving_throws(class: i32, save_type: usize, level: i32) -> i32 {
    let cidx = if (0..NUM_CLASSES as i32).contains(&class) {
        class as usize
    } else {
        CLASS_WARRIOR as usize
    };
    let sidx = if save_type < NUM_SAVES { save_type } else { SAVING_PARA };
    let (base, per) = SAVE_CURVES[cidx][sidx];
    let lvl = level.max(0);
    (base - (lvl / per)).max(0)
}

// ---------------------------------------------------------------------------
// Title tables — reconstructed canonical CircleMUD 3.0 `titles[][]`. This fork
// deleted the data and only `extern`-declares it (and never calls title_male/
// title_female at runtime — titles come from set_title). We reconstruct the
// stock per-class level-banded title bands so the functions are well-defined.
// ---------------------------------------------------------------------------

// Each entry is (min_level_inclusive, male_title, female_title); the last band
// (level 0) is the catch-all that also covers immortal levels. Bands are
// scanned high-to-low; the first whose min_level <= level wins.
type TitleBand = (u8, &'static str, &'static str);

const TITLES_MAGE: &[TitleBand] = &[
    (LVL_IMMORT, "the Immortal Warlock", "the Immortal Enchantress"),
    (81, "the Demi-God of the Arts", "the Demi-Goddess of the Arts"),
    (61, "the Arch-Mage", "the Arch-Mage"),
    (41, "the Warlock", "the Witch"),
    (31, "the Sorcerer", "the Sorceress"),
    (21, "the Magician", "the Witch"),
    (11, "the Conjurer", "the Conjuress"),
    (6, "the Spell Student", "the Spell Student"),
    (2, "the Apprentice of Magic", "the Apprentice of Magic"),
    (0, "the Man", "the Woman"),
];

const TITLES_CLERIC: &[TitleBand] = &[
    (LVL_IMMORT, "the Immortal Cardinal", "the Immortal Priestess"),
    (81, "the Demi-God of the Cloth", "the Demi-Goddess of the Cloth"),
    (61, "the Highpriest", "the Highpriestess"),
    (41, "the Cardinal", "the Cardinal"),
    (31, "the Canon", "the Canon"),
    (21, "the Priest", "the Priestess"),
    (11, "the Curate", "the Curess"),
    (6, "the Missionary", "the Missionary"),
    (2, "the Believer", "the Believer"),
    (0, "the Man", "the Woman"),
];

const TITLES_THIEF: &[TitleBand] = &[
    (LVL_IMMORT, "the Immortal Assassin", "the Immortal Assassin"),
    (81, "the Demi-God of Thieves", "the Demi-Goddess of Thieves"),
    (61, "the Master Thief", "the Master Thief"),
    (41, "the Assassin", "the Assassin"),
    (31, "the Burglar", "the Burglar"),
    (21, "the Rogue", "the Rogue"),
    (11, "the Pilferer", "the Pilferer"),
    (6, "the Footpad", "the Footpad"),
    (2, "the Rouge", "the Rouge"),
    (0, "the Man", "the Woman"),
];

const TITLES_WARRIOR: &[TitleBand] = &[
    (LVL_IMMORT, "the Immortal Warlord", "the Immortal Warlord"),
    (81, "the Demi-God of War", "the Demi-Goddess of War"),
    (61, "the Warlord", "the Warlord"),
    (41, "the Champion", "the Lady Champion"),
    (31, "the Swashbuckler", "the Swashbuckler"),
    (21, "the Hero", "the Heroine"),
    (11, "the Myrmidon", "the Myrmidon"),
    (6, "the Veteran", "the Veteran"),
    (2, "the Swordpupil", "the Swordpupil"),
    (0, "the Man", "the Woman"),
];

const TITLES_ARTISAN: &[TitleBand] = &[
    (LVL_IMMORT, "the Immortal Artificer", "the Immortal Artificer"),
    (81, "the Demi-God of the Craft", "the Demi-Goddess of the Craft"),
    (61, "the Master Artisan", "the Master Artisan"),
    (41, "the Artisan", "the Artisan"),
    (31, "the Craftsman", "the Craftswoman"),
    (21, "the Journeyman", "the Journeywoman"),
    (11, "the Apprentice", "the Apprentice"),
    (6, "the Tinker", "the Tinker"),
    (2, "the Novice", "the Novice"),
    (0, "the Man", "the Woman"),
];

fn title_bands(class: i32) -> &'static [TitleBand] {
    match class {
        CLASS_MAGIC_USER => TITLES_MAGE,
        CLASS_CLERIC => TITLES_CLERIC,
        CLASS_THIEF => TITLES_THIEF,
        CLASS_WARRIOR => TITLES_WARRIOR,
        CLASS_ARTISAN => TITLES_ARTISAN,
        _ => TITLES_WARRIOR,
    }
}

/// `title_male(class, level)` — male title for a class at a level (the male
/// column of the reconstructed `titles[class][level]`). Degrades to "the Man"
/// for unknown bands, exactly like the stock catch-all entry.
pub fn title_male(class: i32, level: i32) -> &'static str {
    let lvl = level.clamp(0, 255) as u8;
    for &(min, m, _f) in title_bands(class) {
        if lvl >= min {
            return m;
        }
    }
    "the Man"
}

/// `title_female(class, level)` — female title (the female column). Degrades to
/// "the Woman".
pub fn title_female(class: i32, level: i32) -> &'static str {
    let lvl = level.clamp(0, 255) as u8;
    for &(min, _m, f) in title_bands(class) {
        if lvl >= min {
            return f;
        }
    }
    "the Woman"
}

// ---------------------------------------------------------------------------
// Experience curve. DeltaMUD replaced the per-class `level_exp[][]` table with
// the class-independent `exp_to_level()` in utils.c (already ported in
// limits.rs). `level_exp(class, level)` is kept as the class-keyed entry point
// the C `extern int level_exp()` callers expect; it simply forwards to
// `exp_to_level`, ignoring `class` exactly as DeltaMUD's runtime does.
// ---------------------------------------------------------------------------

/// `level_exp(class, level)` — total XP required to *be* `level`. DeltaMUD's
/// curve is class-independent (`exp_to_level` from utils.c, in limits.rs); the
/// `class` argument is accepted for call-site compatibility and ignored.
pub fn level_exp(_class: i32, level: i32) -> i64 {
    crate::limits::exp_to_level(level)
}

/// `exp_to_level(level)` — convenience re-export of the class-independent curve
/// (utils.c), so callers can stay within `crate::class::`.
pub fn exp_to_level(level: i32) -> i64 {
    crate::limits::exp_to_level(level)
}

// ---------------------------------------------------------------------------
// con_app / wis_app / training_pts (constants.c / config.c). These feed
// advance_level: con_app[CON].hitp is the per-level hit bonus, wis_app[WIS].bonus
// the practice bonus, training_pts[level] the training-session award. Kept here
// (class-table neighbours) so the leveling layer can read them through the
// accessors below.
// ---------------------------------------------------------------------------

/// `con_app[]` (constants.c) — {hitp_bonus} indexed by constitution 0..=25.
/// (Only the `hitp` column is used by advance_level.)
pub static CON_APP_HITP: [i32; 26] = [
    -4, // 0
    -3, // 1
    -2, -2, -1, -1, // 5
    -1, 0, 0, 0, 0, // 10
    0, 0, 0, 0, 1, // 15
    2, 2, 3, // 18
    3, 4, // 20
    5, 5, 5, 6, 6, // 25
];

/// `wis_app[]` (constants.c) — {bonus} (practice-gain modifier) by wisdom 0..=25.
pub static WIS_APP_BONUS: [i32; 26] = [
    0, 0, 0, 0, 0, 0, // 5
    0, 0, 0, 0, 0, // 10
    0, 2, 2, 3, 3, // 15
    3, 4, 5, // 18
    6, 6, // 20
    6, 6, 7, 7, 7, // 25
];

/// `training_pts[LVL_IMMORT]` (config.c) — training sessions awarded *at* each
/// level (1 at every multiple of 10 from 10..90, else 0). Index 0..100.
pub static TRAINING_PTS: [i32; LVL_IMMORT as usize] = build_training_pts();

const fn build_training_pts() -> [i32; LVL_IMMORT as usize] {
    let mut t = [0i32; LVL_IMMORT as usize];
    // 1 at 10,20,30,...,90 (config.c table). Levels 0..=9 and >=100 are 0.
    let mut lvl = 10;
    while lvl <= 90 {
        t[lvl] = 1;
        lvl += 10;
    }
    t
}

/// `con_app[con].hitp` — per-level hit bonus for a constitution score. Clamps to
/// the table bounds (constitutions outside 0..=25 take the nearest edge value).
pub fn con_app_hitp(con: i32) -> i32 {
    let c = con.clamp(0, 25) as usize;
    CON_APP_HITP[c]
}

/// `wis_app[wis].bonus` — practice-gain modifier for a wisdom score. Clamped.
pub fn wis_app_bonus(wis: i32) -> i32 {
    let w = wis.clamp(0, 25) as usize;
    WIS_APP_BONUS[w]
}

/// `training_pts[level]` — training sessions awarded at `level`. 0 outside the
/// table (incl. immortal levels), matching the C bounds.
pub fn training_pts(level: i32) -> i32 {
    if (0..LVL_IMMORT_I).contains(&level) {
        TRAINING_PTS[level as usize]
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// backstab_mult (class.c) — backstab damage multiplier by attacker level.
// Used by fight.c's do_backstab; reproduced here so combat can call into the
// class layer. 1:1 with class.c's step ladder.
// ---------------------------------------------------------------------------
pub fn backstab_mult(level: i32) -> i32 {
    if level <= 0 {
        1
    } else if level <= 7 {
        2
    } else if level <= 13 {
        3
    } else if level <= 20 {
        4
    } else if level <= 28 {
        5
    } else if level <= 36 {
        6
    } else if level <= 44 {
        7
    } else if level <= 52 {
        8
    } else if level <= 60 {
        9
    } else if level <= 68 {
        10
    } else if level <= 76 {
        11
    } else if level <= 84 {
        12
    } else if level < LVL_IMMORT_I {
        13
    } else {
        20
    }
}

// ---------------------------------------------------------------------------
// invalid_class (class.c) — is a piece of equipment forbidden to this class by
// the ITEM_ANTI_{class} extra flags? handler.c uses this to gate wear/use.
// ---------------------------------------------------------------------------

// ITEM_ANTI_* extra flags (structs.h). Bit positions match the C #defines.
const ITEM_ANTI_MAGIC_USER: i64 = 1 << 4;
const ITEM_ANTI_CLERIC: i64 = 1 << 5;
const ITEM_ANTI_THIEF: i64 = 1 << 6;
const ITEM_ANTI_WARRIOR: i64 = 1 << 7;
const ITEM_ANTI_ARTISAN: i64 = 1 << 18;

/// `invalid_class(class, obj_extra_flags)` — `true` if an object with the given
/// extra-flag bitvector is anti-`class` (the wearer can't use it). Mirrors
/// class.c `invalid_class(ch, obj)`; the caller passes the object's extra-flag
/// mask so this stays a pure helper.
pub fn invalid_class(class: Class, obj_extra_flags: i64) -> bool {
    let has = |bit: i64| obj_extra_flags & bit != 0;
    (has(ITEM_ANTI_MAGIC_USER) && class == Class::MagicUser)
        || (has(ITEM_ANTI_CLERIC) && class == Class::Cleric)
        || (has(ITEM_ANTI_WARRIOR) && class == Class::Warrior)
        || (has(ITEM_ANTI_THIEF) && class == Class::Thief)
        || (has(ITEM_ANTI_ARTISAN) && class == Class::Artisan)
}

// ---------------------------------------------------------------------------
// roll_real_abils (class.c) — class-weighted ability rolling. Each class adds a
// number(2,3) bonus to its two primary stats, then each stat is rolled in its
// racial [min,max] window (capped at the racial max), with the 18-strength
// percentile add. Writes real_abils and copies to aff_abils.
// ---------------------------------------------------------------------------

/// `roll_real_abils(g, ch)` — re-roll a PC's six ability scores per class.c.
/// Uses `g.rng` where C uses `number()`, and the racial bounds from `races.rs`.
/// The race index is the structs.h `GET_RACE` integer (player.race as i32);
/// races beyond the stat_table degrade to a 0 window via get_race_min/max,
/// exactly as the C bounds guard would.
pub fn roll_real_abils(g: &mut GameState, ch: CharId) {
    use crate::races::{get_race_max, get_race_min};

    let (class, race) = match g.get_char(ch) {
        Some(c) => (c.player.class, c.player.race as u8 as i32),
        None => return,
    };

    // Class-weighted +number(2,3) on the two primary stats.
    let (mut sa, mut ia, mut wa, mut da, mut ca) = (0i32, 0i32, 0i32, 0i32, 0i32);
    match class {
        Class::MagicUser => {
            ia = g.rng.number(2, 3);
            wa = g.rng.number(2, 3);
        }
        Class::Cleric => {
            wa = g.rng.number(2, 3);
            ca = g.rng.number(2, 3);
        }
        Class::Thief => {
            da = g.rng.number(2, 3);
            ia = g.rng.number(2, 3);
        }
        Class::Warrior => {
            sa = g.rng.number(2, 3);
            ca = g.rng.number(2, 3);
        }
        Class::Artisan => {
            sa = g.rng.number(2, 3);
            ia = g.rng.number(2, 3);
        }
    }

    // Roll each stat in its racial window, then add the class bonus capped at
    // the racial max. (stat indices 1..=6 = Str,Int,Wis,Dex,Con,Cha.)
    let roll = |g: &mut GameState, stat: i32, bonus: i32| -> i32 {
        let lo = get_race_min(race, stat);
        let hi = get_race_max(race, stat);
        (g.rng.number(lo, hi) + bonus).min(hi)
    };

    let str_v = roll(g, 1, sa);
    let int_v = roll(g, 2, ia);
    let wis_v = roll(g, 3, wa);
    let dex_v = roll(g, 4, da);
    let con_v = roll(g, 5, ca);
    let cha_v = roll(g, 6, 0); // charisma gets no class bonus in class.c

    // 18-strength percentile add.
    let str_add = if str_v == 18 { g.rng.number(0, 100) } else { 0 };

    if let Some(c) = g.get_char_mut(ch) {
        c.real_abils.str_add = str_add as i8;
        c.real_abils.str = str_v as i8;
        c.real_abils.intel = int_v as i8;
        c.real_abils.wis = wis_v as i8;
        c.real_abils.dex = dex_v as i8;
        c.real_abils.con = con_v as i8;
        c.real_abils.cha = cha_v as i8;
        c.aff_abils = c.real_abils;
    }
}

// ---------------------------------------------------------------------------
// do_start (class.c) — initialise a brand-new level-1 character: set level /
// trust / exp, default title, base 10 max_hit, per-class starting skills, then
// run the first advance_level (which awards the level-1 HMV/practice gains),
// fill HMV to max, set hunger/thirst, and stamp playtime.
// ---------------------------------------------------------------------------

/// `do_start_init(g, ch)` — the C `do_start(ch)`. Drives the brand-new-PC setup,
/// delegating the per-level HMV/practice work to `crate::limits::advance_level`
/// (the single source of that logic). Idempotent in the sense that it always
/// resets to a fresh level-1 state. No-op on a missing/NPC id.
pub fn do_start_init(g: &mut GameState, ch: CharId) {
    // Bail on missing / NPC (C do_start is only ever called for PCs).
    match g.get_char(ch) {
        Some(c) if !c.is_npc => {}
        _ => return,
    }

    let class = match g.get_char(ch) {
        Some(c) => c.player.class,
        None => return,
    };

    // Level / trust / exp / base hp / default title.
    // set_title(ch, NULL) → "the Man"/"the Woman" by sex; limits::set_title with
    // None reproduces that exactly.
    if let Some(c) = g.get_char_mut(ch) {
        c.player.level = 1;
        c.trust = 1;
        c.points.exp = 1;
        // roll_real_abils is intentionally NOT re-rolled here (class.c comments
        // it out: "not needed anymore" — stats are rolled at creation time).
        c.points.max_hit = 10;
    }
    crate::limits::set_title(g, ch, None);

    // Per-class starting skills (class.c do_start switch).
    match class {
        Class::MagicUser | Class::Cleric | Class::Artisan => {
            // No starting skills.
        }
        Class::Thief => {
            if let Some(c) = g.get_char_mut(ch) {
                c.set_skill(SKILL_SNEAK, 10);
                c.set_skill(SKILL_HIDE, 5);
                c.set_skill(SKILL_STEAL, 15);
                c.set_skill(SKILL_BACKSTAB, 10);
                c.set_skill(SKILL_PICK_LOCK, 10);
                c.set_skill(SKILL_TRACK, 10);
                c.set_skill(SKILL_SCAN, 1);
            }
        }
        Class::Warrior => {
            if let Some(c) = g.get_char_mut(ch) {
                c.set_skill(SKILL_SCAN, 1);
            }
        }
    }

    // First level's gains (HMV / practices / training / immortal side-effects).
    crate::limits::advance_level(g, ch);

    // Fill HMV to max and set the hunger/thirst/drunk clocks.
    if let Some(c) = g.get_char_mut(ch) {
        c.points.hit = c.points.max_hit;
        c.points.mana = c.points.max_mana;
        c.points.move_points = c.points.max_move;

        c.conditions[THIRST] = 24;
        c.conditions[FULL] = 24;
        c.conditions[DRUNK] = 0;

        c.player.time_played = 0;
    }

    // ch->player.time.logon = time(0) — refresh last-logon timestamp.
    if let Some(c) = g.get_char_mut(ch) {
        c.last_logon = chrono::Utc::now();
    }
}
