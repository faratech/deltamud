//! God-command (GCMD) granular immortal-permission bits.
//!
//! A 1:1 port of `src/gcmd.h`. C gates each immortal command not just by level
//! but by a per-command god-permission bit. The `cmd_info[]` table tags a
//! command with a `GOD_CMD`/`GOD_CMD2`/`GOD_CMD3`/`GOD_CMD4` marker (selecting
//! which of the four bitvectors gates it) plus a `godcmd` bit; a command is
//! usable only if `(cmd.godcmd & GCMDn_FLAGS(ch))` is set in the matching
//! bitvector (`interpreter.c:786-789`).
//!
//! The four bitvectors live on the character as `godcmds1..4` (saved). These
//! constants are stored as `i64` to match the field type (the C field is a
//! `long`); the bit values themselves are the same 32-bit shifts as C.

// ---------------------------------------------------------------------------
// player.godcmds1  (GCMD_*)
// ---------------------------------------------------------------------------
pub const GCMD_GEN: i64 = 1 << 0; // general god commands
pub const GCMD_ADVANCE: i64 = 1 << 1;
pub const GCMD_AT: i64 = 1 << 2;
pub const GCMD_BAN: i64 = 1 << 3;
pub const GCMD_DC: i64 = 1 << 4;
pub const GCMD_ECHO: i64 = 1 << 5;
pub const GCMD_FORCE: i64 = 1 << 6;
pub const GCMD_FREEZE: i64 = 1 << 7;
pub const GCMD_HCONTROL: i64 = 1 << 8;
pub const GCMD_LOAD: i64 = 1 << 9;
pub const GCMD_MUTE: i64 = 1 << 10;
pub const GCMD_SYSLOG: i64 = 1 << 11;
pub const GCMD_PARDON: i64 = 1 << 12;
pub const GCMD_PURGE: i64 = 1 << 13;
pub const GCMD_RELOAD: i64 = 1 << 14;
pub const GCMD_REROLL: i64 = 1 << 15;
pub const GCMD_RESTORE: i64 = 1 << 16;
pub const GCMD_SEND: i64 = 1 << 17;
pub const GCMD_SET: i64 = 1 << 18;
pub const GCMD_SHUTDOWN: i64 = 1 << 19;
pub const GCMD_SKILLSET: i64 = 1 << 20;
pub const GCMD_AUCTIONEER: i64 = 1 << 21;
pub const GCMD_SLOWNS: i64 = 1 << 22;
pub const GCMD_SNOOP: i64 = 1 << 23;
pub const GCMD_SWITCH: i64 = 1 << 24;
pub const GCMD_PLAGUE: i64 = 1 << 25;
pub const GCMD_TRANS: i64 = 1 << 26;
pub const GCMD_UNAFFECT: i64 = 1 << 27;
pub const GCMD_WIZLOCK: i64 = 1 << 28;
pub const GCMD_ISAY: i64 = 1 << 29;
pub const GCMD_CMDSET: i64 = 1 << 31; // Don't change the pos. of this!

// ---------------------------------------------------------------------------
// player.godcmds2  (GCMD2_*)
// ---------------------------------------------------------------------------
pub const GCMD2_OLC: i64 = 1 << 0;
pub const GCMD2_INVIS: i64 = 1 << 1;
pub const GCMD2_MCASTERS: i64 = 1 << 2;
pub const GCMD2_MUDHEAL: i64 = 1 << 3;
pub const GCMD2_REWIZ: i64 = 1 << 4;
pub const GCMD2_GECHO: i64 = 1 << 5;
pub const GCMD2_REPOWER: i64 = 1 << 6;
pub const GCMD2_REWWW: i64 = 1 << 7;
pub const GCMD2_NOTITLE: i64 = 1 << 8;
pub const GCMD2_PAGE: i64 = 1 << 9;
pub const GCMD2_QECHO: i64 = 1 << 10;
pub const GCMD2_ZRESET: i64 = 1 << 11;
pub const GCMD2_SETREBOOT: i64 = 1 << 12;
pub const GCMD2_TMOBDIE: i64 = 1 << 13;
pub const GCMD2_WRESTRICT: i64 = 1 << 14;
pub const GCMD2_ATTACH: i64 = 1 << 15;
pub const GCMD2_IMP: i64 = 1 << 26; // Implementor commands
pub const GCMD2_USERS: i64 = 1 << 27;
pub const GCMD2_ALOAD: i64 = 1 << 28;
pub const GCMD2_RESPEC: i64 = 1 << 29;
pub const GCMD2_QUESTMOBS: i64 = 1 << 30;
pub const GCMD2_REWARD: i64 = 1 << 31;

