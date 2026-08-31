// races.rs — full port of C `src/races.c` (plus the racial stat min/max table
// `stat_table[9][12]` that lives in `class.c` and is read through
// `GET_RACE_MIN`/`GET_RACE_MAX` in `utils.c`).
//
// The C MUD has nine player races, indexed 0..=8 in this exact order:
//   Human, Elf, Gnome, Dwarf, Troll, Goblin, Drow, Orc, Minotaur
// (structs.h RACE_*). NOTE: the runtime `types::Race` enum carries a *different*
// roster/order (Human, Elf, Gnome, Dwarf, Troll, Orc, HalfElf, Kender,
// Minotaur, Vampire, Ogre, HalfOrc). These race tables are keyed by the
// **C structs.h race index** (stored in player.race / GET_RACE) — the integer
// that parse_race returns and that the menu/abbrev tables describe — not by the
// `types::Race` enum ordinal. Keeping these as plain integer tables preserves
// the on-disk/menu contract exactly the way the C source does.
//
// Self-contained module: pure tables + parsers, no GameState dependency.

// ---------------------------------------------------------------------------
// structs.h RACE_* constants (the indices these tables are keyed by).
// ---------------------------------------------------------------------------
pub const RACE_UNDEFINED: i32 = -1;
pub const RACE_HUMAN: i32 = 0;
pub const RACE_ELF: i32 = 1;
pub const RACE_GNOME: i32 = 2;
pub const RACE_DWARF: i32 = 3;
pub const RACE_TROLL: i32 = 4;
pub const RACE_GOBLIN: i32 = 5;
pub const RACE_DROW: i32 = 6;
pub const RACE_ORC: i32 = 7;
pub const RACE_MINOTAUR: i32 = 8;

/// Number of selectable player races (0..=8).
pub const NUM_RACES: usize = 9;

/// `race_abbrevs[]` — three-letter race tags (RACE_ABBR macro). Terminated in
/// C with a `"\n"` sentinel, preserved here verbatim.
pub static RACE_ABBREVS: &[&str] = &[
    "Hum", // RACE_HUMAN
    "Elf", // RACE_ELF
    "Gno", // RACE_GNOME
    "Dwa", // RACE_DWARF
    "Tro", // RACE_TROLL
    "Gob", // RACE_GOBLIN
    "Dro", // RACE_DROW
    "Orc", // RACE_ORC
    "Min", // RACE_MINOTAUR
    "\n",
];

/// `pc_race_types[]` (a.k.a. `race_types`) — full race names. Terminated in C
/// with a `"\n"` sentinel, preserved verbatim.
pub static PC_RACE_TYPES: &[&str] = &[
    "Human",    // RACE_HUMAN
    "Elf",      // RACE_ELF
    "Gnome",    // RACE_GNOME
    "Dwarf",    // RACE_DWARF
    "Troll",    // RACE_TROLL
    "Goblin",   // RACE_GOBLIN
    "Drow",     // RACE_DROW
    "Orc",      // RACE_ORC
    "Minotaur", // RACE_MINOTAUR
    "\n",
];

/// `race_types` — the C source aliases this name to `pc_race_types`.
pub static RACE_TYPES: &[&str] = PC_RACE_TYPES;

/// `race_menu` — the race-selection menu shown in interpreter.c (CON_QRACE).
/// Color is emitted as literal `&`-codes (the output path strips them per
/// player), exactly matching the C string including the trailing `\r\n`.
/// Built with `concat!` so the two-space row indent of the C source survives
/// (`\` continuations strip leading whitespace) (#253).
pub static RACE_MENU: &str = concat!(
    "\r\n",
    "&YSelect a race&n&R:&n\r\n",
    "  &c[&na&c]&n Human    - average creatures of diplomacy, ethics, and adventure\r\n",
    "  &c[&nb&c]&n Elf      - smart, dexterious creatures, lacking strength & endurance\r\n",
    "  &c[&nc&c]&n Gnome    - highly intelligent creatures who remain without wisdom\r\n",
    "  &c[&nd&c]&n Dwarf    - healthy creatures who tend to remain isolated from others\r\n",
    "  &c[&ne&c]&n Troll    - large masters of combat who lack intelligence and speed\r\n",
    "  &c[&nf&c]&n Goblin   - grotesque, evil gnomes, well-known for their trickery\r\n",
    "  &c[&ng&c]&n Drow     - strange, innovative people originating from another land\r\n",
    "  &c[&nh&c]&n Orc      - rabbid wild creatures who take whatever they want\r\n",
    "  &c[&ni&c]&n Minotaur - half human, half bull monsters who thrive on flesh\r\n",
);

