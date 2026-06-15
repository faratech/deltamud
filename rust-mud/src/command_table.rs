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
//!    long godcmd;                  // god-flag bitvector (not modelled here)
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
//!   field, not modelled here, then selects the specific permission). Because
//!   the effective gate for every one of those is "must be an immortal", we
//!   record the numeric `LVL_IMMORT` (101) here. See `LVL_IMMORT` below.
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
#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub min_position: Position,
    pub handler: HandlerId,
    pub min_level: u8,
    pub subcmd: i32,
}

// Shorthand for building rows.
const fn c(
    name: &'static str,
    min_position: Position,
    handler: HandlerId,
    min_level: u8,
    subcmd: i32,
) -> CommandDef {
    CommandDef { name, min_position, handler, min_level, subcmd }
}

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
    c("addsnow", Dead, DoAddsnow, LVL_IMMORT, 0),
    c("at", Dead, DoAt, LVL_IMMORT, 0),
    c("affected", Sleeping, DoAffected, 0, 0),
    c("advance", Dead, DoAdvance, LVL_IMMORT, 0),
    c("advancedmap", Dead, DoGenTog, 0, SCMD_ADVANCEDMAP),
    c("aedit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_AEDIT),
    c("afk", Dead, DoGenTog, 0, SCMD_AFK),
    c("alias", Dead, DoAlias, 0, 0),
    c("aload", Dead, DoAload, LVL_IMMORT, 0),
    c("areas", Dead, DoAreas, 0, 0),
    c("arena", Standing, DoNotHere, 0, 0),
    c("ainfo", Sleeping, DoGenComm, LVL_IMMORT, SCMD_ARENA),
    c("assist", Fighting, DoAssist, 0, 0),
    c("ask", Resting, DoSpecComm, 0, SCMD_ASK),
    c("auction", Sleeping, DoAuction, 0, 0),
    c("auctioneer", Dead, DoAuctioneer, LVL_IMMORT, 0),
    c("autoexit", Dead, DoGenTog, 0, SCMD_AUTOEXIT),
    c("autosplit", Dead, DoGenTog, 0, SCMD_AUTOSPLIT),
    c("autoloot", Dead, DoGenTog, 0, SCMD_AUTOLOOT),
    c("autogold", Dead, DoGenTog, 0, SCMD_AUTOGOLD),
    c("autoquest", Standing, DoAutoquest, 0, 0),
    c("away", Dead, DoGenTog, 0, SCMD_AFK),
    c("backstab", Standing, DoBackstab, 0, 0),
    c("bail", Resting, DoPostbail, 0, 0),
    c("ban", Dead, DoBan, LVL_IMMORT, 0),
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
    c("citizen", Dead, DoCitizen, LVL_IMMORT, 0),
    c("clan", Sleeping, DoClan, 0, 0),
    c("clear", Dead, DoGenPs, 0, SCMD_CLEAR),
    c("close", Sitting, DoGenDoor, 0, SCMD_CLOSE),
    c("cls", Dead, DoGenPs, 0, SCMD_CLEAR),
    c("consider", Resting, DoConsider, 0, 0),
    c("copyto", Dead, DoCopyto, LVL_IMMORT, 0),
    c("color", Dead, DoColor, 0, 0),
    c("commands", Dead, DoCommands, 0, SCMD_COMMANDS),
    c("compact", Dead, DoGenTog, 0, SCMD_COMPACT),
    c("copyover", Dead, DoCopyover, LVL_IMMORT, 0),
    c("credits", Dead, DoGenPs, 0, SCMD_CREDITS),
    c("csay", Sleeping, DoCsay, 0, 0),
    c("dc", Dead, DoDc, LVL_IMMORT, 0),
    c("deathblow", Standing, DoHit, 0, SCMD_DEATHBLOW),
    c("delsnow", Dead, DoDelsnow, LVL_IMMORT, 0),
    c("deposit", Standing, DoGenAtm, 0, SCMD_DEPOSIT),
    c("diagnose", Resting, DoDiagnose, 0, 0),
    c("dismount", Standing, DoDismount, 0, 0),
    c("dig", Dead, DoDig, LVL_IMMORT, 0),
    c("disarm", Fighting, DoDisarm, 0, 0),
    c("display", Dead, DoDisplay, 0, 0),
    c("donate", Resting, DoDrop, 0, SCMD_DONATE),
    c("drink", Resting, DoDrink, 0, SCMD_DRINK),
    c("drop", Resting, DoDrop, 0, SCMD_DROP),
    c("eat", Resting, DoEat, 0, SCMD_EAT),
    c("echo", Sleeping, DoEcho, LVL_IMMORT, SCMD_ECHO),
    c("esave", Dead, DoEsave, LVL_IMMORT, 0),
    c("emote", Resting, DoEcho, 0, SCMD_EMOTE),
    c(":", Resting, DoEcho, 0, SCMD_EMOTE),
    c("email", Dead, DoEmail, 0, 0),
    c("enter", Standing, DoEnter, 0, 0),
    c("equipment", Sleeping, DoEquipment, 0, 0),
    c("exits", Resting, DoExits, 0, 0),
    c("examine", Sitting, DoExamine, 0, 0),
    c("force", Sleeping, DoForce, LVL_IMMORT, 0),
    c("fill", Standing, DoPour, 0, SCMD_FILL),
    c("fillet", Standing, DoFillet, 0, 0),
    c("finger", Dead, DoWhois, 0, 0),
    c("flee", Fighting, DoFlee, 0, 0),
    c("follow", Resting, DoFollow, 0, 0),
    c("forage", Standing, DoForage, 0, 0),
    c("forge", Standing, DoForge, 0, 0),
    c("freeze", Dead, DoWizutil, LVL_IMMORT, SCMD_FREEZE),
    c("get", Resting, DoGet, 0, 0),
    c("gecho", Dead, DoGecho, LVL_IMMORT, 0),
    c("gplague", Dead, DoGplague, LVL_IMMORT, 0),
    c("gdeplague", Dead, DoGcureplague, LVL_IMMORT, 0),
    c("gemote", Sleeping, DoGenComm, 0, SCMD_GMOTE),
    c("give", Resting, DoGive, 0, 0),
    c("goto", Sleeping, DoGoto, LVL_IMMORT, 0),
    c("gold", Resting, DoGold, 0, 0),
    c(".", Sleeping, DoGenComm, 0, SCMD_GOSSIP),
    c("gossip", Sleeping, DoGenComm, 0, SCMD_GOSSIP),
    c("group", Resting, DoGroup, 0, 0),
    c("grab", Resting, DoGrab, 0, 0),
    c("grats", Sleeping, DoGenComm, 0, SCMD_GRATZ),
    c("gsay", Sleeping, DoGsay, 0, 0),
    c("gtell", Sleeping, DoGsay, 0, 0),
    c("hedit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_HEDIT),
    c("help", Dead, DoHelp, 0, 0),
    c("handbook", Dead, DoGenPs, LVL_IMMORT, SCMD_HANDBOOK),
    c("hcontrol", Dead, DoHcontrol, LVL_IMMORT, 0),
    c("hide", Resting, DoHide, 0, 0),
    c("hit", Fighting, DoHit, 0, SCMD_HIT),
    c("hold", Resting, DoGrab, 0, 0),
    c("holler", Resting, DoGenComm, 0, SCMD_HOLLER),
    c("holylight", Dead, DoGenTog, LVL_IMMORT, SCMD_HOLYLIGHT),
    c("house", Resting, DoHouse, 0, 0),
    c("inventory", Dead, DoInventory, 0, 0),
    c("ignore", Dead, DoIgnore, 0, 0),
    c("idea", Dead, DoGenWrite, 0, SCMD_IDEA),
    c("imotd", Dead, DoGenPs, 0, SCMD_IMOTD),
    c("immlist", Dead, DoGenPs, 0, SCMD_IMMLIST),
    c("info", Sleeping, DoGenPs, 0, SCMD_INFO),
    c("insult", Resting, DoInsult, 0, 0),
    c("invis", Dead, DoInvis, LVL_IMMORT, 0),
    c("isay", Dead, DoIsay, LVL_IMMORT, 0),
    c("junk", Resting, DoDrop, 0, SCMD_JUNK),
    c("kill", Fighting, DoKill, 0, 0),
    c("kick", Fighting, DoKick, 0, 0),
    c("look", Resting, DoLook, 0, SCMD_LOOK),
    c("last", Dead, DoLast, LVL_IMMORT, 0),
    c("leave", Standing, DoLeave, 0, 0),
    c("levels", Dead, DoLevels, 0, 0),
    c("list", Standing, DoList, 0, 0),
    c("listen", Resting, DoListen, 0, 0),
    c("lock", Sitting, DoGenDoor, 0, SCMD_LOCK),
    c("lockout", Resting, DoLockout, 0, 0),
    c("load", Dead, DoLoad, LVL_IMMORT, 0),
    c("lweather", Dead, Lweather, LVL_IMMORT, 0),
    c("medit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_MEDIT),
    c("motd", Dead, DoGenPs, 0, SCMD_MOTD),
    c("mail", Standing, DoMail, 0, 0),
    c("map", Dead, DoMap, LVL_IMMORT, 0),
    c("mcheck", Dead, DoMcheck, 0, 0),
    c("mcasters", Dead, DoMcasters, LVL_IMMORT, 0),
    c("meditate", Dead, DoMeditate, 0, 0),
    c("mercy", Dead, DoGenTog, 0, SCMD_MERCY),
    c("mlist", Dead, DoMlist, 0, 0),
    c("mount", Standing, DoMount, 0, 0),
    c("mudheal", Dead, DoMudheal, LVL_IMMORT, 0),
    c("mute", Dead, DoWizutil, LVL_IMMORT, SCMD_SQUELCH),
    c("murder", Fighting, DoHit, 0, SCMD_MURDER),
    c("mobdie", Dead, DoMobdie, LVL_IMMORT, 0),
    c("news", Sleeping, DoGenPs, 0, SCMD_NEWS),
    c("noarena", Dead, DoGenTog, 0, SCMD_NOARENA),
    c("noauction", Dead, DoGenTog, 0, SCMD_NOAUCTION),
    c("nogossip", Dead, DoGenTog, 0, SCMD_NOGOSSIP),
    c("nograts", Dead, DoGenTog, 0, SCMD_NOGRATZ),
    c("nohassle", Dead, DoGenTog, LVL_IMMORT, SCMD_NOHASSLE),
    c("nomap", Dead, DoGenTog, 0, SCMD_NOMAP),
    c("nomstack", Dead, DoGenTog, 0, SCMD_NOLOOKSTAC),
    c("norepeat", Dead, DoGenTog, 0, SCMD_NOREPEAT),
    c("noshout", Sleeping, DoGenTog, 0, SCMD_DEAF),
    c("nosummon", Dead, DoGenTog, 0, SCMD_NOSUMMON),
    c("notell", Dead, DoGenTog, 0, SCMD_NOTELL),
    c("notic", Dead, DoGenTog, 0, SCMD_NOTIC),
    c("notitle", Dead, DoWizutil, LVL_IMMORT, SCMD_NOTITLE),
    c("nowiz", Dead, DoGenTog, LVL_IMMORT, SCMD_NOWIZ),
    c("observe", Resting, DoObserve, 0, 0),
    c("order", Resting, DoOrder, 0, 0),
    c("offer", Standing, DoNotHere, 0, 0),
    c("ooc", Sleeping, DoGenComm, 0, SCMD_GOSSIP),
    c("open", Sitting, DoGenDoor, 0, SCMD_OPEN),
    c("olc", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_SAVEINFO),
    c("olist", Dead, DoOlist, LVL_IMMORT, 0),
    c("oedit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_OEDIT),
    c("put", Resting, DoPut, 0, 0),
    c("page", Dead, DoPage, LVL_IMMORT, 0),
    c("pardon", Dead, DoWizutil, LVL_IMMORT, SCMD_PARDON),
    c("peace", Dead, DoPeace, LVL_IMMORT, 0),
    c("pfileclean", Dead, DoPfileclean, LVL_IMMORT, 0),
    c("pick", Standing, DoGenDoor, 0, SCMD_PICK),
    c("players", Dead, DoPlayers, 0, 0),
    c("policy", Dead, DoGenPs, 0, SCMD_POLICIES),
    c("poofin", Dead, DoPoofset, LVL_IMMORT, SCMD_POOFIN),
    c("poofout", Dead, DoPoofset, LVL_IMMORT, SCMD_POOFOUT),
    c("postbail", Resting, DoPostbail, 0, 0),
    c("pour", Standing, DoPour, 0, SCMD_POUR),
    c("prompt", Dead, DoDisplay, 0, 0),
    c("practice", Resting, DoPractice, 0, 0),
    c("purge", Dead, DoPurge, LVL_IMMORT, 0),
    c("quaff", Resting, DoUse, 0, SCMD_QUAFF),
    c("qecho", Dead, DoQcomm, LVL_IMMORT, SCMD_QECHO),
    c("qchan", Dead, DoGenTog, 0, SCMD_QCHAN),
    c("quest", Standing, DoAutoquest, 0, 0),
    c("questmobs", Standing, DoQuestmobs, LVL_IMMORT, 0),
    c("qui", Dead, DoQuit, 0, 0),
    c("quit", Dead, DoQuit, 0, SCMD_QUIT),
    c("qsay", Resting, DoQcomm, 0, SCMD_QSAY),
    c("ram", Standing, DoGenDoor, 0, SCMD_RAM),
    c("reply", Sleeping, DoReply, 0, 0),
    c("rest", Resting, DoRest, 0, 0),
    c("respec", Resting, DoRespec, LVL_IMMORT, 0),
    c("read", Resting, DoLook, 0, SCMD_READ),
    c("rebalance", Standing, DoRebalance, LVL_IMMORT, 0),
    c("reload", Dead, DoReboot, LVL_IMMORT, 0),
    c("recall", Dead, DoRecall, 0, 0),
    c("recite", Resting, DoUse, 0, SCMD_RECITE),
    c("receive", Standing, DoMail, 0, 2),
    c("remove", Resting, DoRemove, 0, 0),
    c("rename", Dead, DoRename, LVL_IMMORT, 0),
    c("rent", Standing, DoNotHere, 0, 0),
    c("repair", Standing, DoRepair, 0, 0),
    c("report", Resting, DoReport, 0, 0),
    c("reroll", Dead, DoWizutil, LVL_IMMORT, SCMD_REROLL),
    c("rescue", Fighting, DoRescue, 0, 0),
    c("restore", Dead, DoRestore, LVL_IMMORT, 0),
    c("retreat", Dead, DoRetreat, 0, 0),
    c("return", Dead, DoReturn, 0, 0),
    c("redit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_REDIT),
    c("rewiz", Dead, DoRewiz, LVL_IMMORT, 0),
    c("reward", Dead, DoReward, LVL_IMMORT, 0),
    c("rewww", Dead, DoWhoupd, LVL_IMMORT, 0),
    c("rlist", Dead, DoRlist, LVL_IMMORT, 0),
    c("rnumber", Dead, DoRnum, LVL_IMMORT, 0),
    c("roomflags", Dead, DoGenTog, LVL_IMMORT, SCMD_ROOMFLAGS),
    c("sacrifice", Standing, DoSac, 0, 0),
    c("say", Resting, DoSay, 0, 0),
    c("'", Resting, DoSay, 0, 0),
    c("save", Sleeping, DoSave, 0, 0),
    c("score", Dead, DoStatus, 0, 0),
    c("scan", Standing, DoScan, 0, 0),
    c("scribe", Standing, DoScribe, 0, 0),
    c("school", Standing, DoSchool, 0, 0),
    c("sell", Standing, DoSell, 0, 0),
    c("send", Sleeping, DoSend, LVL_IMMORT, 0),
    c("set", Dead, DoSet, LVL_IMMORT, 0),
    c("setreboot", Dead, DoSetreboot, LVL_IMMORT, 0),
    c("sedit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_SEDIT),
    c("shout", Resting, DoGenComm, 0, SCMD_SHOUT),
    c("show", Dead, DoShow, LVL_IMMORT, 0),
    c("shutdow", Dead, DoShutdown, LVL_IMMORT, 0),
    c("shutdown", Dead, DoShutdown, LVL_IMMORT, SCMD_SHUTDOWN),
    c("sip", Resting, DoDrink, 0, SCMD_SIP),
    c("sit", Resting, DoSit, 0, 0),
    c("skillset", Sleeping, DoSkillset, LVL_IMMORT, 0),
    c("sleep", Sleeping, DoSleep, 0, 0),
    c("slowns", Dead, DoGenTog, LVL_IMMORT, SCMD_SLOWNS),
    c("sneak", Standing, DoSneak, 0, 0),
    c("slist", Dead, DoSlist, 0, 0),
    c("snoop", Dead, DoSnoop, LVL_IMMORT, 0),
    c("socials", Dead, DoCommands, 0, SCMD_SOCIALS),
    c("split", Sitting, DoSplit, 0, 0),
    c("speed", Standing, DoSpeed, 0, 0),
    c("stand", Resting, DoStand, 0, 0),
    c("stat", Dead, DoStat, LVL_IMMORT, 0),
    c("status", Dead, DoStatus, 0, 0),
    c("steal", Standing, DoSteal, 0, 0),
    c("stopauc", Dead, DoStopAuction, LVL_IMMORT, 0),
    c("switch", Dead, DoSwitch, LVL_IMMORT, 0),
    c("syslog", Dead, DoSyslog, LVL_IMMORT, 0),
    c("tedit", Dead, DoTedit, LVL_IMMORT, 0),
    c("tell", Dead, DoTell, 0, 0),
    c("take", Resting, DoGet, 0, 0),
    c("tame", Standing, DoTame, 0, 0),
    c("tan", Standing, DoTan, 0, 0),
    c("target", Fighting, DoTarget, 0, 0),
    c("taste", Resting, DoEat, 0, SCMD_TASTE),
    c("teleport", Dead, DoTeleport, LVL_IMMORT, 0),
    c("thaw", Dead, DoWizutil, LVL_IMMORT, SCMD_THAW),
    c("title", Dead, DoTitle, 0, 0),
    c("time", Dead, DoTime, 0, 0),
    c("tmobdie", Dead, DoTmobdie, LVL_IMMORT, 0),
    c("toggle", Dead, DoToggle, 0, 0),
    c("track", Standing, DoTrack, 0, 0),
    c("transfer", Sleeping, DoTrans, LVL_IMMORT, 0),
    c("train", Standing, DoTrain, 0, 0),
    c("trigedit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_TRIGEDIT),
    c("trip", Fighting, DoTrip, 0, 0),
    c("typo", Dead, DoGenWrite, 0, SCMD_TYPO),
    c("unlock", Sitting, DoGenDoor, 0, SCMD_UNLOCK),
    c("ungroup", Dead, DoUngroup, 0, 0),
    c("unban", Dead, DoUnban, LVL_IMMORT, 0),
    c("unaffect", Dead, DoWizutil, LVL_IMMORT, SCMD_UNAFFECT),
    c("uptime", Dead, DoDate, LVL_IMMORT, SCMD_UPTIME),
    c("use", Sitting, DoUse, 0, SCMD_USE),
    c("users", Dead, DoUsers, LVL_IMMORT, 0),
    c("value", Standing, DoValue, 0, 0),
    c("version", Dead, DoGenPs, 0, SCMD_VERSION),
    c("vwear", Dead, DoVwear, LVL_IMMORT, 0),
    c("visible", Resting, DoVisible, 0, 0),
    c("vnum", Dead, DoVnum, LVL_IMMORT, 0),
    c("vstat", Dead, DoVstat, LVL_IMMORT, 0),
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
    c("wiznet", Dead, DoWiznet, LVL_IMMORT, 0),
    c(";", Dead, DoWiznet, LVL_IMMORT, 0),
    c("wizhelp", Sleeping, DoCommands, 0, SCMD_WIZHELP),
    c("wizlist", Dead, DoGenPs, 0, SCMD_WIZLIST),
    c("wizlock", Dead, DoWizlock, LVL_IMMORT, 0),
    c("worth", Resting, DoWorth, 0, 0),
    c("write", Standing, DoWrite, 0, 0),
    c("wrestrict", Dead, DoWrestrict, LVL_IMMORT, 0),
    c("zedit", Dead, DoOlc, LVL_IMMORT, SCMD_OLC_ZEDIT),
    c("zreset", Dead, DoZreset, LVL_IMMORT, 0),
    c("levelme", Dead, DoLevelme, 0, 0),
    c("attach", Dead, DoAttach, LVL_IMMORT, 0),
    c("detach", Dead, DoDetach, LVL_IMMORT, 0),
    c("tlist", Dead, DoTlist, LVL_IMMORT, 0),
    c("tstat", Dead, DoTstat, LVL_IMMORT, 0),
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
