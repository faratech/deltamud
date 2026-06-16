// cmd_informative.rs — full port of C `src/act.informative.c` (the player-level
// informative commands). Every ACMD in that file is ported here against the
// single-owner GameState contract (state.rs / handler.rs / act.rs).
//
// House style (see commands.rs): read needed values into locals first, then
// mutate / send. Look entities up by id each time. Color is emitted as literal
// `&`-codes (the C CC*/screen.h macros expand to these `&x` codes); the output
// path strips them per-player. Direct text via g.send_to_char; room/victim
// broadcasts via act().
//
// Notes on contract gaps (see helpers_needed in the manifest): there is no
// world weather/time, citizen, quest-points or arena tables exposed yet, so the
// few branches that depend on those use the documented defaults (citizen 0,
// quest/arena 0, world time derived from g.pulse) and are called out so the
// integrator can wire the real sources later. The command *logic* and output
// strings are otherwise faithful 1:1.

use crate::act::{act, ActArg, To};
use crate::constants;
use crate::flags::*;
use crate::handler::{get_number, isname};
use crate::interpreter::{half_chop, is_abbrev, one_argument, search_block};
use crate::object::{ObjLoc, ObjectType};
use crate::room::{EX_CLOSED, EX_ISDOOR, EX_LOCKED, EX_PICKPROOF};
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Local flag constants not yet present in flags.rs (structs.h values).
// ---------------------------------------------------------------------------
const PRF_DEAF: i64 = 1 << 2;
const PRF_NOTELL: i64 = 1 << 3;
const PRF_DISPHP: i64 = 1 << 4;
const PRF_DISPMANA: i64 = 1 << 5;
const PRF_DISPMOVE: i64 = 1 << 6;
const PRF_AUTOEXIT: i64 = 1 << 7;
const PRF_SUMMONABLE: i64 = 1 << 10;
const PRF_NOREPEAT: i64 = 1 << 11;
const PRF_NOAUCT: i64 = 1 << 18;
const PRF_NOGOSS: i64 = 1 << 19;
const PRF_NOGRATZ: i64 = 1 << 20;
const PRF_ROOMFLAGS: i64 = 1 << 21;
const PRF_AFK: i64 = 1 << 22;
const PRF_AUTOSPLIT: i64 = 1 << 23;
const PRF_AUTOLOOT: i64 = 1 << 24;
const PRF_AUTOGOLD: i64 = 1 << 25;
const PRF_DISPEXP: i64 = 1 << 26;
const PRF_NOTIC: i64 = 1 << 27;
const PRF_NOLOOKSTACK: i64 = 1 << 30;

const PRF2_QCHAN: i64 = 1 << 0;
const PRF2_NOMAP: i64 = 1 << 2;
const PRF2_MERCY: i64 = 1 << 7;
const PRF2_ADVANCEDMAP: i64 = 1 << 8;
const PRF2_INTANGIBLE: i64 = 1 << 9;
const PRF2_MBUILDING: i64 = 1 << 6;
const PRF2_LOCKOUT: i64 = 1 << 1;

const PLR_WRITING: i64 = 1 << 4;
const PLR_KILLER: i64 = 1 << 0;
const PLR_THIEF: i64 = 1 << 1;
const PLR_QUESTOR: i64 = 1 << 16;

// AFF flags referenced here but not in flags.rs.
const AFF_CONVERGENCE: i64 = 1 << 17;
const AFF_AUTUS: i64 = 1 << 20;
const AFF_PLAGUED: i64 = 1 << 23;
const AFF_CHAINED: i64 = 1 << 24;
const AFF_NOPORTAL: i64 = 1 << 22;
const AFF_SENSE_LIFE2: i64 = AFF_SENSE_LIFE;

// Extra object stat bits (ExtraFlags mirror — values match object.rs).
const ITEM_INVISIBLE_BIT: u64 = 1 << 5;
const ITEM_BLESS_BIT: u64 = 1 << 8;
const ITEM_MAGIC_BIT: u64 = 1 << 6;
const ITEM_GLOW_BIT: u64 = 1 << 0;
const ITEM_HUM_BIT: u64 = 1 << 1;

const CONT_CLOSED: i32 = 1 << 2;

// EX_HIDDEN (structs.h) — not surfaced in room.rs.
const EX_HIDDEN: i32 = 1 << 4;

// Special level constants used by look_at_char / do_status.
const LVL_HERO: u8 = 100;
const LVL_DEMIGOD: u8 = 102;

// jail vnum used by do_checkbail (hardcoded jail_num in C globals.c).
const JAIL_NUM: RoomVnum = 3030;

// ---------------------------------------------------------------------------
// Small pure helpers (no GameState).
// ---------------------------------------------------------------------------

/// CircleMUD CAP(): capitalise first character in place.
fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// CircleMUD fname(): first whitespace-delimited keyword of a namelist.
fn fname(namelist: &str) -> &str {
    namelist.split_whitespace().next().unwrap_or("")
}

/// AN(arg): "an" if arg begins with a vowel, else "a".
fn an(arg: &str) -> &'static str {
    match arg.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a') | Some('e') | Some('i') | Some('o') | Some('u') => "an",
        _ => "a",
    }
}

/// numdisplay(): comma-group a signed integer ("1234567" -> "1,234,567").
fn numdisplay(val: i64) -> String {
    let neg = val < 0;
    let digits = val.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        out.push(*b as char);
        let remaining = len - i - 1;
        if remaining > 0 && remaining % 3 == 0 {
            out.push(',');
        }
    }
    if neg {
        format!("-{}", out)
    } else {
        out
    }
}

/// mlog(b, arg): CircleMUD utils.c mlog -> log(arg) / log(b).
fn mlog(b: f64, arg: i32) -> f64 {
    (arg as f64).ln() / b.ln()
}

/// exp_to_level(arg): cumulative experience needed to reach `arg` (utils.c).
fn exp_to_level(arg: i32) -> i64 {
    if arg < 1 || arg as u8 > LVL_IMPL {
        return 0;
    }
    let modifier = if arg < 10 {
        mlog(3.0, arg)
    } else if arg < 20 {
        mlog(2.9, arg)
    } else if arg < 30 {
        mlog(2.8, arg)
    } else if arg < 40 {
        mlog(2.7, arg)
    } else if arg < 50 {
        mlog(2.6, arg)
    } else if arg < 60 {
        mlog(2.5, arg)
    } else if arg < 70 {
        mlog(2.4, arg)
    } else if arg < 80 {
        mlog(2.3, arg)
    } else if arg < 90 {
        mlog(2.2, arg)
    } else if arg < 96 {
        mlog(2.05, arg)
    } else {
        mlog(2.0, arg)
    };
    if arg == 1 {
        3000
    } else {
        (4000.0 * (arg as f64 * modifier)) as i64 + exp_to_level(arg - 1)
    }
}

fn town_name(home: RoomVnum) -> &'static str {
    match home {
        1 => "Anacreon",
        2 => "Jhaden",
        3 => "Strundhaven",
        4 => "Locris",
        _ => "",
    }
}

fn class_name(class: Class) -> &'static str {
    match class {
        Class::MagicUser => "Mage",
        Class::Cleric => "Cleric",
        Class::Thief => "Thief",
        Class::Warrior => "Warrior",
        Class::Artisan => "Artisan",
    }
}

fn race_name(race: Race) -> &'static str {
    match race {
        Race::Human => "Human",
        Race::Elf => "Elf",
        Race::Gnome => "Gnome",
        Race::Dwarf => "Dwarf",
        Race::Troll => "Troll",
        Race::Orc => "Orc",
        Race::Minotaur => "Minotaur",
        // Remaining DeltaMUD races fall through to a sane label.
        Race::HalfElf => "Half-Elf",
        Race::Kender => "Kender",
        Race::Vampire => "Vampire",
        Race::Ogre => "Ogre",
        Race::HalfOrc => "Half-Orc",
    }
}

fn deity_name(deity: u8) -> &'static str {
    match deity {
        0 => "Aetos",
        1 => "Corgus",
        2 => "Lythern",
        3 => "Pallas",
        4 => "Eros",
        5 => "Cailia",
        6 => "Marbin",
        7 => "Poseidon",
        8 => "Lelu",
        9 => "Chaos",
        10 => "Elestra",
        11 => "Nemonica",
        12 => "Durn",
        13 => "Ranus",
        14 => "Incubus",
        _ => "",
    }
}

/// sex_name(): capitalised subjective pronoun (He/She/It) as used by look_at_char.
fn sex_name(sex: Gender) -> &'static str {
    match sex {
        Gender::Male => "He",
        Gender::Female => "She",
        Gender::Neutral => "It",
    }
}

fn class_abbr(class: Class) -> &'static str {
    match class {
        Class::MagicUser => "Mag",
        Class::Cleric => "Cle",
        Class::Thief => "Thi",
        Class::Warrior => "War",
        Class::Artisan => "Art",
    }
}

fn race_abbr(race: Race) -> &'static str {
    match race {
        Race::Human => "Hum",
        Race::Elf => "Elf",
        Race::Gnome => "Gno",
        Race::Dwarf => "Dwa",
        Race::Troll => "Tro",
        Race::Orc => "Orc",
        Race::Minotaur => "Min",
        Race::HalfElf => "Hel",
        Race::Kender => "Ken",
        Race::Vampire => "Vam",
        Race::Ogre => "Ogr",
        Race::HalfOrc => "Hor",
    }
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

/// rcds(): immortal room-display string. CircleMUD prints the room vnum; we
/// emit just the decimal vnum here (the C builds "vnum" into a static buffer).
fn rcds(g: &GameState, rnum: RoomRnum) -> String {
    g.room_opt(rnum)
        .map(|r| r.number.to_string())
        .unwrap_or_else(|| "?".to_string())
}

// ---------------------------------------------------------------------------
// Visibility / darkness helpers, ported against the room/light model exposed
// by the contract (Room::is_dark, AFF_INFRAVISION, PRF_HOLYLIGHT).
// ---------------------------------------------------------------------------

fn can_see_in_dark(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_INFRAVISION != 0 || c.prf_flags & PRF_HOLYLIGHT != 0)
        .unwrap_or(false)
}

fn room_is_dark(g: &GameState, rnum: RoomRnum) -> bool {
    g.room_opt(rnum).map(|r| r.is_dark()).unwrap_or(false)
}

fn is_blind(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_BLIND != 0)
        .unwrap_or(false)
}

