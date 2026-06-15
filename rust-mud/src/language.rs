// language.rs — full port of C `src/language.c` (the tongues/languages system
// and the drunk-speech garbler `makedrunk`).
//
// In DeltaMUD a PC has MAX_TONGUE (=3) language slots, `ch->talks[]` (a
// `[bool; MAX_TONGUE]` on `Character::talks`). The C source file `language.c`
// itself contains exactly one piece of logic — `makedrunk` — which is the
// drunk-speech filter applied to everything a drunk PC says (used throughout
// act.comm.c: do_say/gossip/tell/shout/auction/etc.). The tongues plumbing
// (the `talks[]` array + MAX_TONGUE) is the data side of the same system; we
// expose small helpers over it here so callers don't reach into the array by
// hand, and a `skill_of_tongue` helper for the "how well do you speak this"
// query. There is no language *command handler* in language.c (no do_languages
// / do_speak), so this module exposes no dispatch arms.
//
// `makedrunk` calls C `number()`; to stay byte-parity with the C RNG we thread
// the shared generator through as `&mut Rng` (the caller passes `g.rng`).
// `makedrunk` is otherwise a pure text transform.

use crate::rng::Rng;
use crate::types::MAX_TONGUE;

// ---------------------------------------------------------------------------
// Tongue slot constants. The three talks[] slots; index order is the on-disk
// contract (player_main talks1/talks2/talks3, *DO*NOT*CHANGE*).
// ---------------------------------------------------------------------------
pub const TONGUE_COMMON: usize = 0;
pub const TONGUE_2: usize = 1;
pub const TONGUE_3: usize = 2;

/// `knows_tongue(talks, tongue)` — can this PC speak language slot `tongue`?
/// Out-of-range slots read as "no", matching a NPC's all-false `talks[]`.
pub fn knows_tongue(talks: &[bool; MAX_TONGUE], tongue: usize) -> bool {
    tongue < MAX_TONGUE && talks[tongue]
}

