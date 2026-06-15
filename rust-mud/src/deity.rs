// deity.rs — full port of C `src/deity.c`.
//
// DeltaMUD has fifteen worshippable deities, indexed 0..=14 in this exact order
// (structs.h DEITY_*):
//   Aetos, Corgus, Lythern, Pallas, Eros, Cailia, Marbin, Poseidon, Lelu,
//   Chaos, Elestra, Nemonica, Durn, Ranus, Incubus
// laid out in the selection menu in three alignment columns (Good / Neutral /
// Evil). The stored value is `player.deity` (GET_DEITY), the integer
// parse_deity returns; these tables are keyed by that integer.
//
// Self-contained module: pure tables + parsers, no GameState dependency. The C
// source has no deity command handler in this file, no per-deity affect tables,
// and no deity-selection logic beyond parse_deity / the menu — deity selection
// proper lives in interpreter.c (CON_QDEITY), which consumes parse_deity and
// deity_menu from here. So this module exposes exactly what deity.c defines.

// ---------------------------------------------------------------------------
// structs.h DEITY_* constants (the indices these tables are keyed by).
// ---------------------------------------------------------------------------
pub const DEITY_UNDEFINED: i32 = -1;
pub const DEITY_AETOS: i32 = 0;
pub const DEITY_CORGUS: i32 = 1;
pub const DEITY_LYTHERN: i32 = 2;
pub const DEITY_PALLAS: i32 = 3;
pub const DEITY_EROS: i32 = 4;
pub const DEITY_CAILIA: i32 = 5;
pub const DEITY_MARBIN: i32 = 6;
pub const DEITY_POSEIDON: i32 = 7;
pub const DEITY_LELU: i32 = 8;
pub const DEITY_CHAOS: i32 = 9;
pub const DEITY_ELESTRA: i32 = 10;
pub const DEITY_NEMONICA: i32 = 11;
pub const DEITY_DURN: i32 = 12;
pub const DEITY_RANUS: i32 = 13;
pub const DEITY_INCUBUS: i32 = 14;

/// Number of selectable deities (0..=14).
pub const NUM_DEITIES: usize = 15;

/// `deity_abbrevs[]` — three-letter deity tags (DEITY_ABBR macro). Terminated
/// in C with a `"\n"` sentinel, preserved here verbatim.
pub static DEITY_ABBREVS: &[&str] = &[
    "Aet", // DEITY_AETOS
    "Cor", // DEITY_CORGUS
    "Lyt", // DEITY_LYTHERN
    "Pal", // DEITY_PALLAS
    "Ero", // DEITY_EROS
    "Cai", // DEITY_CAILIA
    "Mar", // DEITY_MARBIN
    "Pos", // DEITY_POSEIDON
    "Lel", // DEITY_LELU
    "Cha", // DEITY_CHAOS
    "Ele", // DEITY_ELESTRA
    "Nem", // DEITY_NEMONICA
    "Dur", // DEITY_DURN
    "Ran", // DEITY_RANUS
    "Inc", // DEITY_INCUBUS
    "\n",
];

/// `pc_deity_types[]` (a.k.a. `deity_types`) — full deity names. Terminated in
/// C with a `"\n"` sentinel, preserved verbatim.
pub static PC_DEITY_TYPES: &[&str] = &[
    "Aetos",    // DEITY_AETOS
    "Corgus",   // DEITY_CORGUS
    "Lythern",  // DEITY_LYTHERN
    "Pallas",   // DEITY_PALLAS
    "Eros",     // DEITY_EROS
    "Cailia",   // DEITY_CAILIA
    "Marbin",   // DEITY_MARBIN
    "Poseidon", // DEITY_POSEIDON
    "Lelu",     // DEITY_LELU
    "Chaos",    // DEITY_CHAOS
    "Elestra",  // DEITY_ELESTRA
    "Nemonica", // DEITY_NEMONICA
    "Durn",     // DEITY_DURN
    "Ranus",    // DEITY_RANUS
    "Incubus",  // DEITY_INCUBUS
    "\n",
];

/// `deity_types` — the C source aliases this name to `pc_deity_types`.
pub static DEITY_TYPES: &[&str] = PC_DEITY_TYPES;

/// `deity_menu` — the deity-selection menu shown in interpreter.c (CON_QDEITY).
/// Color is emitted as literal `&`-codes (stripped per player on output); this
/// reproduces the C string byte-for-byte, including the three-column layout.
pub static DEITY_MENU: &str = "\r\n\
&YSelect a deity to worship&n&R:&n\r\n\
\r\n\
    Good                Neutral               Evil&n\r\n\
    &g-&y-&g-&y-                &g-&y-&g-&y-&g-&y-&g-               &g-&y-&g-&y-&n\r\n\
  &c[&na&c]&n Aetos           &c[&nf&c]&n Cailia           &c[&nk&c]&n Elestra\r\n\
  &c[&nb&c]&n Corgus          &c[&ng&c]&n Marbin           &c[&nl&c]&n Nemonica\r\n\
  &c[&nc&c]&n Lythern         &c[&nh&c]&n Poseidon         &c[&nm&c]&n Durn\r\n\
  &c[&nd&c]&n Pallas          &c[&ni&c]&n Lelu             &c[&nn&c]&n Ranus\r\n\
  &c[&ne&c]&n Eros            &c[&nj&c]&n Chaos            &c[&no&c]&n Incubus\r\n";

/// `deity_abbrev(deity)` — DEITY_ABBR for a PC, or "--" when `is_npc`.
/// Bounds-guards the deity index (C indexes blindly; we degrade to "--").
pub fn deity_abbrev(deity: i32, is_npc: bool) -> &'static str {
    if is_npc {
        return "--";
    }
    if (0..NUM_DEITIES as i32).contains(&deity) {
        DEITY_ABBREVS[deity as usize]
    } else {
        "--"
    }
}

/// `deity_name(deity)` — full deity name from `pc_deity_types`, or "Undefined"
/// when out of range.
pub fn deity_name(deity: i32) -> &'static str {
    if (0..NUM_DEITIES as i32).contains(&deity) {
        PC_DEITY_TYPES[deity as usize]
    } else {
        "Undefined"
    }
}

/// `parse_deity(arg)` — interpret a deity-menu letter (a..o) during character
/// creation / deity selection. Returns DEITY_UNDEFINED (-1) for anything else.
/// 1:1 with deity.c.
pub fn parse_deity(arg: char) -> i32 {
    match arg.to_ascii_lowercase() {
        'a' => DEITY_AETOS,
        'b' => DEITY_CORGUS,
        'c' => DEITY_LYTHERN,
        'd' => DEITY_PALLAS,
        'e' => DEITY_EROS,
        'f' => DEITY_CAILIA,
        'g' => DEITY_MARBIN,
        'h' => DEITY_POSEIDON,
        'i' => DEITY_LELU,
        'j' => DEITY_CHAOS,
        'k' => DEITY_ELESTRA,
        'l' => DEITY_NEMONICA,
        'm' => DEITY_DURN,
        'n' => DEITY_RANUS,
        'o' => DEITY_INCUBUS,
        _ => DEITY_UNDEFINED,
    }
}

/// `find_deity_bitvector(arg)` — single-bit deity mask for the menu letter
/// a..o. Returns 0 for anything else. 1:1 with deity.c.
pub fn find_deity_bitvector(arg: char) -> i64 {
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
        'j' => 1 << 9,
        'k' => 1 << 10,
        'l' => 1 << 11,
        'm' => 1 << 12,
        'n' => 1 << 13,
        'o' => 1 << 14,
        _ => 0,
    }
}