/// CAN_SEE_OBJ for the (Tier-0) object model: invisible items need detect.
fn can_see_obj(g: &GameState, ch: CharId, oid: ObjId) -> bool {
    let obj = match g.get_obj(oid) {
        Some(o) => o,
        None => return false,
    };
    let invis = obj.extra_flags.bits() & ITEM_INVISIBLE_BIT != 0;
    if !invis {
        return true;
    }
    g.get_char(ch)
        .map(|c| c.affect_flags & AFF_DETECT_INVIS != 0)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// show_obj_to_char / list_obj_to_char (CircleMUD)
// ---------------------------------------------------------------------------

/// Build the show_obj_to_char string for one object (mode mirrors the C modes:
/// 0=description, 1..4=short_description, 5=examine-with-no-special,
/// 6=stat-flags-only). Returns the text (no leading color); the caller sends it.
fn show_obj_string(g: &mut GameState, ch: CharId, oid: ObjId, mode: i32) {
    let (otype, desc, short, action) = match g.get_obj(oid) {
        Some(o) => (
            o.obj_type,
            o.description.clone(),
            o.short_description.clone(),
            o.action_description.clone(),
        ),
        None => return,
    };

    let mut buf = String::new();
    if mode == 0 && !desc.is_empty() {
        buf.push_str(&desc);
    } else if !short.is_empty() && (mode == 1 || mode == 2 || mode == 3 || mode == 4) {
        buf.push_str(&short);
    } else if mode == 5 {
        if otype == ObjectType::Note {
            match action {
                Some(a) if !a.is_empty() => {
                    let text = format!("There is something written upon it:\r\n\r\n{}", a);
                    g.send_to_char(ch, &text);
                }
                _ => act(g, "It's blank.", false, ch, None, ActArg::None, To::Char),
            }
            return;
        } else if otype != ObjectType::LiqContainer {
            buf.push_str("You see nothing special..");
        } else {
            buf.push_str("It looks like a drink container.");
        }
    }

    if mode != 3 {
        let (extra, det_align, det_magic) = match (g.get_obj(oid), g.get_char(ch)) {
            (Some(o), Some(c)) => (
                o.extra_flags.bits(),
                c.affect_flags & AFF_DETECT_ALIGN != 0,
                c.affect_flags & AFF_DETECT_MAGIC != 0,
            ),
            _ => (0u64, false, false),
        };
        if extra & ITEM_INVISIBLE_BIT != 0 {
            buf.push_str(" (invisible)");
        }
        if extra & ITEM_BLESS_BIT != 0 && det_align {
            buf.push_str(" ..It glows blue!");
        }
        if extra & ITEM_MAGIC_BIT != 0 && det_magic {
            buf.push_str(" ..It glows yellow!");
        }
        if extra & ITEM_GLOW_BIT != 0 {
            buf.push_str(" ..It has a soft glowing aura!");
        }
        if extra & ITEM_HUM_BIT != 0 {
            buf.push_str(" ..It emits a faint humming sound!");
        }
    }
    buf.push_str("\r\n");
    g.send_to_char(ch, &buf);
}

/// list_obj_to_char with stacking ((N) prefix) (CircleMUD).
fn list_obj_to_char(g: &mut GameState, ch: CharId, list: &[ObjId], mode: i32, show: bool) {
    let mut found = false;
    let n = list.len();
    for i in 0..n {
        let oid = list[i];
        // De-dup: skip if an earlier identical object already represented this one.
        let mut earlier = false;
        for &j in list.iter().take(i) {
            if same_obj(g, j, oid) {
                earlier = true;
                break;
            }
        }
        if earlier {
            continue;
        }
        // Count copies.
        let mut num = 0;
        for &j in &list[i..] {
            if same_obj(g, j, oid) {
                num += 1;
            }
        }
        if can_see_obj(g, ch, oid) {
            g.send_to_char(ch, "&G");
            if num != 1 {
                g.send_to_char(ch, &format!("({}) ", num));
            }
            show_obj_string(g, ch, oid, mode);
            found = true;
        }
    }
    if !found && show {
        g.send_to_char(ch, " Nothing.\r\n");
    }
}

/// Two objects "stack" if same prototype vnum, or (synthetic) same short desc.
fn same_obj(g: &GameState, a: ObjId, b: ObjId) -> bool {
    let (oa, ob) = match (g.get_obj(a), g.get_obj(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => return false,
    };
    if oa.item_number == NOTHING || ob.item_number == NOTHING {
        oa.short_description == ob.short_description
    } else {
        oa.item_number == ob.item_number
    }
}

// ---------------------------------------------------------------------------
// diag_char_to_char (CircleMUD)
// ---------------------------------------------------------------------------

fn diag_char_to_char(g: &mut GameState, i: CharId, ch: CharId) {
    let (max, hit) = match g.get_char(i) {
        Some(c) => (c.points.max_hit, c.points.hit),
        None => return,
    };
    let percent = if max > 0 { (100 * hit) / max } else { -1 };
    let name = pers(g, ch, i);
    let mut buf = cap(&name);
    let tail = if percent >= 100 {
        " is in excellent condition.\r\n"
    } else if percent >= 90 {
        " has a few scratches.\r\n"
    } else if percent >= 75 {
        " has some small wounds and bruises.\r\n"
    } else if percent >= 50 {
        " has quite a few wounds.\r\n"
    } else if percent >= 30 {
        " has some big nasty wounds and scratches.\r\n"
    } else if percent >= 15 {
        " looks pretty hurt.\r\n"
    } else if percent >= 0 {
        " is in awful condition.\r\n"
    } else {
        " is bleeding awfully from big wounds.\r\n"
    };
    buf.push_str(tail);
    g.send_to_char(ch, &buf);
}

/// PERS(i, ch): visible name (NPC short / PC name), else mystical/someone.
fn pers(g: &GameState, viewer: CharId, target: CharId) -> String {
    if g.can_see(viewer, target) {
        if let Some(c) = g.get_char(target) {
            return if c.is_npc {
                c.short_desc
                    .clone()
                    .unwrap_or_else(|| c.player.name.clone())
            } else {
                c.player.name.clone()
            };
        }
    }
    let imm = g
        .get_char(target)
        .map(|c| !c.is_npc && c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    if imm {
        "A Mystical Being".to_string()
    } else {
        "someone".to_string()
    }
}

// ---------------------------------------------------------------------------
// look_at_char (CircleMUD)
// ---------------------------------------------------------------------------

fn look_at_char(g: &mut GameState, i: CharId, ch: CharId) {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return;
    }
    let desc = g
        .get_char(i)
        .map(|c| c.player.description.clone())
        .unwrap_or_default();
    if !desc.is_empty() {
        g.send_to_char(ch, &desc);
    } else {
        act(
            g,
            "You see nothing special about $m.",
            false,
            i,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
    }

    diag_char_to_char(g, i, ch);

    // Citizen/level title line (PCs only).
    let (is_npc, level, sex, race, class, home) = match g.get_char(i) {
        Some(c) => (
            c.is_npc,
            c.player.level,
            c.player.sex,
            c.player.race,
            c.player.class,
            c.player.hometown,
        ),
        None => return,
    };
    if !is_npc {
        let role = if level == LVL_HERO {
            "hero"
        } else if level == LVL_IMMORT {
            "immortal"
        } else if level == LVL_DEMIGOD {
            "Demi God"
        } else if level >= LVL_GOD {
            "God"
        } else {
            "citizen"
        };
        let line = format!(
            "{} is a {} {} and {} of {}.\r\n",
            sex_name(sex),
            race_name(race),
            class_name(class),
            role,
            town_name(home)
        );
        g.send_to_char(ch, &line);
    }

    // Equipment in use.
    let eq: Vec<(usize, ObjId)> = g
        .get_char(i)
        .map(|c| {
            c.equipment
                .iter()
                .enumerate()
                .filter_map(|(p, o)| o.map(|oid| (p, oid)))
                .collect()
        })
        .unwrap_or_default();
    let any_eq = eq.iter().any(|&(_, oid)| can_see_obj(g, ch, oid));
    if any_eq {
        act(
            g,
            "\r\n$n is using:",
            false,
            i,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        for (p, oid) in &eq {
            if can_see_obj(g, ch, *oid) {
                let label = constants::WHERE.get(*p).copied().unwrap_or("");
                g.send_to_char(ch, label);
                show_obj_string(g, ch, *oid, 1);
            }
        }
    }

    // Thief / immortal peek at inventory.
    let (vch_class, vch_level) = match g.get_char(ch) {
        Some(c) => (c.player.class, c.player.level),
        None => return,
    };
    if ch != i && (vch_class == Class::Thief || vch_level >= LVL_IMMORT) {
        act(
            g,
            "\r\nYou attempt to peek at $s inventory:",
            false,
            i,
            None,
            ActArg::Char(ch),
            To::Vict,
        );
        let inv = g
            .get_char(i)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        let mut found = false;
        for oid in inv {
            if can_see_obj(g, ch, oid) && (g.rng.number(0, 20) < vch_level as i32) {
                show_obj_string(g, ch, oid, 1);
                found = true;
            }
        }
        if !found {
            g.send_to_char(ch, "You can't see anything.\r\n");
        }
    }
}

// ---------------------------------------------------------------------------
// list_all_char / list_char_to_char (CircleMUD)
// ---------------------------------------------------------------------------

const POSITIONS: [&str; 10] = [
    " is lying here, dead.",
    " is lying here, mortally wounded.",
    " is lying here, incapacitated.",
    " is lying here, stunned.",
    " is sleeping here.",
    " is resting here.",
    " is meditating here.",
    " is sitting here.",
    "!FIGHTING!",
    " is standing here.",
];

/// Build the room-listing line for one character (CircleMUD list_all_char).
fn list_all_char(g: &GameState, i: CharId, ch: CharId) -> String {
    let c = match g.get_char(i) {
        Some(c) => c,
        None => return String::new(),
    };

    // NPC with a long description, standing in its default position.
    if c.is_npc {
        if let Some(long) = &c.long_desc {
            if !long.is_empty() {
                let mut buf = String::new();
                if c.affect_flags & AFF_INVISIBLE != 0 {
                    buf.push('*');
                }
                if g.get_char(ch)
                    .map(|v| v.affect_flags & AFF_DETECT_ALIGN != 0)
                    .unwrap_or(false)
                {
                    if c.alignment <= -350 {
                        buf.push_str("(Red Aura) ");
                    } else if c.alignment >= 350 {
                        buf.push_str("(Blue Aura) ");
                    }
                }
                buf.push_str(long);
                // sanctuary / convergence / autus / blind suffixes
                push_aff_glows(&mut buf, c, hssh(c.player.sex));
                return buf;
            }
        }
    }

    let mut buf;
    if c.is_npc {
        buf = cap(c.short_desc.as_deref().unwrap_or(&c.player.name));
    } else if (c.player.level as u8) < LVL_IMMORT && c.citizen > 0 {
        // Mortal town-citizen: prefix the gendered rank title (act.informative.c).
        buf = format!(
            "&Y{}{} {}&Y",
            c.read_citizen(),
            c.player.name,
            c.get_title()
        );
    } else {
        buf = format!("&Y{} {}&Y", c.player.name, c.get_title());
    }

    if c.affect_flags & AFF_INVISIBLE != 0 {
        buf.push_str(" (invisible)");
    }
    if c.affect_flags & AFF_HIDE != 0 {
        buf.push_str(" (hidden)");
    }
    if !c.is_npc && c.desc.is_none() {
        buf.push_str(" (linkless)");
    }
    if c.act_flags & PLR_WRITING != 0 && !c.is_npc {
        buf.push_str(" (writing)");
    }
    if c.prf_flags & PRF_AFK != 0 {
        buf.push_str(" (afk)");
    }
    if c.prf2_flags & PRF2_LOCKOUT != 0 {
        buf.push_str(" (afk-locked)");
    }
    if c.prf2_flags & PRF2_INTANGIBLE != 0 {
        if c.prf2_flags & PRF2_MBUILDING != 0 {
            buf.push_str(" (building)");
        } else {
            buf.push_str(" (dead)");
        }
    }

    // Position / fighting.
    if c.position != Position::Fighting {
        buf.push_str(POSITIONS[c.position as usize]);
    } else if let Some(opp) = c.fighting {
        buf.push_str(" is here, fighting ");
        if opp == ch {
            buf.push_str("YOU!");
        } else {
            if g.get_char(opp).and_then(|o| o.in_room) == c.in_room {
                buf.push_str(&pers(g, ch, opp));
            } else {
                buf.push_str("someone who has already left");
            }
            buf.push('!');
        }
    } else {
        buf.push_str(" is here struggling with thin air.");
    }

    if g.get_char(ch)
        .map(|v| v.affect_flags & AFF_DETECT_ALIGN != 0)
        .unwrap_or(false)
    {
        if c.alignment <= -350 {
            buf.push_str(" (Red Aura)");
        } else if c.alignment >= 350 {
            buf.push_str(" (Blue Aura)");
        }
    }
    buf.push_str("\r\n");
    push_aff_glows(&mut buf, c, hssh(c.player.sex));
    buf
}

fn hssh(sex: Gender) -> &'static str {
    match sex {
        Gender::Male => "he",
        Gender::Female => "she",
        Gender::Neutral => "it",
    }
}

fn push_aff_glows(buf: &mut String, c: &crate::character::Character, hssh: &str) {
    if c.affect_flags & AFF_SANCTUARY != 0 {
        buf.push_str(&format!("...{} glows with a bright light!\r\n", hssh));
    }
    if c.affect_flags & AFF_CONVERGENCE != 0 {
        buf.push_str(&format!("...{} glows in a red aura!\r\n", hssh));
    }
    if c.affect_flags & AFF_AUTUS != 0 {
        buf.push_str(&format!("...{} glows in a green aura!\r\n", hssh));
    }
    if c.affect_flags & AFF_BLIND != 0 {
        buf.push_str(&format!("...{} is groping around blindly!\r\n", hssh));
    }
}

/// list_char_to_char with [count] stacking (CircleMUD).
fn list_char_to_char(g: &mut GameState, list: &[CharId], ch: CharId) {
    let mut lines: Vec<(String, i32)> = Vec::new();
    let dark = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .map(|r| room_is_dark(g, r))
        .unwrap_or(false);
    let see_dark = can_see_in_dark(g, ch);
    for &i in list {
        if i == ch {
            continue;
        }
        let line;
        if g.can_see(ch, i) {
            line = list_all_char(g, i, ch);
        } else if dark
            && !see_dark
            && g.get_char(i)
                .map(|c| c.affect_flags & AFF_INFRAVISION != 0)
                .unwrap_or(false)
        {
            line = "You see a pair of glowing red eyes looking your way.\r\n".to_string();
        } else {
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if let Some(slot) = lines.iter_mut().find(|(s, _)| *s == line) {
            slot.1 += 1;
        } else {
            lines.push((line, 1));
        }
    }
    for (line, count) in lines {
        if count > 1 {
            g.send_to_char(ch, &format!("[{:3}] ", count));
        }
        g.send_to_char(ch, &line);
    }
}

/// list_nostack_char_to_char (no [N] grouping) (CircleMUD).
fn list_nostack_char_to_char(g: &mut GameState, list: &[CharId], ch: CharId) {
    let dark = g
        .get_char(ch)
        .and_then(|c| c.in_room)
        .map(|r| room_is_dark(g, r))
        .unwrap_or(false);
    let see_dark = can_see_in_dark(g, ch);
    for &i in list {
        if i == ch {
            continue;
        }
        if g.can_see(ch, i) {
            let line = list_all_char(g, i, ch);
            g.send_to_char(ch, &line);
        } else if dark
            && !see_dark
            && g.get_char(i)
                .map(|c| c.affect_flags & AFF_INFRAVISION != 0)
                .unwrap_or(false)
        {
            g.send_to_char(
                ch,
                "You see a pair of glowing red eyes looking your way.\r\n",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// do_auto_exits / look_at_room (CircleMUD)
// ---------------------------------------------------------------------------

fn do_auto_exits(g: &mut GameState, ch: CharId) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let imm = g
        .get_char(ch)
        .map(|c| c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    let mut buf = String::new();
    for door in 0..NUM_OF_DIRS {
        let exit = match g.room(rnum).exits[door].clone() {
            Some(e) => e,
            None => continue,
        };
        if g.real_room(exit.to_room).is_none() {
            continue;
        }
        let dchar = DIR_NAMES[door].chars().next().unwrap_or('?');
        if exit.exit_info & EX_CLOSED != 0 {
            buf.push(dchar);
            buf.push_str("* ");
        } else {
            buf.push(dchar);
            buf.push(' ');
        }
    }
    let body = if buf.is_empty() { "None! " } else { &buf };
    let _ = imm;
    g.send_to_char(ch, &format!("&C[ Exits: {}]&n\r\n", body));
}

/// look_at_room (CircleMUD).
pub fn look_at_room(g: &mut GameState, ch: CharId, ignore_brief: bool) {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };

    if room_is_dark(g, rnum) && !can_see_in_dark(g, ch) {
        g.send_to_char(ch, "It is pitch black...\r\n");
        return;
    } else if is_blind(g, ch) {
        g.send_to_char(ch, "You see nothing but infinite darkness...\r\n");
        return;
    }

    let roomflags = g
        .get_char(ch)
        .map(|c| c.prf_flags & PRF_ROOMFLAGS != 0)
        .unwrap_or(false);
    g.send_to_char(ch, "&C");
    if roomflags {
        let (vnum, name, flags) = {
            let r = g.room(rnum);
            (r.number, r.name.clone(), r.room_flags.bits() as i64)
        };
        let flagstr = sprintbit(flags, constants::ROOM_BITS);
        g.send_to_char(ch, &format!("[{}] {} [ {}]", vnum, name, flagstr));
    } else {
        let name = g.room(rnum).name.clone();
        g.send_to_char(ch, &name);
    }
    g.send_to_char(ch, "&n");
    g.send_to_char(ch, "\r\n");

    let brief = g
        .get_char(ch)
        .map(|c| c.prf_flags & PRF_BRIEF != 0)
        .unwrap_or(false);
    let death = g
        .room(rnum)
        .room_flags
        .contains(crate::room::RoomFlags::DEATH);
    if (!brief) || ignore_brief || death {
        let desc = g.room(rnum).description.clone();
        g.send_to_char(ch, &desc);
    }

    if g.get_char(ch)
        .map(|c| c.prf_flags & PRF_AUTOEXIT != 0)
        .unwrap_or(false)
    {
        do_auto_exits(g, ch);
    }

    // Ground objects.
    let contents = g.room(rnum).contents.clone();
    list_obj_to_char(g, ch, &contents, 0, false);

    // People.
    let people = g.room(rnum).people.clone();
    if g.get_char(ch)
        .map(|c| c.prf_flags & PRF_NOLOOKSTACK != 0)
        .unwrap_or(false)
    {
        list_nostack_char_to_char(g, &people, ch);
    } else {
        list_char_to_char(g, &people, ch);
    }
    g.send_to_char(ch, "&n");
}

// ---------------------------------------------------------------------------
// look_in_direction / look_in_obj / look_at_target (CircleMUD)
// ---------------------------------------------------------------------------

fn look_in_direction(g: &mut GameState, ch: CharId, dir: usize) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let exit = g.room(rnum).exits[dir].clone();
    match exit {
        Some(e) => {
            match &e.description {
                Some(d) if !d.is_empty() => g.send_to_char(ch, d),
                _ => g.send_to_char(ch, "You see nothing special.\r\n"),
            }
            if e.exit_info & EX_CLOSED != 0 {
                if let Some(kw) = &e.keyword {
                    g.send_to_char(ch, &format!("The {} is closed.\r\n", fname(kw)));
                }
            } else if e.exit_info & EX_ISDOOR != 0 {
                if let Some(kw) = &e.keyword {
                    g.send_to_char(ch, &format!("The {} is open.\r\n", fname(kw)));
                }
            }
        }
        None => g.send_to_char(ch, "Nothing special there...\r\n"),
    }
}

fn look_in_obj(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.is_empty() {
        g.send_to_char(ch, "Look in what?\r\n");
        return;
    }
    let (obj, bits) = generic_find_obj(g, ch, arg, true, true, true, false);
    let oid = match obj {
        Some(o) => o,
        None => {
            g.send_to_char(
                ch,
                &format!("There doesn't seem to be {} {} here.\r\n", an(arg), arg),
            );
            return;
        }
    };
    let otype = g.get_obj(oid).map(|o| o.obj_type).unwrap();
    if otype != ObjectType::LiqContainer
        && otype != ObjectType::Fountain
        && otype != ObjectType::Container
    {
        g.send_to_char(ch, "There's nothing inside that!\r\n");
        return;
    }
    if otype == ObjectType::Container {
        let closed = g
            .get_obj(oid)
            .map(|o| o.values[1] & CONT_CLOSED != 0)
            .unwrap_or(false);
        if closed {
            g.send_to_char(ch, "It is closed.\r\n");
            return;
        }
        let name = g
            .get_obj(oid)
            .map(|o| fname(&o.name).to_string())
            .unwrap_or_default();
        g.send_to_char(ch, &name);
        match bits {
            FIND_INV => g.send_to_char(ch, " (carried): \r\n"),
            FIND_ROOM => g.send_to_char(ch, " (here): \r\n"),
            FIND_EQUIP => g.send_to_char(ch, " (used): \r\n"),
            _ => {}
        }
        let contents = g
            .get_obj(oid)
            .map(|o| o.contains.clone())
            .unwrap_or_default();
        list_obj_to_char(g, ch, &contents, 2, true);
    } else {
        // drink container / fountain
        let (v0, v1, v2) = g
            .get_obj(oid)
            .map(|o| (o.values[0], o.values[1], o.values[2]))
            .unwrap();
        if v1 <= 0 {
            g.send_to_char(ch, "It is empty.\r\n");
        } else if v0 <= 0 || v1 > v0 {
            g.send_to_char(ch, "Its contents seem somewhat murky.\r\n");
        } else {
            let amt = ((v1 * 3) / v0) as usize;
            let liquid = constants::COLOR_LIQUID
                .get(v2 as usize)
                .copied()
                .unwrap_or("clear");
            let full = constants::FULLNESS.get(amt).copied().unwrap_or("");
            g.send_to_char(
                ch,
                &format!("It's {}full of a {} liquid.\r\n", full, liquid),
            );
        }
    }
}

/// find_exdesc against a list of (keyword, description) pairs.
fn find_exdesc<'a>(word: &str, list: &'a [(String, String)]) -> Option<&'a str> {
    for (kw, desc) in list {
        if isname(word, kw) {
            return Some(desc.as_str());
        }
    }
    None
}

fn look_at_target(g: &mut GameState, ch: CharId, arg: &str) {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return;
    }
    if arg.is_empty() {
        g.send_to_char(ch, "Look at what?\r\n");
        return;
    }

    // Character in room first.
    if let Some(found_char) = g.get_char_room_vis(ch, arg) {
        look_at_char(g, found_char, ch);
        if ch != found_char {
            if g.can_see(found_char, ch) {
                act(
                    g,
                    "$n looks at you.",
                    true,
                    ch,
                    None,
                    ActArg::Char(found_char),
                    To::Vict,
                );
            }
            act(
                g,
                "$n looks at $N.",
                true,
                ch,
                None,
                ActArg::Char(found_char),
                To::NotVict,
            );
        }
        return;
    }

    // Room extra description.
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let exdescs = g.room(rnum).extra_descriptions.clone();
    if let Some(desc) = find_exdesc(arg, &exdescs) {
        let d = desc.to_string();
        g.send_to_char(ch, &d);
        return;
    }

    // Room special exit (the `O` block): looking by its ex_name shows the
    // general description plus an open/closed door note (act.informative.c).
    if let Some(se) = g.room(rnum).special_exit.clone() {
        let matches = se
            .ex_name
            .as_deref()
            .map(|n| isname(arg, n))
            .unwrap_or(false);
        if matches {
            match &se.general_description {
                Some(d) if !d.is_empty() => {
                    let s = d.clone();
                    g.send_to_char(ch, &s);
                }
                _ => g.send_to_char(ch, "You see nothing special.\r\n"),
            }
            if se.exit_info & EX_CLOSED != 0 {
                if let Some(kw) = &se.keyword {
                    let msg = format!("The {} is closed.\r\n", kw);
                    g.send_to_char(ch, &msg);
                }
            } else if se.exit_info & EX_ISDOOR != 0 {
                if let Some(kw) = &se.keyword {
                    let msg = format!("The {} is open.\r\n", kw);
                    g.send_to_char(ch, &msg);
                }
            }
            return;
        }
    }

    // Does the argument match an extra desc in the char's equipment?
    let mut found = false;
    for pos in 0..NUM_WEARS {
        if found {
            break;
        }
        let oid = match g.get_char(ch).and_then(|c| c.equipment[pos]) {
            Some(o) => o,
            None => continue,
        };
        if !can_see_obj(g, ch, oid) {
            continue;
        }
        let exdescs = g
            .get_obj(oid)
            .map(|o| o.ex_descriptions.clone())
            .unwrap_or_default();
        if let Some(desc) = find_exdesc(arg, &exdescs) {
            let d = desc.to_string();
            g.send_to_char(ch, &d);
            found = true;
        }
    }

    // Does the argument match an extra desc in the char's inventory?
    if !found {
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        for oid in inv {
            if !can_see_obj(g, ch, oid) {
                continue;
            }
            let exdescs = g
                .get_obj(oid)
                .map(|o| o.ex_descriptions.clone())
                .unwrap_or_default();
            if let Some(desc) = find_exdesc(arg, &exdescs) {
                let d = desc.to_string();
                g.send_to_char(ch, &d);
                found = true;
                break;
            }
        }
    }

    // Does the argument match an extra desc of an object in the room?
    if !found {
        let contents = g.room(rnum).contents.clone();
        for oid in contents {
            if !can_see_obj(g, ch, oid) {
                continue;
            }
            let exdescs = g
                .get_obj(oid)
                .map(|o| o.ex_descriptions.clone())
                .unwrap_or_default();
            if let Some(desc) = find_exdesc(arg, &exdescs) {
                let d = desc.to_string();
                g.send_to_char(ch, &d);
                found = true;
                break;
            }
        }
    }

    let (obj, bits) = generic_find_obj(g, ch, arg, true, true, false, true);
    if bits != 0 {
        let oid = obj.unwrap();
        if !found {
            show_obj_string(g, ch, oid, 5);
        } else {
            show_obj_string(g, ch, oid, 6);
        }
    } else if !found {
        g.send_to_char(ch, "You do not see that here.\r\n");
    }
}

// ---------------------------------------------------------------------------
// generic_find (object subset) — searches inv / room / equip in C order.
// ---------------------------------------------------------------------------

const FIND_INV: i32 = 1;
const FIND_ROOM: i32 = 2;
const FIND_EQUIP: i32 = 4;

/// Find a single object by keyword, mirroring C generic_find's inv->room->eq
/// search order. Returns (object, which-bit-matched).
fn generic_find_obj(
    g: &GameState,
    ch: CharId,
    arg: &str,
    look_inv: bool,
    look_room: bool,
    _unused_equip_first: bool,
    look_equip: bool,
) -> (Option<ObjId>, i32) {
    if look_inv {
        let inv = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        if let Some(o) = obj_in_list_vis(g, ch, arg, &inv) {
            return (Some(o), FIND_INV);
        }
    }
    if look_room {
        if let Some(rnum) = g.get_char(ch).and_then(|c| c.in_room) {
            let contents = g.room(rnum).contents.clone();
            if let Some(o) = obj_in_list_vis(g, ch, arg, &contents) {
                return (Some(o), FIND_ROOM);
            }
        }
    }
    if look_equip {
        let eq: Vec<ObjId> = g
            .get_char(ch)
            .map(|c| c.equipment.iter().flatten().copied().collect())
            .unwrap_or_default();
        if let Some(o) = obj_in_list_vis(g, ch, arg, &eq) {
            return (Some(o), FIND_EQUIP);
        }
    }
    (None, 0)
}

/// Visibility-gated object find within a list (N.name ordinal supported).
fn obj_in_list_vis(g: &GameState, ch: CharId, arg: &str, list: &[ObjId]) -> Option<ObjId> {
    let (mut count, name) = get_number(arg);
    if count == 0 {
        return None;
    }
    for &oid in list {
        let obj = match g.get_obj(oid) {
            Some(o) => o,
            None => continue,
        };
        if isname(&name, &obj.name) && can_see_obj(g, ch, oid) {
            count -= 1;
            if count == 0 {
                return Some(oid);
            }
        }
    }
    None
}

// ===========================================================================
// ACMD handlers
// ===========================================================================

pub fn do_look(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return;
    }
    let pos = g
        .get_char(ch)
        .map(|c| c.position)
        .unwrap_or(Position::Standing);
    let rnum = g.get_char(ch).and_then(|c| c.in_room);

    if pos < Position::Sleeping {
        g.send_to_char(ch, "You can't see anything but stars!\r\n");
        return;
    }
    if is_blind(g, ch) {
        g.send_to_char(ch, "You can't see a damned thing, you're blind!\r\n");
        return;
    }
    if let Some(r) = rnum {
        if room_is_dark(g, r) && !can_see_in_dark(g, ch) {
            g.send_to_char(ch, "It is pitch black...\r\n");
            let people = g.room(r).people.clone();
            if g.get_char(ch)
                .map(|c| c.prf_flags & PRF_NOLOOKSTACK != 0)
                .unwrap_or(false)
            {
                list_nostack_char_to_char(g, &people, ch);
            } else {
                list_char_to_char(g, &people, ch);
            }
            return;
        }
    }

    let (arg, arg2) = half_chop(argument);

    // SCMD_READ == 1.
    if subcmd == 1 {
        if arg.is_empty() {
            g.send_to_char(ch, "Read what?\r\n");
        } else {
            look_at_target(g, ch, &arg);
        }
        return;
    }

    if arg.is_empty() {
        look_at_room(g, ch, true);
    } else if is_abbrev(&arg, "in") {
        look_in_obj(g, ch, &arg2);
    } else if let Some(dir) = search_block(&arg, constants::DIRS) {
        if dir < NUM_OF_DIRS {
            look_in_direction(g, ch, dir);
        } else {
            look_at_target(g, ch, &arg);
        }
    } else if is_abbrev(&arg, "at") {
        look_at_target(g, ch, &arg2);
    } else {
        look_at_target(g, ch, &arg);
    }
}

pub fn do_examine(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        g.send_to_char(ch, "Examine what?\r\n");
        return;
    }
    look_at_target(g, ch, &arg);

    let (obj, _bits) = generic_find_obj(g, ch, &arg, true, true, false, true);
    let oid = match obj {
        Some(o) => o,
        None => return,
    };
    let (otype, tslots, cslots) = g
        .get_obj(oid)
        .map(|o| (o.obj_type, o.values[2], o.values[3]))
        .unwrap();

    if otype == ObjectType::LiqContainer
        || otype == ObjectType::Fountain
        || otype == ObjectType::Container
    {
        g.send_to_char(ch, "When you look inside, you see:\r\n");
        look_in_obj(g, ch, &arg);
    }

    // Condition is a percentage of total slots (DeltaMUD: tslots=v2, cslots=v3).
    let condition = if tslots != 0 {
        (cslots * 100) / tslots
    } else {
        0
    };
    let line = if condition == 0 {
        "It appears to be indestructable!\r\n"
    } else if condition <= 10 {
        "It appears to be in extremley poor condition.\r\n"
    } else if condition <= 20 {
        "It appears to be in poor condition.\r\n"
    } else if condition <= 30 {
        "It appears to be in fair condition.\r\n"
    } else if condition <= 40 {
        "It appears to be in moderate condition.\r\n"
    } else if condition <= 50 {
        "It appears to be in good condition.\r\n"
    } else if condition <= 60 {
        "It appears to be in very good condition.\r\n"
    } else if condition <= 70 {
        "It appears to bein excellent condition.\r\n"
    } else if condition <= 80 {
        "It appears to be in superior condition.\r\n"
    } else if condition <= 90 {
        "It appears to be in extremely superior condition.\r\n"
    } else {
        "It appears to be as good as new!\r\n"
    };
    g.send_to_char(ch, line);
}

pub fn do_exits(g: &mut GameState, ch: CharId, _argument: &str, _subcmd: i32) {
    if is_blind(g, ch) {
        g.send_to_char(ch, "You can't see a damned thing, you're blind!\r\n");
        return;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let imm = g
        .get_char(ch)
        .map(|c| c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    let roomflags = g
        .get_char(ch)
        .map(|c| c.prf_flags & PRF_ROOMFLAGS != 0)
        .unwrap_or(false);
    let see_dark = can_see_in_dark(g, ch);

    let mut buf = String::new();
    for door in 0..NUM_OF_DIRS {
        let exit = match g.room(rnum).exits[door].clone() {
            Some(e) => e,
            None => continue,
        };
        let to_rnum = match g.real_room(exit.to_room) {
            Some(r) => r,
            None => continue,
        };
        let closed = exit.exit_info & EX_CLOSED != 0;
        let hidden = exit.exit_info & EX_HIDDEN != 0;

        if !closed {
            if hidden && !imm {
                continue;
            }
            let mut line2;
            if imm || roomflags {
                let vcds = rcds(g, to_rnum);
                let name = g.room(to_rnum).name.clone();
                line2 = format!(
                    "&Y{:<5}&n &R-&n &C[&n{}&C]&n {}\r\n",
                    DIR_NAMES[door], vcds, name
                );
            } else {
                line2 = format!("&Y{:<5}&n &R-&n ", DIR_NAMES[door]);
                if room_is_dark(g, to_rnum) && !see_dark {
                    line2.push_str("Too dark to tell\r\n");
                } else {
                    let name = g.room(to_rnum).name.clone();
                    line2.push_str(&name);
                    line2.push_str("\r\n");
                }
            }
            buf.push_str(&cap(&line2));
        } else {
            let line2;
            if imm {
                let vcds = rcds(g, to_rnum);
                let name = g.room(to_rnum).name.clone();
                let mut s = format!(
                    "&Y{:<5}&n &R-&n &C[&n{}&C]&n {} &R-&n &CCLOSED&n",
                    DIR_NAMES[door], vcds, name
                );
                if exit.exit_info & EX_LOCKED != 0 {
                    s.push_str(" &R-&n &CLOCKED&n");
                }
                if exit.exit_info & EX_PICKPROOF != 0 {
                    s.push_str(" &R-&n &CPICKPROOF&n");
                }
                s.push_str("\r\n");
                line2 = s;
            } else {
                let kw = exit.keyword.as_deref().unwrap_or("");
                line2 = format!(
                    "&Y{:<5}&n &R-&n The {} is closed\r\n",
                    DIR_NAMES[door],
                    fname(kw)
                );
            }
            buf.push_str(&cap(&line2));
        }
    }

    g.send_to_char(ch, "&CObvious exits:&n\r\n");
    if !buf.is_empty() {
        g.send_to_char(ch, &buf);
    } else {
        g.send_to_char(ch, " None.\r\n");
    }
}

pub fn do_gold(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let gold = g.get_char(ch).map(|c| c.points.gold).unwrap_or(0);
    if gold == 0 {
        g.send_to_char(ch, "You're broke!\r\n");
    } else if gold == 1 {
        g.send_to_char(ch, "You have one miserable little gold coin.\r\n");
    } else {
        g.send_to_char(
            ch,
            &format!("You have {} gold coins.\r\n", numdisplay(gold as i64)),
        );
    }
}

pub fn do_checkbail(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // pk_allowed is off by default; jailterm requires PLR_KILLER in the jail.
    let (killer, rnum) = g
        .get_char(ch)
        .map(|c| (c.act_flags & PLR_KILLER != 0, c.in_room))
        .unwrap_or((false, None));
    let in_jail = rnum == g.real_room(JAIL_NUM);
    if !(killer && in_jail) {
        g.send_to_char(ch, "You're (happily) not serving a jailterm right now.\r\n");
        return;
    }
    let bail = g.get_char(ch).map(|c| c.bail_amt).unwrap_or(0);
    g.send_to_char(
        ch,
        &format!(
            "Bail for this offence has been set at {} gold coins.\r\n",
            bail
        ),
    );
}

pub fn do_mcheck(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let jurisdicted = {
        let r = g.room(rnum);
        r.sector_type == crate::room::SectorType::City
            || r.room_flags.contains(crate::room::RoomFlags::INDOORS)
    };
    if jurisdicted {
        g.send_to_char(ch, "This area is protected under jurisdiction.\r\n");
    } else {
        g.send_to_char(ch, "This area is not under jurisdiction.\r\n");
    }
}

pub fn do_worth(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return,
    };
    let gold = c.points.gold;
    let bank = c.points.bank_gold;
    let eq: Vec<ObjId> = c.equipment.iter().flatten().copied().collect();
    let inv = c.carrying.clone();
    let eq_val: i32 = eq
        .iter()
        .filter_map(|o| g.get_obj(*o).map(|x| x.cost))
        .sum();
    let inven_val: i32 = inv
        .iter()
        .filter_map(|o| g.get_obj(*o).map(|x| x.cost))
        .sum();

    g.send_to_char(
        ch,
        &format!(
            "You have {} gold coins on hand.\r\n",
            numdisplay(gold as i64)
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "You have {} gold coins in the bank.\r\n",
            numdisplay(bank as i64)
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "You have inventory worth {} gold coins.\r\n",
            numdisplay(inven_val as i64)
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "You have equipment worth {} gold coins.\r\n",
            numdisplay(eq_val as i64)
        ),
    );
    if gold + inven_val + eq_val == 0 {
        g.send_to_char(ch, "Ouch! Looks like you're broke!\r\n");
    } else {
        let net = gold as i64 + bank as i64 + inven_val as i64 + eq_val as i64;
        g.send_to_char(
            ch,
            &format!("For a net worth of {} gold coins.\r\n", numdisplay(net)),
        );
    }
}

/// do_status — 'status'/'score' (CircleMUD/DeltaMUD long score sheet).
pub fn do_status(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (target_name, _) = half_chop(argument);
    let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);

    let (vict, person) = if target_name.is_empty() {
        (ch, "You are".to_string())
    } else if ch_level < LVL_IMMORT {
        (ch, "You are".to_string())
    } else {
        match g.get_char_room_vis(ch, &target_name) {
            Some(v) if !g.get_char(v).map(|c| c.is_npc).unwrap_or(true) => {
                if ch_level < g.get_char(v).map(|c| c.player.level).unwrap_or(1) {
                    g.send_to_char(
                        ch,
                        "You can't see the score of people above your level.\r\n",
                    );
                    return;
                }
                let name = g
                    .get_char(v)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default();
                g.send_to_char(ch, &format!("&W***** Score for {} ***** &n\r\n", name));
                (v, format!("{} is", name))
            }
            _ => {
                g.send_to_char(ch, "Who is that?\r\n");
                return;
            }
        }
    };

    let c = match g.get_char(vict) {
        Some(c) => c.clone(),
        None => return,
    };

    // Inventory + eq worth.
    let eq_val: i32 = c
        .equipment
        .iter()
        .flatten()
        .filter_map(|o| g.get_obj(*o).map(|x| x.cost))
        .sum();
    let inven_val: i32 = c
        .carrying
        .iter()
        .filter_map(|o| g.get_obj(*o).map(|x| x.cost))
        .sum();

    let mut buf = String::new();
    buf.push_str(&format!(
        "&G{:<10}:&n {} {} &G(&nlevel {}&G)&n.\r\n",
        person,
        c.player.name,
        c.get_title(),
        c.player.level
    ));

    let role = if c.player.level == LVL_HERO {
        "hero"
    } else if c.player.level == LVL_IMMORT {
        "immortal"
    } else if c.player.level == LVL_DEMIGOD {
        "Demi God"
    } else if c.player.level >= LVL_GOD {
        "God"
    } else {
        "citizen"
    };
    buf.push_str(&format!(
        "&GClass     :&n {} {}, {} of {}.\r\n",
        race_name(c.player.race),
        class_name(c.player.class),
        role,
        town_name(c.player.hometown)
    ));

    // Age + playtime: derived from char timestamps; full mud-time calendar is
    // not in the contract, so age years start at 17 (do_start) and play time
    // is taken from player.time_played (seconds).
    let years = 17i64;
    buf.push_str(&format!(
        "&GAge       :&n {} years, {} months, {} days, {} hours old.\r\n",
        years, 0, 0, 0
    ));

    let played = c.player.time_played.max(0);
    let play_days = played / 86400;
    let play_hours = (played % 86400) / 3600;
    buf.push_str(&format!(
        "&GPlaytime  :&n {:<2} days and {:<2} hours    &GQuest Pts    :&n {}\r\n",
        play_days,
        play_hours,
        numdisplay(c.quest_points as i64)
    ));
    buf.push_str(&format!(
        "&GHeight    :&n {:<3} centimeters         &GWeight       :&n {:<3} pounds\r\n",
        c.player.height, c.player.weight
    ));
    buf.push_str("&b---------------------------------------------------------------------&n\r\n");

    buf.push_str(&format!(
        "&CHit Pts   :&n {:<5} ",
        numdisplay(c.points.hit as i64)
    ));
    buf.push_str(&format!(
        "(max: {:<5})      ",
        numdisplay(c.points.max_hit as i64)
    ));
    buf.push_str(&format!(
        "&CMana Pts     :&n {:<5} ",
        numdisplay(c.points.mana as i64)
    ));
    buf.push_str(&format!(
        "(max: {:<5})\r\n",
        numdisplay(c.points.max_mana as i64)
    ));

    buf.push_str(&format!(
        "&CMove Pts  :&n {:<5} ",
        numdisplay(c.points.move_points as i64)
    ));
    buf.push_str(&format!(
        "(max: {:<5})      ",
        numdisplay(c.points.max_move as i64)
    ));
    buf.push_str(&format!(
        "&CAlignment    :&n {} \r\n",
        numdisplay(c.alignment as i64)
    ));

    let tnl = exp_to_level(c.player.level as i32) - c.points.exp;
    buf.push_str(&format!(
        "&CExp Pts   :&n {:<12}            ",
        numdisplay(c.points.exp)
    ));
    let tnl_str = if c.player.level < 100 {
        numdisplay(tnl)
    } else {
        "N/A".to_string()
    };
    buf.push_str(&format!("&CExp to Level :&n {} \r\n", tnl_str));

    buf.push_str(&format!(
        "&CDefense   :&n {:<4}                    &CGold on Hand :&n {} \r\n",
        c.points.defense,
        numdisplay(c.points.gold as i64)
    ));
    buf.push_str(&format!(
        "&CEq Worth  :&n {:<15}         &CGold in Bank :&n {} \r\n",
        numdisplay((eq_val + inven_val) as i64),
        numdisplay(c.points.bank_gold as i64)
    ));
    // GET_ARENAWINS / GET_ARENALOSSES (player_specials.saved.wins/losses).
    buf.push_str(&format!(
        "&CArena Wins:&n {:<3}                     &CArena Losses :&n {} \r\n",
        c.wins, c.losses
    ));
    buf.push_str(&format!(
        "&yHometown  :&n {:<3}                &yDeity        :&n {} \r\n",
        town_name(c.player.hometown),
        deity_name(c.player.deity)
    ));
    buf.push_str("&b---------------------------------------------------------------------&n\r\n");

    buf.push_str(&format!(
        "&YStrength  :&n {:<2}/{:<4}    &YIntelligence  :&n {:<2}         &YWisdom   :&n {:<2}\r\n",
        c.aff_abils.str, c.aff_abils.str_add, c.aff_abils.intel, c.aff_abils.wis
    ));
    buf.push_str(&format!(
        "&YDexterity :&n {:<2}         &YConstitution  :&n {:<2}         &YCharisma :&n {:<2}\r\n",
        c.aff_abils.dex, c.aff_abils.con, c.aff_abils.cha
    ));
    buf.push_str("&b---------------------------------------------------------------------&n\r\n");

    // Position line.
    let pl = match c.position {
        Position::Dead => format!("{} DEAD!\r\n", person),
        Position::MortallyWounded => {
            format!("{} mortally wounded!  You should seek help!\r\n", person)
        }
        Position::Incapacitated => format!("{} incapacitated, slowly fading away...\r\n", person),
        Position::Stunned => format!("{} stunned! Unable to move!\r\n", person),
        Position::Sleeping => format!("{} sleeping.\r\n", person),
        Position::Resting => format!("{} resting.\r\n", person),
        Position::Sitting => format!("{} sitting.\r\n", person),
        Position::Fighting => match c.fighting {
            Some(opp) => format!("{} fighting {}.\r\n", person, pers(g, vict, opp)),
            None => format!("{} fighting thin air.\r\n", person),
        },
        Position::Standing => format!("{} standing.\r\n", person),
        Position::Meditating => format!("{} meditating.\r\n", person),
    };
    buf.push_str(&pl);

    // Conditions.
    if c.conditions[DRUNK] > 4 {
        buf.push_str(&format!("{} intoxicated.\r\n", person));
    }
    if c.conditions[FULL] <= 0 && c.conditions[FULL] > -24 {
        buf.push_str(&format!("{} hungry.\r\n", person));
    } else if c.conditions[FULL] < -23 && c.conditions[FULL] != -100 {
        buf.push_str(&format!("{} extremely hungry.\r\n", person));
    }
    if c.conditions[THIRST] <= 0 && c.conditions[THIRST] > -12 {
        buf.push_str(&format!("{} thirsty.\r\n", person));
    } else if c.conditions[THIRST] < -12 && c.conditions[THIRST] != -100 {
        buf.push_str(&format!("{} extremely thirsty.\r\n", person));
    }

    // Affect summary lines.
    let af = c.affect_flags;
    let same = ch == vict;
    let his = match c.player.sex {
        Gender::Male => "his",
        Gender::Female => "her",
        Gender::Neutral => "its",
    };
    let cap_his = match c.player.sex {
        Gender::Male => "His",
        Gender::Female => "Her",
        Gender::Neutral => "Its",
    };
    let cap_he = match c.player.sex {
        Gender::Male => "He",
        Gender::Female => "She",
        Gender::Neutral => "It",
    };

    if af & AFF_BLIND != 0 {
        buf.push_str(&format!("{} blinded!\r\n", person));
    }
    if af & AFF_INVISIBLE != 0 {
        buf.push_str(&format!("{} invisible.\r\n", person));
    }
    if af & AFF_DETECT_ALIGN != 0 {
        buf.push_str(&format!("{} able to see other people's auras.\r\n", person));
    }
    if af & AFF_DETECT_INVIS != 0 {
        buf.push_str(&format!(
            "{} sensitive to the presence of invisible things.\r\n",
            person
        ));
    }
    if af & AFF_SENSE_LIFE2 != 0 {
        buf.push_str(&format!(
            "{} sensitive to the presence of life.\r\n",
            person
        ));
    }
    if af & AFF_CURSE != 0 {
        buf.push_str(&format!("{} under someone's curse!\r\n", person));
    }
    if af & AFF_SLEEP != 0 {
        buf.push_str(&format!(
            "{} sleeping and are unable to wake up.\r\n",
            person
        ));
    }
    if af & AFF_SANCTUARY != 0 {
        buf.push_str(&format!("{} protected by Sanctuary.\r\n", person));
    }
    if af & AFF_CONVERGENCE != 0 {
        let pron = if same { "your" } else { his };
        buf.push_str(&format!(
            "{} able to converge {} magic power.\r\n",
            person, pron
        ));
    }
    if af & AFF_AUTUS != 0 {
        let pron = if same { "your" } else { his };
        buf.push_str(&format!(
            "{} able to conserve {} mana use.\r\n",
            person, pron
        ));
    }
    if af & AFF_NOPORTAL != 0 {
        buf.push_str(&format!(
            "{} protected by the heavens from mortal portals.\r\n",
            person
        ));
    }
    if af & AFF_POISON != 0 {
        buf.push_str(&format!("{} poisoned!\r\n", person));
    }
    if af & AFF_PLAGUED != 0 {
        buf.push_str(&format!("{} sick with the plague!\r\n", person));
    }
    if af & AFF_CHARM != 0 {
        let (who, verb) = if same {
            ("You", " have")
        } else {
            (cap_he, " has")
        };
        buf.push_str(&format!("{}{} been charmed!\r\n", who, verb));
    }
    if af & AFF_INFRAVISION != 0 {
        let pron = if same { "Your" } else { cap_his };
        buf.push_str(&format!("{} eyes are glowing red.\r\n", pron));
    }
    if af & AFF_CHAINED != 0 {
        let pron = if same { "Your" } else { cap_his };
        buf.push_str(&format!("{} feet are chained together!\r\n", pron));
    }
    if c.prf_flags & PRF_SUMMONABLE != 0 {
        buf.push_str(&format!("{} summonable by other players.\r\n", person));
    }
    if af & AFF_SNEAK != 0 {
        buf.push_str(&format!("{} currently sneaking about.\r\n", person));
    }
    if af & AFF_HIDE != 0 {
        buf.push_str(&format!(
            "{} currently hidden from plain sight.\r\n",
            person
        ));
    }

    g.send_to_char(ch, &buf);
}

pub fn do_inventory(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "You are carrying:\r\n");
    let inv = g
        .get_char(ch)
        .map(|c| c.carrying.clone())
        .unwrap_or_default();
    list_obj_to_char(g, ch, &inv, 1, true);
}

