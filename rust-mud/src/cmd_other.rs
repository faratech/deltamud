// cmd_other.rs — full port of C `src/act.other.c` (the miscellaneous
// player-level commands). Every ACMD in that file is ported here against the
// single-owner GameState contract (state.rs / handler.rs / act.rs).
//
// House style (see cmd_informative.rs / cmd_item.rs): read needed values into
// locals first, then mutate / send. Re-look entities up by id each time; never
// hold a &Character/&Object across a mutation. Color is emitted as literal
// `&`-codes; the output path strips them per-player. Room/victim broadcasts go
// through act(); direct text via g.send_to_char.
//
// Contract gaps that depend on not-yet-ported systems are implemented to
// degrade exactly as the C does when those structures are empty (documented in
// the manifest): the spell_info table + call_magic effect routines (Batch 6),
// player-file persistence (the async Game loop owns save_char/Crash_*), the
// mudlog immortal channel, the per-skill app tables for the rarely-built
// dex_app_skill (ported inline here from constants.c), and the arena/jail/bank
// globals (ported as the documented default vnums used elsewhere in the port).

use crate::act::{ActArg, To, act};
use crate::character::Affect;
use crate::constants::{STR_APP, strength_apply_index};
use crate::flags::*;
use crate::handler::isname;
use crate::interpreter::{half_chop, one_argument};
use crate::object::{Object, ObjectGraphOrder, ObjectType, WearFlags, walk_object_graph};
use crate::spell_parser::{skill_name, spell_info};
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Local flag constants not present in flags.rs (structs.h values). These mirror
// the same values cmd_informative.rs already relies on.
// ---------------------------------------------------------------------------
const PRF_SUMMONABLE: i64 = 1 << 10;
const PRF_DEAF: i64 = 1 << 2;
const PRF_NOTELL: i64 = 1 << 3;
const PRF_DISPHP: i64 = 1 << 4;
const PRF_DISPMANA: i64 = 1 << 5;
const PRF_DISPMOVE: i64 = 1 << 6;
const PRF_AUTOEXIT: i64 = 1 << 7;
const PRF_ROOMFLAGS: i64 = 1 << 21;
const PRF_NOREPEAT: i64 = 1 << 11;
const PRF_NOAUCT: i64 = 1 << 18;
const PRF_NOGOSS: i64 = 1 << 19;
const PRF_NOGRATZ: i64 = 1 << 20;
const PRF_NOWIZ: i64 = 1 << 15;
const PRF_AFK: i64 = 1 << 22;
const PRF_AUTOSPLIT: i64 = 1 << 23;
const PRF_AUTOLOOT: i64 = 1 << 24;
const PRF_AUTOGOLD: i64 = 1 << 25;
const PRF_DISPEXP: i64 = 1 << 26;
const PRF_NOTIC: i64 = 1 << 27;
const PRF_NOARENA: i64 = 1 << 9;
const PRF_NOLOOKSTACK: i64 = 1 << 30;

const PRF2_DISPMOB: i64 = 1 << 5;
const PRF2_QCHAN: i64 = 1 << 0;
const PRF2_NOMAP: i64 = 1 << 2;
const PRF2_MERCY: i64 = 1 << 7;
const PRF2_ADVANCEDMAP: i64 = 1 << 8;

// PLR_* (structs.h).
const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;
const PLR_NOTITLE: i64 = 1 << 9;

// ROOM_FLAGGED bits used here (structs.h) for the magic-fizzle / peaceful
// gates. Values match room.rs RoomFlags (NO_MAGIC=1<<7, PEACEFUL=1<<4).
const ROOM_NOMAGIC: u32 = 1 << 7;
const ROOM_PEACEFUL: u32 = 1 << 4;

/// Peaceful-room theft is an administrative exception.  Body display level,
/// descriptorless NPC level, and force/script re-entry never confer it.
fn has_direct_implementor_authority(g: &GameState, ch: CharId) -> bool {
    crate::interpreter::authenticated_input_authority(g, ch)
        .is_some_and(|authority| authority.authority >= i32::from(LVL_IMPL))
}

// ---------------------------------------------------------------------------
// SCMD_* values (interpreter.h) — passed as integers by the dispatcher. Listed
// here so the match arms read like the C switch.
// ---------------------------------------------------------------------------
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
const SCMD_NOLOOKSTAC: i32 = 22;
const SCMD_NOARENA: i32 = 23;
const SCMD_NOMAP: i32 = 24;
const SCMD_MERCY: i32 = 25;
const SCMD_ADVANCEDMAP: i32 = 26;

// do_use
const SCMD_USE: i32 = 0;
const SCMD_QUAFF: i32 = 1;
const SCMD_RECITE: i32 = 2;

// do_gen_write
const SCMD_BUG: i32 = 0;
const SCMD_TYPO: i32 = 1;
const SCMD_IDEA: i32 = 2;

// do_gen_atm
const SCMD_BALANCE: i32 = 0;
const SCMD_DEPOSIT: i32 = 1;
const SCMD_WITHDRAW: i32 = 2;
const SCMD_BANK: i32 = 3;

// ---------------------------------------------------------------------------
// Game globals (constants from globals.c / config). Hardcoded to the same
// values the rest of the port uses; the integrator can wire live config later.
// ---------------------------------------------------------------------------
// C config.c: bail_multiplier = 20000, xp_multiplier = 5 (#180). The jail
// and newbie rooms come from Config (config.c jail_num=400, newbie_room=2200,
// #181); the old 3030/3070 copies here were the wrong rooms.
const BAIL_MULTIPLIER: i32 = 20_000;
const XP_MULTIPLIER: i32 = 5;
const DEFAULT_STAFF_LVL: i32 = 12;
const DEFAULT_WAND_LVL: i32 = 12;
const TOP_SPELL_DEFINE: i32 = 1099;

// CircleMUD limits.
const MAX_TITLE_LENGTH: usize = 40;

// Skill numbers (spells.h, Reserved Skill[] — DO NOT CHANGE).
const SKILL_SNEAK: u16 = 508;
const SKILL_HIDE: u16 = 503;
const SKILL_STEAL: u16 = 509;
const SKILL_TAN: u16 = 529;
const SKILL_FILLET: u16 = 530;
const SKILL_CARVE: u16 = 531;

// Item-stat / container bits (object.rs ExtraFlags mirror; structs.h ITEM_*).
const ITEM_WEAR_TAKE: u32 = 1 << 0;
const CONT_FOOD_CORPSE: i32 = 1; // GET_OBJ_VAL(cont,3)==1 marks a corpse

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/// is_number(): optional leading '-' then all digits (interpreter.c).
fn is_number(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let body = s.strip_prefix('-').unwrap_or(s);
    !body.is_empty() && body.chars().all(|c| c.is_ascii_digit())
}

fn command_atoi(g: &mut GameState, ch: CharId, s: &str) -> Option<i32> {
    match crate::text::parse_i32_atoi(s) {
        Ok(value) => Some(value),
        Err(crate::text::ParseIntError::Overflow) => {
            g.send_to_char(ch, "That number is outside the supported range.\r\n");
            None
        }
        Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
    }
}

/// AN(arg): "an" if arg begins with a vowel, else "a".
fn an(arg: &str) -> &'static str {
    match arg.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a') | Some('e') | Some('i') | Some('o') | Some('u') => "an",
        _ => "a",
    }
}

/// CAP(): capitalise first character of a buffer in place.
fn cap(s: &mut String) {
    if let Some(first) = s.chars().next() {
        if first.is_ascii_lowercase() {
            let upper = first.to_ascii_uppercase();
            s.replace_range(0..first.len_utf8(), &upper.to_string());
        }
    }
}

/// delete_doubledollar(): collapse "$$" -> "$" (utils.c).
fn delete_doubledollar(s: &str) -> String {
    s.replace("$$", "$")
}

/// CircleMUD CLASS_ABBR(ch) (utils.h:492 -> class.c `class_abbrevs[]`):
/// "Mu/Cl/Th/Wa/Ar". Delegates to the authoritative `class.rs` table — the
/// local copy had drifted to "Mag/Cle/Thi/War/Art" (#251).
fn class_abbr(class: Class) -> &'static str {
    crate::class::class_abbrev(class)
}

/// dex_app_skill[].sneak / .hide / .p_pocket (constants.c). Index clamps to
/// 0..=25 like C indexing into the fixed table.
fn dex_app_sneak(dex: i8) -> i32 {
    DEX_APP_SKILL[clamp_dex(dex)].3
}
fn dex_app_hide(dex: i8) -> i32 {
    DEX_APP_SKILL[clamp_dex(dex)].4
}
fn dex_app_p_pocket(dex: i8) -> i32 {
    DEX_APP_SKILL[clamp_dex(dex)].0
}
fn clamp_dex(dex: i8) -> usize {
    (dex.max(0) as usize).min(DEX_APP_SKILL.len() - 1)
}

/// {p_pocket, p_locks, traps, sneak, hide} indexed by dexterity (constants.c).
const DEX_APP_SKILL: [(i32, i32, i32, i32, i32); 26] = [
    (-99, -99, -90, -99, -60),
    (-90, -90, -60, -90, -50),
    (-80, -80, -40, -80, -45),
    (-70, -70, -30, -70, -40),
    (-60, -60, -30, -60, -35),
    (-50, -50, -20, -50, -30),
    (-40, -40, -20, -40, -25),
    (-30, -30, -15, -30, -20),
    (-20, -20, -15, -20, -15),
    (-15, -10, -10, -20, -10),
    (-10, -5, -10, -15, -5),
    (-5, 0, -5, -10, 0),
    (0, 0, 0, -5, 0),
    (0, 0, 0, 0, 0),
    (0, 0, 0, 0, 0),
    (0, 0, 0, 0, 0),
    (0, 5, 0, 0, 0),
    (5, 10, 0, 5, 5),
    (10, 15, 5, 10, 10),
    (15, 20, 10, 15, 15),
    (15, 20, 10, 15, 15),
    (20, 25, 10, 15, 20),
    (20, 25, 15, 20, 20),
    (25, 25, 15, 20, 20),
    (25, 30, 15, 25, 25),
    (25, 30, 15, 25, 25),
];

// ---------------------------------------------------------------------------
// Affect helpers (CircleMUD affect_to_char / affect_from_char, scoped to what
// the contract exposes: the Character::affected Vec + affect_flags bitvector).
// ---------------------------------------------------------------------------

/// affected_by_spell: is there an active affect of this spell type?
fn affected_by_spell(g: &GameState, cid: CharId, spell_type: i32) -> bool {
    g.get_char(cid)
        .map(|c| c.affected.iter().any(|a| a.spell_type == spell_type))
        .unwrap_or(false)
}

/// affect_from_char: remove every affect of `spell_type`, then recompute.
fn affect_from_char(g: &mut GameState, cid: CharId, spell_type: i32) {
    g.affect_remove_spell(cid, spell_type);
    // affect_total only re-derives ability mods; the AFF_ bitvector is rebuilt
    // from the surviving affects so the flag actually clears.
    rebuild_aff_flags(g, cid);
}

/// affect_to_char: append an affect and recompute (CircleMUD).
fn affect_to_char(g: &mut GameState, cid: CharId, af: Affect) {
    if let Some(c) = g.get_char_mut(cid) {
        c.affected.push(af);
    }
    g.affect_total(cid);
    rebuild_aff_flags(g, cid);
}

/// Recompute affect_flags from the surviving affects (mirrors the bitvector
/// half of affect_total which the contract's affect_total() also does, kept
/// here so our local add/remove are self-consistent).
fn rebuild_aff_flags(g: &mut GameState, cid: CharId) {
    let bits: i64 = g
        .get_char(cid)
        .map(|c| c.affected.iter().fold(0i64, |acc, a| acc | a.bitvector))
        .unwrap_or(0);
    // Preserve any permanently-set flags that aren't spell-derived (perm
    // affects carry their own bitvector entry, so this fold already includes
    // them). We OR rather than overwrite to avoid clobbering equipment-derived
    // flags that affect_total has just set.
    if let Some(c) = g.get_char_mut(cid) {
        c.affect_flags |= bits;
    }
}

/// check_perm_duration: true if a permanent affect (type==-1, duration==-1)
/// carries this bitvector (fight.c / appear()).
fn check_perm_duration(g: &GameState, cid: CharId, bitvector: i64) -> bool {
    g.get_char(cid)
        .map(|c| {
            c.affected
                .iter()
                .any(|a| a.spell_type == -1 && a.duration == -1 && a.bitvector & bitvector != 0)
        })
        .unwrap_or(false)
}

/// appear(): break invisibility / hide, announce arrival (fight.c).
pub(crate) fn appear(g: &mut GameState, cid: CharId) {
    // SPELL_INVISIBLE == 5 (spells.h). We clear the spell affect if present,
    // then the AFF_INVISIBLE flag, unless a permanent affect holds it.
    const SPELL_INVISIBLE: i32 = 5;
    if !check_perm_duration(g, cid, AFF_INVISIBLE) {
        if affected_by_spell(g, cid, SPELL_INVISIBLE) {
            affect_from_char(g, cid, SPELL_INVISIBLE);
        }
        if let Some(c) = g.get_char_mut(cid) {
            c.affect_flags &= !AFF_INVISIBLE;
        }
    }
    if !check_perm_duration(g, cid, AFF_HIDE) {
        if let Some(c) = g.get_char_mut(cid) {
            c.affect_flags &= !AFF_HIDE;
        }
    }

    let (still_hid, still_inv, imm) = g
        .get_char(cid)
        .map(|c| {
            (
                c.affect_flags & AFF_HIDE != 0,
                c.affect_flags & AFF_INVISIBLE != 0,
                c.player.level >= LVL_IMMORT,
            )
        })
        .unwrap_or((false, false, false));
    if !still_hid && !still_inv {
        if imm {
            act(
                g,
                "You feel a strange presence as $n appears, seemingly from nowhere.",
                false,
                cid,
                None,
                ActArg::None,
                To::Room,
            );
        } else {
            act(
                g,
                "$n slowly fades into existence.",
                false,
                cid,
                None,
                ActArg::None,
                To::Room,
            );
        }
    }
}

// ===========================================================================
// do_gen_tog — the master preference-toggle table (brief / compact / autoexit
// / nohassle / holylight / color* / nogossip / noauction / nograts / nowiz …).
// ===========================================================================

/// tog_messages[subcmd][on?]: [off, on] message pair. Order MUST match the C
/// table in act.other.c (indexed by SCMD_*).
const TOG_MESSAGES: [[&str; 2]; 27] = [
    [
        "You are now safe from summoning by other players.\r\n",
        "You may now be summoned by other players.\r\n",
    ],
    ["Nohassle disabled.\r\n", "Nohassle enabled.\r\n"],
    ["Brief mode off.\r\n", "Brief mode on.\r\n"],
    ["Compact mode off.\r\n", "Compact mode on.\r\n"],
    [
        "You can now hear tells.\r\n",
        "You are now deaf to tells.\r\n",
    ],
    [
        "You can now hear auctions.\r\n",
        "You are now deaf to auctions.\r\n",
    ],
    [
        "You can now hear shouts.\r\n",
        "You are now deaf to shouts.\r\n",
    ],
    [
        "You can now hear gossip.\r\n",
        "You are now deaf to gossip.\r\n",
    ],
    [
        "You can now hear the congratulation messages.\r\n",
        "You are now deaf to the congratulation messages.\r\n",
    ],
    [
        "You can now hear the Wiz-channel.\r\n",
        "You are now deaf to the Wiz-channel.\r\n",
    ],
    [
        "You are no longer part of the Quest.\r\n",
        "Okay, you are part of the Quest!\r\n",
    ],
    [
        "You will no longer see the room flags.\r\n",
        "You will now see the room flags.\r\n",
    ],
    [
        "You will now have your communication repeated.\r\n",
        "You will no longer have your communication repeated.\r\n",
    ],
    ["HolyLight mode off.\r\n", "HolyLight mode on.\r\n"],
    [
        "Nameserver_is_slow changed to NO; IP addresses will now be resolved.\r\n",
        "Nameserver_is_slow changed to YES; sitenames will no longer be resolved.\r\n",
    ],
    ["Autoexits disabled.\r\n", "Autoexits enabled.\r\n"],
    ["Autosplit disabled.\r\n", "Autosplit enabled.\r\n"],
    ["Autoloot disabled.\r\n", "Autoloot enabled.\r\n"],
    ["Autogold disabled.\r\n", "Autogold enabled.\r\n"],
    [
        "You are no longer set as away from keyboard.\r\n",
        "You have been set as away from keyboard.\r\n",
    ],
    [
        "You will now hear the bells of Anacreon ring.\r\n",
        "You will no longer hear the bells of Anacreon.\r\n",
    ],
    [
        "You will now be visible in the 'who' list.\r\n",
        "You will no longer be visible in the 'who' list.\r\n",
    ],
    ["Mob stacking enabled.\r\n", "Mob stacking disabled.\r\n"],
    [
        "You can now hear the arena announcements.\r\n",
        "You are now deaf to the arena announcements.\r\n",
    ],
    [
        "You will now see the world map of Deltania.\r\n",
        "You will no longer see the world map of Deltania.\r\n",
    ],
    [
        "You no longer have mercy on your enemies, and shall &Rkill&n them.\r\n",
        "You now have mercy on your enemies, and shall spare their lives.\r\n",
    ],
    [
        "You will now see the standard map.\r\n",
        "You will now see the advanced map.\r\n",
    ],
];

/// Toggle a PRF_ bit and return the resulting state (PRF_TOG_CHK semantics:
/// TOGGLE_BIT then test the bit).
fn prf_tog_chk(g: &mut GameState, ch: CharId, flag: i64) -> bool {
    let mut state = false;
    if let Some(c) = g.get_char_mut(ch) {
        c.prf_flags ^= flag;
        state = c.prf_flags & flag != 0;
    }
    state
}

fn prf2_tog_chk(g: &mut GameState, ch: CharId, flag: i64) -> bool {
    let mut state = false;
    if let Some(c) = g.get_char_mut(ch) {
        c.prf2_flags ^= flag;
        state = c.prf2_flags & flag != 0;
    }
    state
}

