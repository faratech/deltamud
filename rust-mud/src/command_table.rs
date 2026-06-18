//! Command dispatch table — a 1:1 port of the C `cmd_info[]` array from
//! `src/interpreter.c`.
//!
//! The C struct is:
//! ```c
//! struct command_info {
//!    const char *command;
//!    const char *sort_as;          // abbreviation alias (not modelled here)
//!    byte minimum_position;
//!    void (*command_pointer)(...);
//!    long minimum_level;
//!    long godcmd;                  // god-flag bitvector (modelled: see godcmd /
//!                                  // godcmd_set on the Rust command entry)
//!    int subcmd;
//! };
//! ```
//!
//! This table preserves the EXACT C order and EXACT command-name strings.
//! Order is load-bearing: abbreviation matching in `command_interpreter`
//! walks the table top-to-bottom and stops at the first prefix match, so the
//! leading `RESERVED` sentinel, the six direction entries, and the trailing
//! `"\n"` terminator must all appear in their original positions.
//!
//! Mapping notes:
//! * `min_position` maps to `crate::types::Position` BY NAME (the Rust enum
//!   inserts a `Meditating` variant at index 5, shifting Resting/Sitting/
//!   Fighting/Standing by one relative to the C `POS_*` integers — so e.g.
//!   `POS_RESTING` -> `Position::Resting`, never `Position::Meditating`).
//! * `min_level` is the numeric C `minimum_level`. Mortal commands use `0`.
//!   God commands in C store a negative *flag-class sentinel* in this slot
//!   (`GOD_CMD` = -1, `GOD_CMD2` = -2, `GOD_CMD3` = -5; the godcmd bitvector
//!   field — modelled here as the `godcmd`/`godcmd_set` entry fields — then
//!   selects the specific permission). Because the effective level gate for
//!   every one of those is "must be an immortal", we record the numeric
//!   `LVL_IMMORT` (101) here and let the godcmd bit do the fine-grained gating.
//!   See `LVL_IMMORT` below.
//! * `subcmd` is the numeric `SCMD_*` value, inlined as an integer.

use crate::types::Position;

// ---------------------------------------------------------------------------
// Level constants (from src/structs.h)
// ---------------------------------------------------------------------------

/// Lowest immortal level. Every C entry whose `minimum_level` is a god-command
/// sentinel (`GOD_CMD`/`GOD_CMD2`/`GOD_CMD3` = -1/-2/-5) is gated, at minimum,
/// to immortals; we surface that as `LVL_IMMORT`.
pub const LVL_IMMORT: u8 = 101;
pub const LVL_HERO: u8 = 100;
pub const LVL_DEMIGOD: u8 = 102;
pub const LVL_GOD: u8 = 103;
pub const LVL_GRGOD: u8 = 104;
pub const LVL_IMPL: u8 = 105;

// ---------------------------------------------------------------------------
// SCMD_* subcommand constants (from src/interpreter.h)
// ---------------------------------------------------------------------------

// directions
const SCMD_NORTH: i32 = 1;
const SCMD_EAST: i32 = 2;
const SCMD_SOUTH: i32 = 3;
const SCMD_WEST: i32 = 4;
const SCMD_UP: i32 = 5;
const SCMD_DOWN: i32 = 6;

// do_gen_ps
const SCMD_INFO: i32 = 0;
const SCMD_HANDBOOK: i32 = 1;
const SCMD_CREDITS: i32 = 2;
const SCMD_NEWS: i32 = 3;
const SCMD_WIZLIST: i32 = 4;
const SCMD_POLICIES: i32 = 5;
const SCMD_VERSION: i32 = 6;
const SCMD_IMMLIST: i32 = 7;
const SCMD_MOTD: i32 = 8;
const SCMD_IMOTD: i32 = 9;
const SCMD_CLEAR: i32 = 10;
const SCMD_WHOAMI: i32 = 11;
const SCMD_CIRCLEMUD: i32 = 12;

// do_gen_tog
const SCMD_NOSUMMON: i32 = 0;
const SCMD_NOHASSLE: i32 = 1;
const SCMD_BRIEF: i32 = 2;
const SCMD_COMPACT: i32 = 3;
const SCMD_NOTELL: i32 = 4;
const SCMD_NOAUCTION: i32 = 5;
const SCMD_DEAF: i32 = 6;
const SCMD_NOGOSSIP: i32 = 7;
const SCMD_NOGRATZ: i32 = 8;
const SCMD_NOWIZ: i32 = 9;
const SCMD_QCHAN: i32 = 10;
const SCMD_ROOMFLAGS: i32 = 11;
const SCMD_NOREPEAT: i32 = 12;
const SCMD_HOLYLIGHT: i32 = 13;
const SCMD_SLOWNS: i32 = 14;
const SCMD_AUTOEXIT: i32 = 15;
const SCMD_AUTOSPLIT: i32 = 16;
const SCMD_AUTOLOOT: i32 = 17;
const SCMD_AUTOGOLD: i32 = 18;
const SCMD_AFK: i32 = 19;
const SCMD_NOTIC: i32 = 20;
#[allow(dead_code)]
const SCMD_CLOAK: i32 = 21;
const SCMD_NOLOOKSTAC: i32 = 22;
const SCMD_NOARENA: i32 = 23;
const SCMD_NOMAP: i32 = 24;
const SCMD_MERCY: i32 = 25;
const SCMD_ADVANCEDMAP: i32 = 26;