pub fn do_equipment(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "You are using:\r\n");
    let mut found = false;
    for pos in 0..NUM_WEARS {
        let oid = g.get_char(ch).and_then(|c| c.equipment[pos]);
        if let Some(oid) = oid {
            let label = constants::WHERE.get(pos).copied().unwrap_or("");
            if can_see_obj(g, ch, oid) {
                g.send_to_char(ch, label);
                show_obj_string(g, ch, oid, 1);
                found = true;
            } else {
                g.send_to_char(ch, label);
                g.send_to_char(ch, "Something.\r\n");
                found = true;
            }
        }
    }
    if !found {
        g.send_to_char(ch, " Nothing.\r\n");
    }
}

pub fn do_time(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // World calendar/weather not in the contract; derive a stable in-game clock
    // from the heartbeat pulse so the command renders the same structure.
    let total_hours = (g.pulse / (PASSES_PER_SEC * 75)) as i64; // SECS_PER_MUD_HOUR=75
    let hours = (total_hours % 24) as i32;
    let day = ((total_hours / 24) % 35) as i32;
    let month = (((total_hours / 24) / 35) % 17) as i32;
    let year = ((total_hours / 24) / 35 / 17) as i32;

    let hour12 = if hours % 12 == 0 { 12 } else { hours % 12 };
    let ampm = if hours >= 12 { "pm" } else { "am" };
    let weekday = (((35 * month) + day + 1) % 7).rem_euclid(7) as usize;
    let weekday_name = constants::WEEKDAYS
        .get(weekday)
        .copied()
        .unwrap_or(constants::WEEKDAYS[0]);
    g.send_to_char(
        ch,
        &format!("It is {} o'clock {}, on {}\r\n", hour12, ampm, weekday_name),
    );

    let day1 = day + 1;
    let suf = if day1 == 1 {
        "st"
    } else if day1 == 2 {
        "nd"
    } else if day1 == 3 {
        "rd"
    } else if day1 < 20 {
        "th"
    } else if day1 % 10 == 1 {
        "st"
    } else if day1 % 10 == 2 {
        "nd"
    } else if day1 % 10 == 3 {
        "rd"
    } else {
        "th"
    };
    let month_name = constants::MONTH_NAME
        .get(month as usize)
        .copied()
        .unwrap_or(constants::MONTH_NAME[0]);
    g.send_to_char(
        ch,
        &format!(
            "The {}{} Day of the {}, Year {}.\r\n",
            day1, suf, month_name, year
        ),
    );
}