pub fn do_gen_tog(g: &mut GameState, ch: CharId, _arg: &str, subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        return;
    }

    let result: bool = match subcmd {
        SCMD_NOSUMMON => {
            let (killer, in_jail) = g
                .get_char(ch)
                .map(|c| {
                    (
                        c.act_flags & PLR_KILLER != 0,
                        c.in_room == g.real_room(g.config.jail_num),
                    )
                })
                .unwrap_or((false, false));
            // pk_allowed is off by default.
            if killer && in_jail {
                g.send_to_char(
                    ch,
                    "Sorry. You can't make yourself summonable right now.\r\n",
                );
                return;
            }
            prf_tog_chk(g, ch, PRF_SUMMONABLE)
        }
        SCMD_NOHASSLE => prf_tog_chk(g, ch, PRF_NOHASSLE),
        SCMD_BRIEF => prf_tog_chk(g, ch, PRF_BRIEF),
        SCMD_NOLOOKSTAC => prf_tog_chk(g, ch, PRF_NOLOOKSTACK),
        SCMD_COMPACT => prf_tog_chk(g, ch, PRF_COMPACT),
        SCMD_NOTELL => prf_tog_chk(g, ch, PRF_NOTELL),
        SCMD_NOAUCTION => {
            let (killer, in_jail) = g
                .get_char(ch)
                .map(|c| {
                    (
                        c.act_flags & PLR_KILLER != 0,
                        c.in_room == g.real_room(g.config.jail_num),
                    )
                })
                .unwrap_or((false, false));
            if killer && in_jail {
                g.send_to_char(
                    ch,
                    "Sorry. You can't listen to the auction channel right now.\r\n",
                );
                return;
            }
            prf_tog_chk(g, ch, PRF_NOAUCT)
        }
        SCMD_DEAF => prf_tog_chk(g, ch, PRF_DEAF),
        SCMD_NOGOSSIP => prf_tog_chk(g, ch, PRF_NOGOSS),
        SCMD_NOGRATZ => prf_tog_chk(g, ch, PRF_NOGRATZ),
        SCMD_NOWIZ => prf_tog_chk(g, ch, PRF_NOWIZ),
        SCMD_NOARENA => prf_tog_chk(g, ch, PRF_NOARENA),
        SCMD_QCHAN => prf2_tog_chk(g, ch, PRF2_QCHAN),
        SCMD_ROOMFLAGS => prf_tog_chk(g, ch, PRF_ROOMFLAGS),
        SCMD_NOREPEAT => prf_tog_chk(g, ch, PRF_NOREPEAT),
        SCMD_HOLYLIGHT => prf_tog_chk(g, ch, PRF_HOLYLIGHT),
        SCMD_SLOWNS => {
            // C act.other.c:1707: `result = (nameserver_is_slow =
            // !nameserver_is_slow)` — the stored state flips and the message
            // reports the NEW value, starting from YES (config.c:254). So the
            // first toggle prints "changed to NO", the second "changed to YES".
            g.nameserver_is_slow = !g.nameserver_is_slow;
            g.nameserver_is_slow
        }
        SCMD_AUTOEXIT => prf_tog_chk(g, ch, PRF_AUTOEXIT),
        SCMD_AUTOSPLIT => prf_tog_chk(g, ch, PRF_AUTOSPLIT),
        SCMD_AUTOLOOT => {
            let r = prf_tog_chk(g, ch, PRF_AUTOLOOT);
            if g.get_char(ch)
                .map(|c| c.prf_flags & PRF_AUTOGOLD != 0)
                .unwrap_or(false)
            {
                if let Some(c) = g.get_char_mut(ch) {
                    c.prf_flags &= !PRF_AUTOGOLD;
                }
            }
            r
        }
        SCMD_AUTOGOLD => {
            let r = prf_tog_chk(g, ch, PRF_AUTOGOLD);
            if g.get_char(ch)
                .map(|c| c.prf_flags & PRF_AUTOLOOT != 0)
                .unwrap_or(false)
            {
                if let Some(c) = g.get_char_mut(ch) {
                    c.prf_flags &= !PRF_AUTOLOOT;
                }
            }
            r
        }
        SCMD_AFK => {
            let r = prf_tog_chk(g, ch, PRF_AFK);
            if g.get_char(ch)
                .map(|c| c.prf_flags & PRF_AFK != 0)
                .unwrap_or(false)
            {
                act(
                    g,
                    "$n has gone AFK.",
                    true,
                    ch,
                    None,
                    ActArg::None,
                    To::Room,
                );
            } else {
                act(
                    g,
                    "$n has come back from AFK.",
                    true,
                    ch,
                    None,
                    ActArg::None,
                    To::Room,
                );
            }
            r
        }
        SCMD_NOTIC => prf_tog_chk(g, ch, PRF_NOTIC),
        SCMD_NOMAP => prf2_tog_chk(g, ch, PRF2_NOMAP),
        SCMD_MERCY => prf2_tog_chk(g, ch, PRF2_MERCY),
        SCMD_ADVANCEDMAP => prf2_tog_chk(g, ch, PRF2_ADVANCEDMAP),
        _ => {
            // SYSERR: Unknown subcmd in do_gen_toggle
            return;
        }
    };

    let idx = subcmd as usize;
    if idx < TOG_MESSAGES.len() {
        let msg = if result {
            TOG_MESSAGES[idx][1]
        } else {
            TOG_MESSAGES[idx][0]
        };
        g.send_to_char(ch, msg);
    }
}

// ===========================================================================
// do_display — the prompt configuration command.
// ===========================================================================

pub fn do_display(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        g.send_to_char(ch, "Mosters don't need displays.  Go away.\r\n");
        return;
    }
    let argument = argument.trim();

    if argument.is_empty() {
        g.send_to_char(ch, "Usage: prompt { H | M | V | E | F } | all | none }\r\n");
        return;
    }

    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);

    if argument.eq_ignore_ascii_case("on") || argument.eq_ignore_ascii_case("all") {
        if let Some(c) = g.get_char_mut(ch) {
            if level < LVL_IMMORT {
                c.prf_flags |= PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE | PRF_DISPEXP;
                c.prf2_flags |= PRF2_DISPMOB;
            } else {
                c.prf_flags |= PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE;
                c.prf2_flags |= PRF2_DISPMOB;
            }
        }
    } else {
        if let Some(c) = g.get_char_mut(ch) {
            c.prf_flags &= !(PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE | PRF_DISPEXP);
            c.prf2_flags &= !PRF2_DISPMOB;
        }
        // LVL_HERO == 100 (structs.h); the 'e' flag is denied to heroes+.
        const LVL_HERO: Level = 100;
        for cch in argument.chars() {
            match cch.to_ascii_lowercase() {
                'h' => {
                    if let Some(c) = g.get_char_mut(ch) {
                        c.prf_flags |= PRF_DISPHP;
                    }
                }
                'm' => {
                    if let Some(c) = g.get_char_mut(ch) {
                        c.prf_flags |= PRF_DISPMANA;
                    }
                }
                'v' => {
                    if let Some(c) = g.get_char_mut(ch) {
                        c.prf_flags |= PRF_DISPMOVE;
                    }
                }
                'f' => {
                    if let Some(c) = g.get_char_mut(ch) {
                        c.prf2_flags |= PRF2_DISPMOB;
                    }
                }
                'e' => {
                    if level < LVL_HERO {
                        if let Some(c) = g.get_char_mut(ch) {
                            c.prf_flags |= PRF_DISPEXP;
                        }
                    }
                }
                _ => {
                    g.send_to_char(ch, "Usage: prompt { H | M | V | E | F } | all | none }\r\n");
                    return;
                }
            }
        }
    }

    g.send_to_char(ch, "Ok.\r\n");
}

// ===========================================================================
// do_quit / do_save
// ===========================================================================

/// item_count: equipment + inventory + bounded container contents (utils.c).
///
/// The C implementation recurses. Use the shared graph walker here so a
/// corrupt legacy graph cannot overflow the game thread while a player quits.
fn item_count(g: &GameState, ch: CharId) -> i32 {
    let (eq, carrying) = match g.get_char(ch) {
        Some(c) => (
            c.equipment.iter().flatten().copied().collect::<Vec<_>>(),
            c.carrying.clone(),
        ),
        None => return 0,
    };
    let walk = walk_object_graph(
        eq.into_iter().chain(carrying),
        ObjectGraphOrder::Preorder,
        "quit item_count",
        |oid| {
            g.get_obj(oid).map(|obj| {
                if obj.obj_type == ObjectType::Container {
                    obj.contains.clone()
                } else {
                    Vec::new()
                }
            })
        },
    );
    i32::try_from(walk.visits.len()).unwrap_or(i32::MAX)
}

/// quit_warning(): the "you'll lose your stuff" nag.
fn quit_warning(g: &mut GameState, ch: CharId) {
    g.send_to_char(
        ch,
        "You will lose all your stuff! You must rent at an inn.\r\nIf you have still want to quit, type quit y.\r\n",
    );
}

/// really_quit(): the actual leave-the-game path. The async Game loop owns
/// player-file persistence + rent-save (Crash_rentsave / save_char); we mirror
/// the C decision logic and announce, then request descriptor close so the
/// loop performs the save+extract exactly as C extract_char does.
fn really_quit(g: &mut GameState, ch: CharId) {
    let (is_npc, has_desc, pos, invis_lev) = match g.get_char(ch) {
        Some(c) => (c.is_npc, c.desc.is_some(), c.position, c.invis_level),
        None => return,
    };
    if is_npc || !has_desc {
        return;
    }

    if pos == Position::Fighting {
        g.send_to_char(ch, "No way!  You're fighting for your life!\r\n");
        return;
    } else if pos < Position::Stunned {
        g.send_to_char(ch, "You die before your time...\r\n");
        // C act.other.c:442: die(ch, NULL) - a full combat death (corpse, XP
        // penalty, condition/criminal side effects). The old path just closed
        // the socket, saving the player intact at negative HP: an exploitable
        // death dodge (#310).
        crate::combat::die(g, None, ch);
        return;
    }

    if invis_lev == 0 {
        act(
            g,
            "$n has left the game.",
            true,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
    }
    g.send_to_char(
        ch,
        "\r\nYou decide to sit down and rest. You soon fade into a deep sleep.\r\n",
    );

    // Kill any other sockets connected to the same player (anti-dupe). Compare
    // by persistent idnum, like C's GET_IDNUM.
    let idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(-1);
    let my_conn = g.get_char(ch).and_then(|c| c.desc);
    if idnum >= 0 {
        let conns: Vec<ConnId> = g.descriptors.keys().copied().collect();
        for conn in conns {
            if Some(conn) == my_conn {
                continue;
            }
            let other = g.descriptors.get(&conn).and_then(|d| d.character);
            if let Some(oc) = other {
                if g.get_char(oc).map(|c| c.idnum).unwrap_or(-2) == idnum {
                    if let Some(d) = g.descriptors.get_mut(&conn) {
                        d.state = crate::connection::ConState::Close;
                    }
                }
            }
        }
    }

    request_quit_close(g, ch);
}

/// Request that the Game loop close this descriptor (which triggers the save +
/// extract_char path the C really_quit performs inline).
fn request_quit_close(g: &mut GameState, ch: CharId) {
    if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.state = crate::connection::ConState::Close;
        }
    }
}

pub fn do_quit(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);

    if arg.eq_ignore_ascii_case("y") || level >= LVL_IMMORT {
        really_quit(g, ch);
    } else if item_count(g, ch) == 0 {
        g.send_to_char(ch, "Holding no possessions, you decide to quit..\r\n");
        really_quit(g, ch);
    } else if arg.is_empty() && level < LVL_IMMORT {
        quit_warning(g, ch);
    }
}

pub fn do_save(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    if g.get_char(ch)
        .map(|c| c.is_npc || c.desc.is_none())
        .unwrap_or(true)
    {
        return;
    }
    // C act.other.c:490-507 do_save: write_aliases, save_char(NOWHERE),
    // Crash_crashsave, and House_crashsave when the room is flagged
    // ROOM_HOUSE_CRASH. Before #308 this only printed the acknowledgment and
    // persisted nothing (#308).
    let (name, in_room) = match g.get_char(ch) {
        Some(c) => (c.player.name.clone(), c.in_room),
        None => return,
    };
    // save_char(ch, NOWHERE): SQL row via the async bridge.
    g.request_player_save(ch);
    // Crash_crashsave: rent/crash object file.
    let lib = g.config.lib_path.clone();
    crate::objsave::crash_save(g, ch, &lib);
    // write_aliases(ch).
    let idnum = g.get_char(ch).map(|c| c.idnum).unwrap_or(0);
    let _ = crate::alias::write_aliases(&lib, &name, idnum);
    // House_crashsave when the room is flagged ROOM_HOUSE_CRASH.
    if let Some(rnum) = in_room {
        // C ROOM_HOUSE_CRASH is bit 12; RoomFlags names that bit NO_RECALL
        // (see the room.rs 12-15 naming note), so test the raw bit.
        let (vnum, is_house_crash) = match g.room_opt(rnum) {
            Some(r) => (r.number, r.room_flags.bits() & (1 << 12) != 0),
            None => (0, false),
        };
        if is_house_crash {
            crate::house::house_crashsave(g, vnum);
        }
    }
    g.send_to_char(ch, &format!("Saving {}.\r\n", name));
}

// ===========================================================================
// do_practice — list skills (no in-guild practice).
// ===========================================================================

pub fn do_practice(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    if !arg.is_empty() {
        g.send_to_char(ch, "You can only practice skills in your guild.\r\n");
    } else {
        list_skills(g, ch);
    }
}

/// list_skills (spec_procs.c:140-171): the practice-session header followed by
/// every skill/spell the character's class+level can know, alphabetically
/// ordered (C spell_sort_info[]) and paged through the descriptor pager.
/// `%-20s %s` renders each row as C does.
fn list_skills(g: &mut GameState, ch: CharId) {
    // MAX_STRING_LENGTH (structs.h:569).
    const MAX_STRING_LENGTH: usize = 16384;

    let (learn, class, level) = g
        .get_char(ch)
        .map(|c| (c.spells_to_learn, c.player.class, c.player.level))
        .unwrap_or((0, Class::Warrior, 1));

    // C: `if (!GET_PRACTICES(ch)) strcpy(buf, "You have no practice sessions
    // remaining.") else sprintf("You have %d practice session%s remaining.")`.
    let mut buf = if learn == 0 {
        "You have no practice sessions remaining.\r\n".to_string()
    } else {
        format!(
            "You have {} practice session{} remaining.\r\n",
            learn,
            if learn == 1 { "" } else { "s" }
        )
    };
    // SPLSKL(ch) = prac_types[prac_params[PRAC_TYPE][class]] ("spell"/"skill").
    let kind = if crate::class::prac_type_is_spell(class) {
        "spell"
    } else {
        "skill"
    };
    buf.push_str(&format!("You know of the following {}s:\r\n", kind));

    let mut buf2 = buf.clone();
    // spell_sort_info[] (spec_procs.c:74-90): the spell/skill indices sorted by
    // strcmp() on their spells[] names.
    let mut sort_info: Vec<i32> = (1..MAX_SKILLS as i32).collect();
    sort_info.sort_by(|&a, &b| skill_name(a).cmp(skill_name(b)));

    for &i in &sort_info {
        if buf2.len() >= MAX_STRING_LENGTH - 32 {
            buf2.push_str("**OVERFLOW**\r\n");
            break;
        }
        if level as i32 >= spell_info(i).min_level[class as usize] {
            let prof = g.get_char(ch).map(|c| c.skill(i as u16)).unwrap_or(0);
            buf2.push_str(&format!(
                "{:<20} {}\r\n",
                skill_name(i),
                how_good(prof as i32)
            ));
        }
    }

    // C hands the whole block to page_string(ch->desc, buf2, 1).
    let conn = g.get_char(ch).and_then(|c| c.desc);
    match conn {
        Some(conn) => crate::modify::page_string(g, conn, &buf2),
        None => g.send_to_char(ch, &buf2),
    }
}

/// how_good(percent) (spec_procs.c): proficiency descriptor + numeric percent,
/// e.g. " (good) 75".
fn how_good(percent: i32) -> String {
    let label = if percent == 0 {
        " (not learned)"
    } else if percent <= 10 {
        " (awful)"
    } else if percent <= 20 {
        " (bad)"
    } else if percent <= 40 {
        " (poor)"
    } else if percent <= 55 {
        " (average)"
    } else if percent <= 70 {
        " (fair)"
    } else if percent <= 80 {
        " (good)"
    } else if percent <= 85 {
        " (very good)"
    } else {
        " (superb)"
    };
    format!("{} {}", label, percent)
}

// ===========================================================================
// do_visible
// ===========================================================================

pub fn do_visible(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);

    if level >= LVL_IMMORT {
        perform_immort_vis(g, ch);
        return;
    }

    if g.get_char(ch)
        .map(|c| c.affect_flags & AFF_INVISIBLE != 0)
        .unwrap_or(false)
    {
        appear(g, ch);
        g.send_to_char(ch, "You break the spell of invisibility.\r\n");
    } else {
        g.send_to_char(ch, "You are already visible.\r\n");
    }
}

/// perform_immort_vis (act.wizard.c).
fn perform_immort_vis(g: &mut GameState, ch: CharId) {
    let (invis_lev, aff) = g
        .get_char(ch)
        .map(|c| (c.invis_level, c.affect_flags))
        .unwrap_or((0, 0));
    if invis_lev == 0 && aff & (AFF_HIDE | AFF_INVISIBLE) == 0 {
        g.send_to_char(ch, "You are already fully visible.\r\n");
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.invis_level = 0;
    }
    appear(g, ch);
    g.send_to_char(ch, "You are now fully visible.\r\n");
}

// ===========================================================================
// do_title
// ===========================================================================

pub fn do_title(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let argument = delete_doubledollar(argument.trim());

    let (is_npc, notitle) = g
        .get_char(ch)
        .map(|c| (c.is_npc, c.act_flags & PLR_NOTITLE != 0))
        .unwrap_or((false, false));

    if is_npc {
        g.send_to_char(ch, "Your title is fine... go away.\r\n");
    } else if notitle {
        g.send_to_char(
            ch,
            "You can't title yourself -- you shouldn't have abused it!\r\n",
        );
    } else if argument.contains('(') || argument.contains(')') {
        g.send_to_char(ch, "Titles can't contain the ( or ) characters.\r\n");
    } else if argument.contains('[') || argument.contains(']') {
        g.send_to_char(ch, "Titles can't contain the [ or ] characters.\r\n");
    } else if argument.contains('<') || argument.contains('>') {
        g.send_to_char(ch, "Titles can't contain the < or > characters.\r\n");
    } else if argument.len() > MAX_TITLE_LENGTH {
        g.send_to_char(
            ch,
            &format!(
                "Sorry, titles can't be longer than {} characters.\r\n",
                MAX_TITLE_LENGTH
            ),
        );
    } else {
        set_title(g, ch, &argument);
        let (name, title) = g
            .get_char(ch)
            .map(|c| (c.player.name.clone(), c.get_title()))
            .unwrap_or_default();
        g.send_to_char(ch, &format!("Okay, you're now {} {}.\r\n", name, title));
    }
}

/// set_title (utils.c): empty -> default title; else store verbatim.
fn set_title(g: &mut GameState, ch: CharId, title: &str) {
    if let Some(c) = g.get_char_mut(ch) {
        if title.is_empty() {
            c.player.title = None;
        } else {
            c.player.title = Some(title.to_string());
        }
    }
}

// ===========================================================================
// do_group / do_ungroup / do_report / do_split  (the grouping suite)
// ===========================================================================

/// CAN_GROUP(ch, vict): levels within +/-10 of each other.
fn can_group(g: &GameState, ch: CharId, vict: CharId) -> bool {
    let l1 = g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(0);
    let l2 = g.get_char(vict).map(|c| c.player.level as i32).unwrap_or(0);
    let d = l2 - l1;
    (-10..=10).contains(&d)
}