// do_wizutil
const SCMD_REROLL: i32 = 0;
const SCMD_PARDON: i32 = 1;
const SCMD_NOTITLE: i32 = 2;
const SCMD_SQUELCH: i32 = 3;
const SCMD_FREEZE: i32 = 4;
const SCMD_THAW: i32 = 5;
const SCMD_UNAFFECT: i32 = 6;

// do_spec_comm
const SCMD_WHISPER: i32 = 0;
const SCMD_ASK: i32 = 1;

// do_gen_comm
const SCMD_HOLLER: i32 = 0;
const SCMD_SHOUT: i32 = 1;
const SCMD_GOSSIP: i32 = 2;
#[allow(dead_code)]
const SCMD_AUCTION: i32 = 3;
const SCMD_GRATZ: i32 = 4;
const SCMD_GMOTE: i32 = 5;
const SCMD_ARENA: i32 = 6;

// do_shutdown
#[allow(dead_code)]
const SCMD_SHUTDOW: i32 = 0;
const SCMD_SHUTDOWN: i32 = 1;

// do_quit
#[allow(dead_code)]
const SCMD_QUI: i32 = 0;
const SCMD_QUIT: i32 = 1;

// do_date
#[allow(dead_code)]
const SCMD_DATE: i32 = 0;
const SCMD_UPTIME: i32 = 1;

// do_commands
const SCMD_COMMANDS: i32 = 0;
const SCMD_SOCIALS: i32 = 1;
const SCMD_WIZHELP: i32 = 2;

// do_drop
const SCMD_DROP: i32 = 0;
const SCMD_JUNK: i32 = 1;
const SCMD_DONATE: i32 = 2;

// do_gen_write
const SCMD_BUG: i32 = 0;
const SCMD_TYPO: i32 = 1;
const SCMD_IDEA: i32 = 2;

// do_look
const SCMD_LOOK: i32 = 0;
const SCMD_READ: i32 = 1;

// do_qcomm
const SCMD_QSAY: i32 = 0;
const SCMD_QECHO: i32 = 1;

// do_pour
const SCMD_POUR: i32 = 0;
const SCMD_FILL: i32 = 1;

// do_poof
const SCMD_POOFIN: i32 = 0;
const SCMD_POOFOUT: i32 = 1;

// do_hit
const SCMD_HIT: i32 = 0;
const SCMD_MURDER: i32 = 1;
const SCMD_DEATHBLOW: i32 = 2;

// do_eat
const SCMD_EAT: i32 = 0;
const SCMD_TASTE: i32 = 1;
const SCMD_DRINK: i32 = 2;
const SCMD_SIP: i32 = 3;

// do_use
const SCMD_USE: i32 = 0;
const SCMD_QUAFF: i32 = 1;
const SCMD_RECITE: i32 = 2;

// do_echo
const SCMD_ECHO: i32 = 0;
const SCMD_EMOTE: i32 = 1;

// do_gen_door
const SCMD_OPEN: i32 = 0;
const SCMD_CLOSE: i32 = 1;
const SCMD_UNLOCK: i32 = 2;
const SCMD_LOCK: i32 = 3;
const SCMD_PICK: i32 = 4;
const SCMD_RAM: i32 = 5;

// do_olc
const SCMD_OLC_REDIT: i32 = 0;
const SCMD_OLC_OEDIT: i32 = 1;
const SCMD_OLC_ZEDIT: i32 = 2;
const SCMD_OLC_MEDIT: i32 = 3;
const SCMD_OLC_SEDIT: i32 = 4;
const SCMD_OLC_TRIGEDIT: i32 = 5;
const SCMD_OLC_HEDIT: i32 = 6;
const SCMD_OLC_AEDIT: i32 = 7;
const SCMD_OLC_SAVEINFO: i32 = 8;

// do_gen_atm
const SCMD_BALANCE: i32 = 0;
const SCMD_DEPOSIT: i32 = 1;
const SCMD_WITHDRAW: i32 = 2;
const SCMD_BANK: i32 = 3;

// ---------------------------------------------------------------------------
// Handler identifiers — one variant per DISTINCT do_* function in cmd_info[].
// Names match the C function: do_move -> DoMove, lweather -> Lweather, etc.
// ---------------------------------------------------------------------------