pub fn do_help(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if g.get_char(ch).and_then(|c| c.desc).is_none() {
        return;
    }
    let arg = argument.trim();
    if arg.is_empty() {
        // The general help screen text isn't in the contract.
        g.send_to_char(ch, "No help available.\r\n");
        return;
    }
    // The help index/table isn't loaded in the contract.
    g.send_to_char(ch, "There is no help on that word.\r\n");
}

const WIZ_LEVELS: [&str; 5] = [
    "    Immortal   ",
    "     Sage      ",
    "     Seer      ",
    "    Prophet    ",
    "* Implementor *",
];

const WHO_USAGE: &str = "Usage: who [minlev[-maxlev]] [-n name] [-c classes] [-rzqimo] [-a]\r\n\
\r\n\
Classes: (M)age, (C)leric, (T)hief, (W)arrior\r\n\
\r\n\
 Switches: \r\n\
_.,-'^'-,._\r\n\
\r\n\
  -r = who is in the current room\r\n\
  -z = who is in the current zone\r\n\
  -a = who is in the arena\r\n\
\r\n\
  -q = only show autoquesters\r\n\
  -i = only show immortals\r\n\
  -m = only show mortals\r\n\
  -o = only show outlaws\r\n\
\r\n";

/// find_class_bitvector(arg): single-letter class -> class bitmask.
fn find_class_bitvector(c: char) -> i64 {
    match c.to_ascii_lowercase() {
        'm' => 1 << (Class::MagicUser as i64),
        'c' => 1 << (Class::Cleric as i64),
        't' => 1 << (Class::Thief as i64),
        'w' => 1 << (Class::Warrior as i64),
        _ => 0,
    }
}