// ---------------------------------------------------------------------------
// player.godcmds3  (GCMD3_*)
// ---------------------------------------------------------------------------
pub const GCMD3_PEACE: i64 = 1 << 0;
pub const GCMD3_IMPOLC: i64 = 1 << 1;
pub const GCMD3_ADDSNOW: i64 = 1 << 2;
pub const GCMD3_DELSNOW: i64 = 1 << 3;
pub const GCMD3_COPYOVER: i64 = 1 << 4;
pub const GCMD3_LWEATHER: i64 = 1 << 5;
pub const GCMD3_MAP: i64 = 1 << 6;
pub const GCMD3_PFILECLEAN: i64 = 1 << 7;
pub const GCMD3_REBALANCE: i64 = 1 << 8;

// ---------------------------------------------------------------------------
// player.godcmds4  (GCMD4_*)
// ---------------------------------------------------------------------------
pub const GCMD4_NOTHING: i64 = 1 << 0; // this does nothing

// ---------------------------------------------------------------------------
// Grant helpers (act.wizard.c do_advance:1738-1745)
// ---------------------------------------------------------------------------

/// Grant the appropriate god-command bits for a character who has just reached
/// `level`, mirroring C `do_advance` (act.wizard.c:1738-1745):
///
/// * crossing `LVL_IMMORT`: `SET_BIT(godcmds1, GCMD_GEN)`.
/// * crossing `LVL_IMPL`: `godcmds1 = (~GCMD_CMDSET) | godcmds1` (every bit
///   except GCMD_CMDSET), and `godcmds2 = godcmds3 = godcmds4 = ~0` (all bits).
///
/// `LVL_IMMORT`/`LVL_IMPL` are passed in to avoid a module cycle with
/// `command_table`'s level constants.
pub fn grant_advance(
    godcmds1: &mut i64,
    godcmds2: &mut i64,
    godcmds3: &mut i64,
    godcmds4: &mut i64,
    level: u8,
    lvl_immort: u8,
    lvl_impl: u8,
) {
    if level >= lvl_immort {
        *godcmds1 |= GCMD_GEN;
    }
    if level >= lvl_impl {
        // C: GCMD_FLAGS = (~GCMD_CMDSET) | GCMD_FLAGS  (all bits but CMDSET)
        *godcmds1 = (!GCMD_CMDSET) | *godcmds1;
        // C: ~0 on a `long` field => every bit set.
        *godcmds2 = !0;
        *godcmds3 = !0;
        *godcmds4 = !0;
    }
}

/// Canonical capability set for an administrative rank transition.
///
/// Historical `grant_advance` is additive, which is appropriate while normal
/// leveling crosses a threshold but unsafe for an explicit demotion: stale
/// Implementor bits would survive. `advance` therefore rebuilds the set from
/// zero. Mortals receive no god commands, ordinary immortals receive only the
/// general bit, and Implementors receive the historical full grant.
pub fn canonical_advance_grants(level: u8, lvl_immort: u8, lvl_impl: u8) -> (i64, i64, i64, i64) {
    let (mut godcmds1, mut godcmds2, mut godcmds3, mut godcmds4) = (0, 0, 0, 0);
    grant_advance(
        &mut godcmds1,
        &mut godcmds2,
        &mut godcmds3,
        &mut godcmds4,
        level,
        lvl_immort,
        lvl_impl,
    );
    (godcmds1, godcmds2, godcmds3, godcmds4)
}