/// perform_group: try to add `vict` to `ch`'s group (CircleMUD). Returns true
/// if newly grouped.
fn perform_group(g: &mut GameState, ch: CharId, vict: CharId) -> bool {
    let already = g
        .get_char(vict)
        .map(|c| c.affect_flags & AFF_GROUP != 0)
        .unwrap_or(false);
    if already || !g.can_see(ch, vict) {
        return false;
    }
    if !can_group(g, ch, vict) {
        act(
            g,
            "$N is out of your grouping range.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
        return false;
    }

    // Existing grouped followers must all be within range of the new member.
    let followers = g
        .get_char(ch)
        .map(|c| c.followers.clone())
        .unwrap_or_default();
    for f in followers {
        let grouped = g
            .get_char(f)
            .map(|c| c.affect_flags & AFF_GROUP != 0)
            .unwrap_or(false);
        if !grouped {
            continue;
        }
        if !can_group(g, vict, f) {
            let (vn, fn_) = (
                g.get_char(vict)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default(),
                g.get_char(f)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default(),
            );
            g.send_to_char(
                ch,
                &format!(
                    "{} may not group with {} (they are not within grouping range).\r\n",
                    vn, fn_
                ),
            );
            return false;
        }
    }

    if let Some(c) = g.get_char_mut(vict) {
        c.affect_flags |= AFF_GROUP;
    }

    if ch != vict {
        act(
            g,
            "$N is now a member of your group.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
    }
    act(
        g,
        "You are now a member of $n's group.",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::Vict,
    );
    act(
        g,
        "$N is now a member of $n's group.",
        false,
        ch,
        None,
        ActArg::Char(vict),
        To::NotVict,
    );
    true
}

/// print_group: show the current group roster (CircleMUD).
fn print_group(g: &mut GameState, ch: CharId) {
    if g.get_char(ch)
        .map(|c| c.affect_flags & AFF_GROUP == 0)
        .unwrap_or(true)
    {
        g.send_to_char(ch, "But you are not the member of a group!\r\n");
        return;
    }
    g.send_to_char(ch, "Your group consists of:\r\n");

    let k = g.get_char(ch).and_then(|c| c.master).unwrap_or(ch);

    if g.get_char(k)
        .map(|c| c.affect_flags & AFF_GROUP != 0)
        .unwrap_or(false)
    {
        let line = group_line(g, k, true);
        act(g, &line, false, ch, None, ActArg::Char(k), To::Char);
    }

    let followers = g
        .get_char(k)
        .map(|c| c.followers.clone())
        .unwrap_or_default();
    for f in followers {
        if g.get_char(f)
            .map(|c| c.affect_flags & AFF_GROUP == 0)
            .unwrap_or(true)
        {
            continue;
        }
        let line = group_line(g, f, false);
        act(g, &line, false, ch, None, ActArg::Char(f), To::Char);
    }
}

fn group_line(g: &GameState, who: CharId, head: bool) -> String {
    let c = match g.get_char(who) {
        Some(c) => c,
        None => return String::new(),
    };
    format!(
        "     [{:3}H {:3}M {:3}V] [{:2} {}] $N{}",
        c.points.hit,
        c.points.mana,
        c.points.move_points,
        c.player.level,
        class_abbr(c.player.class),
        if head { " (Head of group)" } else { "" }
    )
}

pub fn do_group(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        print_group(g, ch);
        return;
    }

    if g.get_char(ch).map(|c| c.master.is_some()).unwrap_or(false) {
        act(
            g,
            "You can not enroll group members without being head of a group.",
            false,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }

    if arg.eq_ignore_ascii_case("all") {
        perform_group(g, ch, ch);
        let mut found = 0;
        let followers = g
            .get_char(ch)
            .map(|c| c.followers.clone())
            .unwrap_or_default();
        for f in followers {
            if perform_group(g, ch, f) {
                found += 1;
            }
        }
        if found == 0 {
            g.send_to_char(ch, "Everyone following you is already in your group.\r\n");
        }
        return;
    }

    let vict = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No-one by that name here.\r\n");
            return;
        }
    };

    let vmaster = g.get_char(vict).and_then(|c| c.master);
    if vmaster != Some(ch) && vict != ch {
        act(
            g,
            "$N must follow you to enter your group.",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Char,
        );
    } else if g
        .get_char(vict)
        .map(|c| c.affect_flags & AFF_GROUP == 0)
        .unwrap_or(true)
    {
        perform_group(g, ch, vict);
    } else {
        if ch != vict {
            act(
                g,
                "$N is no longer a member of your group.",
                false,
                ch,
                None,
                ActArg::Char(vict),
                To::Char,
            );
        }
        act(
            g,
            "You have been kicked out of $n's group!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::Vict,
        );
        act(
            g,
            "$N has been kicked out of $n's group!",
            false,
            ch,
            None,
            ActArg::Char(vict),
            To::NotVict,
        );
        if let Some(c) = g.get_char_mut(vict) {
            c.affect_flags &= !AFF_GROUP;
        }
    }
}

pub fn do_ungroup(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        let (has_master, grouped) = g
            .get_char(ch)
            .map(|c| (c.master.is_some(), c.affect_flags & AFF_GROUP != 0))
            .unwrap_or((false, false));
        if has_master || !grouped {
            g.send_to_char(ch, "But you lead no group!\r\n");
            return;
        }
        let name = g
            .get_char(ch)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        let msg = format!("{} has disbanded the group.\r\n", name);
        let followers = g
            .get_char(ch)
            .map(|c| c.followers.clone())
            .unwrap_or_default();
        for f in followers {
            if g.get_char(f)
                .map(|c| c.affect_flags & AFF_GROUP != 0)
                .unwrap_or(false)
            {
                if let Some(c) = g.get_char_mut(f) {
                    c.affect_flags &= !AFF_GROUP;
                }
                g.send_to_char(f, &msg);
                if g.get_char(f)
                    .map(|c| c.affect_flags & AFF_CHARM == 0)
                    .unwrap_or(true)
                {
                    stop_follower(g, f);
                }
            }
        }
        if let Some(c) = g.get_char_mut(ch) {
            c.affect_flags &= !AFF_GROUP;
        }
        g.send_to_char(ch, "You disband the group.\r\n");
        return;
    }

    let tch = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "There is no such person!\r\n");
            return;
        }
    };
    if g.get_char(tch).and_then(|c| c.master) != Some(ch) {
        g.send_to_char(ch, "That person is not following you!\r\n");
        return;
    }
    if g.get_char(tch)
        .map(|c| c.affect_flags & AFF_GROUP == 0)
        .unwrap_or(true)
    {
        g.send_to_char(ch, "That person isn't in your group.\r\n");
        return;
    }

    if let Some(c) = g.get_char_mut(tch) {
        c.affect_flags &= !AFF_GROUP;
    }
    act(
        g,
        "$N is no longer a member of your group.",
        false,
        ch,
        None,
        ActArg::Char(tch),
        To::Char,
    );
    act(
        g,
        "You have been kicked out of $n's group!",
        false,
        ch,
        None,
        ActArg::Char(tch),
        To::Vict,
    );
    act(
        g,
        "$N has been kicked out of $n's group!",
        false,
        ch,
        None,
        ActArg::Char(tch),
        To::NotVict,
    );

    if g.get_char(tch)
        .map(|c| c.affect_flags & AFF_CHARM == 0)
        .unwrap_or(true)
    {
        stop_follower(g, tch);
    }
}

/// stop_follower (handler.c): detach `ch` from its master and announce. The
/// follow system stores master + followers ids; we keep both sides consistent.
fn stop_follower(g: &mut GameState, ch: CharId) {
    let master = match g.get_char(ch).and_then(|c| c.master) {
        Some(m) => m,
        None => return,
    };
    let charmed = g
        .get_char(ch)
        .map(|c| c.affect_flags & AFF_CHARM != 0)
        .unwrap_or(false);
    if charmed {
        act(
            g,
            "You realize that $N is a jerk!",
            false,
            ch,
            None,
            ActArg::Char(master),
            To::Char,
        );
        act(
            g,
            "$n realizes that $N is a jerk!",
            false,
            ch,
            None,
            ActArg::Char(master),
            To::NotVict,
        );
        act(
            g,
            "$n hates your guts!",
            false,
            ch,
            None,
            ActArg::Char(master),
            To::Vict,
        );
    } else {
        act(
            g,
            "You stop following $N.",
            false,
            ch,
            None,
            ActArg::Char(master),
            To::Char,
        );
        act(
            g,
            "$n stops following $N.",
            true,
            ch,
            None,
            ActArg::Char(master),
            To::NotVict,
        );
        act(
            g,
            "$n stops following you.",
            true,
            ch,
            None,
            ActArg::Char(master),
            To::Vict,
        );
    }
    if let Some(m) = g.get_char_mut(master) {
        m.followers.retain(|&f| f != ch);
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.master = None;
    }
}

pub fn do_report(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    if g.get_char(ch)
        .map(|c| c.affect_flags & AFF_GROUP == 0)
        .unwrap_or(true)
    {
        g.send_to_char(ch, "But you are not a member of any group!\r\n");
        return;
    }

    let mut buf = {
        let c = match g.get_char(ch) {
            Some(c) => c,
            None => return,
        };
        format!(
            "{} reports: {}/{}hp, {}/{}mp, {}/{}mv\r\n",
            c.player.name,
            c.points.hit,
            c.points.max_hit,
            c.points.mana,
            c.points.max_mana,
            c.points.move_points,
            c.points.max_move
        )
    };
    cap(&mut buf);

    let k = g.get_char(ch).and_then(|c| c.master).unwrap_or(ch);
    let followers = g
        .get_char(k)
        .map(|c| c.followers.clone())
        .unwrap_or_default();
    for f in followers {
        let grouped = g
            .get_char(f)
            .map(|c| c.affect_flags & AFF_GROUP != 0)
            .unwrap_or(false);
        if grouped && f != ch {
            g.send_to_char(f, &buf);
        }
    }
    if k != ch {
        g.send_to_char(k, &buf);
    }
    g.send_to_char(ch, &buf);
}

pub fn do_split(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        return;
    }
    let (arg, _) = one_argument(argument);

    if !is_number(&arg) {
        g.send_to_char(
            ch,
            "How many coins do you wish to split with your group?\r\n",
        );
        return;
    }

    let Some(amount) = command_atoi(g, ch, &arg) else {
        return;
    };
    if amount <= 0 {
        g.send_to_char(ch, "Sorry, you can't do that.\r\n");
        return;
    }
    let my_gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
    if amount > my_gold {
        g.send_to_char(ch, "You don't seem to have that much gold to split.\r\n");
        return;
    }

    let my_room = g.get_char(ch).and_then(|c| c.in_room);
    let k = g.get_char(ch).and_then(|c| c.master).unwrap_or(ch);

    // Count eligible group members present in the room.
    let mut num = 0;
    let k_grouped = g
        .get_char(k)
        .map(|c| c.affect_flags & AFF_GROUP != 0)
        .unwrap_or(false);
    let k_room = g.get_char(k).and_then(|c| c.in_room);
    if k_grouped && k_room == my_room {
        num = 1;
    }
    let followers = g
        .get_char(k)
        .map(|c| c.followers.clone())
        .unwrap_or_default();
    for f in &followers {
        let (grouped, is_npc, froom) = g
            .get_char(*f)
            .map(|c| (c.affect_flags & AFF_GROUP != 0, c.is_npc, c.in_room))
            .unwrap_or((false, true, None));
        if grouped && !is_npc && froom == my_room {
            num += 1;
        }
    }

    let im_grouped = g
        .get_char(ch)
        .map(|c| c.affect_flags & AFF_GROUP != 0)
        .unwrap_or(false);
    if !(num > 0 && im_grouped) {
        g.send_to_char(ch, "With whom do you wish to share your gold?\r\n");
        return;
    }

    let share = amount / num;
    let name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();

    // ch pays out share*(num-1).
    if let Some(c) = g.get_char_mut(ch) {
        crate::gold::debit(
            c,
            crate::gold::Account::Carried,
            i64::from(share) * i64::from(num - 1),
        );
    }

    // Head of group (if it's not ch).
    let k_npc = g.get_char(k).map(|c| c.is_npc).unwrap_or(true);
    if k_grouped && k_room == my_room && !k_npc && k != ch {
        if let Some(c) = g.get_char_mut(k) {
            crate::gold::credit(c, crate::gold::Account::Carried, i64::from(share));
        }
        g.send_to_char(
            k,
            &format!(
                "{} splits {} coins; you receive {}.\r\n",
                name, amount, share
            ),
        );
    }

    for f in &followers {
        let (grouped, is_npc, froom) = g
            .get_char(*f)
            .map(|c| (c.affect_flags & AFF_GROUP != 0, c.is_npc, c.in_room))
            .unwrap_or((false, true, None));
        if grouped && !is_npc && froom == my_room && *f != ch {
            if let Some(c) = g.get_char_mut(*f) {
                crate::gold::credit(c, crate::gold::Account::Carried, i64::from(share));
            }
            g.send_to_char(
                *f,
                &format!(
                    "{} splits {} coins; you receive {}.\r\n",
                    name, amount, share
                ),
            );
        }
    }

    g.send_to_char(
        ch,
        &format!(
            "You split {} coins among {} members -- {} coins each.\r\n",
            amount, num, share
        ),
    );
}

// ===========================================================================
// do_use — wands / staves / scrolls / potions entry point.
// ===========================================================================

pub fn do_use(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    let (arg, buf) = half_chop(argument);
    let cmd_name = match subcmd {
        SCMD_QUAFF => "quaff",
        SCMD_RECITE => "recite",
        _ => "use",
    };

    if arg.is_empty() {
        g.send_to_char(ch, &format!("What do you want to {}?\r\n", cmd_name));
        return;
    }

    // Try the held item first.
    let mut mag_item = g.get_char(ch).and_then(|c| c.equipment[WEAR_HOLD]);
    let held_matches = match mag_item {
        Some(oid) => g
            .get_obj(oid)
            .map(|o| isname(&arg, &o.name))
            .unwrap_or(false),
        None => false,
    };

    if !held_matches {
        match subcmd {
            SCMD_RECITE | SCMD_QUAFF => {
                let inv = g
                    .get_char(ch)
                    .map(|c| c.carrying.clone())
                    .unwrap_or_default();
                match g.get_obj_in_list_vis(ch, &arg, &inv) {
                    Some(oid) => mag_item = Some(oid),
                    None => {
                        g.send_to_char(
                            ch,
                            &format!("You don't seem to have {} {}.\r\n", an(&arg), arg),
                        );
                        return;
                    }
                }
            }
            SCMD_USE => {
                g.send_to_char(
                    ch,
                    &format!("You don't seem to be holding {} {}.\r\n", an(&arg), arg),
                );
                return;
            }
            _ => {
                // SYSERR: Unknown subcmd passed to do_use
                return;
            }
        }
    }

    let oid = match mag_item {
        Some(o) => o,
        None => return,
    };
    let otype = g
        .get_obj(oid)
        .map(|o| o.obj_type)
        .unwrap_or(ObjectType::Other);

    match subcmd {
        SCMD_QUAFF => {
            if otype != ObjectType::Potion {
                g.send_to_char(ch, "You can only quaff potions.");
                return;
            }
        }
        SCMD_RECITE => {
            if otype != ObjectType::Scroll {
                g.send_to_char(ch, "You can only recite scrolls.");
                return;
            }
        }
        SCMD_USE => {
            if otype != ObjectType::Wand && otype != ObjectType::Staff {
                g.send_to_char(ch, "You can't seem to figure out how to use it.\r\n");
                return;
            }
        }
        _ => {}
    }

    mag_objectmagic(g, ch, oid, &buf);
}

pub fn do_recite(g: &mut GameState, ch: CharId, argument: &str) {
    do_use(g, ch, argument, SCMD_RECITE);
}