pub fn do_who(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    // Argument parsing (CircleMUD do_who parser).
    let mut low = 0u8;
    let mut high = LVL_IMPL;
    let mut showclass: i64 = 0;
    let mut who_room = false;
    let mut who_zone = false;
    let mut who_quest = false;
    let mut who_arena = false;
    let mut outlaws = false;
    let mut noimm = false;
    let mut nomort = false;
    let mut who_fight = false;
    let mut name_search = String::new();

    let mut rest = argument.trim().to_string();
    while !rest.is_empty() {
        let (arg, after) = half_chop(&rest);
        let first = arg.chars().next().unwrap_or(' ');
        if first.is_ascii_digit() {
            // "low-high"
            let mut it = arg.split('-');
            if let Some(l) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                low = l;
            }
            if let Some(h) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                high = h;
            }
            rest = after;
        } else if first == '-' {
            let mode = arg.chars().nth(1).unwrap_or(' ');
            match mode {
                'f' => {
                    who_fight = true;
                    rest = after;
                }
                'o' => {
                    outlaws = true;
                    rest = after;
                }
                'z' => {
                    who_zone = true;
                    rest = after;
                }
                'q' => {
                    who_quest = true;
                    rest = after;
                }
                'l' => {
                    let (lv, r) = half_chop(&after);
                    let mut it = lv.split('-');
                    if let Some(l) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                        low = l;
                    }
                    if let Some(h) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                        high = h;
                    }
                    rest = r;
                }
                'n' => {
                    let (n, r) = half_chop(&after);
                    name_search = n;
                    rest = r;
                }
                'a' => {
                    who_arena = true;
                    rest = after;
                }
                'r' => {
                    who_room = true;
                    rest = after;
                }
                'c' => {
                    let (cl, r) = half_chop(&after);
                    for c in cl.chars() {
                        showclass |= find_class_bitvector(c);
                    }
                    rest = r;
                }
                'i' => {
                    nomort = true;
                    rest = after;
                }
                'm' => {
                    noimm = true;
                    rest = after;
                }
                _ => {
                    g.send_to_char(ch, WHO_USAGE);
                    return;
                }
            }
        } else {
            g.send_to_char(ch, WHO_USAGE);
            return;
        }
    }

    let imm_header = "Immortals Currently Online\r\n&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&n\r\n";
    let mort_header = "Mortals Currently Online\r\n&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&n\r\n";

    let ch_room = g.get_char(ch).and_then(|c| c.in_room);
    let ch_zone = ch_room.map(|r| g.room(r).zone);

    let mut immlist: Vec<(u8, String)> = Vec::new();
    let mut mortlist: Vec<(u8, String)> = Vec::new();

    let players: Vec<CharId> = g.players_by_name.values().copied().collect();
    for wch in players {
        if !g.can_see(ch, wch) {
            continue;
        }
        let c = match g.get_char(wch) {
            Some(c) => c,
            None => continue,
        };
        let lvl = c.player.level;
        if lvl < low || lvl > high {
            continue;
        }
        if (noimm && lvl >= LVL_IMMORT) || (nomort && lvl < LVL_IMMORT) {
            continue;
        }
        if !name_search.is_empty() {
            let nm = c.player.name.to_lowercase();
            let title = c.get_title().to_lowercase();
            if nm != name_search.to_lowercase() && !title.contains(&name_search.to_lowercase()) {
                continue;
            }
        }
        if outlaws && c.act_flags & PLR_KILLER == 0 && c.act_flags & PLR_THIEF == 0 {
            continue;
        }
        if who_quest && c.act_flags & PLR_QUESTOR == 0 {
            continue;
        }
        if who_zone && c.in_room.map(|r| g.room(r).zone) != ch_zone {
            continue;
        }
        if who_room && c.in_room != ch_room {
            continue;
        }
        if who_arena && !crate::arena::is_arena_combatant(wch) {
            continue;
        }
        if showclass != 0 && (showclass & (1 << (c.player.class as i64))) == 0 {
            continue;
        }

        let ismortal = lvl < LVL_IMMORT;
        let mut line: String;
        // Mortal town-citizens (citizen rank > 0) get a colored rank-title prefix
        // between the bracket and the name (act.informative.c). CCNRM is "&n".
        let is_citizen = lvl < LVL_IMMORT && c.citizen > 0;
        if lvl >= LVL_IMMORT {
            let widx = (lvl - LVL_IMMORT) as usize;
            let wname = WIZ_LEVELS.get(widx).copied().unwrap_or(WIZ_LEVELS[0]);
            line = format!("&Y[{}] {} {}&n", wname, c.player.name, c.get_title());
        } else if lvl == LVL_HERO {
            if is_citizen {
                line = format!(
                    "&B[&n100 &YHero &B]{} {}&n{} {}&n",
                    c.citizen_color(),
                    c.read_citizen(),
                    c.player.name,
                    c.get_title()
                );
            } else {
                line = format!(
                    "&B[&n100 &YHero &B]&n {} {}&n",
                    c.player.name,
                    c.get_title()
                );
            }
        } else if is_citizen {
            line = format!(
                "&B[&n{:2} {} {}&B]{} {}&n{} {}&n",
                lvl,
                race_abbr(c.player.race),
                class_abbr(c.player.class),
                c.citizen_color(),
                c.read_citizen(),
                c.player.name,
                c.get_title()
            );
        } else {
            line = format!(
                "&B[&n{:2} {} {}&B]&n {} {}&n",
                lvl,
                race_abbr(c.player.race),
                class_abbr(c.player.class),
                c.player.name,
                c.get_title()
            );
        }

        if c.invis_level > 0 {
            line.push_str(&format!(" (i{})", c.invis_level));
        } else if c.affect_flags & AFF_INVISIBLE != 0 {
            line.push_str(" (invis)");
        }
        if c.act_flags & PLR_WRITING != 0 {
            line.push_str(" (writing)");
        }
        if c.prf_flags & PRF_AFK != 0 {
            line.push_str(" (away)");
        }
        if c.prf_flags & PRF_DEAF != 0 {
            line.push_str(" (deaf)");
        }
        if c.prf_flags & PRF_NOTELL != 0 {
            line.push_str(" (notell)");
        }
        if c.prf2_flags & PRF2_QCHAN != 0 {
            line.push_str(" (qchan)");
        }
        if c.act_flags & PLR_QUESTOR != 0 {
            line.push_str(" (autoquest)");
        }
        if c.act_flags & PLR_THIEF != 0 {
            line.push_str(" (&RTHIEF&n)");
        }
        if c.act_flags & PLR_KILLER != 0 {
            line.push_str(" (&RKILLER&n)");
        }
        if c.prf2_flags & PRF2_MBUILDING != 0 {
            line.push_str(" (&Ybuilding&n)");
        }
        if c.prf2_flags & PRF2_INTANGIBLE != 0 && c.prf2_flags & PRF2_MBUILDING == 0 {
            line.push_str(" (&Kdead&n)");
        }
        if who_arena {
            line.push_str(&format!(" (Arena Wins/Losses: {}/{})", c.wins, c.losses));
        }
        if who_fight {
            if let Some(opp) = c.fighting {
                let ch_imm = g
                    .get_char(ch)
                    .map(|x| x.player.level >= LVL_IMMORT)
                    .unwrap_or(false);
                if ch_imm && lvl < LVL_IMMORT {
                    line.push_str(&format!(
                        " (Fighting {})",
                        g.get_char(opp)
                            .map(|o| o.player.name.clone())
                            .unwrap_or_default()
                    ));
                }
            }
        }
        if c.clan > -1 && c.clan_rank > 0 {
            // Clan name table not in the contract; render rank/clan ids.
            line.push_str(&format!(" &B[&nrank {} of clan {}&B]", c.clan_rank, c.clan));
        }
        line.push_str("&n\r\n");

        if ismortal {
            mortlist.push((lvl, line));
        } else {
            immlist.push((lvl, line));
        }
    }

    // Sort ascending by level, then emit in descending order (C quicksort + reverse loop).
    immlist.sort_by_key(|x| x.0);
    mortlist.sort_by_key(|x| x.0);

    let wizards = immlist.len();
    let mortals = mortlist.len();

    if wizards > 0 {
        let mut out = String::from(imm_header);
        for (_, l) in immlist.iter().rev() {
            out.push_str(l);
        }
        g.send_to_char(ch, &out);
        g.send_to_char(ch, "\r\n");
    }
    if mortals > 0 {
        let mut out = String::from(mort_header);
        for (_, l) in mortlist.iter().rev() {
            out.push_str(l);
        }
        g.send_to_char(ch, &out);
        g.send_to_char(ch, "\r\n");
    }

    let mut summary = String::new();
    if wizards + mortals == 0 {
        summary.push_str("No wizards or mortals are currently visible to you.\r\n");
        g.send_to_char(ch, &summary);
        return;
    }
    let mut tail = String::new();
    if wizards > 0 {
        tail = format!(
            "There {} {} visible immortal{}{}",
            if wizards == 1 { "is" } else { "are" },
            wizards,
            if wizards == 1 { "" } else { "s" },
            if mortals > 0 { " and there" } else { "." }
        );
    }
    if mortals > 0 {
        let head = if wizards > 0 {
            tail.clone()
        } else {
            "There".to_string()
        };
        tail = format!(
            "{} {} {} visible mortal{}.",
            head,
            if mortals == 1 { "is" } else { "are" },
            mortals,
            if mortals == 1 { "" } else { "s" }
        );
    }
    tail.push_str("\r\n");
    g.send_to_char(ch, &tail);
}

