// Bit-flag constants (structs.h). Kept as plain i64 masks to match the DB
// long columns; helper predicates live on Character / GameState. Tier-0
// defines the subset in active use; later batches extend in place.

// AFF_* — char->char_specials affected_by / affect bitvector.
pub const AFF_BLIND: i64 = 1 << 0;
pub const AFF_INVISIBLE: i64 = 1 << 1;
pub const AFF_DETECT_ALIGN: i64 = 1 << 2;
pub const AFF_DETECT_INVIS: i64 = 1 << 3;
pub const AFF_DETECT_MAGIC: i64 = 1 << 4;
pub const AFF_SENSE_LIFE: i64 = 1 << 5;
pub const AFF_SANCTUARY: i64 = 1 << 7;
pub const AFF_GROUP: i64 = 1 << 8;
pub const AFF_CURSE: i64 = 1 << 9;
pub const AFF_INFRAVISION: i64 = 1 << 10;
pub const AFF_POISON: i64 = 1 << 11;
pub const AFF_SLEEP: i64 = 1 << 14;
pub const AFF_SNEAK: i64 = 1 << 18;
pub const AFF_HIDE: i64 = 1 << 19;
pub const AFF_CHARM: i64 = 1 << 21;

// PRF_* — player preferences.
pub const PRF_BRIEF: i64 = 1 << 0;
pub const PRF_COMPACT: i64 = 1 << 1;
pub const PRF_NOHASSLE: i64 = 1 << 8;
pub const PRF_NOREPEAT: i64 = 1 << 11;
pub const PRF_HOLYLIGHT: i64 = 1 << 12;
pub const PRF_COLOR_1: i64 = 1 << 13;
pub const PRF_COLOR_2: i64 = 1 << 14;

// PLR_* — player flags (subset).
pub const PLR_FROZEN: i64 = 1 << 1;
pub const PLR_INVSTART: i64 = 1 << 4;

// MOB act flags (subset).
pub const MOB_SPEC: i64 = 1 << 0;
pub const MOB_SENTINEL: i64 = 1 << 1;
pub const MOB_SCAVENGER: i64 = 1 << 2;
pub const MOB_ISNPC: i64 = 1 << 3;
pub const MOB_AGGRESSIVE: i64 = 1 << 5;
pub const MOB_WIMPY: i64 = 1 << 8;

// APPLY_* — affect location codes (DeltaMUD set; structs.h).
pub const APPLY_NONE: i32 = 0;
pub const APPLY_STR: i32 = 1;
pub const APPLY_DEX: i32 = 2;
pub const APPLY_INT: i32 = 3;
pub const APPLY_WIS: i32 = 4;
pub const APPLY_CON: i32 = 5;
pub const APPLY_AGE: i32 = 9;
pub const APPLY_CHAR_WEIGHT: i32 = 10;
pub const APPLY_CHAR_HEIGHT: i32 = 11;
pub const APPLY_MANA: i32 = 12;
pub const APPLY_HIT: i32 = 13;
pub const APPLY_MOVE: i32 = 14;
pub const APPLY_DEFENSE: i32 = 17;
pub const APPLY_MDEFENSE: i32 = 18;
pub const APPLY_POWER: i32 = 19;
pub const APPLY_MPOWER: i32 = 20;
pub const APPLY_TECHNIQUE: i32 = 21;