/// mag_objectmagic (spell_parser.c): the magic-item entry point. The spell
/// effect routines (call_magic -> mag_damage/mag_affects/…) require the
/// spell_info table (Batch 6). Until that lands, the message flow, charge
/// decrement, target resolution, and obj extraction are ported faithfully;
/// call_magic returns 0 for unresolved spells, degrading exactly like C when a
/// spell number isn't in the table.
fn mag_objectmagic(g: &mut GameState, ch: CharId, obj: ObjId, argument: &str) {
    let (arg, _) = one_argument(argument);

    // generic_find over CHAR_ROOM | OBJ_INV | OBJ_ROOM | OBJ_EQUIP.
    let (tch, tobj, k) = generic_find_target(g, ch, &arg);

    // Immortal-protection: a mortal cannot target an immortal PC.
    if !g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        if let Some(t) = tch {
            let t_npc = g.get_char(t).map(|c| c.is_npc).unwrap_or(true);
            let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
            let t_level = g.get_char(t).map(|c| c.player.level).unwrap_or(1);
            if !t_npc && ch_level < LVL_IMMORT && t_level >= LVL_IMMORT {
                g.send_to_char(
                    ch,
                    "A blinding flash of white light dispels your magic!\r\n",
                );
                act(
                    g,
                    "$n attempts to cast magic on $N.\r\nA blinding flash of white light dispels $n's magic.",
                    false,
                    ch,
                    None,
                    ActArg::Char(t),
                    To::Room,
                );
                return;
            }
        }
    }

    let action_desc = g.get_obj(obj).and_then(|o| o.action_description.clone());

    match g.get_obj(obj).map(|o| o.obj_type) {
        Some(ObjectType::Staff) => {
            act(
                g,
                "You tap $p three times on the ground.",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            match &action_desc {
                Some(d) if !d.is_empty() => {
                    act(g, d, false, ch, Some(obj), ActArg::None, To::Room);
                }
                _ => act(
                    g,
                    "$n taps $p three times on the ground.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Room,
                ),
            }

            let charges = g.get_obj(obj).map(|o| o.values[2]).unwrap_or(0);
            if charges <= 0 {
                act(
                    g,
                    "It seems powerless.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Char,
                );
                act(
                    g,
                    "Nothing seems to happen.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Room,
                );
            } else {
                if let Some(o) = g.get_obj_mut(obj) {
                    o.values[2] -= 1;
                }
                wait_state(g, ch, PULSE_VIOLENCE as i32);
                let (level, spellnum) = g
                    .get_obj(obj)
                    .map(|o| (o.values[0], o.values[3]))
                    .unwrap_or((0, 0));
                let lvl = if level != 0 { level } else { DEFAULT_STAFF_LVL };
                // Affect everyone else in the room.
                let rnum = g.get_char(ch).and_then(|c| c.in_room);
                let people = rnum.map(|r| g.room(r).people.clone()).unwrap_or_default();
                for tgt in people {
                    if tgt == ch {
                        continue;
                    }
                    crate::magic::call_magic(g, ch, Some(tgt), None, spellnum, lvl);
                }
            }
        }
        Some(ObjectType::Wand) => {
            if k == FIND_CHAR_ROOM {
                let t = tch.unwrap();
                if t == ch {
                    act(
                        g,
                        "You point $p at yourself.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::None,
                        To::Char,
                    );
                    act(
                        g,
                        "$n points $p at $mself.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::None,
                        To::Room,
                    );
                } else {
                    act(
                        g,
                        "You point $p at $N.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::Char(t),
                        To::Char,
                    );
                    match &action_desc {
                        Some(d) if !d.is_empty() => {
                            act(g, d, false, ch, Some(obj), ActArg::Char(t), To::Room)
                        }
                        _ => act(
                            g,
                            "$n points $p at $N.",
                            true,
                            ch,
                            Some(obj),
                            ActArg::Char(t),
                            To::Room,
                        ),
                    }
                }
            } else if let Some(to) = tobj {
                act(
                    g,
                    "You point $p at $P.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::Obj(to),
                    To::Char,
                );
                match &action_desc {
                    Some(d) if !d.is_empty() => {
                        act(g, d, false, ch, Some(obj), ActArg::Obj(to), To::Room)
                    }
                    _ => act(
                        g,
                        "$n points $p at $P.",
                        true,
                        ch,
                        Some(obj),
                        ActArg::Obj(to),
                        To::Room,
                    ),
                }
            } else {
                act(
                    g,
                    "At what should $p be pointed?",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Char,
                );
                return;
            }

            let charges = g.get_obj(obj).map(|o| o.values[2]).unwrap_or(0);
            if charges <= 0 {
                act(
                    g,
                    "It seems powerless.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Char,
                );
                act(
                    g,
                    "Nothing seems to happen.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Room,
                );
                return;
            }
            if let Some(o) = g.get_obj_mut(obj) {
                o.values[2] -= 1;
            }
            wait_state(g, ch, PULSE_VIOLENCE as i32);
            let (level, spellnum) = g
                .get_obj(obj)
                .map(|o| (o.values[0], o.values[3]))
                .unwrap_or((0, 0));
            let lvl = if level != 0 { level } else { DEFAULT_WAND_LVL };
            crate::magic::call_magic(g, ch, tch, tobj, spellnum, lvl);
        }
        Some(ObjectType::Scroll) => {
            let mut target = tch;
            if !arg.is_empty() {
                if k == 0 {
                    act(
                        g,
                        "There is nothing to here to affect with $p.",
                        false,
                        ch,
                        Some(obj),
                        ActArg::None,
                        To::Char,
                    );
                    return;
                }
            } else {
                target = Some(ch);
            }

            act(
                g,
                "You recite $p which dissolves.",
                true,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            match &action_desc {
                Some(d) if !d.is_empty() => act(g, d, false, ch, Some(obj), ActArg::None, To::Room),
                _ => act(
                    g,
                    "$n recites $p.",
                    false,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Room,
                ),
            }

            wait_state(g, ch, PULSE_VIOLENCE as i32);
            let (v0, v1, v2, v3) = g
                .get_obj(obj)
                .map(|o| (o.values[0], o.values[1], o.values[2], o.values[3]))
                .unwrap_or((0, 0, 0, 0));

            // Consume the scroll BEFORE casting: a cast can damage the target
            // (mag_damage), trip the auto-retreat/auto-recall thresholds, and
            // recite THIS SAME still-unextracted scroll again -> unbounded
            // recursion through call_magic -> damage -> do_recite. (C reads
            // the spells first and lets the object linger, which is safe only
            // because its recite path cannot re-enter; ours can.)
            extract_obj_from_world(g, obj);

            for spellnum in [v1, v2, v3] {
                if crate::magic::call_magic(g, ch, target, tobj, spellnum, v0) == 0 {
                    break;
                }
            }
        }
        Some(ObjectType::Potion) => {
            act(
                g,
                "You quaff $p.",
                false,
                ch,
                Some(obj),
                ActArg::None,
                To::Char,
            );
            match &action_desc {
                Some(d) if !d.is_empty() => act(g, d, false, ch, Some(obj), ActArg::None, To::Room),
                _ => act(
                    g,
                    "$n quaffs $p.",
                    true,
                    ch,
                    Some(obj),
                    ActArg::None,
                    To::Room,
                ),
            }

            wait_state(g, ch, PULSE_VIOLENCE as i32);
            let (v0, v1, v2, v3) = g
                .get_obj(obj)
                .map(|o| (o.values[0], o.values[1], o.values[2], o.values[3]))
                .unwrap_or((0, 0, 0, 0));
            for spellnum in [v1, v2, v3] {
                if crate::magic::call_magic(g, ch, Some(ch), None, spellnum, v0) == 0 {
                    break;
                }
            }

            extract_obj_from_world(g, obj);
        }
        _ => {
            // SYSERR: Unknown object_type in mag_objectmagic
        }
    }
}

const FIND_CHAR_ROOM: i32 = 1;
const FIND_OBJ_INV: i32 = 2;
const FIND_OBJ_ROOM: i32 = 4;
const FIND_OBJ_EQUIP: i32 = 8;

/// generic_find over CHAR_ROOM | OBJ_INV | OBJ_ROOM | OBJ_EQUIP (handler.c).
/// Returns (target_char, target_obj, matched-bit).
fn generic_find_target(
    g: &GameState,
    ch: CharId,
    arg: &str,
) -> (Option<CharId>, Option<ObjId>, i32) {
    if arg.is_empty() {
        return (None, None, 0);
    }
    // FIND_CHAR_ROOM
    if let Some(t) = g.get_char_room_vis(ch, arg) {
        return (Some(t), None, FIND_CHAR_ROOM);
    }
    // FIND_OBJ_INV
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    if let Some(o) = g.get_obj_in_list_vis(ch, arg, &inv) {
        return (None, Some(o), FIND_OBJ_INV);
    }
    // FIND_OBJ_ROOM
    if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
        let contents = g.room(rnum).contents.clone();
        if let Some(o) = g.get_obj_in_list_vis(ch, arg, &contents) {
            return (None, Some(o), FIND_OBJ_ROOM);
        }
    }
    // FIND_OBJ_EQUIP
    let eq: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    if let Some(o) = g.get_obj_in_list_vis(ch, arg, &eq) {
        return (None, Some(o), FIND_OBJ_EQUIP);
    }
    (None, None, 0)
}

/// WAIT_STATE(ch, cycles): impose a command delay (utils.h sets d->wait, which
/// comm.c decrements per pulse before pulling the next line off d->input). Now
/// wired: game.rs queues player input per descriptor and the heartbeat drains it
/// through the wait gate, so this lag is observable exactly as in C.
fn wait_state(g: &mut GameState, ch: CharId, cycles: i32) {
    g.set_wait_state(ch, cycles);
}

/// extract_obj(): atomically detach and remove the validated object graph.
fn extract_obj_from_world(g: &mut GameState, oid: ObjId) {
    g.extract_obj(oid);
}

// ===========================================================================
// do_wimpy  (and the sibling do_recall / do_retreat, which share this file)
// ===========================================================================

pub fn do_wimpy(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);

    if arg.is_empty() {
        let wimp = g.get_char(ch).map(|c| c.wimp_level).unwrap_or(0);
        if wimp != 0 {
            g.send_to_char(
                ch,
                &format!("Your current wimp level is {} hit points.\r\n", wimp),
            );
        } else {
            g.send_to_char(ch, "At the moment, you're not a wimp.  (sure, sure...)\r\n");
        }
        return;
    }

    if arg
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        let Some(wimp_lev) = command_atoi(g, ch, &arg) else {
            return;
        };
        if wimp_lev != 0 {
            let max_hit = g.get_char(ch).map(|c| c.points.max_hit).unwrap_or(0);
            if wimp_lev < 0 {
                g.send_to_char(ch, "Heh, heh, heh.. we are jolly funny today, eh?\r\n");
            } else if wimp_lev > max_hit {
                g.send_to_char(ch, "That doesn't make much sense, now does it?\r\n");
            } else if wimp_lev > (max_hit >> 1) {
                g.send_to_char(
                    ch,
                    "You can't set your wimp level above half your hit points.\r\n",
                );
            } else {
                g.send_to_char(
                    ch,
                    &format!(
                        "Okay, you'll wimp out if you drop below {} hit points.\r\n",
                        wimp_lev
                    ),
                );
                if let Some(c) = g.get_char_mut(ch) {
                    c.wimp_level = wimp_lev;
                }
            }
        } else {
            g.send_to_char(
                ch,
                "Okay, you'll now tough out fights to the bitter end.\r\n",
            );
            if let Some(c) = g.get_char_mut(ch) {
                c.wimp_level = 0;
            }
        }
    } else {
        g.send_to_char(
            ch,
            "Specify at how many hit points you want to wimp out at.  (0 to disable)\r\n",
        );
    }
}

// ===========================================================================
// do_gen_write — bug / typo / idea writers.
// ===========================================================================

pub fn do_gen_write(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    let (cmd_name, filename) = match subcmd {
        SCMD_BUG => ("bug", "misc/bugs"),
        SCMD_TYPO => ("typo", "misc/typos"),
        SCMD_IDEA => ("idea", "misc/ideas"),
        _ => return,
    };

    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        g.send_to_char(ch, "Monsters can't have ideas - Go away.\r\n");
        return;
    }

    let argument = delete_doubledollar(argument.trim());
    if argument.is_empty() {
        g.send_to_char(ch, "That must be a mistake...\r\n");
        return;
    }

    let name = g
        .get_char(ch)
        .map(|c| c.player.name.clone())
        .unwrap_or_default();
    let room = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .and_then(|r| g.room_opt(r))
        .map(|r| format!("{:5}", r.number))
        .unwrap_or_else(|| "    ?".to_string());

    // C act.other.c:1544-1545: mudlog the report for the immortals.
    crate::syslog::mudlog(
        g,
        &format!("{} {} (room {}): {}", name, cmd_name, room, argument),
        crate::syslog::PFT,
        LVL_IMMORT,
    );

    // C writes the report to lib/misc/{bugs,typos,ideas} (db.h BUG_FILE /
    // TYPO_FILE / IDEA_FILE), refusing once the file reaches max_filesize
    // (config.c:232 = 50000 bytes).
    let path = std::path::Path::new(&g.config.lib_path).join(filename);
    let full = match std::fs::metadata(&path) {
        // C perror()s and silently returns when the file cannot be statted.
        Err(_) => {
            eprintln!("Error statting file: {}", path.display());
            return;
        }
        Ok(m) => m,
    };
    if full.len() >= MAX_FILESIZE {
        g.send_to_char(
            ch,
            "Sorry, the file is full right now.. try again later.\r\n",
        );
        return;
    }
    let mut fl = match std::fs::OpenOptions::new().append(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("do_gen_write: {}: {}", path.display(), e);
            g.send_to_char(ch, "Could not open the file.  Sorry.\r\n");
            return;
        }
    };

    // C act.other.c:1564: "%-8s (%6.6s) [%5s] %s\n" — name, the asctime "Mmm dd"
    // slice, the room vnum, then the report.
    let stamp = chrono::Local::now().format("%b %e").to_string();
    use std::io::Write;
    let line = format!("{:<8} ({:6.6}) [{}] {}\n", name, stamp, room, argument);
    if fl.write_all(line.as_bytes()).is_err() {
        g.send_to_char(ch, "Could not open the file.  Sorry.\r\n");
        return;
    }
    g.send_to_char(ch, "Okay.  Thanks!\r\n");
}

/// max_filesize (config.c:232) — the bug/typo/idea files stop accepting
/// reports at this many bytes.
const MAX_FILESIZE: u64 = 50000;

// ===========================================================================
// do_sneak / do_hide / do_steal  (the thief skills in this file)
// ===========================================================================

pub fn do_sneak(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "Okay, you'll try to move silently for a while.\r\n");
    if g.get_char(ch)
        .map(|c| c.affect_flags & AFF_SNEAK != 0)
        .unwrap_or(false)
    {
        affect_from_char(g, ch, SKILL_SNEAK as i32);
    }

    let percent = g.rng.number(1, 101);
    let (skill, dex, level) = g
        .get_char(ch)
        .map(|c| (c.skill(SKILL_SNEAK) as i32, c.aff_abils.dex, c.player.level))
        .unwrap_or((0, 13, 1));

    if percent > g.rng.number(1, 10) + skill + dex_app_sneak(dex) {
        return;
    }

    affect_to_char(
        g,
        ch,
        Affect {
            spell_type: SKILL_SNEAK as i32,
            duration: level as i32,
            modifier: 0,
            location: APPLY_NONE,
            bitvector: AFF_SNEAK,
            caster: None,
        },
    );
}

pub fn do_hide(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "You attempt to hide yourself.\r\n");

    let hidden = g
        .get_char(ch)
        .map(|c| c.affect_flags & AFF_HIDE != 0)
        .unwrap_or(false);
    if hidden && !check_perm_duration(g, ch, AFF_HIDE) {
        if let Some(c) = g.get_char_mut(ch) {
            c.affect_flags &= !AFF_HIDE;
        }
    }

    let percent = g.rng.number(1, 101);
    let (skill, dex) = g
        .get_char(ch)
        .map(|c| (c.skill(SKILL_HIDE) as i32, c.aff_abils.dex))
        .unwrap_or((0, 13));

    if percent > g.rng.number(1, 10) + skill + dex_app_hide(dex) {
        return;
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.affect_flags |= AFF_HIDE;
    }
}

pub fn do_steal(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (obj_name, rest) = one_argument(argument);
    let (vict_name, _) = one_argument(rest);

    let vict = match g.get_char_room_vis(ch, &vict_name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "Steal what from who?\r\n");
            return;
        }
    };
    if vict == ch {
        g.send_to_char(ch, "Come on now, that's rather stupid!\r\n");
        return;
    }

    // Peaceful-room gate.
    let (in_room, ch_level) = g
        .get_char(ch)
        .map(|c| (c.in_room, c.player.level))
        .unwrap_or((None, 1));
    if let Some(r) = in_room {
        let peaceful = g.room(r).room_flags.bits() & ROOM_PEACEFUL != 0;
        if peaceful && !has_direct_implementor_authority(g, ch) {
            g.send_to_char(
                ch,
                "This room just has such a peaceful, easy feeling...\r\n",
            );
            return;
        }
    }

    let dex = g.get_char(ch).map(|c| c.aff_abils.dex).unwrap_or(13);
    let mut percent = g.rng.number(1, 101) - dex_app_p_pocket(dex);

    let vict_level = g.get_char(vict).map(|c| c.player.level).unwrap_or(1);
    if vict_level > ch_level {
        percent += (vict_level as i32 - ch_level as i32).abs();
    }
    let vict_pos = g
        .get_char(vict)
        .map(|c| c.position)
        .unwrap_or(Position::Standing);
    if vict_pos < Position::Sleeping {
        percent = -1; // always success
    }

    // C act.other.c:672-678: with config.c:86 pt_allowed = YES, pcsteal
    // stays 0 and PC-thieving runs on the normal skill roll; immortals AND
    // shopkeeper mobs always auto-fail the roll (#309, #316). The old code
    // hard-inverted pt_allowed, making PC theft unconditionally fatal.
    let vict_npc = g.get_char(vict).map(|c| c.is_npc).unwrap_or(false);
    const PT_ALLOWED: bool = true; // config.c:86
    let pcsteal = !PT_ALLOWED && !vict_npc;
    let vict_vnum = g
        .get_char(vict)
        .and_then(|c| if c.is_npc { Some(c.nr) } else { None });
    let is_keeper = vict_vnum
        .map(crate::shop::is_shop_keeper_vnum)
        .unwrap_or(false);
    if vict_level >= LVL_IMMORT || pcsteal || is_keeper {
        percent = 101;
    }

    let mut ohoh = false;
    let steal_skill = g
        .get_char(ch)
        .map(|c| c.skill(SKILL_STEAL) as i32)
        .unwrap_or(0);
    let awake = vict_pos > Position::Sleeping;

    if !obj_name.eq_ignore_ascii_case("coins") && !obj_name.eq_ignore_ascii_case("gold") {
        // Look for the named object in the victim's inventory.
        let inv = g
            .get_char(vict)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        let mut obj = g.get_obj_in_list_vis(vict, &obj_name, &inv);

        if obj.is_none() {
            // Worn equipment.
            let mut found_eq = None;
            let eq: Vec<(usize, ObjId)> = g
                .get_char(vict)
                .map(|c| {
                    c.equipment
                        .iter()
                        .enumerate()
                        .filter_map(|(p, o)| o.map(|oid| (p, oid)))
                        .collect()
                })
                .unwrap_or_default();
            for (pos, oid) in eq {
                let name_match = g
                    .get_obj(oid)
                    .map(|o| isname(&obj_name, &o.name))
                    .unwrap_or(false);
                if name_match && can_see_obj(g, ch, oid) {
                    found_eq = Some((pos, oid));
                    break;
                }
            }
            match found_eq {
                None => {
                    act(
                        g,
                        "$E hasn't got that item.",
                        false,
                        ch,
                        None,
                        ActArg::Char(vict),
                        To::Char,
                    );
                    return;
                }
                Some((pos, oid)) => {
                    if vict_pos > Position::Stunned {
                        g.send_to_char(ch, "Steal the equipment now?  Impossible!\r\n");
                        return;
                    }
                    act(
                        g,
                        "You unequip $p and steal it.",
                        false,
                        ch,
                        Some(oid),
                        ActArg::None,
                        To::Char,
                    );
                    act(
                        g,
                        "$n steals $p from $N.",
                        false,
                        ch,
                        Some(oid),
                        ActArg::Char(vict),
                        To::NotVict,
                    );
                    if let Some(taken) = g.unequip_char(vict, pos) {
                        g.obj_to_char(taken, ch);
                    }
                    return;
                }
            }
        } else {
            // Object in inventory.
            let oid = obj.take().unwrap();
            let weight = g.get_obj(oid).map(|o| o.weight).unwrap_or(0);
            percent += weight;

            if awake && percent > steal_skill {
                ohoh = true;
                act(g, "Oops..", false, ch, None, ActArg::None, To::Char);
                act(
                    g,
                    "$n tried to steal something from you!",
                    false,
                    ch,
                    None,
                    ActArg::Char(vict),
                    To::Vict,
                );
                act(
                    g,
                    "$n tries to steal something from $N.",
                    true,
                    ch,
                    None,
                    ActArg::Char(vict),
                    To::NotVict,
                );
                // C act.other.c:726: with pt_markable a successful theft
                // from a player brands the thief PLR_THIEF (the City Watch
                // and bounty system react to the flag). Default off to match
                // the oracle; MUD_PT_MARKABLE=1 enables it.
                if g.config.pt_markable {
                    if let Some(c) = g.get_char_mut(ch) {
                        c.act_flags |= PLR_THIEF;
                    }
                }
            } else {
                // Carry-capacity checks (CAN_CARRY_N / CAN_CARRY_W approximated
                // by item count + weight; the full str_app table lives in
                // cmd_item.rs). Mirror C's success branch.
                let (carry_items, carry_weight) = g
                    .get_char(ch)
                    .map(|c| (c.carry_items as i32, c.carry_weight))
                    .unwrap_or((0, 0));
                let can_n = carry_items + 1 < can_carry_n(g, ch);
                if can_n {
                    if carry_weight + weight < can_carry_w(g, ch) {
                        g.obj_from_anywhere(oid);
                        g.obj_to_char(oid, ch);
                        g.send_to_char(ch, "Got it!\r\n");
                    }
                } else {
                    g.send_to_char(ch, "You cannot carry that much.\r\n");
                }
            }
        }
    } else {
        // Steal coins.
        if awake && percent > steal_skill {
            ohoh = true;
            act(g, "Oops..", false, ch, None, ActArg::None, To::Char);
            act(
                g,
                "You discover that $n has $s hands in your wallet.",
                false,
                ch,
                None,
                ActArg::Char(vict),
                To::Vict,
            );
            act(
                g,
                "$n tries to steal gold from $N.",
                true,
                ch,
                None,
                ActArg::Char(vict),
                To::NotVict,
            );
        } else {
            let vgold = g.get_char(vict).map(|c| c.points.gold).unwrap_or(0);
            let mut gold = (i64::from(vgold) * i64::from(g.rng.number(1, 10))) / 100;
            gold = gold.min(1782);
            if gold > 0 {
                let moved = crate::gold::transfer_between(
                    g,
                    vict,
                    crate::gold::Account::Carried,
                    ch,
                    crate::gold::Account::Carried,
                    gold,
                );
                if moved && gold > 1 {
                    g.send_to_char(ch, &format!("Bingo!  You got {} gold coins.\r\n", gold));
                } else if moved {
                    g.send_to_char(ch, "You manage to swipe a solitary gold coin.\r\n");
                } else {
                    g.send_to_char(ch, "You couldn't carry any more gold...\r\n");
                }
            } else {
                g.send_to_char(ch, "You couldn't get any gold...\r\n");
            }
        }
    }

    if ohoh && vict_npc && awake {
        crate::combat::hit(g, vict, ch);
    }
}