const USERS_FORMAT: &str =
    "format: users [-l minlevel[-maxlevel]] [-n name] [-h host] [-c classlist] [-o] [-p]\r\n";

pub fn do_users(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let mut low = 0u8;
    let mut high = LVL_IMPL;
    let mut showclass: i64 = 0;
    let mut outlaws = false;
    let mut name_search = String::new();
    let mut host_search = String::new();

    let mut rest = argument.trim().to_string();
    while !rest.is_empty() {
        let (arg, after) = half_chop(&rest);
        if arg.starts_with('-') {
            let mode = arg.chars().nth(1).unwrap_or(' ');
            match mode {
                'o' | 'k' => {
                    outlaws = true;
                    rest = after;
                }
                'p' => {
                    rest = after;
                }
                'd' => {
                    rest = after;
                }
                'l' => {
                    let (lv, r) = half_chop(&after);
                    let mut it = lv.split('-');
                    if let Some(l) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                        low = l;
                    }
                    if let Some(h) = it.next().and_then(|s| s.parse::<u8>().ok()) {
                        high = h;
                    }
                    rest = r;
                }
                'n' => {
                    let (n, r) = half_chop(&after);
                    name_search = n;
                    rest = r;
                }
                'h' => {
                    let (n, r) = half_chop(&after);
                    host_search = n;
                    rest = r;
                }
                'c' => {
                    let (cl, r) = half_chop(&after);
                    for c in cl.chars() {
                        showclass |= find_class_bitvector(c);
                    }
                    rest = r;
                }
                _ => {
                    g.send_to_char(ch, USERS_FORMAT);
                    return;
                }
            }
        } else {
            g.send_to_char(ch, USERS_FORMAT);
            return;
        }
    }

    let mut header = String::new();
    header.push_str("Num Class   Name         State          Idl Login@   Site\r\n");
    header.push_str(
        "--- ------- ------------ -------------- --- -------- ------------------------\r\n",
    );
    g.send_to_char(ch, &header);

    let mut num_can_see = 0;
    // Iterate descriptors (connections); only PLAYING ones carry a character.
    let conns: Vec<ConnId> = g.descriptors.keys().copied().collect();
    for conn in conns {
        let (tch, host, desc_num) = match g.descriptors.get(&conn) {
            Some(d) => (d.character, d.host.clone(), d.id.0),
            None => continue,
        };
        let tch = match tch {
            Some(t) => t,
            None => continue,
        };
        let c = match g.get_char(tch) {
            Some(c) => c,
            None => continue,
        };
        if !host_search.is_empty() && !host.contains(&host_search) {
            continue;
        }
        if !name_search.is_empty() && !c.player.name.eq_ignore_ascii_case(&name_search) {
            continue;
        }
        if !g.can_see(ch, tch) || c.player.level < low || c.player.level > high {
            continue;
        }
        if outlaws && c.act_flags & PLR_KILLER == 0 && c.act_flags & PLR_THIEF == 0 {
            continue;
        }
        if showclass != 0 && (showclass & (1 << (c.player.class as i64))) == 0 {
            continue;
        }

        let classname = format!("[{:2} {}]", c.player.level, class_abbr(c.player.class));
        let state = "Playing";
        let idletime = format!("{:3}", c.timer);
        let name = c.player.name.clone();
        let timeptr = "--------";
        let mut line = format!(
            "{:3} {:<7} {:<12} {:<14} {:<3} {:<8} ",
            desc_num, classname, name, state, idletime, timeptr
        );
        if !host.is_empty() {
            line.push_str(&format!("[{}]\r\n", host));
        } else {
            line.push_str("[Hostname unknown]\r\n");
        }
        g.send_to_char(ch, &line);
        num_can_see += 1;
    }

    g.send_to_char(
        ch,
        &format!("\r\n{} visible sockets connected.\r\n", num_can_see),
    );
}