/// One C ACMD handler function. One variant per DISTINCT handler referenced in
/// `cmd_info[]`. `DoNotImplemented` covers any entry whose handler is a pure
/// sentinel (the `RESERVED` row and the `"\n"` terminator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerId {
    DoMove,
    DoAddsnow,
    DoAt,
    DoAffected,
    DoAdvance,
    DoGenTog,
    DoOlc,
    DoAlias,
    DoAload,
    DoAreas,
    DoNotHere,
    DoGenComm,
    DoAssist,
    DoSpecComm,
    DoAuction,
    DoAuctioneer,
    DoAutoquest,
    DoBackstab,
    DoPostbail,
    DoBan,
    DoGenAtm,
    DoBash,
    DoBerserk,
    DoBed,
    DoBid,
    DoBlanket,
    DoBrew,
    DoBuck,
    DoBuild,
    DoGenWrite,
    DoCarve,
    DoCast,
    DoCamouflage,
    DoChainFooting,
    DoCheckbail,
    DoGenPs,
    DoCitizen,
    DoClan,
    DoGenDoor,
    DoConsider,
    DoCopyto,
    DoColor,
    DoCommands,
    DoCopyover,
    DoCsay,
    DoDc,
    DoHit,
    DoDelsnow,
    DoDiagnose,
    DoDismount,
    DoDig,
    DoDisarm,
    DoDisplay,
    DoDrop,
    DoDrink,
    DoEat,
    DoEcho,
    DoEsave,
    DoEmail,
    DoEnter,
    DoEquipment,
    DoExits,
    DoExamine,
    DoForce,
    DoPour,
    DoFillet,
    DoWhois,
    DoFlee,
    DoFollow,
    DoForage,
    DoForge,
    DoWizutil,
    DoGet,
    DoGecho,
    DoGplague,
    DoGcureplague,
    DoGive,
    DoGoto,
    DoGold,
    DoGroup,
    DoGrab,
    DoGsay,
    DoHelp,
    DoHcontrol,
    DoHide,
    DoHouse,
    DoInventory,
    DoIgnore,
    DoInsult,
    DoInvis,
    DoIsay,
    DoKill,
    DoKick,
    DoLook,
    DoLast,
    DoLeave,
    DoLevels,
    DoListen,
    DoLockout,
    DoLoad,
    Lweather,
    DoMap,
    DoMcheck,
    DoMcasters,
    DoMeditate,
    DoMlist,
    DoMount,
    DoMudheal,
    DoMobdie,
    DoOlist,
    DoPut,
    DoPage,
    DoPeace,
    DoPfileclean,
    DoPlayers,
    DoPoofset,
    DoPractice,
    DoPurge,
    DoQcomm,
    DoQuestmobs,
    DoQuit,
    DoReply,
    DoRest,
    DoRespec,
    DoRebalance,
    DoReboot,
    DoRecall,
    DoRemove,
    DoRename,
    DoRepair,
    DoReport,
    DoRescue,
    DoRestore,
    DoRetreat,
    DoReturn,
    DoRewiz,
    DoReward,
    DoWhoupd,
    DoRlist,
    DoRnum,
    DoSac,
    DoSay,
    DoSave,
    DoStatus,
    DoScan,
    DoScribe,
    DoSchool,
    DoSend,
    DoSet,
    DoSetreboot,
    DoShow,
    DoShutdown,
    DoSit,
    DoSkillset,
    DoSleep,
    DoSneak,
    DoSlist,
    DoSnoop,
    DoSplit,
    DoSpeed,
    DoStand,
    DoStat,
    DoSteal,
    DoStopAuction,
    DoSwitch,
    DoSyslog,
    DoTedit,
    DoTell,
    DoTame,
    DoTan,
    DoTarget,
    DoTeleport,
    DoTitle,
    DoTime,
    DoTmobdie,
    DoToggle,
    DoTrack,
    DoTrans,
    DoTrain,
    DoTrip,
    DoUngroup,
    DoUnban,
    DoDate,
    DoUse,
    DoUsers,
    DoVwear,
    DoVisible,
    DoVnum,
    DoVstat,
    DoWake,
    DoWear,
    Pweather,
    DoWho,
    DoWhere,
    DoWield,
    DoWimpy,
    DoWiznet,
    DoWizlock,
    DoWorth,
    DoWrite,
    DoWrestrict,
    DoZreset,
    DoLevelme,
    DoAttach,
    DoDetach,
    DoTlist,
    DoTstat,
    DoMasound,
    DoMkill,
    DoMjunk,
    DoMecho,
    DoMechoaround,
    DoMsend,
    DoMload,
    DoMpurge,
    DoMgoto,
    DoMat,
    DoMteleport,
    DoMforce,
    DoMexp,
    DoMgold,
    DoMhunt,
    DoMremember,
    DoMforget,
    DoMtransform,
    DoObserve,
    DoOrder,
    DoBuy,
    DoSell,
    DoList,
    DoValue,
    DoAppraise,
    DoMail,
    DoNotImplemented,
}

/// One row of the C `cmd_info[]` table.
///
/// `godcmd`/`godcmd_set` model the C per-command god-permission contract
/// (`src/gcmd.h`, gated at `interpreter.c:786-789`). For a god command the
/// `minimum_level` slot held a negative marker (`GOD_CMD`=-1, `GOD_CMD2`=-2,
/// `GOD_CMD3`=-5, `GOD_CMD4`=-6) selecting which of the four `godcmds1..4`
/// bitvectors gates the command; that selector is captured here as
/// `godcmd_set` (1..4), and the required bit as `godcmd`. Non-god commands
/// have `godcmd_set == 0` (no gate). The `min_level` slot for god commands
/// surfaces `LVL_IMMORT` (the effective floor), so the level check still works.
#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub min_position: Position,
    pub handler: HandlerId,
    pub min_level: u8,
    pub subcmd: i32,
    /// Required god-command bit (raw value from `src/gcmd.h`). Meaningful only
    /// when `godcmd_set != 0`. `0` means "no specific bit" — like the handful
    /// of C god entries whose `godcmd` field is `0` (e.g. `citizen`, `mobdie`,
    /// `slowns`), reachable only by the Implementor (handled in the gate).
    pub godcmd: i64,
    /// Which `godcmds` bitvector gates this command: 0 = none (not a god
    /// command), 1 = godcmds1 (GOD_CMD), 2 = godcmds2 (GOD_CMD2),
    /// 3 = godcmds3 (GOD_CMD3), 4 = godcmds4 (GOD_CMD4).
    pub godcmd_set: u8,
}