/// CAN_CARRY_N (objsave.c macro): 5 + DEX/2 + LEVEL/2. The exact str/dex tables
/// live in cmd_item.rs; this mirrors the CircleMUD formula closely enough for
/// the steal capacity gate, which only needs a sane bound.
fn can_carry_n(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch)
        .map(|c| 5 + (c.aff_abils.dex as i32) / 2 + (c.player.level as i32) / 2)
        .unwrap_or(5)
}

/// CAN_CARRY_W(ch) = str_app[STRENGTH_APPLY_INDEX(ch)].carry_w.
fn can_carry_w(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch)
        .map(|c| {
            STR_APP[strength_apply_index(c.aff_abils.str as i32, c.aff_abils.str_add as i32)]
                .carry_w
        })
        .unwrap_or(100)
}

/// CAN_SEE_OBJ (Tier-0): invisible items need detect.
fn can_see_obj(g: &GameState, ch: CharId, oid: ObjId) -> bool {
    let invis = g
        .get_obj(oid)
        .map(|o| o.extra_flags.contains(crate::object::ExtraFlags::INVISIBLE))
        .unwrap_or(false);
    if !invis {
        return true;
    }
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_DETECT_INVIS != 0)
        .unwrap_or(false)
}

// ===========================================================================
// do_recall / do_retreat — autorecall / autoretreat hp thresholds.
// (These share act.other.c; do_wimpy's sibling logic, ported faithfully.)
// ===========================================================================

pub fn do_recall(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    threshold_command(g, ch, argument, RecallKind::Recall);
}

pub fn do_retreat(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    threshold_command(g, ch, argument, RecallKind::Retreat);
}

enum RecallKind {
    Recall,
    Retreat,
}

fn threshold_command(g: &mut GameState, ch: CharId, argument: &str, kind: RecallKind) {
    let (arg, _) = one_argument(argument);
    let (none_msg, cur_word, set_word, spec_word) = match kind {
        RecallKind::Recall => (
            "At the moment, you won't autorecall.\r\n",
            "recall",
            "recall",
            "recall",
        ),
        RecallKind::Retreat => (
            "At the moment, you won't autoretreat.\r\n",
            "retreat",
            "retreat",
            "retreat",
        ),
    };
    let cur = match kind {
        RecallKind::Recall => g.get_char(ch).map(|c| c.recall_level).unwrap_or(0),
        RecallKind::Retreat => g.get_char(ch).map(|c| c.retreat_level).unwrap_or(0),
    };

    if arg.is_empty() {
        if cur != 0 {
            g.send_to_char(
                ch,
                &format!("Your current {} level is {} hit points.\r\n", cur_word, cur),
            );
        } else {
            g.send_to_char(ch, none_msg);
        }
        return;
    }
    if arg
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        let Some(lev) = command_atoi(g, ch, &arg) else {
            return;
        };
        if lev != 0 {
            let max_hit = g.get_char(ch).map(|c| c.points.max_hit).unwrap_or(0);
            if lev < 0 {
                g.send_to_char(ch, "Heh, heh, heh.. we are jolly funny today, eh?\r\n");
            } else if lev > max_hit {
                g.send_to_char(ch, "That doesn't make much sense, now does it?\r\n");
            } else if lev > (max_hit >> 1) {
                g.send_to_char(
                    ch,
                    &format!(
                        "You can't set your {} level above half your hit points.\r\n",
                        set_word
                    ),
                );
            } else {
                g.send_to_char(
                    ch,
                    &format!(
                        "Okay, you'll {} out if you drop below {} hit points.\r\n",
                        set_word, lev
                    ),
                );
                if let Some(c) = g.get_char_mut(ch) {
                    match kind {
                        RecallKind::Recall => c.recall_level = lev,
                        RecallKind::Retreat => c.retreat_level = lev,
                    }
                }
            }
        } else {
            match kind {
                RecallKind::Recall => {
                    g.send_to_char(ch, "You will no longer autorecall from combat..\r\n")
                }
                RecallKind::Retreat => {
                    g.send_to_char(ch, "You will no longer autoretreat from combat..\r\n")
                }
            }
            if let Some(c) = g.get_char_mut(ch) {
                match kind {
                    RecallKind::Recall => c.recall_level = 0,
                    RecallKind::Retreat => c.retreat_level = 0,
                }
            }
        }
    } else {
        g.send_to_char(
            ch,
            &format!(
                "Specify how many hit points you want to {} out at. (0 to disable)\r\n",
                spec_word
            ),
        );
    }
}

// ===========================================================================
// do_train / do_school / do_observe / do_lockout
// ===========================================================================

pub fn do_train(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    if !arg.is_empty() {
        g.send_to_char(ch, "You cannot train here.\r\n");
        return;
    }
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return,
    };
    let training = c.training as i32;
    let line1 = format!(
        "Hit:{} Mana:{} Str:{} Con:{} Wis:{} Int:{} Dex:{} Cha:{}\r\n",
        c.points.max_hit,
        c.points.max_mana,
        c.aff_abils.str,
        c.aff_abils.con,
        c.aff_abils.wis,
        c.aff_abils.intel,
        c.aff_abils.dex,
        c.aff_abils.cha
    );
    let line2 = if training == 1 {
        format!("You have {} training session.\r\n", training)
    } else {
        format!("You have {} training sessions.\r\n", training)
    };
    g.send_to_char(ch, &line1);
    g.send_to_char(ch, &line2);
}

pub fn do_school(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
    if level > 2 && level < LVL_IMMORT {
        g.send_to_char(
            ch,
            "Sorry, but the newbie school is only for newbies (level 1 or 2)!\r\n",
        );
        return;
    }
    let dest = match g.real_room(g.config.newbie_room) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "Sorry, newbie school is temporarily unavaliable.\r\n");
            return;
        }
    };
    act(
        g,
        "$n has been ferried to the Newbie School!",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    g.char_from_room(ch);
    g.char_to_room(ch, dest);
    act(
        g,
        "$n suddenly appears in the room.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    crate::cmd_informative::look_at_room(g, ch, false);
}

pub fn do_observe(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    // ARENA_OBSERVEROOM (config.c / arena.rs): the observatory.
    const ARENA_OBSERVEROOM: RoomVnum = 4899;

    let in_room = g.get_char(ch).and_then(|c| c.in_room);
    let stat = crate::arena::arena_stat(ch);
    let observatory = g.real_room(ARENA_OBSERVEROOM);

    // C act.other.c:1801-1805: an observer must be standing in the observatory.
    if stat != crate::arena::ARENA_OBSERVER || in_room != observatory {
        g.send_to_char(ch, "You can't do that now! Get to the observatory!\r\n");
        return;
    }

    let (arg, _) = one_argument(argument);

    // No argument: report who is currently being watched.
    if arg.is_empty() {
        let who = crate::arena::arena_observing(ch)
            .map(|v| {
                g.get_char(v)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| "nobody".to_string());
        g.send_to_char(
            ch,
            &format!("You're currently observing the actions of {}.\r\n", who),
        );
        return;
    }

    // get_char_vis(): the room first, then a world scan — combatants are in the
    // arena, not in the observatory, so the world scan is the normal path.
    let victim = match get_char_vis(g, ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No such person around.\r\n");
            return;
        }
    };

    let vlevel = g.get_char(victim).map(|c| c.player.level).unwrap_or(0);
    if vlevel >= LVL_IMMORT && victim != ch {
        g.send_to_char(ch, "You dare not.\r\n");
        return;
    }

    if victim == ch {
        crate::arena::deobserve(ch);
        g.send_to_char(ch, "Ok. You're observing nobody now.\r\n");
        return;
    }

    if !crate::arena::is_arena_combatant(victim) {
        g.send_to_char(ch, "Hey! That person's not an arena combatant!\r\n");
    } else {
        crate::arena::deobserve(ch);
        crate::arena::linkobserve(ch, victim);
        let vname = g
            .get_char(victim)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        g.send_to_char(
            ch,
            &format!("You're now observing the actions of {}.\r\n", vname),
        );
    }
}

pub fn do_lockout(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let argument = argument.trim();

    let locked = g
        .get_char(ch)
        .map(|c| c.prf2_flags & PRF2_LOCKOUT != 0)
        .unwrap_or(false);
    if locked {
        if argument.is_empty() || argument.len() > crate::password::MAX_PASSWORD_INPUT_BYTES {
            g.send_to_char(
                ch,
                "Password mismatch! Sorry.\r\nTo unlock please type 'unlock <yourpassword>'\r\n",
            );
            return;
        }
        let Some(principal) = g
            .principal_authority(ch)
            .filter(|principal| principal.is_authenticated_player() && principal.principal == ch)
        else {
            g.send_to_char(
                ch,
                "Password verification is unavailable for this session; reconnect to unlock.\r\n",
            );
            return;
        };
        let Some((descriptor, idnum, name, hash)) = g.get_char(ch).and_then(|character| {
            let descriptor = character.desc?;
            let session_hash = g
                .descriptors
                .get(&descriptor)
                .and_then(|descriptor| descriptor.password_hash.clone());
            character
                .pending_password_hash
                .clone()
                .or(session_hash)
                .map(|hash| {
                    (
                        descriptor,
                        character.idnum,
                        character.get_name().to_string(),
                        hash,
                    )
                })
        }) else {
            g.send_to_char(
                ch,
                "Password verification is unavailable after recovery; reconnect to unlock.\r\n",
            );
            return;
        };
        g.queue_lockout_unlock(crate::state::LockoutUnlockRequest {
            character: ch,
            principal: principal.principal,
            descriptor,
            idnum,
            name,
            expected_hash: hash,
            plaintext_password: argument.to_string(),
        });
        g.send_to_char(ch, "Password verification queued.\r\n");
        return;
    }
    g.send_to_char(
        ch,
        "OK. Your terminal is now locked.\r\nTo unlock please type 'unlock <yourpassword>'\r\n",
    );
    if let Some(c) = g.get_char_mut(ch) {
        c.prf2_flags |= PRF2_LOCKOUT;
    }
    act(
        g,
        "$n has gone AFK-lockout.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
}

/// Clear a terminal lock only after the async Game shell has completed the
/// bounded password check and revalidated the exact authenticated session.
pub(crate) fn complete_lockout_unlock(g: &mut GameState, ch: CharId) {
    g.send_to_char(ch, "OK. Your terminal is now unlocked.\r\n");
    if let Some(character) = g.get_char_mut(ch) {
        character.prf2_flags &= !PRF2_LOCKOUT;
    }
    act(
        g,
        "$n has come back from AFK-lockout.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
}

// ===========================================================================
// do_tan / do_fillet / do_carve — corpse-crafting.
// ===========================================================================

/// Find a corpse (ITEM_CONTAINER with val[3]==1) in the room by keyword.
fn find_corpse_in_room(g: &GameState, ch: CharId, name: &str) -> Option<ObjId> {
    let rnum = g.get_char(ch).and_then(|c| c.in_room)?;
    let contents = g.room(rnum).contents.clone();
    for oid in contents {
        let ok = g
            .get_obj(oid)
            .map(|o| {
                isname(name, &o.name)
                    && o.obj_type == ObjectType::Container
                    && o.values[3] == CONT_FOOD_CORPSE
            })
            .unwrap_or(false);
        if ok && can_see_obj(g, ch, oid) {
            return Some(oid);
        }
    }
    None
}

pub fn do_tan(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (buf, rest) = two_arguments(argument);
    let (buf2, _) = (rest.0, rest.1);

    if buf.is_empty() {
        g.send_to_char(ch, "Tan what?\r\n");
        return;
    }
    if buf2.is_empty() {
        g.send_to_char(ch, "Tan from what?\r\n");
        return;
    }

    let found = match find_corpse_in_room(g, ch, &buf2) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You can't tailor anything from that!\r\n");
            return;
        }
    };

    let (level, mut tan_skill) = g
        .get_char(ch)
        .map(|c| (c.player.level as i32, c.skill(SKILL_TAN) as i32))
        .unwrap_or((1, 0));

    let mut newone = Object::new(NOTHING, String::new(), String::new());
    newone.obj_type = ObjectType::Armor;
    newone.values[0] = level / 25 + tan_skill / 50;
    newone.values[1] = 0;
    newone.values[2] = 20; // tslots
    newone.values[3] = 20; // cslots
    newone.rent = level;
    newone.cost = 10 * level;
    newone.timer = 0;

    let mut craft_buf = buf.clone();
    if tan_skill < g.rng.number(1, 100) {
        craft_buf = "babalooza".to_string();
        if g.rng.number(1, 100) < 3 {
            tan_skill += 1;
            if let Some(c) = g.get_char_mut(ch) {
                c.set_skill(SKILL_TAN, tan_skill.clamp(0, 255) as u8);
            }
        }
    }

    let cb = craft_buf.as_str();
    if matches!(cb, "hat" | "cap" | "helm" | "head" | "helmet") {
        newone.name = "cap leather".into();
        newone.short_description = "a leather cap".into();
        newone.description = "A hand made leather cap has been left here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::HEAD;
        newone.weight = 5;
    } else if matches!(cb, "gloves" | "gauntlets" | "hands") {
        newone.name = "gloves leather".into();
        newone.short_description = "some leather gloves".into();
        newone.description = "Some hand made leather gloves have been left here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::HANDS;
        newone.weight = 5;
    } else if matches!(cb, "sleeves" | "vambraces" | "arms") {
        newone.name = "sleeves leather".into();
        newone.short_description = "some leather sleeves".into();
        newone.description = "Some hand made leather sleeves have been left here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::ARMS;
        newone.weight = 10;
    } else if matches!(cb, "chest" | "breast" | "body" | "protector" | "jacket") {
        newone.name = "chest protector leather".into();
        newone.short_description = "a leather chest protector".into();
        newone.description = "A hand made leather chest protector has been left here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::BODY;
        newone.weight = 25;
    } else if matches!(cb, "legs" | "pants" | "greaves" | "chaps") {
        newone.name = "greaves leather".into();
        newone.short_description = "some leather greaves".into();
        newone.description = "Some hand made leather greaves have been left here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::LEGS;
        newone.weight = 15;
    } else if matches!(cb, "boots" | "feet" | "shoes" | "sandals") {
        newone.name = "boots leather".into();
        newone.short_description = "some leather boots".into();
        newone.description = "Some hand made leather boots have been left here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::FEET;
        newone.weight = 10;
    } else {
        newone.name = "strange armor leather".into();
        newone.short_description = "some strange looking armor".into();
        newone.description = "Somone has left an aborted leather tanning experiment here.".into();
        newone.action_description = Some("Act-D".into());
        newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE);
        newone.values[0] = 0;
        newone.weight = 50;
    }

    let short = newone.short_description.clone();
    let otype = newone.obj_type as u8;

    extract_obj_from_world(g, found);
    let nid = g.create_obj(newone);
    g.obj_to_char(nid, ch);

    g.send_to_char(ch, &format!("You have made {} {}!\r\n", short, otype));
    act(
        g,
        &format!("$n has made {}!\r\n", short),
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
}

pub fn do_fillet(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (buf, _) = two_arguments(argument);

    if buf.is_empty() {
        g.send_to_char(ch, "Fillet from what?\r\n");
        return;
    }

    let found = match find_corpse_in_room(g, ch, &buf) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You can't fillet anything from that!\r\n");
            return;
        }
    };

    let (level, mut fillet_skill) = g
        .get_char(ch)
        .map(|c| (c.player.level as i32, c.skill(SKILL_FILLET) as i32))
        .unwrap_or((1, 0));
    let found_weight = g.get_obj(found).map(|o| o.weight).unwrap_or(0);

    let mut newone = Object::new(NOTHING, String::new(), String::new());
    newone.obj_type = ObjectType::Food;
    newone.rent = 0;
    newone.cost = level;
    newone.timer = 0;
    newone.values[2] = 1; // tslots
    newone.values[3] = 1; // cslots base; overwritten by spoil flag below
    newone.weight = found_weight / 10;

    if fillet_skill < g.rng.number(1, 100) {
        newone.values[0] = 1;
        if g.rng.number(1, 100) < 3 {
            fillet_skill += 1;
            if let Some(c) = g.get_char_mut(ch) {
                c.set_skill(SKILL_FILLET, fillet_skill.clamp(0, 255) as u8);
            }
        }
    } else {
        newone.values[0] = 12;
    }

    newone.values[3] = if g.rng.number(1, 20) == 1 { 1 } else { 0 };

    newone.name = "meat fillet".into();
    newone.short_description = "some fresh meat".into();
    newone.description = "A juicy hunk of freshly filleted meat is curing here.".into();
    newone.action_description = Some("Act-D".into());

    let short = newone.short_description.clone();

    extract_obj_from_world(g, found);
    let nid = g.create_obj(newone);
    g.obj_to_char(nid, ch);

    g.send_to_char(ch, &format!("You slice {} from the corpse!\r\n", short));
    act(
        g,
        &format!("$n slices {} from the corpse!\r\n", short),
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
}