/// do_gen_ps — page various static text blocks. Most text bodies are not in the
/// contract; the ones with no source render an empty/placeholder line, while
/// clear/whoami/version are fully reproduced.
pub fn do_gen_ps(g: &mut GameState, ch: CharId, _arg: &str, subcmd: i32) {
    // SCMD_* (do_gen_ps): see command_table.rs.
    match subcmd {
        10 => g.send_to_char(ch, "\x1b[H\x1b[J"), // SCMD_CLEAR
        11 => {
            // SCMD_WHOAMI
            let name = g
                .get_char(ch)
                .map(|c| c.player.name.clone())
                .unwrap_or_default();
            g.send_to_char(ch, &format!("{}\r\n", name));
        }
        6 => {
            // SCMD_VERSION
            g.send_to_char(ch, constants::CIRCLEMUD_VERSION);
        }
        8 => {
            // SCMD_MOTD — the only text body the contract exposes (GameState::motd).
            let motd = g.motd.clone();
            g.send_to_char(ch, &motd);
        }
        4 => {
            // SCMD_WIZLIST — C send_to_char(wizlist, ch). wizlist is the global
            // loaded from lib/text/wizlist, which the native autowiz writes.
            let lib_path = g.config.lib_path.clone();
            match crate::autowiz::read_wizlist(&lib_path) {
                Some(body) => g.send_to_char(ch, &body),
                None => g.send_to_char(ch, "The wizlist is not available.\r\n"),
            }
        }
        7 => {
            // SCMD_IMMLIST — C send_to_char(immlist, ch); lib/text/immlist.
            let lib_path = g.config.lib_path.clone();
            match crate::autowiz::read_immlist(&lib_path) {
                Some(body) => g.send_to_char(ch, &body),
                None => g.send_to_char(ch, "The immlist is not available.\r\n"),
            }
        }
        // The remaining bodies (credits/news/info/imotd/handbook/policies/
        // circlemud) are not loaded in the contract.
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// where (mortal + immortal)
// ---------------------------------------------------------------------------

fn perform_mortal_where(g: &mut GameState, ch: CharId, arg: &str) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    if room_is_dark(g, rnum) && !can_see_in_dark(g, ch) {
        g.send_to_char(ch, "It is pitch black...\r\n");
        return;
    }
    if is_blind(g, ch) {
        g.send_to_char(ch, "You can't see a damned thing, you're blind!\r\n");
        return;
    }

    let ch_zone = g.room(rnum).zone;
    if arg.is_empty() {
        g.send_to_char(ch, "Players in your Zone\r\n--------------------\r\n");
        let players: Vec<CharId> = g.players_by_name.values().copied().collect();
        for i in players {
            let ok = g.can_see(ch, i)
                && g.get_char(i)
                    .and_then(|c| c.in_room)
                    .map(|r| g.room(r).zone == ch_zone)
                    .unwrap_or(false);
            if ok {
                let name = g
                    .get_char(i)
                    .map(|c| c.player.name.clone())
                    .unwrap_or_default();
                let rname = g
                    .get_char(i)
                    .and_then(|c| c.in_room)
                    .map(|r| g.room(r).name.clone())
                    .unwrap_or_default();
                g.send_to_char(ch, &format!("&Y{:<20} &R-&n {}\r\n", name, rname));
            }
        }
    } else {
        // Print only the FIRST matching char in the zone.
        let chars: Vec<CharId> = g.char_ids();
        for i in chars {
            let c = match g.get_char(i) {
                Some(c) => c,
                None => continue,
            };
            let in_zone = c
                .in_room
                .map(|r| g.room(r).zone == ch_zone)
                .unwrap_or(false);
            if in_zone && g.can_see(ch, i) && isname(arg, &c.player.name) {
                let name = c.player.name.clone();
                let rname = c
                    .in_room
                    .map(|r| g.room(r).name.clone())
                    .unwrap_or_default();
                g.send_to_char(ch, &format!("&Y{:<25} &R-&n {}\r\n", name, rname));
                return;
            }
        }
        g.send_to_char(ch, "No-one around by that name.\r\n");
    }
}

fn perform_immort_where(g: &mut GameState, ch: CharId, arg: &str) {
    if arg.is_empty() {
        g.send_to_char(ch, "Players\r\n-------\r\n");
        let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
        let players: Vec<CharId> = g.players_by_name.values().copied().collect();
        for i in players {
            let c = match g.get_char(i) {
                Some(c) => c,
                None => continue,
            };
            if g.can_see(ch, i) && c.in_room.is_some() && !(ch_level < c.player.level) {
                let name = c.player.name.clone();
                let rnum = c.in_room.unwrap();
                let vcds = rcds(g, rnum);
                let rname = g.room(rnum).name.clone();
                g.send_to_char(
                    ch,
                    &format!("&Y{:<20} &R- &C[&n{}&C]&n {}\r\n", name, vcds, rname),
                );
            }
        }
    } else {
        let mut found = false;
        let mut num = 0;
        let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
        let chars: Vec<CharId> = g.char_ids();
        for i in chars {
            let c = match g.get_char(i) {
                Some(c) => c,
                None => continue,
            };
            if g.can_see(ch, i)
                && c.in_room.is_some()
                && isname(arg, &c.player.name)
                && !(ch_level < c.player.level)
            {
                found = true;
                num += 1;
                let name = c.player.name.clone();
                let rnum = c.in_room.unwrap();
                let vcds = rcds(g, rnum);
                let rname = g.room(rnum).name.clone();
                g.send_to_char(
                    ch,
                    &format!(
                        "M&c{:3}&n. {:<25} &R-&n &C[&n{}&C]&n {}\r\n",
                        num, name, vcds, rname
                    ),
                );
            }
        }
        // Object location search.
        num = 0;
        let objs: Vec<ObjId> = g.obj_ids();
        for k in objs {
            if !can_see_obj(g, ch, k) {
                continue;
            }
            let matches = g.get_obj(k).map(|o| isname(arg, &o.name)).unwrap_or(false);
            if matches {
                found = true;
                num += 1;
                print_object_location(g, num, k, ch, true);
            }
        }
        if !found {
            g.send_to_char(ch, "Couldn't find any such thing.\r\n");
        }
    }
}

fn print_object_location(g: &mut GameState, num: i32, oid: ObjId, ch: CharId, recur: bool) {
    let (short, loc) = match g.get_obj(oid) {
        Some(o) => (o.short_description.clone(), o.loc),
        None => return,
    };
    let mut buf = if num > 0 {
        format!("O&c{:3}&n. {:<25} &R-&n ", num, short)
    } else {
        format!("&c{:>33}&n", " &R-&n ")
    };
    match loc {
        ObjLoc::Room(rnum) => {
            let vcds = rcds(g, rnum);
            let rname = g.room(rnum).name.clone();
            buf.push_str(&format!("&C[&n{}&C]&n {}\r\n", vcds, rname));
            g.send_to_char(ch, &buf);
        }
        ObjLoc::Carried(cid) => {
            let who = pers(g, ch, cid);
            buf.push_str(&format!("carried by {}\r\n", who));
            g.send_to_char(ch, &buf);
        }
        ObjLoc::Worn(cid, _) => {
            let (their_level, ch_level) = (
                g.get_char(cid).map(|c| c.player.level).unwrap_or(0),
                g.get_char(ch).map(|c| c.player.level).unwrap_or(0),
            );
            if their_level <= ch_level {
                let who = pers(g, ch, cid);
                buf.push_str(&format!("worn by {}\r\n", who));
                g.send_to_char(ch, &buf);
            }
        }
        ObjLoc::Contained(container) => {
            let cshort = g
                .get_obj(container)
                .map(|o| o.short_description.clone())
                .unwrap_or_default();
            buf.push_str(&format!(
                "inside {}{}\r\n",
                cshort,
                if recur { ", which is" } else { " " }
            ));
            g.send_to_char(ch, &buf);
            if recur {
                print_object_location(g, 0, container, ch, recur);
            }
        }
        ObjLoc::Nowhere => {
            buf.push_str("in an unknown location\r\n");
            g.send_to_char(ch, &buf);
        }
    }
}

pub fn do_where(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    let imm = g
        .get_char(ch)
        .map(|c| c.player.level >= LVL_IMMORT)
        .unwrap_or(false);
    if imm {
        perform_immort_where(g, ch, &arg);
    } else {
        perform_mortal_where(g, ch, &arg);
    }
}

// ---------------------------------------------------------------------------
// consider / diagnose
// ---------------------------------------------------------------------------

fn estimate_difficulty(g: &GameState, ch: CharId, victim: CharId) -> i32 {
    let (cl, cd, cmd, cp, cmp, ct) = g
        .get_char(ch)
        .map(|c| {
            (
                c.player.level as i32,
                c.points.defense as i32,
                c.points.mdefense as i32,
                c.points.power as i32,
                c.points.mpower as i32,
                c.points.technique as i32,
            )
        })
        .unwrap_or((0, 0, 0, 0, 0, 0));
    let (vl, vd, vmd, vp, vmp, vt) = g
        .get_char(victim)
        .map(|c| {
            (
                c.player.level as i32,
                c.points.defense as i32,
                c.points.mdefense as i32,
                c.points.power as i32,
                c.points.mpower as i32,
                c.points.technique as i32,
            )
        })
        .unwrap_or((0, 0, 0, 0, 0, 0));
    (vl - cl) + (vd - cd) + (vmd - cmd) + (vp - cp) + (vmp - cmp) + (vt - ct)
}

pub fn do_consider(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    let victim = match g.get_char_room_vis(ch, &arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "Consider killing who?\r\n");
            return;
        }
    };
    if victim == ch {
        g.send_to_char(ch, "Easy!  Very easy indeed!\r\n");
        return;
    }
    let (vlevel, vnpc) = g
        .get_char(victim)
        .map(|c| (c.player.level, c.is_npc))
        .unwrap_or((1, false));
    if vlevel >= 101 {
        g.send_to_char(ch, "Now that's not a smart thing to even think about!\r\n");
        return;
    }
    if !vnpc {
        g.send_to_char(ch, "Would you like to borrow a cross and a shovel?\r\n");
        return;
    }

    let diff = estimate_difficulty(g, ch, victim);
    let msg = if diff <= -10 {
        "Now where did that chicken go?\r\n"
    } else if diff <= -5 {
        "You could do it with a needle!\r\n"
    } else if diff <= -2 {
        "Easy.\r\n"
    } else if diff <= -1 {
        "Fairly easy.\r\n"
    } else if diff == 0 {
        "The perfect match!\r\n"
    } else if diff <= 1 {
        "You would need some luck!\r\n"
    } else if diff <= 2 {
        "You would need a lot of luck!\r\n"
    } else if diff <= 3 {
        "You would need a lot of luck and great equipment!\r\n"
    } else if diff <= 5 {
        "Do you feel lucky, punk?\r\n"
    } else if diff <= 10 {
        "Just stay there while we get you an ambulance.\r\n"
    } else if diff <= 20 {
        "Are you mad!?\r\n"
    } else if diff <= 40 {
        "The undertaker is on his way with your coffin.\r\n"
    } else if diff <= 60 {
        "Got a deathwish?!?\r\n"
    } else if diff <= 80 {
        "You ARE mad!\r\n"
    } else if diff <= 100 {
        "You ARE completely stark raving MAD!!\r\n"
    } else {
        "What are you, immortal?!?\r\n"
    };
    g.send_to_char(ch, msg);
}

pub fn do_diagnose(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg, _) = one_argument(argument);
    if !arg.is_empty() {
        match g.get_char_room_vis(ch, &arg) {
            Some(vict) => diag_char_to_char(g, vict, ch),
            None => g.send_to_char(ch, "No-one by that name here.\r\n"),
        }
    } else {
        let opp = g.get_char(ch).and_then(|c| c.fighting);
        match opp {
            Some(v) => diag_char_to_char(g, v, ch),
            None => g.send_to_char(ch, "Diagnose who?\r\n"),
        }
    }
}

// ---------------------------------------------------------------------------
// color / toggle
// ---------------------------------------------------------------------------

const CTYPES: [&str; 5] = ["off", "sparse", "normal", "complete", "\n"];

fn color_lev(c: &crate::character::Character) -> i32 {
    let mut lev = 0;
    if c.prf_flags & PRF_COLOR_1 != 0 {
        lev += 1;
    }
    if c.prf_flags & PRF_COLOR_2 != 0 {
        lev += 2;
    }
    lev
}

pub fn do_color(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        return;
    }
    let (arg, _) = one_argument(argument);
    if arg.is_empty() {
        let lev = g.get_char(ch).map(color_lev).unwrap_or(0) as usize;
        let name = CTYPES.get(lev).copied().unwrap_or("off");
        g.send_to_char(ch, &format!("Your current color level is {}.\r\n", name));
        return;
    }
    let tp = match search_block(&arg, &CTYPES) {
        Some(i) if i < 4 => i as i32,
        _ => {
            g.send_to_char(ch, "Usage: color { Off | Sparse | Normal | Complete }\r\n");
            return;
        }
    };
    if let Some(c) = g.get_char_mut(ch) {
        c.prf_flags &= !(PRF_COLOR_1 | PRF_COLOR_2);
        c.prf_flags |= (PRF_COLOR_1 * (tp & 1) as i64) | ((PRF_COLOR_2 * (tp & 2) as i64) >> 1);
    }
    g.send_to_char(
        ch,
        &format!("Your &Rcolor&n is now {}.\r\n", CTYPES[tp as usize]),
    );
}

pub fn do_toggle(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let c = match g.get_char(ch) {
        Some(c) if !c.is_npc => c.clone(),
        _ => return,
    };
    let onoff = |b: bool| if b { "ON" } else { "OFF" };
    let yesno = |b: bool| if b { "YES" } else { "NO" };

    let wimp = if c.wimp_level == 0 {
        "OFF".to_string()
    } else {
        format!("{:<3}", c.wimp_level)
    };

    let p = c.prf_flags;
    let p2 = c.prf2_flags;
    let lev = color_lev(&c) as usize;
    let color = CTYPES.get(lev).copied().unwrap_or("off");

    let buf = format!(
        "Hit Pnt Display: {:<3}         Brief Mode: {:<3}     Summon Protect: {:<3}\r\n\
   Move Display: {:<3}       Compact Mode: {:<3}            On Quest: {:<3}\r\n\
   Mana Display: {:<3}             NoTell: {:<3}        Repeat Comm.: {:<3}\r\n\
 Auto Show Exit: {:<3}               Deaf: {:<3}          Wimp Level: {:<3}\r\n\
 Gossip Channel: {:<3}    Auction Channel: {:<3}       Grats Channel: {:<3}\r\n\
   Auto Looting: {:<3}     Auto Splitting: {:<3}           Auto Gold: {:<3}\r\n\
    Exp Display: {:<3}           AFK Mode: {:<3}               NoTic: {:<3}\r\n\
   Mob Stacking: {:<3}          World Map: {:<3}         Color Level: {:<8}\r\n\
          Mercy: {:<3}       Advanced Map: {:<3}\r\n",
        onoff(p & PRF_DISPHP != 0),
        onoff(p & PRF_BRIEF != 0),
        onoff(p & PRF_SUMMONABLE == 0),
        onoff(p & PRF_DISPMOVE != 0),
        onoff(p & PRF_COMPACT != 0),
        yesno(p2 & PRF2_QCHAN != 0),
        onoff(p & PRF_DISPMANA != 0),
        onoff(p & PRF_NOTELL != 0),
        yesno(p & PRF_NOREPEAT == 0),
        onoff(p & PRF_AUTOEXIT != 0),
        yesno(p & PRF_DEAF != 0),
        wimp,
        onoff(p & PRF_NOGOSS == 0),
        onoff(p & PRF_NOAUCT == 0),
        onoff(p & PRF_NOGRATZ == 0),
        onoff(p & PRF_AUTOLOOT != 0),
        onoff(p & PRF_AUTOSPLIT != 0),
        onoff(p & PRF_AUTOGOLD != 0),
        onoff(p & PRF_DISPEXP != 0),
        onoff(p & PRF_AFK != 0),
        onoff(p & PRF_NOTIC == 0),
        onoff(p & PRF_NOLOOKSTACK == 0),
        onoff(p2 & PRF2_NOMAP == 0),
        color,
        onoff(p2 & PRF2_MERCY != 0),
        onoff(p2 & PRF2_ADVANCEDMAP != 0),
    );
    g.send_to_char(ch, &buf);
}

// ---------------------------------------------------------------------------
// commands / socials / wizhelp (do_commands)
// ---------------------------------------------------------------------------