/// `skill_of_tongue(talks, tongue)` — proficiency in a tongue. The C tongue
/// system is binary (talks[] is a bool array), so this returns 100 if the
/// speaker knows the tongue and 0 otherwise — the natural numeric reading of
/// the boolean slot for callers that want a percentage.
pub fn skill_of_tongue(talks: &[bool; MAX_TONGUE], tongue: usize) -> i32 {
    if knows_tongue(talks, tongue) {
        100
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// makedrunk — the drunk-speech garbler (language.c). For each letter, if the
// speaker is drunk enough (DRUNK condition > that letter's min_drunk_level),
// the letter is swapped for a random slur from its replacement table; digits
// become random digits; everything else is copied verbatim. Below the
// threshold the original character (with its original case) is kept.
// ---------------------------------------------------------------------------

/// One row of the C `drunk[]` table: the per-letter slur rules.
struct DrunkRule {
    min_drunk_level: i32,
    number_of_rep: i32,
    replacement: &'static [&'static str],
}

/// `drunk[]` (language.c) — one row per letter A..Z (26 rows). `number_of_rep`
/// is the inclusive upper bound passed to `number(0, n)`, so each `replacement`
/// slice has exactly `number_of_rep + 1` entries (matching the C initializers).
static DRUNK: [DrunkRule; 26] = [
    DrunkRule { min_drunk_level: 3, number_of_rep: 10, replacement: &["a", "a", "a", "A", "aa", "ah", "Ah", "ao", "aw", "oa", "ahhhh"] }, // A
    DrunkRule { min_drunk_level: 8, number_of_rep: 5, replacement: &["b", "b", "b", "B", "B", "vb"] },                                     // B
    DrunkRule { min_drunk_level: 3, number_of_rep: 5, replacement: &["c", "c", "C", "cj", "sj", "zj"] },                                   // C
    DrunkRule { min_drunk_level: 5, number_of_rep: 2, replacement: &["d", "d", "D"] },                                                     // D
    DrunkRule { min_drunk_level: 3, number_of_rep: 3, replacement: &["e", "e", "eh", "E"] },                                              // E
    DrunkRule { min_drunk_level: 4, number_of_rep: 5, replacement: &["f", "f", "ff", "fff", "fFf", "F"] },                                 // F
    DrunkRule { min_drunk_level: 8, number_of_rep: 2, replacement: &["g", "g", "G"] },                                                     // G
    DrunkRule { min_drunk_level: 9, number_of_rep: 6, replacement: &["h", "h", "hh", "hhh", "Hhh", "HhH", "H"] },                          // H
    DrunkRule { min_drunk_level: 7, number_of_rep: 6, replacement: &["i", "i", "Iii", "ii", "iI", "Ii", "I"] },                           // I
    DrunkRule { min_drunk_level: 9, number_of_rep: 5, replacement: &["j", "j", "jj", "Jj", "jJ", "J"] },                                   // J
    DrunkRule { min_drunk_level: 7, number_of_rep: 2, replacement: &["k", "k", "K"] },                                                     // K
    DrunkRule { min_drunk_level: 3, number_of_rep: 2, replacement: &["l", "l", "L"] },                                                     // L
    DrunkRule { min_drunk_level: 5, number_of_rep: 8, replacement: &["m", "m", "mm", "mmm", "mmmm", "mmmmm", "MmM", "mM", "M"] },          // M
    DrunkRule { min_drunk_level: 6, number_of_rep: 6, replacement: &["n", "n", "nn", "Nn", "nnn", "nNn", "N"] },                          // N
    DrunkRule { min_drunk_level: 3, number_of_rep: 6, replacement: &["o", "o", "ooo", "ao", "aOoo", "Ooo", "ooOo"] },                      // O
    DrunkRule { min_drunk_level: 3, number_of_rep: 2, replacement: &["p", "p", "P"] },                                                     // P
    DrunkRule { min_drunk_level: 5, number_of_rep: 5, replacement: &["q", "q", "Q", "ku", "ququ", "kukeleku"] },                           // Q
    DrunkRule { min_drunk_level: 4, number_of_rep: 2, replacement: &["r", "r", "R"] },                                                     // R
    DrunkRule { min_drunk_level: 2, number_of_rep: 5, replacement: &["s", "ss", "zzZzssZ", "ZSssS", "sSzzsss", "sSss"] },                  // S
    DrunkRule { min_drunk_level: 5, number_of_rep: 2, replacement: &["t", "t", "T"] },                                                     // T
    DrunkRule { min_drunk_level: 3, number_of_rep: 6, replacement: &["u", "u", "uh", "Uh", "Uhuhhuh", "uhU", "uhhu"] },                    // U
    DrunkRule { min_drunk_level: 4, number_of_rep: 2, replacement: &["v", "v", "V"] },                                                     // V
    DrunkRule { min_drunk_level: 4, number_of_rep: 2, replacement: &["w", "w", "W"] },                                                     // W
    DrunkRule { min_drunk_level: 5, number_of_rep: 6, replacement: &["x", "x", "X", "ks", "iks", "kz", "xz"] },                            // X
    DrunkRule { min_drunk_level: 3, number_of_rep: 2, replacement: &["y", "y", "Y"] },                                                     // Y
    DrunkRule { min_drunk_level: 2, number_of_rep: 9, replacement: &["z", "z", "ZzzZz", "Zzz", "Zsszzsz", "szz", "sZZz", "ZSz", "zZ", "Z"] }, // Z
];

/// `makedrunk(string, ch)` — garble `text` for a speaker whose DRUNK condition
/// is `drunk_level`. With `drunk_level <= 0` the text is returned unchanged
/// (the common, sober path), exactly as the C function early-returns.
///
/// `rng` is the shared game RNG (`g.rng`); it is advanced exactly as the C code
/// advances the global generator (`number(0, n)` per replaced letter/digit),
/// preserving dual-MUD RNG parity.
pub fn makedrunk(text: &str, drunk_level: i32, rng: &mut Rng) -> String {
    // Sober (or hangover with non-positive counter): no change.
    if drunk_level <= 0 {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 16);

    // C iterates byte-by-byte over the C string. We mirror that over the
    // input's bytes so multi-byte sequences and control chars copy verbatim,
    // matching the C `else buf[pos++] = *string;` fall-through exactly.
    for &b in text.as_bytes() {
        let c = b as char;
        let upper = c.to_ascii_uppercase();
        if upper.is_ascii_uppercase() {
            // 'A'..='Z' after upcasing.
            let idx = (upper as u8 - b'A') as usize;
            let rule = &DRUNK[idx];
            if drunk_level > rule.min_drunk_level {
                // number(0, number_of_rep): inclusive => 0..=number_of_rep.
                let r = rng.number(0, rule.number_of_rep) as usize;
                out.push_str(rule.replacement[r]);
            } else {
                // Below threshold: keep the ORIGINAL character (case intact).
                out.push(c);
            }
        } else if c.is_ascii_digit() {
            // Digits become a random digit '0'..'9'.
            let d = (b'0' + rng.number(0, 9) as u8) as char;
            out.push(d);
        } else {
            // Punctuation, spaces, control chars, high bytes: copy verbatim.
            out.push(c);
        }
    }

    out
}