pub fn do_carve(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (buf, rest) = two_arguments(argument);
    let buf2 = rest.0;

    if buf.is_empty() {
        g.send_to_char(ch, "Carve what?\r\n");
        return;
    }
    if buf2.is_empty() {
        g.send_to_char(ch, "Carve from what?\r\n");
        return;
    }

    let found = match find_corpse_in_room(g, ch, &buf2) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You can't carve from that!\r\n");
            return;
        }
    };

    let (level, mut carve_skill) = g
        .get_char(ch)
        .map(|c| (c.player.level as i32, c.skill(SKILL_CARVE) as i32))
        .unwrap_or((1, 0));

    let mut newone = Object::new(NOTHING, String::new(), String::new());
    newone.obj_type = ObjectType::Weapon;
    newone.values[1] = level / 10;
    newone.values[2] = carve_skill / 20;
    newone.rent = level;
    newone.cost = 10 * level;
    newone.values[3] = 0;
    newone.wear_flags = WearFlags::from_bits_truncate(ITEM_WEAR_TAKE) | WearFlags::WIELD;
    newone.timer = 0;

    let mut craft_buf = buf.clone();
    if carve_skill < g.rng.number(1, 100) {
        craft_buf = "babalooza".to_string();
        if g.rng.number(1, 100) < 3 {
            carve_skill += 1;
            if let Some(c) = g.get_char_mut(ch) {
                c.set_skill(SKILL_CARVE, carve_skill.clamp(0, 255) as u8);
            }
        }
    }

    match craft_buf.as_str() {
        "dagger" => {
            newone.name = "dagger bone".into();
            newone.short_description = "a bone dagger".into();
            newone.description = "A hand made bone dagger has been left here.".into();
            newone.action_description = Some("Act-D".into());
            newone.weight = 4;
        }
        "club" => {
            newone.name = "club bone".into();
            newone.short_description = "a bone club".into();
            newone.description = "A hand made bone club has been left here.".into();
            newone.action_description = Some("Act-D".into());
            newone.weight = 10;
        }
        "spear" => {
            newone.name = "spear bone".into();
            newone.short_description = "a bone spear".into();
            newone.description = "A hand made bone spear has been left here.".into();
            newone.action_description = Some("Act-D".into());
            newone.weight = 12;
        }
        "sword" => {
            newone.name = "sword bone".into();
            newone.short_description = "a bone sword".into();
            newone.description = "A hand made bone sword has been left here.".into();
            newone.action_description = Some("Act-D".into());
            newone.weight = 10;
        }
        _ => {
            newone.name = "weapon strange bone".into();
            newone.short_description = "a strange bone weapon".into();
            newone.description = "A piece of mangled bone that could be a weapon is here.".into();
            newone.action_description = Some("Act-D".into());
            newone.values[1] = 1;
            newone.values[2] = 1;
            newone.weight = 14;
        }
    }

    let short = newone.short_description.clone();

    extract_obj_from_world(g, found);
    let nid = g.create_obj(newone);
    g.obj_to_char(nid, ch);

    g.send_to_char(ch, &format!("You have made {}!\r\n", short));
    act(
        g,
        &format!("$n has made {}!\r\n", short),
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
}

/// two_arguments: split off the first two whitespace tokens (interpreter.c).
/// Returns (first, (second, remainder)).
fn two_arguments(argument: &str) -> (String, (String, String)) {
    let (a, rest1) = any_one_arg(argument);
    let (b, rest2) = any_one_arg(rest1);
    (a, (b, rest2.to_string()))
}

/// any_one_arg replicated locally (interpreter.c) — first token (lowercased)
/// and the remainder, NOT skipping fill words.
fn any_one_arg(argument: &str) -> (String, &str) {
    let s = argument.trim_start();
    match s.find(char::is_whitespace) {
        Some(pos) => (s[..pos].to_lowercase(), s[pos..].trim_start()),
        None => (s.to_lowercase(), ""),
    }
}

/// get_char_vis(ch, arg) (handler.c): the visible character matching `arg` in
/// the actor's room first, then a whole-world scan. Implemented here (as in
/// cmd_wizard.rs / cmd_social.rs) because the shared contract exposes only the
/// room-scoped finder.
fn get_char_vis(g: &GameState, ch: CharId, arg: &str) -> Option<CharId> {
    if let Some(id) = g.get_char_room_vis(ch, arg) {
        return Some(id);
    }
    let (mut count, name) = crate::handler::get_number(arg);
    if count == 0 {
        return None;
    }
    for cid in g.char_ids() {
        let target_name = g
            .get_char(cid)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        if isname(&name, &target_name) && g.can_see(ch, cid) {
            count -= 1;
            if count == 0 {
                return Some(cid);
            }
        }
    }
    None
}

// ===========================================================================
// do_gen_atm — bank commands (balance / deposit / withdraw / bank menu).
// ===========================================================================

/// atm_is_in_room (act.other.c:2194-2225): an ITEM_ATM object lying in the
/// room, a MOB_BANKER mob in the room, a carried ITEM_ATM bankcard, or a worn
/// ITEM_ATM bankcard. The carried test additionally requires that the object
/// has no equipment position (`find_eq_pos(ch, obj, NULL) < 0`) — i.e. it is
/// not a wearable item, which only counts while actually worn.
fn atm_is_in_room(g: &GameState, ch: CharId) -> bool {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return false,
    };

    // (1) any ATM object in the room.
    for oid in g.room(rnum).contents.clone() {
        if g.get_obj(oid)
            .map(|o| o.obj_type == ObjectType::Atm)
            .unwrap_or(false)
        {
            return true;
        }
    }

    // (2) a banker mob in the room (Mulder 10/6/99).
    const MOB_BANKER: i64 = 1 << 29;
    for vict in g.room(rnum).people.clone() {
        let is_banker = g
            .get_char(vict)
            .map(|c| c.is_npc && c.act_flags & MOB_BANKER != 0)
            .unwrap_or(false);
        if is_banker && g.can_see(ch, vict) {
            return true;
        }
    }

    // (3) carrying a bankcard (an unwearable ITEM_ATM in the inventory).
    for oid in g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default()
    {
        if let Some(o) = g.get_obj(oid) {
            if o.obj_type == ObjectType::Atm && !wearable_eq_pos(o) {
                return true;
            }
        }
    }

    // (4) wearing a bankcard.
    for slot in g
        .get_char(ch)
        .map(|c| c.equipment)
        .unwrap_or([None; NUM_WEARS])
    {
        if let Some(oid) = slot {
            if g.get_obj(oid)
                .map(|o| o.obj_type == ObjectType::Atm)
                .unwrap_or(false)
            {
                return true;
            }
        }
    }

    false
}

/// `find_eq_pos(ch, obj, NULL) < 0` — the object has no equipment position,
/// i.e. none of the C CAN_WEAR positions (everything except TAKE/WIELD/HOLD)
/// is set on its wear bits.
fn wearable_eq_pos(o: &Object) -> bool {
    (o.wear_flags - WearFlags::TAKE - WearFlags::WIELD - WearFlags::HOLD).bits() != 0
}

pub fn do_gen_atm(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        return;
    }
    if !atm_is_in_room(g, ch) {
        g.send_to_char(ch, "You can't do that here!\r\n");
        return;
    }

    match subcmd {
        SCMD_BALANCE => {
            let bank = g.get_char(ch).map(|c| c.points.bank_gold).unwrap_or(0);
            if bank > 0 {
                g.send_to_char(ch, &format!("Your current balance is {} coins.\r\n", bank));
            } else {
                g.send_to_char(ch, "You currently have no money deposited.\r\n");
            }
        }
        SCMD_BANK => {
            g.send_to_char(
                ch,
                "\r\nDeltaMUD Bank Commands:\r\n&y-----------------------&n\r\nbalance                 &c-&n displays your account balance\r\nwithdraw &y[amount]&n       &c-&n withdraws X number of coins\r\ndeposit &y[amount]&n        &c-&n deposits X number of coins\r\ndeposit &y[name] &y[amount]&n &c-&n put gold in someone elses account\r\n",
            );
        }
        SCMD_DEPOSIT => {
            let (b1, rest) = two_arguments(argument.trim());
            let b2 = rest.0;
            if b2.is_empty() {
                let Some(amount) = command_atoi(g, ch, argument.trim()) else {
                    return;
                };
                let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
                if amount <= 0 {
                    g.send_to_char(ch, "How much do you want to deposit?\r\n");
                } else if gold < amount {
                    g.send_to_char(ch, "You don't have that many coins!\r\n");
                } else {
                    if let Some(c) = g.get_char_mut(ch) {
                        if !crate::gold::transfer(
                            c,
                            crate::gold::Account::Carried,
                            crate::gold::Account::Bank,
                            i64::from(amount),
                        ) {
                            g.send_to_char(ch, "That deposit would exceed your account limit.\r\n");
                            return;
                        }
                    }
                    g.send_to_char(ch, &format!("You deposit {} coins.\r\n", amount));
                }
            } else {
                // Transfer deposit: deposit <name> <amount>. C act.other.c:2276
                // uses get_char_vis() — the room first, then a whole-world scan —
                // so gold can be sent to a player anywhere in the game.
                let vict = get_char_vis(g, ch, &b1);
                match vict {
                    None => g.send_to_char(ch, "No-one by that name here.\r\n"),
                    Some(v) if v == ch || g.get_char(v).map(|c| c.is_npc).unwrap_or(true) => {
                        g.send_to_char(ch, "What's the point of that?\r\n");
                    }
                    Some(v) => {
                        let Some(amount) = command_atoi(g, ch, &b2) else {
                            return;
                        };
                        let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
                        if amount <= 0 {
                            g.send_to_char(ch, "How much do you want to deposit?\r\n");
                        } else if gold < amount {
                            g.send_to_char(ch, "You don't have that many coins!\r\n");
                        } else {
                            if !crate::gold::transfer_between(
                                g,
                                ch,
                                crate::gold::Account::Carried,
                                v,
                                crate::gold::Account::Bank,
                                i64::from(amount),
                            ) {
                                g.send_to_char(
                                    ch,
                                    "That deposit would exceed the recipient's account limit.\r\n",
                                );
                                return;
                            }
                            act(
                                g,
                                &format!("You deposit {} coins into $N's account.", amount),
                                false,
                                ch,
                                None,
                                ActArg::Char(v),
                                To::Char,
                            );
                            act(
                                g,
                                &format!("$n deposits {} coins into your account.", amount),
                                false,
                                ch,
                                None,
                                ActArg::Char(v),
                                To::Vict,
                            );
                            act(
                                g,
                                "$n makes a bank transaction.",
                                true,
                                ch,
                                None,
                                ActArg::Char(v),
                                To::NotVict,
                            );
                        }
                    }
                }
            }
        }
        SCMD_WITHDRAW => {
            let Some(amount) = command_atoi(g, ch, argument.trim()) else {
                return;
            };
            let bank = g.get_char(ch).map(|c| c.points.bank_gold).unwrap_or(0);
            if amount <= 0 {
                g.send_to_char(ch, "How much do you want to withdraw?\r\n");
            } else if bank < amount {
                g.send_to_char(ch, "You don't have that many coins deposited!\r\n");
            } else {
                if let Some(c) = g.get_char_mut(ch) {
                    if !crate::gold::transfer(
                        c,
                        crate::gold::Account::Bank,
                        crate::gold::Account::Carried,
                        i64::from(amount),
                    ) {
                        g.send_to_char(
                            ch,
                            "That withdrawal would exceed your carried-gold limit.\r\n",
                        );
                        return;
                    }
                }
                g.send_to_char(ch, &format!("You withdraw {} coins.\r\n", amount));
                act(
                    g,
                    "$n makes a bank transaction.",
                    true,
                    ch,
                    None,
                    ActArg::None,
                    To::Room,
                );
            }
        }
        _ => {
            // C spell_parser.c:844-846: unknown magic-item type logs a
            // SYSERR for the operator (#254).
            log::error!("SYSERR: Unknown object_type in mag_objectmagic");
        }
    }
}

// ===========================================================================
// do_postbail — pay jail bail.
// ===========================================================================

pub fn do_postbail(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // pk_allowed off by default: jailterm == PLR_KILLER && in jail room.
    let (killer, in_jail) = g
        .get_char(ch)
        .map(|c| {
            (
                c.act_flags & PLR_KILLER != 0,
                c.in_room == g.real_room(g.config.jail_num),
            )
        })
        .unwrap_or((false, false));
    if !(killer && in_jail) {
        g.send_to_char(ch, "You're (happily) not serving a jailterm right now.\r\n");
        return;
    }

    let mut gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
    // GET_BAIL_AMT is a player_specials slot (not modelled) -> default 0 ->
    // bail_multiplier.
    let mut bail = 0;
    if bail <= 0 {
        bail = BAIL_MULTIPLIER;
    }

    // Value of weapon/armor in equipment and inventory.
    let (eq, inv) = g
        .get_char(ch)
        .map(|c| {
            (
                c.equipment.iter().flatten().copied().collect::<Vec<_>>(),
                c.carrying.clone(),
            )
        })
        .unwrap_or((Vec::new(), Vec::new()));
    let eq_val: i32 = eq
        .iter()
        .filter_map(|&o| g.get_obj(o))
        .filter(|o| o.obj_type == ObjectType::Armor || o.obj_type == ObjectType::Weapon)
        .map(|o| o.cost)
        .sum();
    let inven_val: i32 = inv
        .iter()
        .filter_map(|&o| g.get_obj(o))
        .filter(|o| o.obj_type == ObjectType::Armor || o.obj_type == ObjectType::Weapon)
        .map(|o| o.cost)
        .sum();

    g.send_to_char(
        ch,
        &format!(
            "Bail for this offence has been set at {} gold coins.\r\n",
            bail
        ),
    );

    if bail > gold {
        g.send_to_char(ch, "You don't have enough gold on you.\r\n");
        if inven_val > 0 {
            g.send_to_char(ch, "You will have to sell off some of your inventory.\r\n");
        } else if eq_val > 0 {
            g.send_to_char(ch, "You will have to sell off some of your equipment.\r\n");
        } else {
            g.send_to_char(
                ch,
                "You're out of items to sell. Taking your experience points instead.\r\n",
            );
            if let Some(c) = g.get_char_mut(ch) {
                c.points.exp -= ((bail - gold).abs() * XP_MULTIPLIER) as i64;
                crate::gold::set(c, crate::gold::Account::Carried, i64::from(bail));
            }
            gold = bail;
        }
    }

    if bail <= gold {
        let name = g
            .get_char(ch)
            .map(|c| c.player.name.clone())
            .unwrap_or_default();
        g.send_to_char(ch, "Congratulations, you're a free man!\r\n");
        g.send_to_all_players(&format!("&m[&YINFO&m]&n {} has posted bail.\r\n", name));
        if let Some(c) = g.get_char_mut(ch) {
            c.act_flags &= !(PLR_THIEF | PLR_KILLER);
            c.prf_flags &= !PRF_NOAUCT;
            crate::gold::debit(c, crate::gold::Account::Carried, i64::from(bail));
        }
        let cur_room = g.get_char(ch).and_then(|c| c.in_room);
        if let Some(r) = cur_room {
            g.send_to_room(
                r,
                &format!("{} posts bail and is suddenly taken back home.", name),
                None,
            );
        }
        // mortal_start_room[GET_HOME(ch)] — the per-town recall room. Hometown
        // is a vnum on PlayerData; we route them to that room if loaded, else
        // the void (rnum 0), matching C's real_room fallback.
        let home_vnum = g.get_char(ch).map(|c| c.player.hometown).unwrap_or(NOWHERE);
        let dest = g.real_room(home_vnum).unwrap_or(0);
        g.char_from_room(ch);
        g.char_to_room(ch, dest);
        crate::cmd_informative::look_at_room(g, ch, false);
    }
}

// ===========================================================================
// do_not_here — generic "can't do that here" fallback.
// ===========================================================================

pub fn do_not_here(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "Sorry, but you cannot do that here!\r\n");
}

// ===========================================================================
// do_affected — show active aura affects.
// ===========================================================================

pub fn do_affected(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(
        ch,
        "You close your eyes and focus on your aura.\r\n*******************************************\r\n",
    );

    let affects = g
        .get_char(ch)
        .map(|c| c.affected.clone())
        .unwrap_or_default();
    for aff in affects {
        if aff.spell_type == -1 && aff.duration == -1 {
            let bitstr = sprintbit(aff.bitvector, crate::constants::AFFECTED_BITS);
            g.send_to_char(
                ch,
                &format!("  &C{:<21}&npermanent duration.\r\n", bitstr.trim_end()),
            );
            continue;
        }
        // C act.other.c:300-301: `spells[aff->type]` when the type is in range,
        // else the literal "TYPE UNDEFINED". skill_name() is the ported spells[]
        // lookup ("!RESERVED!" / "!UNUSED!" / "UNDEFINED" filler included).
        let label = if aff.spell_type >= 0 && aff.spell_type <= MAX_SKILLS as i32 {
            skill_name(aff.spell_type).to_string()
        } else {
            "TYPE UNDEFINED".to_string()
        };
        let mut buf = format!("  &C{:<21}&n", label);
        let hours = aff.duration + 1;
        if aff.modifier != 0 {
            let loc = crate::constants::APPLY_TYPES
                .get(aff.location as usize)
                .copied()
                .unwrap_or("UNDEFINED");
            buf.push_str(&format!(
                "modifies {} by {:+} for {} hour{}",
                loc,
                aff.modifier,
                hours,
                if aff.duration == 0 { "." } else { "s." }
            ));
        } else {
            buf.push_str(&format!(
                "affects you for {} hour{}",
                hours,
                if aff.duration == 0 { "." } else { "s." }
            ));
        }
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }
    g.send_to_char(ch, "*******************************************\r\n");
}

/// sprintbit(): render a bit-flag long against a name table (CircleMUD).
fn sprintbit(bits: i64, table: &[&str]) -> String {
    let mut out = String::new();
    for (i, name) in table.iter().enumerate() {
        if *name == "\n" {
            break;
        }
        if i < 32 && (bits & (1 << i)) != 0 {
            out.push_str(name);
            out.push(' ');
        }
    }
    if out.is_empty() {
        out.push_str("NOBITS ");
    }
    out
}

// ===========================================================================
// do_slist — class spell/skill listing.
// ===========================================================================

pub fn do_slist(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let argument = argument.trim();
    let class = if is_abbrev_local(argument, "mage") {
        Class::MagicUser
    } else if is_abbrev_local(argument, "cleric") {
        Class::Cleric
    } else if is_abbrev_local(argument, "thief") {
        Class::Thief
    } else if is_abbrev_local(argument, "warrior") {
        Class::Warrior
    } else if is_abbrev_local(argument, "artisan") {
        Class::Artisan
    } else {
        g.get_char(ch)
            .map(|c| c.player.class)
            .unwrap_or(Class::Warrior)
    };

    let class_label = match class {
        Class::MagicUser => "Mage",
        Class::Cleric => "Cleric",
        Class::Thief => "Thief",
        Class::Warrior => "Warrior",
        Class::Artisan => "Artisan",
    };
    g.send_to_char(
        ch,
        &format!(
            "&GSpell/Skill Listing For The {}:\r\nLvl: &BSpells/Skills",
            class_label
        ),
    );
    let class_idx = class as usize;
    let skills = g.get_char(ch).map(|c| c.skills.clone()).unwrap_or_default();
    for level in 1..LVL_IMMORT as i32 {
        let mut row = String::new();
        for spellnum in 0..=TOP_SPELL_DEFINE {
            let si = spell_info(spellnum);
            if si.min_level[class_idx] == level {
                let pct = skills.get(&(spellnum as u16)).copied().unwrap_or(0);
                if row.is_empty() {
                    row.push_str(&format!(
                        "\r\n&G{:<3}: {}{:<20}",
                        level,
                        if pct == 100 { "&m" } else { "&C" },
                        skill_name(spellnum)
                    ));
                } else {
                    row.push_str(&format!(
                        "{}{:<20}",
                        if pct == 100 { "&m" } else { "&C" },
                        skill_name(spellnum)
                    ));
                }
            }
        }
        if !row.is_empty() {
            g.send_to_char(ch, &row);
        }
    }
    g.send_to_char(ch, "\r\n");
}