// Shorthand for building NON-god rows (godcmd_set = 0).
const fn c(
    name: &'static str,
    min_position: Position,
    handler: HandlerId,
    min_level: u8,
    subcmd: i32,
) -> CommandDef {
    CommandDef {
        name,
        min_position,
        handler,
        min_level,
        subcmd,
        godcmd: 0,
        godcmd_set: 0,
    }
}

// Shorthand for building god rows. `set` selects the godcmds bitvector (1..4),
// matching the C GOD_CMD/GOD_CMD2/GOD_CMD3/GOD_CMD4 marker; `godcmd` is the
// required bit (raw value from src/gcmd.h). `min_level` is always LVL_IMMORT
// (the C minimum_level slot held a negative marker, but the effective floor is
// immortal).
const fn g(
    name: &'static str,
    min_position: Position,
    handler: HandlerId,
    set: u8,
    godcmd: i64,
    subcmd: i32,
) -> CommandDef {
    CommandDef {
        name,
        min_position,
        handler,
        min_level: LVL_IMMORT,
        subcmd,
        godcmd,
        godcmd_set: set,
    }
}

use crate::gcmd::*;
use HandlerId::*;
use Position::*;

/// The `cmd_info` table, in EXACT C order. The first row is the `RESERVED`
/// sentinel (index 0, reserved for specprocs); the six direction rows must
/// follow before any other command; the last row is the `"\n"` terminator.
pub static CMD_INFO: &[CommandDef] = &[
    // index 0 — must be first, reserved for specprocs
    c("RESERVED", Dead, DoNotImplemented, 0, 0),
    // directions — must come before other commands but after RESERVED
    c("north", Standing, DoMove, 0, SCMD_NORTH),
    c("east", Standing, DoMove, 0, SCMD_EAST),
    c("south", Standing, DoMove, 0, SCMD_SOUTH),
    c("west", Standing, DoMove, 0, SCMD_WEST),
    c("up", Standing, DoMove, 0, SCMD_UP),
    c("down", Standing, DoMove, 0, SCMD_DOWN),
    // main list
    g("addsnow", Dead, DoAddsnow, 3, GCMD3_ADDSNOW, 0),
    g("at", Dead, DoAt, 1, GCMD_AT, 0),
    c("affected", Sleeping, DoAffected, 0, 0),
    g("advance", Dead, DoAdvance, 1, GCMD_ADVANCE, 0),
    c("advancedmap", Dead, DoGenTog, 0, SCMD_ADVANCEDMAP),
    g("aedit", Dead, DoOlc, 3, GCMD3_IMPOLC, SCMD_OLC_AEDIT),
    c("afk", Dead, DoGenTog, 0, SCMD_AFK),
    c("alias", Dead, DoAlias, 0, 0),
    g("aload", Dead, DoAload, 2, GCMD2_ALOAD, 0),
    c("areas", Dead, DoAreas, 0, 0),
    c("arena", Standing, DoNotHere, 0, 0),
    g("ainfo", Sleeping, DoGenComm, 1, GCMD_GEN, SCMD_ARENA),
    c("assist", Fighting, DoAssist, 0, 0),
    c("ask", Resting, DoSpecComm, 0, SCMD_ASK),
    c("auction", Sleeping, DoAuction, 0, 0),
    g("auctioneer", Dead, DoAuctioneer, 1, GCMD_AUCTIONEER, 0),
    c("autoexit", Dead, DoGenTog, 0, SCMD_AUTOEXIT),
    c("autosplit", Dead, DoGenTog, 0, SCMD_AUTOSPLIT),
    c("autoloot", Dead, DoGenTog, 0, SCMD_AUTOLOOT),
    c("autogold", Dead, DoGenTog, 0, SCMD_AUTOGOLD),
    c("autoquest", Standing, DoAutoquest, 0, 0),
    c("away", Dead, DoGenTog, 0, SCMD_AFK),
    c("backstab", Standing, DoBackstab, 0, 0),
    c("bail", Resting, DoPostbail, 0, 0),
    g("ban", Dead, DoBan, 1, GCMD_BAN, 0),
    c("bank", Standing, DoGenAtm, 0, SCMD_BANK),
    c("balance", Standing, DoGenAtm, 0, SCMD_BALANCE),
    c("bash", Fighting, DoBash, 0, 0),
    c("berserk", Fighting, DoBerserk, 0, 0),
    c("bed", Standing, DoBed, 0, 0),
    c("bid", Sleeping, DoBid, 0, 0),
    c("blanket", Fighting, DoBlanket, 0, 0),
    c("brew", Standing, DoBrew, 0, 0),
    c("brief", Dead, DoGenTog, 0, SCMD_BRIEF),
    c("buck", Standing, DoBuck, 0, 0),
    c("build", Standing, DoBuild, 0, 0),
    c("buy", Standing, DoBuy, 0, 0),
    c("bug", Dead, DoGenWrite, 0, SCMD_BUG),
    c("camp", Standing, DoNotHere, 0, 0),
    c("carve", Standing, DoCarve, 0, 0),
    c("cast", Sitting, DoCast, 0, 0),
    c("camouflage", Fighting, DoCamouflage, 0, 0),
    c("chain", Fighting, DoChainFooting, 0, 0),
    c("check", Standing, DoMail, 0, 1),
    c("checkbail", Resting, DoCheckbail, 0, 0),
    c("circlemud", Dead, DoGenPs, 0, SCMD_CIRCLEMUD),
    g("citizen", Dead, DoCitizen, 3, 0, 0),
    c("clan", Sleeping, DoClan, 0, 0),
    c("clear", Dead, DoGenPs, 0, SCMD_CLEAR),
    c("close", Sitting, DoGenDoor, 0, SCMD_CLOSE),
    c("cls", Dead, DoGenPs, 0, SCMD_CLEAR),
    c("consider", Resting, DoConsider, 0, 0),
    g("copyto", Dead, DoCopyto, 2, GCMD2_OLC, 0),
    c("color", Dead, DoColor, 0, 0),
    c("commands", Dead, DoCommands, 0, SCMD_COMMANDS),
    c("compact", Dead, DoGenTog, 0, SCMD_COMPACT),
    g("copyover", Dead, DoCopyover, 3, GCMD3_COPYOVER, 0),
    c("credits", Dead, DoGenPs, 0, SCMD_CREDITS),
    c("csay", Sleeping, DoCsay, 0, 0),
    g("dc", Dead, DoDc, 1, GCMD_DC, 0),
    c("deathblow", Standing, DoHit, 0, SCMD_DEATHBLOW),
    g("delsnow", Dead, DoDelsnow, 3, GCMD3_DELSNOW, 0),
    c("deposit", Standing, DoGenAtm, 0, SCMD_DEPOSIT),
    c("diagnose", Resting, DoDiagnose, 0, 0),
    c("dismount", Standing, DoDismount, 0, 0),
    g("dig", Dead, DoDig, 2, GCMD2_OLC, 0),
    c("disarm", Fighting, DoDisarm, 0, 0),
    c("display", Dead, DoDisplay, 0, 0),
    c("donate", Resting, DoDrop, 0, SCMD_DONATE),
    c("drink", Resting, DoDrink, 0, SCMD_DRINK),
    c("drop", Resting, DoDrop, 0, SCMD_DROP),
    c("eat", Resting, DoEat, 0, SCMD_EAT),
    g("echo", Sleeping, DoEcho, 1, GCMD_GEN, SCMD_ECHO),
    g("esave", Dead, DoEsave, 2, GCMD2_OLC, 0),
    c("emote", Resting, DoEcho, 0, SCMD_EMOTE),
    c(":", Resting, DoEcho, 0, SCMD_EMOTE),
    c("email", Dead, DoEmail, 0, 0),
    c("enter", Standing, DoEnter, 0, 0),
    c("equipment", Sleeping, DoEquipment, 0, 0),
    c("exits", Resting, DoExits, 0, 0),
    c("examine", Sitting, DoExamine, 0, 0),
    g("force", Sleeping, DoForce, 1, GCMD_FORCE, 0),
    c("fill", Standing, DoPour, 0, SCMD_FILL),
    c("fillet", Standing, DoFillet, 0, 0),
    c("finger", Dead, DoWhois, 0, 0),
    c("flee", Fighting, DoFlee, 0, 0),
    c("follow", Resting, DoFollow, 0, 0),
    c("forage", Standing, DoForage, 0, 0),
    c("forge", Standing, DoForge, 0, 0),
    g("freeze", Dead, DoWizutil, 1, GCMD_FREEZE, SCMD_FREEZE),
    c("get", Resting, DoGet, 0, 0),
    g("gecho", Dead, DoGecho, 2, GCMD2_GECHO, 0),
    g("gplague", Dead, DoGplague, 1, GCMD_PLAGUE, 0),
    g("gdeplague", Dead, DoGcureplague, 1, GCMD_PLAGUE, 0),
    c("gemote", Sleeping, DoGenComm, 0, SCMD_GMOTE),
    c("give", Resting, DoGive, 0, 0),
    g("goto", Sleeping, DoGoto, 1, GCMD_GEN, 0),
    c("gold", Resting, DoGold, 0, 0),
    c(".", Sleeping, DoGenComm, 0, SCMD_GOSSIP),
    c("gossip", Sleeping, DoGenComm, 0, SCMD_GOSSIP),
    c("group", Resting, DoGroup, 0, 0),
    c("grab", Resting, DoGrab, 0, 0),
    c("grats", Sleeping, DoGenComm, 0, SCMD_GRATZ),
    c("gsay", Sleeping, DoGsay, 0, 0),
    c("gtell", Sleeping, DoGsay, 0, 0),
    g("hedit", Dead, DoOlc, 3, GCMD3_IMPOLC, SCMD_OLC_HEDIT),
    c("help", Dead, DoHelp, 0, 0),
    g("handbook", Dead, DoGenPs, 1, GCMD_GEN, SCMD_HANDBOOK),
    g("hcontrol", Dead, DoHcontrol, 1, GCMD_HCONTROL, 0),
    c("hide", Resting, DoHide, 0, 0),
    c("hit", Fighting, DoHit, 0, SCMD_HIT),
    c("hold", Resting, DoGrab, 0, 0),
    c("holler", Resting, DoGenComm, 0, SCMD_HOLLER),
    g("holylight", Dead, DoGenTog, 1, GCMD_GEN, SCMD_HOLYLIGHT),
    c("house", Resting, DoHouse, 0, 0),
    c("inventory", Dead, DoInventory, 0, 0),
    c("ignore", Dead, DoIgnore, 0, 0),
    c("idea", Dead, DoGenWrite, 0, SCMD_IDEA),
    c("imotd", Dead, DoGenPs, 0, SCMD_IMOTD),
    c("immlist", Dead, DoGenPs, 0, SCMD_IMMLIST),
    c("info", Sleeping, DoGenPs, 0, SCMD_INFO),
    c("insult", Resting, DoInsult, 0, 0),
    g("invis", Dead, DoInvis, 2, GCMD2_INVIS, 0),
    g("isay", Dead, DoIsay, 1, GCMD_ISAY, 0),
    c("junk", Resting, DoDrop, 0, SCMD_JUNK),
    c("kill", Fighting, DoKill, 0, 0),
    c("kick", Fighting, DoKick, 0, 0),
    c("look", Resting, DoLook, 0, SCMD_LOOK),
    g("last", Dead, DoLast, 2, GCMD2_USERS, 0),
    c("leave", Standing, DoLeave, 0, 0),
    c("levels", Dead, DoLevels, 0, 0),
    c("list", Standing, DoList, 0, 0),
    c("listen", Resting, DoListen, 0, 0),
    c("lock", Sitting, DoGenDoor, 0, SCMD_LOCK),
    c("lockout", Resting, DoLockout, 0, 0),
    g("load", Dead, DoLoad, 1, GCMD_LOAD, 0),
    g("lweather", Dead, Lweather, 3, GCMD3_LWEATHER, 0),
    g("medit", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_MEDIT),
    c("motd", Dead, DoGenPs, 0, SCMD_MOTD),
    c("mail", Standing, DoMail, 0, 0),
    g("map", Dead, DoMap, 3, GCMD3_MAP, 0),
    c("mcheck", Dead, DoMcheck, 0, 0),
    g("mcasters", Dead, DoMcasters, 2, GCMD2_MCASTERS, 0),
    c("meditate", Dead, DoMeditate, 0, 0),
    c("mercy", Dead, DoGenTog, 0, SCMD_MERCY),
    c("mlist", Dead, DoMlist, 0, 0),
    c("mount", Standing, DoMount, 0, 0),
    g("mudheal", Dead, DoMudheal, 2, GCMD2_MUDHEAL, 0),
    g("mute", Dead, DoWizutil, 1, GCMD_MUTE, SCMD_SQUELCH),
    c("murder", Fighting, DoHit, 0, SCMD_MURDER),
    g("mobdie", Dead, DoMobdie, 1, 0, 0),
    c("news", Sleeping, DoGenPs, 0, SCMD_NEWS),
    c("noarena", Dead, DoGenTog, 0, SCMD_NOARENA),
    c("noauction", Dead, DoGenTog, 0, SCMD_NOAUCTION),
    c("nogossip", Dead, DoGenTog, 0, SCMD_NOGOSSIP),
    c("nograts", Dead, DoGenTog, 0, SCMD_NOGRATZ),
    g("nohassle", Dead, DoGenTog, 1, GCMD_GEN, SCMD_NOHASSLE),
    c("nomap", Dead, DoGenTog, 0, SCMD_NOMAP),
    c("nomstack", Dead, DoGenTog, 0, SCMD_NOLOOKSTAC),
    c("norepeat", Dead, DoGenTog, 0, SCMD_NOREPEAT),
    c("noshout", Sleeping, DoGenTog, 0, SCMD_DEAF),
    c("nosummon", Dead, DoGenTog, 0, SCMD_NOSUMMON),
    c("notell", Dead, DoGenTog, 0, SCMD_NOTELL),
    c("notic", Dead, DoGenTog, 0, SCMD_NOTIC),
    g("notitle", Dead, DoWizutil, 2, GCMD2_NOTITLE, SCMD_NOTITLE),
    g("nowiz", Dead, DoGenTog, 1, GCMD_GEN, SCMD_NOWIZ),
    c("observe", Resting, DoObserve, 0, 0),
    c("order", Resting, DoOrder, 0, 0),
    c("offer", Standing, DoNotHere, 0, 0),
    c("ooc", Sleeping, DoGenComm, 0, SCMD_GOSSIP),
    c("open", Sitting, DoGenDoor, 0, SCMD_OPEN),
    g("olc", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_SAVEINFO),
    g("olist", Dead, DoOlist, 2, GCMD2_OLC, 0),
    g("oedit", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_OEDIT),
    c("put", Resting, DoPut, 0, 0),
    g("page", Dead, DoPage, 2, GCMD2_PAGE, 0),
    g("pardon", Dead, DoWizutil, 1, GCMD_PARDON, SCMD_PARDON),
    g("peace", Dead, DoPeace, 3, GCMD3_PEACE, 0),
    g("pfileclean", Dead, DoPfileclean, 3, GCMD3_PFILECLEAN, 0),
    c("pick", Standing, DoGenDoor, 0, SCMD_PICK),
    c("players", Dead, DoPlayers, 0, 0),
    c("policy", Dead, DoGenPs, 0, SCMD_POLICIES),
    g("poofin", Dead, DoPoofset, 1, GCMD_GEN, SCMD_POOFIN),
    g("poofout", Dead, DoPoofset, 1, GCMD_GEN, SCMD_POOFOUT),
    c("postbail", Resting, DoPostbail, 0, 0),
    c("pour", Standing, DoPour, 0, SCMD_POUR),
    c("prompt", Dead, DoDisplay, 0, 0),
    c("practice", Resting, DoPractice, 0, 0),
    g("purge", Dead, DoPurge, 1, GCMD_PURGE, 0),
    c("quaff", Resting, DoUse, 0, SCMD_QUAFF),
    g("qecho", Dead, DoQcomm, 2, GCMD2_QECHO, SCMD_QECHO),
    c("qchan", Dead, DoGenTog, 0, SCMD_QCHAN),
    c("quest", Standing, DoAutoquest, 0, 0),
    g("questmobs", Standing, DoQuestmobs, 2, GCMD2_QUESTMOBS, 0),
    c("qui", Dead, DoQuit, 0, 0),
    c("quit", Dead, DoQuit, 0, SCMD_QUIT),
    c("qsay", Resting, DoQcomm, 0, SCMD_QSAY),
    c("ram", Standing, DoGenDoor, 0, SCMD_RAM),
    c("reply", Sleeping, DoReply, 0, 0),
    c("rest", Resting, DoRest, 0, 0),
    g("respec", Resting, DoRespec, 2, GCMD2_RESPEC, 0),
    c("read", Resting, DoLook, 0, SCMD_READ),
    g("rebalance", Standing, DoRebalance, 3, GCMD3_REBALANCE, 0),
    g("reload", Dead, DoReboot, 1, GCMD_RELOAD, 0),
    c("recall", Dead, DoRecall, 0, 0),
    c("recite", Resting, DoUse, 0, SCMD_RECITE),
    c("receive", Standing, DoMail, 0, 2),
    c("remove", Resting, DoRemove, 0, 0),
    g("rename", Dead, DoRename, 2, GCMD2_IMP, 0),
    c("rent", Standing, DoNotHere, 0, 0),
    c("repair", Standing, DoRepair, 0, 0),
    c("report", Resting, DoReport, 0, 0),
    g("reroll", Dead, DoWizutil, 1, GCMD_REROLL, SCMD_REROLL),
    c("rescue", Fighting, DoRescue, 0, 0),
    g("restore", Dead, DoRestore, 1, GCMD_RESTORE, 0),
    c("retreat", Dead, DoRetreat, 0, 0),
    c("return", Dead, DoReturn, 0, 0),
    g("redit", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_REDIT),
    g("rewiz", Dead, DoRewiz, 2, GCMD2_REWIZ, 0),
    g("reward", Dead, DoReward, 2, GCMD2_REWARD, 0),
    g("rewww", Dead, DoWhoupd, 2, GCMD2_REWWW, 0),
    g("rlist", Dead, DoRlist, 2, GCMD2_OLC, 0),
    // C: GOD_CMD (set 1) but godcmd bit is GCMD2_OLC — the bit VALUE is what is
    // checked against godcmds1, so this requires GCMD2_OLC's value in godcmds1.
    g("rnumber", Dead, DoRnum, 1, GCMD2_OLC, 0),
    g("roomflags", Dead, DoGenTog, 1, GCMD_GEN, SCMD_ROOMFLAGS),
    c("sacrifice", Standing, DoSac, 0, 0),
    c("say", Resting, DoSay, 0, 0),
    c("'", Resting, DoSay, 0, 0),
    c("save", Sleeping, DoSave, 0, 0),
    c("score", Dead, DoStatus, 0, 0),
    c("scan", Standing, DoScan, 0, 0),
    c("scribe", Standing, DoScribe, 0, 0),
    c("school", Standing, DoSchool, 0, 0),
    c("sell", Standing, DoSell, 0, 0),
    g("send", Sleeping, DoSend, 1, GCMD_SEND, 0),
    g("set", Dead, DoSet, 1, GCMD_SET, 0),
    g("setreboot", Dead, DoSetreboot, 2, GCMD2_SETREBOOT, 0),
    g("sedit", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_SEDIT),
    c("shout", Resting, DoGenComm, 0, SCMD_SHOUT),
    g("show", Dead, DoShow, 1, GCMD_GEN, 0),
    g("shutdow", Dead, DoShutdown, 1, GCMD_SHUTDOWN, 0),
    g(
        "shutdown",
        Dead,
        DoShutdown,
        1,
        GCMD_SHUTDOWN,
        SCMD_SHUTDOWN,
    ),
    c("sip", Resting, DoDrink, 0, SCMD_SIP),
    c("sit", Resting, DoSit, 0, 0),
    g("skillset", Sleeping, DoSkillset, 1, GCMD_SKILLSET, 0),
    c("sleep", Sleeping, DoSleep, 0, 0),
    g("slowns", Dead, DoGenTog, 1, 0, SCMD_SLOWNS),
    c("sneak", Standing, DoSneak, 0, 0),
    c("slist", Dead, DoSlist, 0, 0),
    g("snoop", Dead, DoSnoop, 1, GCMD_SNOOP, 0),
    c("socials", Dead, DoCommands, 0, SCMD_SOCIALS),
    c("split", Sitting, DoSplit, 0, 0),
    c("speed", Standing, DoSpeed, 0, 0),
    c("stand", Resting, DoStand, 0, 0),
    g("stat", Dead, DoStat, 2, GCMD2_OLC, 0),
    c("status", Dead, DoStatus, 0, 0),
    c("steal", Standing, DoSteal, 0, 0),
    g("stopauc", Dead, DoStopAuction, 1, GCMD_AUCTIONEER, 0),
    g("switch", Dead, DoSwitch, 1, GCMD_SWITCH, 0),
    g("syslog", Dead, DoSyslog, 1, GCMD_SYSLOG, 0),
    g("tedit", Dead, DoTedit, 3, GCMD3_IMPOLC, 0),
    c("tell", Dead, DoTell, 0, 0),
    c("take", Resting, DoGet, 0, 0),
    c("tame", Standing, DoTame, 0, 0),
    c("tan", Standing, DoTan, 0, 0),
    c("target", Fighting, DoTarget, 0, 0),
    c("taste", Resting, DoEat, 0, SCMD_TASTE),
    g("teleport", Dead, DoTeleport, 1, GCMD_TRANS, 0),
    g("thaw", Dead, DoWizutil, 1, GCMD_FREEZE, SCMD_THAW),
    c("title", Dead, DoTitle, 0, 0),
    c("time", Dead, DoTime, 0, 0),
    g("tmobdie", Dead, DoTmobdie, 2, GCMD2_TMOBDIE, 0),
    c("toggle", Dead, DoToggle, 0, 0),
    c("track", Standing, DoTrack, 0, 0),
    g("transfer", Sleeping, DoTrans, 1, GCMD_TRANS, 0),
    c("train", Standing, DoTrain, 0, 0),
    g("trigedit", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_TRIGEDIT),
    c("trip", Fighting, DoTrip, 0, 0),
    c("typo", Dead, DoGenWrite, 0, SCMD_TYPO),
    c("unlock", Sitting, DoGenDoor, 0, SCMD_UNLOCK),
    c("ungroup", Dead, DoUngroup, 0, 0),
    g("unban", Dead, DoUnban, 1, GCMD_BAN, 0),
    g("unaffect", Dead, DoWizutil, 1, GCMD_UNAFFECT, SCMD_UNAFFECT),
    g("uptime", Dead, DoDate, 1, GCMD_GEN, SCMD_UPTIME),
    c("use", Sitting, DoUse, 0, SCMD_USE),
    g("users", Dead, DoUsers, 2, GCMD2_USERS, 0),
    c("value", Standing, DoValue, 0, 0),
    c("version", Dead, DoGenPs, 0, SCMD_VERSION),
    // C: GOD_CMD (set 1) but godcmd bit is GCMD2_OLC — bit value checked vs godcmds1.
    g("vwear", Dead, DoVwear, 1, GCMD2_OLC, 0),
    c("visible", Resting, DoVisible, 0, 0),
    g("vnum", Dead, DoVnum, 2, GCMD2_OLC, 0),
    g("vstat", Dead, DoVstat, 2, GCMD2_OLC, 0),
    c("wake", Sleeping, DoWake, 0, 0),
    c("wear", Resting, DoWear, 0, 0),
    c("weather", Resting, Pweather, 0, 0),
    c("who", Dead, DoWho, 0, 0),
    c("whoami", Dead, DoGenPs, 0, SCMD_WHOAMI),
    c("whois", Dead, DoWhois, 0, 0),
    c("where", Resting, DoWhere, 0, 0),
    c("whisper", Resting, DoSpecComm, 0, SCMD_WHISPER),
    c("wield", Resting, DoWield, 0, 0),
    c("wimpy", Dead, DoWimpy, 0, 0),
    c("withdraw", Standing, DoGenAtm, 0, SCMD_WITHDRAW),
    g("wiznet", Dead, DoWiznet, 1, GCMD_GEN, 0),
    g(";", Dead, DoWiznet, 1, GCMD_GEN, 0),
    c("wizhelp", Sleeping, DoCommands, 0, SCMD_WIZHELP),
    c("wizlist", Dead, DoGenPs, 0, SCMD_WIZLIST),
    g("wizlock", Dead, DoWizlock, 1, GCMD_WIZLOCK, 0),
    c("worth", Resting, DoWorth, 0, 0),
    c("write", Standing, DoWrite, 0, 0),
    g("wrestrict", Dead, DoWrestrict, 2, GCMD2_WRESTRICT, 0),
    g("zedit", Dead, DoOlc, 2, GCMD2_OLC, SCMD_OLC_ZEDIT),
    g("zreset", Dead, DoZreset, 2, GCMD2_ZRESET, 0),
    c("levelme", Dead, DoLevelme, 0, 0),
    g("attach", Dead, DoAttach, 2, GCMD2_ATTACH, 0),
    g("detach", Dead, DoDetach, 2, GCMD2_ATTACH, 0),
    g("tlist", Dead, DoTlist, 2, GCMD2_OLC, 0),
    g("tstat", Dead, DoTstat, 2, GCMD2_OLC, 0),
    c("masound", Dead, DoMasound, 0, 0),
    c("mkill", Standing, DoMkill, 0, 0),
    c("mjunk", Sitting, DoMjunk, 0, 0),
    c("mecho", Dead, DoMecho, 0, 0),
    c("mechoaround", Dead, DoMechoaround, 0, 0),
    c("msend", Dead, DoMsend, 0, 0),
    c("mload", Dead, DoMload, 0, 0),
    c("mpurge", Dead, DoMpurge, 0, 0),
    c("mgoto", Dead, DoMgoto, 0, 0),
    c("mat", Dead, DoMat, 0, 0),
    c("mteleport", Dead, DoMteleport, 0, 0),
    c("mforce", Dead, DoMforce, 0, 0),
    c("mexp", Dead, DoMexp, 0, 0),
    c("mgold", Dead, DoMgold, 0, 0),
    c("mhunt", Dead, DoMhunt, 0, 0),
    c("mremember", Dead, DoMremember, 0, 0),
    c("mforget", Dead, DoMforget, 0, 0),
    c("mtransform", Dead, DoMtransform, 0, 0),
    // terminator — must be last
    c("\n", Dead, DoNotImplemented, 0, 0),
];