/// `stat_table[9][12]` (defined in class.c) — racial {min,max} ability bounds.
/// Twelve columns are six {min,max} pairs in stat order: Str, Int, Wis, Dex,
/// Con, Cha. Used by `roll_real_abils` via GET_RACE_MIN/GET_RACE_MAX.
pub static STAT_TABLE: [[i32; 12]; NUM_RACES] = [
    /*            Str      Int      Wis      Dex      Con      Cha   */
    /* Human    */
    [6, 16, 6, 16, 6, 16, 6, 16, 6, 16, 6, 16],
    /* Elf      */ [4, 15, 8, 17, 3, 16, 7, 19, 6, 14, 8, 15],
    /* Gnome    */ [6, 16, 7, 15, 5, 17, 5, 16, 8, 17, 5, 15],
    /* Dwarf    */ [9, 18, 4, 14, 4, 16, 4, 15, 12, 18, 3, 15],
    /* Troll    */ [12, 19, 4, 15, 5, 15, 5, 16, 6, 18, 3, 15],
    /* Goblin   */ [7, 17, 7, 16, 5, 15, 6, 18, 8, 17, 3, 13],
    /* Drow     */ [4, 15, 8, 17, 3, 16, 7, 19, 6, 14, 8, 15],
    /* Orc      */ [7, 18, 5, 17, 3, 15, 4, 15, 13, 19, 4, 12],
    /* Minotaur */ [8, 18, 5, 15, 6, 15, 6, 17, 8, 19, 3, 12],
];

/// `GET_RACE_MIN(race, stat)` (utils.c). `stat` is 1..=6 (Str..Cha). Returns 0
/// for out-of-range race/stat, exactly like the C guard `race>=0 && race<=8 &&
/// stat>=1 && stat<=6`.
pub fn get_race_min(race: i32, stat: i32) -> i32 {
    if (0..=8).contains(&race) && (1..=6).contains(&stat) {
        STAT_TABLE[race as usize][(stat * 2 - 2) as usize]
    } else {
        0
    }
}

/// `GET_RACE_MAX(race, stat)` (utils.c). See `get_race_min`.
pub fn get_race_max(race: i32, stat: i32) -> i32 {
    if (0..=8).contains(&race) && (1..=6).contains(&stat) {
        STAT_TABLE[race as usize][(stat * 2 - 1) as usize]
    } else {
        0
    }
}

/// `race_abbrev(race)` — RACE_ABBR for a PC, or "--" when `is_npc`.
/// Bounds-guards the race index (C indexes blindly; we degrade to "--").
pub fn race_abbrev(race: i32, is_npc: bool) -> &'static str {
    if is_npc {
        return "--";
    }
    if (0..NUM_RACES as i32).contains(&race) {
        RACE_ABBREVS[race as usize]
    } else {
        "--"
    }
}

/// `race_name(race)` — full race name from `pc_race_types`, or "Undefined" when
/// out of range.
pub fn race_name(race: i32) -> &'static str {
    if (0..NUM_RACES as i32).contains(&race) {
        PC_RACE_TYPES[race as usize]
    } else {
        "Undefined"
    }
}

/// `parse_race(arg)` — interpret a race-menu letter (a..i) during character
/// creation. Returns RACE_UNDEFINED (-1) for anything else. 1:1 with races.c.
pub fn parse_race(arg: char) -> i32 {
    match arg.to_ascii_lowercase() {
        'a' => RACE_HUMAN,
        'b' => RACE_ELF,
        'c' => RACE_GNOME,
        'd' => RACE_DWARF,
        'e' => RACE_TROLL,
        'f' => RACE_GOBLIN,
        'g' => RACE_DROW,
        'h' => RACE_ORC,
        'i' => RACE_MINOTAUR,
        _ => RACE_UNDEFINED,
    }
}

/// `find_race_bitvector(arg)` — single-bit race mask for the menu letter a..i.
/// Returns 0 for anything else. 1:1 with races.c.
pub fn find_race_bitvector(arg: char) -> i64 {
    match arg.to_ascii_lowercase() {
        'a' => 1 << 0,
        'b' => 1 << 1,
        'c' => 1 << 2,
        'd' => 1 << 3,
        'e' => 1 << 4,
        'f' => 1 << 5,
        'g' => 1 << 6,
        'h' => 1 << 7,
        'i' => 1 << 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #335: race_abbrevs[] is C's nine rows, so Goblin -> "Gob" and
    /// Drow -> "Dro" (not the neighbouring "Orc"/"Min").
    #[test]
    fn race_abbrevs_match_races_c() {
        assert_eq!(
            &RACE_ABBREVS[..NUM_RACES],
            &["Hum", "Elf", "Gno", "Dwa", "Tro", "Gob", "Dro", "Orc", "Min"]
        );
        assert_eq!(race_abbrev(RACE_GOBLIN, false), "Gob");
        assert_eq!(race_abbrev(RACE_DROW, false), "Dro");
    }

    /// #253: the race menu keeps its two-space row indent.
    #[test]
    fn race_menu_keeps_its_two_space_row_indent() {
        for line in RACE_MENU.split("\r\n").filter(|l| l.contains("&c[&n")) {
            assert!(line.starts_with("  "), "row lost its indent: {:?}", line);
        }
        assert!(RACE_MENU.contains("\r\n  &c[&nf&c]&n Goblin"));
    }
}