fn is_abbrev_local(arg: &str, full: &str) -> bool {
    !arg.is_empty() && full.to_lowercase().starts_with(&arg.to_lowercase())
}

// ===========================================================================
// do_email — show / register an email address.
// ===========================================================================

pub fn do_email(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let argument = argument.trim();
    let (farg, _) = one_argument(argument);
    let authenticated_input = crate::interpreter::authenticated_input_authority(g, ch)
        .filter(|authority| authority.principal == ch);

    if farg.is_empty() {
        if authenticated_input.is_none() {
            g.send_to_char(ch, "Email changes require direct authenticated input.\r\n");
            return;
        }
        // No target: register/clear the caller's own email and persist the C
        // extra-data sidecar (plredata/<bucket>/<name>.data).
        let mut name = None;
        if let Some(c) = g.get_char_mut(ch) {
            c.email = if argument.is_empty() {
                None
            } else {
                Some(argument.to_string())
            };
            name = Some(c.get_name().to_string());
        }
        if let Some(name) = name {
            let email = g.get_char(ch).and_then(|c| c.email.as_deref());
            write_extra_email(&g.config.lib_path, &name, email);
        }
        g.send_to_char(ch, "Ok.\r\n");
        return;
    }

    // Targeting another player: prefer their live slot if online, otherwise
    // read the C-compatible extra-data file. C treats no leading '*' as private
    // to mortals and leading '*' as publicly visible.
    let viewer_can_read_private =
        authenticated_input.is_some_and(|authority| authority.authority >= i32::from(LVL_IMMORT));
    let mut email = None;
    let mut known_player = false;
    if let Some(target) = g.find_player_by_name(&farg) {
        known_player = true;
        if let Some(c) = g.get_char(target) {
            email = c
                .email
                .clone()
                .or_else(|| read_extra_email(&g.config.lib_path, c.get_name()));
        }
    } else if let Some(id) = g.get_id_by_name(&farg) {
        known_player = true;
        let name = g.get_name_by_id(id).unwrap_or_else(|| farg.clone());
        email = read_extra_email(&g.config.lib_path, &name);
    }

    if known_player {
        send_email_result(g, ch, viewer_can_read_private, email.as_deref());
        return;
    }

    // Unknown name: register the caller's own email to `argument` (C falls
    // through to the self-registration branch when the file is absent).
    if authenticated_input.is_none() {
        g.send_to_char(ch, "Email changes require direct authenticated input.\r\n");
        return;
    }
    let mut name = None;
    if let Some(c) = g.get_char_mut(ch) {
        c.email = if argument.is_empty() {
            None
        } else {
            Some(argument.to_string())
        };
        name = Some(c.get_name().to_string());
    }
    if let Some(name) = name {
        let email = g.get_char(ch).and_then(|c| c.email.as_deref());
        write_extra_email(&g.config.lib_path, &name, email);
    }
    g.send_to_char(ch, "Ok.\r\n");
}

fn send_email_result(g: &mut GameState, ch: CharId, may_read_private: bool, email: Option<&str>) {
    let Some(addr) = email.filter(|addr| !addr.is_empty()) else {
        g.send_to_char(ch, "They have not registered an email address.\r\n");
        return;
    };
    if let Some(public) = addr.strip_prefix('*') {
        if public.is_empty() {
            g.send_to_char(ch, "They have not registered an email address.\r\n");
        } else {
            g.send_to_char(ch, &format!("{}\r\n", public));
        }
    } else if !may_read_private {
        g.send_to_char(ch, "Their email address is private.\r\n");
    } else {
        g.send_to_char(ch, &format!("{}\r\n", addr));
    }
}

fn extra_data_filename(lib: &str, name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() {
        return None;
    }
    let lname = name.to_lowercase();
    let first = lname.chars().next().unwrap_or('z');
    let middle = match first {
        'a'..='e' => "A-E",
        'f'..='j' => "F-J",
        'k'..='o' => "K-O",
        'p'..='t' => "P-T",
        'u'..='z' => "U-Z",
        _ => "ZZZ",
    };
    Some(
        std::path::Path::new(lib)
            .join("plredata")
            .join(middle)
            .join(format!("{}.data", lname)),
    )
}

fn read_extra_email(lib: &str, name: &str) -> Option<String> {
    let path = extra_data_filename(lib, name)?;
    let data = std::fs::read_to_string(path).ok()?;
    data.lines().find_map(|line| {
        line.strip_prefix("EMAIL ")
            .map(|s| s.trim_end_matches('\r').to_string())
    })
}