pub fn do_commands(g: &mut GameState, ch: CharId, argument: &str, subcmd: i32) {
    use crate::command_table::CMD_INFO;

    let (arg, _) = one_argument(argument);
    let (vict, vname) = if !arg.is_empty() {
        match g.find_player_by_name(&arg) {
            Some(v) if !g.get_char(v).map(|c| c.is_npc).unwrap_or(true) && g.can_see(ch, v) => {
                let vl = g.get_char(v).map(|c| c.player.level).unwrap_or(1);
                let cl = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
                if cl < vl {
                    g.send_to_char(
                        ch,
                        "You can't see the commands of people above your level.\r\n",
                    );
                    return;
                }
                (
                    v,
                    g.get_char(v)
                        .map(|c| c.player.name.clone())
                        .unwrap_or_default(),
                )
            }
            _ => {
                g.send_to_char(ch, "Who is that?\r\n");
                return;
            }
        }
    } else {
        (ch, String::new())
    };

    // SCMD_COMMANDS=0, SCMD_SOCIALS=1, SCMD_WIZHELP=2.
    let want_socials = subcmd == 1;
    let want_wiz = subcmd == 2;

    let vict_level = g.get_char(vict).map(|c| c.player.level).unwrap_or(1);
    let vict_npc = g.get_char(vict).map(|c| c.is_npc).unwrap_or(false);

    let mut buf = format!(
        "The following {}{} are available to {}:\r\n",
        if want_wiz { "privileged " } else { "" },
        if want_socials { "socials" } else { "commands" },
        if vict == ch { "you".to_string() } else { vname }
    );

    // Build a sorted command-name list from CMD_INFO (skipping RESERVED + "\n").
    let mut entries: Vec<(&'static str, u8, bool)> = Vec::new(); // (name, min_level, is_wizcmd)
    for e in CMD_INFO {
        if e.name == "RESERVED" || e.name == "\n" {
            continue;
        }
        let is_wiz = e.min_level >= LVL_IMMORT;
        entries.push((e.name, e.min_level, is_wiz));
    }
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut no = 1;
    for (name, min_level, is_wiz) in entries {
        // Socials aren't a distinct type in the Rust table; the socials list is
        // therefore empty (matching "no socials defined" behaviour).
        if want_socials {
            continue;
        }
        let want_type_ok = if want_wiz { is_wiz } else { !is_wiz };
        if !want_type_ok {
            continue;
        }
        if vict_level < min_level && vict_level < LVL_GOD {
            continue;
        }
        if vict_npc {
            continue;
        }
        let color = if want_wiz { "&Y" } else { "&g" };
        buf.push_str(&format!("{}{:<11}&n", color, name));
        if no % 7 == 0 {
            buf.push_str("\r\n");
        }
        no += 1;
    }
    buf.push_str("\r\n");
    g.send_to_char(ch, &buf);
}

// ---------------------------------------------------------------------------
// players / mudheal / forage
// ---------------------------------------------------------------------------

pub fn do_players(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    // Backed by a SQL query in C (SELECT name,act,level from player_main); the
    // contract has no DB handle here, so list the currently-loaded players.
    let mut buf =
        String::from("\r\nKey:\r\n&RDeleted &YImmortal &BBuilder\r\n&GHero    &WMortal\r\n\r\n");
    let mut names: Vec<(String, u8)> = g
        .players_by_name
        .values()
        .filter_map(|&id| {
            g.get_char(id)
                .map(|c| (c.player.name.clone(), c.player.level))
        })
        .collect();
    names.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    let mut count = 0;
    for (name, level) in names {
        let color = if level >= LVL_IMMORT {
            "&Y"
        } else if level == LVL_HERO {
            "&G"
        } else {
            "&W"
        };
        buf.push_str(&format!("{}{:<20} ", color, name));
        count += 1;
        if count == 3 {
            count = 0;
            buf.push_str("\r\n");
        }
    }
    g.send_to_char(ch, &buf);
}

pub fn do_mudheal(g: &mut GameState, _ch: CharId, _arg: &str, _subcmd: i32) {
    let players: Vec<CharId> = g.players_by_name.values().copied().collect();
    for p in players {
        if let Some(c) = g.get_char_mut(p) {
            c.points.hit = c.points.max_hit;
            c.points.mana = c.points.max_mana;
            c.points.move_points = c.points.max_move;
        }
        g.send_to_char(p, "You have been fully healed by the gods!\r\n");
        // update_pos: a healthy character returns to standing if not fighting.
        if let Some(c) = g.get_char_mut(p) {
            if c.position < Position::Stunned && c.points.hit > 0 {
                c.position = Position::Standing;
            }
        }
    }
}

pub fn do_forage(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let (mv, level, sect) = match g.get_char(ch) {
        Some(c) => (
            c.points.move_points,
            c.player.level,
            c.in_room.map(|r| g.room(r).sector_type),
        ),
        None => return,
    };
    if mv < 120 {
        g.send_to_char(ch, "You do not have enough energy right now.\r\n");
        return;
    }
    use crate::room::SectorType;
    let sect = sect.unwrap_or(SectorType::Inside);
    if sect != SectorType::Field
        && sect != SectorType::Forest
        && sect != SectorType::Hills
        && sect != SectorType::Mountain
    {
        g.send_to_char(ch, "You cannot forage on this type of terrain!\r\n");
        return;
    }
    // GET_SKILL(ch, SKILL_FORAGE) — proficiency from the skills map.
    let skill = g
        .get_char(ch)
        .map(|c| c.skill(crate::spell_parser::SKILL_FORAGE as u16) as i32)
        .unwrap_or(0);
    if skill <= 0 {
        g.send_to_char(ch, "You have no idea how to forage!\r\n");
        return;
    }
    g.send_to_char(ch, "You start searching the area for signs of food.\r\n");
    act(
        g,
        "$n starts foraging the area for food.\r\n",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );

    if g.rng.number(1, 101) > skill {
        g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= 120 - level as i32;
        }
        g.send_to_char(ch, "\r\nYou have no luck finding anything to eat.\r\n");
        return;
    }
    // Success: create one of the food prototypes (vnum chosen 1..7).
    let item_no = match g.rng.number(1, 7) {
        1 => 3015,
        2 => 3009,
        3 => 6023,
        4 => 1020,
        5 => 3010,
        6 => 2505,
        _ => 10015,
    };
    g.set_wait_state(ch, PULSE_VIOLENCE as i32 * 2); // Not really necessary
    if let Some(c) = g.get_char_mut(ch) {
        c.points.move_points -= 120 - level as i32;
    }
    if let Some(oid) = g.load_object(item_no) {
        g.obj_to_char(oid, ch);
        let short = g
            .get_obj(oid)
            .map(|o| o.short_description.clone())
            .unwrap_or_default();
        g.send_to_char(ch, &format!("You have found {}!\r\n", short));
        act(
            g,
            "$n has found something in his forage attempt.\r\n",
            false,
            ch,
            None,
            ActArg::None,
            To::Room,
        );
    }
}

// ---------------------------------------------------------------------------
// whois / scan / rnum / levels
// ---------------------------------------------------------------------------

pub fn do_whois(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let arg = argument.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Do a whois on which player?\r\n");
        return;
    }
    // C reads the player file (retrieve_player_entry); the contract only exposes
    // currently-loaded players, so resolve against those.
    let victim = g.find_player_by_name(arg);
    match victim {
        Some(v) => {
            let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
            let c = g.get_char(v).unwrap();
            if ch_level < LVL_IMMORT && c.player.level >= LVL_IMMORT {
                g.send_to_char(ch, "Information about immortals is unavailable.\r\n");
                return;
            }
            let name = c.player.name.clone();
            let level = c.player.level;
            let race = race_name(c.player.race);
            let class = class_name(c.player.class);
            g.send_to_char(
                ch,
                &format!(
                    "{} is a level {}, {} {}.\r\n{} was last on at (unknown)\r\n",
                    name, level, race, class, name
                ),
            );
        }
        None => g.send_to_char(ch, "There is no such player.\r\n"),
    }
}

pub fn do_scan(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let distance = [
        "right here",
        "immediately ",
        "nearby ",
        "a ways ",
        "far ",
        "very far ",
        "extremely far ",
        "impossibly far ",
    ];

    if is_blind(g, ch) {
        act(
            g,
            "You can't see anything, you're blind!",
            true,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }
    let (mv, level) = g
        .get_char(ch)
        .map(|c| (c.points.move_points, c.player.level))
        .unwrap_or((0, 1));
    if mv < 3 && level < LVL_IMMORT {
        act(
            g,
            "You are too exhausted.",
            true,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
        return;
    }

    let mut maxdis = 1
        + ((g
            .get_char(ch)
            .map(|c| c.skill(crate::spell_parser::SKILL_SCAN as u16) as i32)
            .unwrap_or(0)
            * 5)
            / 100);
    if level >= LVL_IMMORT {
        maxdis = 7;
    }

    act(
        g,
        "You quickly scan the area and see:",
        true,
        ch,
        None,
        ActArg::None,
        To::Char,
    );
    act(
        g,
        "$n quickly scans the area.",
        false,
        ch,
        None,
        ActArg::None,
        To::Room,
    );
    if level < LVL_IMMORT {
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= 3;
        }
    }

    let is_in = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let mut found = 0;
    for dir in 0..NUM_OF_DIRS {
        let mut cur = is_in;
        for dis in 0..=maxdis as usize {
            if (dis == 0 && dir == 0) || dis > 0 {
                let people = g.room(cur).people.clone();
                for i in people {
                    if !(i == ch && dis == 0) && g.can_see(ch, i) {
                        let name = g
                            .get_char(i)
                            .map(|c| c.player.name.clone())
                            .unwrap_or_default();
                        let to_the = if dis > 0 && dir < (NUM_OF_DIRS - 2) {
                            "to the "
                        } else {
                            ""
                        };
                        let dname = if dis > 0 { DIR_NAMES[dir] } else { "" };
                        let wards = if dis > 0 && dir > (NUM_OF_DIRS - 3) {
                            "wards"
                        } else {
                            ""
                        };
                        let line = format!(
                            "&Y{:>33}&R:&W {}{}{}{}&n",
                            name,
                            distance.get(dis).copied().unwrap_or(""),
                            to_the,
                            dname,
                            wards
                        );
                        act(g, &line, true, ch, None, ActArg::None, To::Char);
                        found += 1;
                    }
                }
            }
            // Advance one room in this direction, if possible.
            let next = g.room(cur).exits[dir].as_ref().and_then(|e| {
                if e.exit_info & EX_CLOSED != 0 {
                    None
                } else {
                    g.real_room(e.to_room)
                }
            });
            match next {
                Some(r) if r != is_in => cur = r,
                _ => break,
            }
        }
    }
    if found == 0 {
        act(
            g,
            "Nobody anywhere near you.",
            true,
            ch,
            None,
            ActArg::None,
            To::Char,
        );
    }
}

pub fn do_rnum(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let vnum = g.room(rnum).number;
    g.send_to_char(
        ch,
        &format!("You are in room {} (real room: {}).\r\n", vnum, rnum),
    );
}

pub fn do_levels(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(true) {
        g.send_to_char(ch, "You ain't nothin' but a hound-dog.\r\n");
        return;
    }
    let mut buf =
        String::from("Total/per-level experience points needed to complete each level:\r\n\r\n");
    let mut lastxp = 0i64;
    for i in 1..(LVL_IMMORT as i32) {
        let xp = exp_to_level(i);
        buf.push_str(&format!("&b[&c{:<4}&b]&n {}", i, numdisplay(xp)));
        buf.push_str(&format!(" - {}\r\n", numdisplay(xp - lastxp)));
        lastxp = xp;
    }
    let xp = exp_to_level(LVL_IMMORT as i32);
    buf.push_str(&format!("&b[&Y{:<4}&b]&n {}", LVL_IMMORT, numdisplay(xp)));
    buf.push_str(&format!(" - {}\r\n", numdisplay(xp - lastxp)));
    g.send_to_char(ch, &buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Character;
    use crate::config::Config;
    use crate::connection::Descriptor;
    use crate::room::{Exit, Room};

    #[test]
    fn scan_uses_trained_scan_skill_for_distance() {
        let mut g = GameState::new(Config::default());
        let r0 = g.add_room(Room::new(100, 0, "Start".to_string(), "Start.".to_string()));
        let r1 = g.add_room(Room::new(
            101,
            0,
            "Middle".to_string(),
            "Middle.".to_string(),
        ));
        let r2 = g.add_room(Room::new(102, 0, "Far".to_string(), "Far.".to_string()));
        g.rooms[r0].exits[NORTH] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: NOTHING,
            to_room: 101,
        });
        g.rooms[r1].exits[NORTH] = Some(Exit {
            description: None,
            keyword: None,
            exit_info: 0,
            key: NOTHING,
            to_room: 102,
        });

        let conn = ConnId(1);
        g.descriptors
            .insert(conn, Descriptor::new(conn, "test".to_string()));
        let mut scanner = Character::new_player("Scanner".to_string(), Class::Warrior, Race::Human);
        scanner.desc = Some(conn);
        scanner.points.move_points = 10;
        scanner.set_skill(crate::spell_parser::SKILL_SCAN as u16, 20);
        let scanner = g.create_char(scanner);
        g.char_to_room(scanner, r0);

        let target = g.create_char(Character::new_player(
            "Target".to_string(),
            Class::Warrior,
            Race::Human,
        ));
        g.char_to_room(target, r2);

        do_scan(&mut g, scanner, "", 0);

        let out = &g.descriptors.get(&conn).unwrap().outbuf;
        assert!(out.contains("Target"));
        assert!(out.contains("nearby to the north"));
    }
}