fn write_extra_email(lib: &str, name: &str, email: Option<&str>) {
    let Some(path) = extra_data_filename(lib, name) else {
        return;
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<String> = existing
        .lines()
        .filter(|line| !line.starts_with("EMAIL "))
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    if let Some(addr) = email.filter(|addr| !addr.is_empty()) {
        kept.insert(0, format!("EMAIL {}", addr));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if kept.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let body = format!("{}\n", kept.join("\n"));
        let _ = std::fs::write(path, body);
    }
}

// ===========================================================================
// do_build — mortal building program toggle / goto.
// ===========================================================================

/// Leave mortal build mode without re-entering the player command authority
/// gate. Idle/session cleanup invokes this trusted internal transition; the
/// public command path still requires direct authenticated input.
pub(crate) fn exit_build_mode(g: &mut GameState, ch: CharId) -> bool {
    const PRF2_INTANGIBLE: i64 = 1 << 9;
    const PRF2_MBUILDING: i64 = 1 << 6;

    if !g
        .get_char(ch)
        .is_some_and(|character| character.prf2_flags & PRF2_MBUILDING != 0)
    {
        return false;
    }
    let back = g.get_char(ch).and_then(|character| character.was_in_room);
    match back {
        Some(room) => {
            g.send_to_char(ch, "Exiting build mode.\r\n");
            g.char_from_room(ch);
            g.char_to_room(ch, room);
            act(
                g,
                "$n has arrived from building mode.",
                true,
                ch,
                None,
                ActArg::None,
                To::Room,
            );
            crate::cmd_informative::look_at_room(g, ch, true);
        }
        None => {
            g.send_to_char(
                ch,
                "AHH! Something happened and your original room didn't save!\r\nSending you to the void.\r\n",
            );
            g.char_from_room(ch);
            g.char_to_room(ch, 0);
        }
    }
    if let Some(character) = g.get_char_mut(ch) {
        character.prf2_flags &= !(PRF2_MBUILDING | PRF2_INTANGIBLE);
    }
    true
}

pub fn do_build(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    // PRF/PRF2/GCMD building flags (structs.h / gcmd.h).
    const PRF2_INTANGIBLE: i64 = 1 << 9;
    const PRF2_MBUILDING: i64 = 1 << 6;
    const PRF_ROOMFLAGS_L: i64 = PRF_ROOMFLAGS;

    let argument = argument.trim();
    let Some(authority) = crate::interpreter::authenticated_input_authority(g, ch)
        .filter(|authority| authority.principal_is_player && authority.principal == ch)
    else {
        g.send_to_char(
            ch,
            "You are not authorized for the mortal building program.\r\n",
        );
        return;
    };

    // IS_ARENACOMBATANT not modelled (arena subsystem) -> never a combatant.
    let intang = g
        .get_char(ch)
        .map(|c| c.prf2_flags & PRF2_INTANGIBLE != 0)
        .unwrap_or(false);
    let mbuilding = g
        .get_char(ch)
        .map(|c| c.prf2_flags & PRF2_MBUILDING != 0)
        .unwrap_or(false);
    if intang && !mbuilding {
        g.send_to_char(ch, "Intangible players cannot build!\r\n");
        return;
    }

    let Some(target_vnum) = command_atoi(g, ch, argument) else {
        return;
    };
    let target_rnum = g.real_room(target_vnum);

    if target_rnum.is_none() && argument != "off" {
        if target_vnum < 0 {
            g.send_to_char(ch, "Slap yourself for trying that.\r\n");
        } else {
            g.send_to_char(ch, "That room doesn't exist.\r\n");
        }
        return;
    }

    if argument == "off" {
        if !mbuilding {
            g.send_to_char(ch, "You weren't building to begin with.\r\n");
            return;
        }
        exit_build_mode(g, ch);
        return;
    }

    let dest = target_rnum.unwrap();
    if authority.authority >= i32::from(LVL_IMMORT) {
        g.send_to_char(
            ch,
            "Sorry, immortals don't really participate in the mortal building program.\r\n",
        );
        return;
    }

    // PLR_MBUILDER (structs.h) — membership in the building program.
    const PLR_MBUILDER: i64 = 1 << 18;
    if g.get_char(ch)
        .map(|c| c.act_flags & PLR_MBUILDER == 0)
        .unwrap_or(true)
    {
        g.send_to_char(ch, "You're not part of the mortal building program.\r\n");
        return;
    }
    let Some(zone_rnum) = crate::olc::real_zone(g, target_vnum) else {
        g.send_to_char(ch, "You don't have permission to edit that zone.\r\n");
        return;
    };
    if !crate::olc::can_edit_zone(g, ch, zone_rnum) {
        g.send_to_char(ch, "You don't have permission to edit that zone.\r\n");
        return;
    }

    let pos = g
        .get_char(ch)
        .map(|c| c.position)
        .unwrap_or(Position::Standing);
    if pos != Position::Standing {
        g.send_to_char(ch, "You are not in the correct position for that.r\n");
        return;
    }

    if mbuilding {
        // Already building: build == goto.
        g.char_from_room(ch);
        g.char_to_room(ch, dest);
        crate::cmd_informative::look_at_room(g, ch, true);
        return;
    }

    if let Some(c) = g.get_char_mut(ch) {
        c.prf2_flags |= PRF2_MBUILDING | PRF2_INTANGIBLE;
    }
    g.send_to_char(
        ch,
        "You enter build mode.\r\nPlease remember that any and all bugs you find in this MUD should\r\nNOT be abused and should be reported immediatley.\r\n",
    );
    act(
        g,
        "$n enters building mode.",
        true,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    let cur = g.get_char(ch).and_then(|c| c.in_room);
    if let Some(c) = g.get_char_mut(ch) {
        c.was_in_room = cur;
    }
    g.char_from_room(ch);
    g.char_to_room(ch, dest);

    // OLC protection flags: drop summonable, set roomflags + nohassle +
    // holylight (the GCMD load/purge/olc/zreset/peace privileges live in gcmd,
    // not on the Tier-0 Character — documented gap).
    if let Some(c) = g.get_char_mut(ch) {
        c.prf_flags &= !PRF_SUMMONABLE;
        c.prf_flags |= PRF_ROOMFLAGS_L | PRF_NOHASSLE | PRF_HOLYLIGHT;
    }
    crate::cmd_informative::look_at_room(g, ch, true);
}

// ===========================================================================
// do_mobdie — scripted mob self-damage (NPC-only debug hook).
// ===========================================================================

pub fn do_mobdie(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    // mobdie_pwd / mobdie_enabled are admin globals; with the feature disabled
    // by default, every invocation hits the "Huh?!?" branch exactly as C does.
    let is_npc = g.get_char(ch).map(|c| c.is_npc).unwrap_or(false);
    if !is_npc || arg.is_empty() {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }
    // mobdie_enabled defaults false -> always refuse.
    g.send_to_char(ch, "Huh?!?\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::state::PlayerIndex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_lib(name: &str) -> String {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("deltamud-{}-{}", name, stamp));
        std::fs::create_dir_all(&path).unwrap();
        path.to_string_lossy().to_string()
    }

    fn connected_player(g: &mut GameState, conn: ConnId, name: &str, level: Level) -> CharId {
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut ch = Character::new_player(name.to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.player.level = level;
        ch.trust = i32::from(level);
        let id = g.create_char(ch);
        let descriptor = g.descriptors.get_mut(&conn).unwrap();
        descriptor.state = crate::connection::ConState::Playing;
        descriptor.character = Some(id);
        g.players_by_name.insert(name.to_lowercase(), id);
        id
    }

    fn output(g: &GameState, conn: ConnId) -> &str {
        &g.descriptors.get(&conn).unwrap().outbuf
    }

    fn peaceful_steal_fixture(level: Level, trust: i32) -> (GameState, CharId, CharId, ConnId) {
        let mut g = GameState::new(Config::default());
        let room = g.add_room(crate::room::Room::new(
            87_100,
            0,
            "Sanctuary".to_string(),
            "A peaceful test room.".to_string(),
        ));
        g.room_mut(room)
            .room_flags
            .insert(crate::room::RoomFlags::PEACEFUL);
        let conn = ConnId(87_101);
        let thief = connected_player(&mut g, conn, "Thief", level);
        {
            let thief = g.get_char_mut(thief).unwrap();
            thief.trust = trust;
            thief.idnum = 87_101;
            thief.set_skill(SKILL_STEAL, 100);
        }
        let mut victim = Character::new_npc(87_102);
        victim.player.name = "Target".to_string();
        victim.position = Position::Sleeping;
        victim.points.gold = 100;
        let victim = g.create_char(victim);
        g.char_to_room(thief, room);
        g.char_to_room(victim, room);
        (g, thief, victim, conn)
    }

    #[test]
    fn peaceful_steal_override_requires_direct_persisted_implementor_trust() {
        let (mut display_g, display, _, display_conn) = peaceful_steal_fixture(LVL_IMPL, 1);
        crate::interpreter::run_authenticated_command(
            &mut display_g,
            display,
            "steal coins Target",
        );
        assert!(output(&display_g, display_conn).contains("peaceful, easy feeling"));

        let (mut trusted_g, trusted, _, trusted_conn) =
            peaceful_steal_fixture(1, i32::from(LVL_IMPL));
        crate::interpreter::run_authenticated_command(
            &mut trusted_g,
            trusted,
            "steal coins Target",
        );
        assert!(!output(&trusted_g, trusted_conn).contains("peaceful, easy feeling"));

        let (mut indirect_g, indirect, _, indirect_conn) =
            peaceful_steal_fixture(1, i32::from(LVL_IMPL));
        do_steal(&mut indirect_g, indirect, "coins Target", 0);
        assert!(output(&indirect_g, indirect_conn).contains("peaceful, easy feeling"));
    }

    #[test]
    fn do_train_displays_training_counter() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));

        let mut ch = Character::new_player("Mort".to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.spells_to_learn = 9;
        ch.training = 2;
        let ch = g.create_char(ch);

        do_train(&mut g, ch, "", 0);

        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("You have 2 training sessions.\r\n"));
        assert!(!out.contains("You have 9 training sessions.\r\n"));
    }

    #[test]
    fn can_carry_w_uses_strength_table_for_steal_capacity() {
        let mut g = GameState::new(Config::default());
        let mut ch = Character::new_player("Lift".to_string(), Class::Warrior, Race::Human);
        ch.aff_abils.str = 10;
        ch.aff_abils.str_add = 0;
        let ch = g.create_char(ch);

        assert_eq!(can_carry_w(&g, ch), 115);

        let c = g.get_char_mut(ch).unwrap();
        c.aff_abils.str = 18;
        c.aff_abils.str_add = 100;
        assert_eq!(can_carry_w(&g, ch), 480);
    }

    #[test]
    fn item_count_is_cycle_safe_and_depth_bounded() {
        let mut g = GameState::new(Config::default());
        let ch = connected_player(&mut g, ConnId(41), "Counter", 1);

        let mut first = Object::new(NOTHING, "first".into(), "first".into());
        first.obj_type = ObjectType::Container;
        let first = g.create_obj(first);
        let mut second = Object::new(NOTHING, "second".into(), "second".into());
        second.obj_type = ObjectType::Container;
        let second = g.create_obj(second);
        g.get_char_mut(ch).unwrap().carrying.push(first);
        g.get_obj_mut(first).unwrap().loc = crate::object::ObjLoc::Carried(ch);
        g.get_obj_mut(first).unwrap().contains.push(second);
        g.get_obj_mut(second).unwrap().loc = crate::object::ObjLoc::Contained(first);
        // Inject a corrupt legacy back-edge without going through obj_to_obj.
        g.get_obj_mut(second).unwrap().contains.push(first);
        assert_eq!(item_count(&g, ch), 2);

        g.get_char_mut(ch).unwrap().carrying.clear();
        let mut chain = Vec::new();
        for index in 0..crate::object::MAX_OBJECT_GRAPH_DEPTH + 5 {
            let mut object =
                Object::new(NOTHING, format!("chain {index}"), format!("chain {index}"));
            object.obj_type = ObjectType::Container;
            chain.push(g.create_obj(object));
        }
        g.get_char_mut(ch).unwrap().carrying.push(chain[0]);
        g.get_obj_mut(chain[0]).unwrap().loc = crate::object::ObjLoc::Carried(ch);
        for pair in chain.windows(2) {
            g.get_obj_mut(pair[0]).unwrap().contains.push(pair[1]);
            g.get_obj_mut(pair[1]).unwrap().loc = crate::object::ObjLoc::Contained(pair[0]);
        }
        assert_eq!(
            item_count(&g, ch),
            crate::object::MAX_OBJECT_GRAPH_DEPTH as i32
        );
    }

    #[test]
    fn recall_and_retreat_threshold_commands_store_character_fields() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));

        let mut ch = Character::new_player("Runner".to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        ch.points.max_hit = 40;
        let ch = g.create_char(ch);

        do_recall(&mut g, ch, "19", 0);
        assert_eq!(g.get_char(ch).unwrap().recall_level, 19);
        do_recall(&mut g, ch, "", 0);
        assert!(
            g.descriptors
                .get(&conn)
                .unwrap()
                .outbuf
                .contains("Your current recall level is 19 hit points.\r\n")
        );

        do_retreat(&mut g, ch, "17", 0);
        assert_eq!(g.get_char(ch).unwrap().retreat_level, 17);
        do_retreat(&mut g, ch, "0", 0);
        assert_eq!(g.get_char(ch).unwrap().retreat_level, 0);
    }

    #[test]
    fn split_command_accepts_i32_edges_and_rejects_adjacent_overflow() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(4_040_001);
        let ch = connected_player(&mut g, conn, "Splitter", 1);
        crate::gold::set(
            g.get_char_mut(ch).unwrap(),
            crate::gold::Account::Carried,
            i64::from(i32::MAX),
        );
        let original_gold = g.get_char(ch).unwrap().points.gold;

        for (input, expected) in [
            (
                "2147483647",
                "You don't seem to have that much gold to split.\r\n",
            ),
            ("-2147483648", "Sorry, you can't do that.\r\n"),
            (
                "2147483648",
                "That number is outside the supported range.\r\n",
            ),
            (
                "-2147483649",
                "That number is outside the supported range.\r\n",
            ),
        ] {
            g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
            do_split(&mut g, ch, input, 0);
            assert_eq!(output(&g, conn), expected, "input={input:?}");
            assert_eq!(g.get_char(ch).unwrap().points.gold, original_gold);
        }
    }

    #[test]
    fn do_slist_lists_class_spell_rows_from_spell_info() {
        use crate::spell_parser::SKILL_KICK;

        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "War", 1);
        g.get_char_mut(ch)
            .unwrap()
            .set_skill(SKILL_KICK as u16, 100);

        do_slist(&mut g, ch, "warrior", 0);

        let out = output(&g, conn);
        assert!(out.contains("Spell/Skill Listing For The Warrior"));
        assert!(out.contains("&G1  : &mkick"));
        assert!(out.contains("&G3  : &Cmount"));

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_slist(&mut g, ch, "cleric", 0);
        let out = output(&g, conn);
        assert!(out.contains("Spell/Skill Listing For The Cleric"));
        assert!(out.contains("&G1  : &Ccure light"));
        assert!(!out.contains("kick"));
    }

    #[test]
    fn do_email_self_set_writes_extra_data_file() {
        let lib = temp_lib("email-write");
        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        let ch = connected_player(&mut g, ConnId(1), "Alice", 1);

        crate::interpreter::run_authenticated_command(&mut g, ch, "email *alice@example.test");

        assert_eq!(output(&g, ConnId(1)), "Ok.\r\n");
        let path = extra_data_filename(&lib, "Alice").unwrap();
        let data = std::fs::read_to_string(path).unwrap();
        assert!(data.contains("EMAIL *alice@example.test\n"));
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn do_email_offline_player_reads_extra_data() {
        let lib = temp_lib("email-offline");
        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        let ch = connected_player(&mut g, ConnId(1), "Viewer", 1);
        g.player_table.push(PlayerIndex {
            idnum: 42,
            name: "Target".to_string(),
            level: 1,
            trust: 1,
            class: Class::Warrior,
            last_logon: 0,
            host: String::new(),
            act_flags: 0,
            clan: -1,
            clan_rank: -1,
        });
        write_extra_email(&lib, "Target", Some("*target@example.test"));

        do_email(&mut g, ch, "Target", 0);

        assert_eq!(output(&g, ConnId(1)), "target@example.test\r\n");
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn do_email_offline_missing_email_reports_unregistered() {
        let lib = temp_lib("email-missing");
        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        let ch = connected_player(&mut g, ConnId(1), "Viewer", 1);
        g.player_table.push(PlayerIndex {
            idnum: 43,
            name: "Silent".to_string(),
            level: 1,
            trust: 1,
            class: Class::Warrior,
            last_logon: 0,
            host: String::new(),
            act_flags: 0,
            clan: -1,
            clan_rank: -1,
        });

        do_email(&mut g, ch, "Silent", 0);

        assert_eq!(
            output(&g, ConnId(1)),
            "They have not registered an email address.\r\n"
        );
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn do_email_privacy_matches_c_star_rule() {
        let lib = temp_lib("email-privacy");
        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        let mortal = connected_player(&mut g, ConnId(1), "Mort", 1);
        let imm = connected_player(&mut g, ConnId(2), "Imm", LVL_IMMORT);
        let target = connected_player(&mut g, ConnId(3), "Target", 1);

        g.get_char_mut(target).unwrap().email = Some("private@example.test".to_string());
        do_email(&mut g, mortal, "Target", 0);
        assert_eq!(output(&g, ConnId(1)), "Their email address is private.\r\n");

        crate::interpreter::run_authenticated_command(&mut g, imm, "email Target");
        assert_eq!(output(&g, ConnId(2)), "private@example.test\r\n");

        g.descriptors.get_mut(&ConnId(1)).unwrap().outbuf.clear();
        g.get_char_mut(target).unwrap().email = Some("*public@example.test".to_string());
        do_email(&mut g, mortal, "Target", 0);
        assert_eq!(output(&g, ConnId(1)), "public@example.test\r\n");
        let _ = std::fs::remove_dir_all(lib);
    }

    fn add_test_room(g: &mut GameState, vnum: RoomVnum) -> RoomRnum {
        g.add_room(crate::room::Room::new(
            vnum,
            0,
            "A test room".to_string(),
            "A featureless test room.".to_string(),
        ))
    }

    #[test]
    fn mortal_build_requires_authenticated_zone_ownership() {
        let mut g = GameState::new(Config::default());
        for (number, builders) in [(1, "Owner"), (2, "OtherBuilder")] {
            g.zones.push(crate::world::Zone {
                number,
                name: format!("Zone {number}"),
                builders: builders.into(),
                lifespan: 30,
                age: 0,
                top: number * 100 + 99,
                reset_mode: 2,
                min_level: 0,
                max_level: 60,
                status_mode: 0,
                map_x: None,
                map_y: None,
                reset_commands: Vec::new(),
            });
        }
        let origin = g.add_room(crate::room::Room::new(
            101,
            0,
            "Origin".into(),
            "Origin.".into(),
        ));
        let destination = g.add_room(crate::room::Room::new(
            201,
            1,
            "Destination".into(),
            "Destination.".into(),
        ));
        let conn = ConnId(91);
        let ch = connected_player(&mut g, conn, "Owner", 1);
        g.get_char_mut(ch).unwrap().act_flags |= 1 << 18; // PLR_MBUILDER
        g.char_to_room(ch, origin);

        crate::interpreter::run_authenticated_command(&mut g, ch, "build 201");
        assert_eq!(g.get_char(ch).unwrap().in_room, Some(origin));
        assert!(output(&g, conn).contains("permission to edit that zone"));

        g.zones[1].builders = "Owner".into();
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        crate::interpreter::run_command(&mut g, ch, "build 201");
        assert_eq!(g.get_char(ch).unwrap().in_room, Some(origin));
        assert!(output(&g, conn).contains("not authorized"));

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        crate::interpreter::run_authenticated_command(&mut g, ch, "build 201");
        assert_eq!(g.get_char(ch).unwrap().in_room, Some(destination));
        assert_ne!(g.get_char(ch).unwrap().prf2_flags & (1 << 6), 0);
    }

    #[test]
    fn do_affected_names_the_spell_314() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Aured", 30);
        g.get_char_mut(ch).unwrap().affected.push(Affect {
            spell_type: 25, // sanctuary
            duration: 4,
            modifier: 0,
            location: 0,
            bitvector: crate::flags::AFF_SANCTUARY,
            caster: None,
        });

        do_affected(&mut g, ch, "", 0);

        let out = output(&g, conn);
        assert!(out.contains("sanctuary"), "got: {}", out);
        assert!(!out.contains("spell #"), "got: {}", out);
    }

    #[test]
    fn slowns_alternates_between_yes_and_no_322() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Togger", LVL_IMMORT);

        // config.c:254 seeds nameserver_is_slow = YES, so the first toggle
        // reports NO and the second YES (act.other.c:1706-1708).
        do_gen_tog(&mut g, ch, "", SCMD_SLOWNS);
        assert_eq!(
            output(&g, conn),
            "Nameserver_is_slow changed to NO; IP addresses will now be resolved.\r\n"
        );
        assert!(!g.nameserver_is_slow);

        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_gen_tog(&mut g, ch, "", SCMD_SLOWNS);
        assert_eq!(
            output(&g, conn),
            "Nameserver_is_slow changed to YES; sitenames will no longer be resolved.\r\n"
        );
        assert!(g.nameserver_is_slow);
    }

    #[test]
    fn deposit_transfer_reaches_a_player_in_another_room_323() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Donor", 10);
        let here = add_test_room(&mut g, 3001);
        g.char_to_room(ch, here);

        let mut target = Character::new_player("Away".to_string(), Class::Warrior, Race::Human);
        target.player.level = 10;
        let away = g.create_char(target);
        let there = add_test_room(&mut g, 3015);
        g.char_to_room(away, there);

        // The bank has to be reachable: a type-28 ATM object in the room.
        let mut machine =
            crate::object::Object::new(NOTHING, "atm machine".to_string(), "an atm".to_string());
        machine.obj_type = ObjectType::Atm;
        let machine = g.create_obj(machine);
        g.obj_to_room(machine, here);

        crate::gold::set(
            g.get_char_mut(ch).unwrap(),
            crate::gold::Account::Carried,
            100,
        );

        do_gen_atm(&mut g, ch, "Away 40", SCMD_DEPOSIT);

        assert!(output(&g, conn).contains("You deposit 40 coins into"));
        assert_eq!(g.get_char(ch).unwrap().points.gold, 60);
        assert_eq!(g.get_char(away).unwrap().points.bank_gold, 40);
    }

    #[test]
    fn atm_recognises_room_and_carried_bankcards_326() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Banker", 10);
        let here = add_test_room(&mut g, 3001);
        g.char_to_room(ch, here);

        // No ATM anywhere: refuses.
        assert!(!atm_is_in_room(&g, ch));

        // A type-28 (ITEM_ATM) object lying in the room counts.
        let mut machine =
            crate::object::Object::new(NOTHING, "atm machine".to_string(), "an atm".to_string());
        machine.obj_type = ObjectType::Atm;
        let machine = g.create_obj(machine);
        g.obj_to_room(machine, here);
        assert!(atm_is_in_room(&g, ch));
        g.extract_obj(machine);
        assert!(!atm_is_in_room(&g, ch));

        // An unwearable bankcard in the inventory counts too.
        let mut card = crate::object::Object::new(
            NOTHING,
            "bankcard card".to_string(),
            "a bankcard".to_string(),
        );
        card.obj_type = ObjectType::Atm;
        let card = g.create_obj(card);
        g.obj_to_char(card, ch);
        assert!(atm_is_in_room(&g, ch));
        g.extract_obj(card);
        assert!(!atm_is_in_room(&g, ch));

        // A wearable ITEM_ATM does not count while merely carried (C: the
        // carried test requires find_eq_pos(ch, obj, NULL) < 0).
        let mut tabard = crate::object::Object::new(
            NOTHING,
            "bank tabard".to_string(),
            "a bank tabard".to_string(),
        );
        tabard.obj_type = ObjectType::Atm;
        tabard.wear_flags = WearFlags::TAKE | WearFlags::ABOUT;
        let tabard = g.create_obj(tabard);
        g.obj_to_char(tabard, ch);
        assert!(
            !atm_is_in_room(&g, ch),
            "a wearable ATM object must not count while carried"
        );

        // ...but it counts once worn.
        g.obj_from_anywhere(tabard);
        g.equip_char(ch, tabard, WEAR_ABOUT);
        assert!(atm_is_in_room(&g, ch));
        let _ = output(&g, conn);
    }

    #[test]
    fn lockout_defers_password_kdf_and_success_publication_313() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Locked", 10);
        g.descriptors.get_mut(&conn).unwrap().password_hash =
            Some(crate::password::hash_password("sesame"));
        g.get_char_mut(ch).unwrap().prf2_flags |= 1 << 1; // PRF2_LOCKOUT

        do_lockout(&mut g, ch, "sesame", 0);
        assert_eq!(output(&g, conn), "Password verification queued.\r\n");
        assert_ne!(g.get_char(ch).unwrap().prf2_flags & (1 << 1), 0);
        assert_eq!(g.lockout_unlock_requests.len(), 1);
        assert_eq!(g.lockout_unlock_requests[0].plaintext_password, "sesame");
    }

    #[test]
    fn lockout_rejects_an_empty_password_313() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Still", 10);
        g.descriptors.get_mut(&conn).unwrap().password_hash =
            Some(crate::password::hash_password("pw"));
        g.get_char_mut(ch).unwrap().prf2_flags |= 1 << 1;

        do_lockout(&mut g, ch, "", 0);
        assert!(output(&g, conn).contains("Password mismatch!"));
        assert_ne!(g.get_char(ch).unwrap().prf2_flags & (1 << 1), 0);
    }

    #[test]
    fn lockout_without_a_copyover_password_cache_fails_closed() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Recovered", 10);
        assert!(g.descriptors[&conn].password_hash.is_none());
        g.get_char_mut(ch).unwrap().prf2_flags |= 1 << 1;

        do_lockout(&mut g, ch, "anything", 0);

        assert_eq!(
            output(&g, conn),
            "Password verification is unavailable after recovery; reconnect to unlock.\r\n"
        );
        assert_ne!(g.get_char(ch).unwrap().prf2_flags & (1 << 1), 0);
    }

    #[test]
    fn gen_write_appends_report_to_the_typo_file_321() {
        let lib = temp_lib("genwrite");
        std::fs::create_dir_all(std::path::Path::new(&lib).join("misc")).unwrap();
        for f in ["bugs", "typos", "ideas"] {
            std::fs::write(std::path::Path::new(&lib).join("misc").join(f), "").unwrap();
        }
        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Reporter", 10);
        let here = add_test_room(&mut g, 3001);
        g.char_to_room(ch, here);

        do_gen_write(&mut g, ch, "the ceiling leaks", SCMD_TYPO);
        assert_eq!(output(&g, conn), "Okay.  Thanks!\r\n");

        // C act.other.c:1564: "%-8s (%6.6s) [%5s] %s\n", with rcds()'s "%5d".
        let data =
            std::fs::read_to_string(std::path::Path::new(&lib).join("misc").join("typos")).unwrap();
        assert!(data.starts_with("Reporter "), "got: {:?}", data);
        assert!(
            data.contains(") [ 3001] the ceiling leaks\n"),
            "got: {:?}",
            data
        );
        // The other two files stay untouched.
        for f in ["bugs", "ideas"] {
            let other = std::fs::read(std::path::Path::new(&lib).join("misc").join(f)).unwrap();
            assert!(other.is_empty());
        }
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn gen_write_refuses_when_the_file_is_full_321() {
        let lib = temp_lib("genwrite-full");
        std::fs::create_dir_all(std::path::Path::new(&lib).join("misc")).unwrap();
        std::fs::write(
            std::path::Path::new(&lib).join("misc").join("bugs"),
            "x".repeat(MAX_FILESIZE as usize),
        )
        .unwrap();

        let mut cfg = Config::default();
        cfg.lib_path = lib.clone();
        let mut g = GameState::new(cfg);
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Reporter", 10);
        let here = add_test_room(&mut g, 3001);
        g.char_to_room(ch, here);

        do_gen_write(&mut g, ch, "stuck", SCMD_BUG);
        assert_eq!(
            output(&g, conn),
            "Sorry, the file is full right now.. try again later.\r\n"
        );
        let _ = std::fs::remove_dir_all(lib);
    }

    #[test]
    fn practice_lists_class_skills_with_min_level_gating_312() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Recruit", 1);

        do_practice(&mut g, ch, "", 0);

        let out = output(&g, conn);
        // The C header, verbatim (spec_procs.c:146 / 148).
        assert!(out.contains("You have no practice sessions remaining.\r\n"));
        assert!(out.contains("You know of the following skills:\r\n"));
        // kick is min_level 1 for warriors: listed even though not learned.
        assert!(out.contains(&format!("{:<20} {}\r\n", "kick", how_good(0))));
        // mount is min_level 3 for warriors: gated out at level 1.
        assert!(!out.contains("mount"));

        while crate::modify::page_active(conn) {
            crate::modify::page_input(&mut g, conn, "q");
        }
        assert!(!crate::modify::page_active(conn));
    }

    #[test]
    fn practice_shows_remaining_sessions_and_proficiency_312() {
        use crate::spell_parser::SKILL_KICK;
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Fighter", 30);
        g.get_char_mut(ch).unwrap().spells_to_learn = 1;
        g.get_char_mut(ch).unwrap().set_skill(SKILL_KICK as u16, 65);

        do_practice(&mut g, ch, "", 0);

        let out = output(&g, conn);
        assert!(out.contains("You have 1 practice session remaining.\r\n"));
        assert!(out.contains(&format!("{:<20} {}\r\n", "kick", how_good(65))));
        // Gating is by level, not proficiency: parry is warrior level 31.
        assert!(!out.contains("parry"));

        // The pager is process-global and keyed by ConnId; drain it so a later
        // test on this connection is not treated as a pager command.
        while crate::modify::page_active(conn) {
            crate::modify::page_input(&mut g, conn, "q");
        }
        assert!(!crate::modify::page_active(conn));
    }

    #[test]
    fn observe_blocks_a_non_observer_outside_the_observatory_311() {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let ch = connected_player(&mut g, conn, "Watcher", 10);
        let here = add_test_room(&mut g, 3001);
        g.char_to_room(ch, here);

        do_observe(&mut g, ch, "", 0);
        assert_eq!(
            output(&g, conn),
            "You can't do that now! Get to the observatory!\r\n"
        );
    }

    #[test]
    fn observe_reports_and_retargets_from_the_observatory_311() {
        use crate::arena::{ARENA_COMBATANT1, ARENA_OBSERVER, set_stat_for_test};
        let _guard = crate::lock_ok::lock(&crate::arena::ARENA_TEST_LOCK);
        crate::arena::reset_for_tests();
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);

        // The arena status table is process-global and keyed by CharId, so burn
        // a block of ids that no other test's chars (which all start from 1)
        // can ever reach before touching the table.
        for _ in 0..500 {
            let _ = g.create_char(Character::new_player(
                "Filler".to_string(),
                Class::Warrior,
                Race::Human,
            ));
        }

        let ch = connected_player(&mut g, conn, "Watcher", 10);
        let obs = add_test_room(&mut g, 4899);
        g.char_to_room(ch, obs);
        set_stat_for_test(ch, ARENA_OBSERVER);

        // An arena combatant somewhere else in the world.
        let mut foe = Character::new_player("Gladiatr".to_string(), Class::Warrior, Race::Human);
        foe.player.level = 10;
        let foe = g.create_char(foe);
        let pit = add_test_room(&mut g, 3005);
        g.char_to_room(foe, pit);
        set_stat_for_test(foe, ARENA_COMBATANT1);

        // No argument: "nobody" initially.
        do_observe(&mut g, ch, "", 0);
        assert!(output(&g, conn).contains("observing the actions of nobody."));

        // An immortal is refused even though visible.
        let mut god = Character::new_player("Zap".to_string(), Class::Warrior, Race::Human);
        god.player.level = LVL_IMPL;
        let god = g.create_char(god);
        let den = add_test_room(&mut g, 3006);
        g.char_to_room(god, den);
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_observe(&mut g, ch, "zap", 0);
        assert_eq!(output(&g, conn), "You dare not.\r\n");

        // The world-scope lookup finds the combatant and links the chain.
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_observe(&mut g, ch, "gladiatr", 0);
        assert_eq!(
            output(&g, conn),
            "You're now observing the actions of Gladiatr.\r\n"
        );
        assert_eq!(crate::arena::arena_observing(ch), Some(foe));

        // Observing yourself by name detaches (C act.other.c:1826-1830).
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        do_observe(&mut g, ch, "watcher", 0);
        assert_eq!(output(&g, conn), "Ok. You're observing nobody now.\r\n");
        assert_eq!(crate::arena::arena_observing(ch), None);

        // Drop our entries so the shared table stays clean.
        crate::arena::forget_char(ch);
        crate::arena::forget_char(foe);
        crate::arena::forget_char(god);
        crate::arena::reset_for_tests();
    }
}
