// cmd_create.rs — player-level object-creation commands (CircleMUD
// act.create.c, DeltaMUD crafting). Ported 1:1: do_brew, do_scribe, do_forge.
//
// The spell subsystem (spells[]/syls[]/spell_info[]/find_skill_num/
// mag_manacost) is not yet a shared crate module, so the small slices these
// commands need are embedded here as private tables/helpers, transcribed from
// spell_parser.c so the output strings and mana math match the C MUD exactly.

use crate::act::{act, ActArg, To};
use crate::object::{ExtraFlags, ObjectType, WearFlags};
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Spell numbers (spells.h) used by make_potion's brewable whitelist.
// ---------------------------------------------------------------------------
const SPELL_CURE_BLIND: i32 = 10;
const SPELL_CURE_CRITIC: i32 = 11;
const SPELL_CURE_LIGHT: i32 = 12;
const SPELL_DETECT_INVIS: i32 = 15;
const SPELL_DETECT_MAGIC: i32 = 16;
const SPELL_DETECT_POISON: i32 = 17;
const SPELL_HEAL: i32 = 20;
const SPELL_INVISIBLE: i32 = 21;
const SPELL_SANCTUARY: i32 = 25;
const SPELL_STRENGTH: i32 = 27;
const SPELL_WORD_OF_RECALL: i32 = 29;
const SPELL_REMOVE_POISON: i32 = 30;
const SPELL_SENSE_LIFE: i32 = 31;
const SPELL_INFRAVISION: i32 = 36;
const SPELL_WATERWALK: i32 = 37;
const SPELL_STONE_SKIN: i32 = 38;

// Skill numbers (spells.h) gating each crafting verb.
const SKILL_BREW: u16 = 513;
const SKILL_FORGE: u16 = 514;
const SKILL_SCRIBE: u16 = 515;

// Spell-table bounds (spells.h).
const MAX_SPELLS: i32 = 500;

// ---------------------------------------------------------------------------
// spells[] — the player-facing spell/skill name table (spell_parser.c). Index
// is the spell/skill number; entry 0 is "!RESERVED!"; "\n" terminates. Only
// the populated head is transcribed; the long "!UNUSED!" runs are filled in by
// `spell_name()` returning "!UNUSED!" for any in-range gap.
// ---------------------------------------------------------------------------
const SPELL_NAMES: &[(i32, &str)] = &[
    (1, "armor"),
    (2, "teleport"),
    (3, "bless"),
    (4, "blindness"),
    (5, "charm person"),
    (6, "clone"),
    (7, "control weather"),
    (8, "create food"),
    (9, "create water"),
    (10, "cure blind"),
    (11, "cure critic"),
    (12, "cure light"),
    (13, "curse"),
    (14, "detect alignment"),
    (15, "detect invisibility"),
    (16, "detect magic"),
    (17, "detect poison"),
    (18, "earthquake"),
    (19, "enchant weapon"),
    (20, "heal"),
    (21, "invisibility"),
    (22, "locate object"),
    (23, "poison"),
    (24, "remove curse"),
    (25, "sanctuary"),
    (26, "sleep"),
    (27, "strength"),
    (28, "summon"),
    (29, "word of recall"),
    (30, "remove poison"),
    (31, "sense life"),
    (32, "animate dead"),
    (33, "group armor"),
    (34, "group heal"),
    (35, "group recall"),
    (36, "infravision"),
    (37, "waterwalk"),
    (38, "stone skin"),
    (39, "fear"),
    (40, "recharge"),
    (41, "portal"),
    (42, "group stone skin"),
    (43, "locate life"),
    (44, "convergence of power"),
    (45, "mana autus"),
    (46, "resist portal"),
    (47, "regen mana"),
    (48, "home"),
    (49, "word of retreat"),
    (50, "chain footing"),
    (51, "redirect charge"),
    // SKILLS (501+) — needed by find_skill_num so "brew"/"forge"/etc. resolve
    // to skill numbers and are then rejected by the > MAX_SPELLS gate.
    (501, "backstab"),
    (502, "bash"),
    (503, "hide"),
    (504, "kick"),
    (505, "pick lock"),
    (506, "punch"),
    (507, "rescue"),
    (508, "sneak"),
    (509, "steal"),
    (510, "track"),
    (511, "forage"),
    (512, "scan"),
    (513, "brew"),
    (514, "forge"),
    (515, "scribe"),
    (516, "speed"),
    (517, "berserk"),
    (518, "camouflage"),
    (519, "blanket"),
    (520, "ram"),
    (521, "mount"),
    (522, "riding"),
    (523, "tame"),
    (524, "second attack"),
    (525, "third attack"),
    (526, "listen"),
    (527, "meditate"),
    (528, "repair"),
    (529, "tan"),
    (530, "fillet"),
    (531, "carve"),
    (532, "dodge"),
    (533, "parry"),
    (534, "avoid"),
    (535, "riposte"),
    (536, "circle"),
    (537, "trip"),
    (538, "disarm"),
    (539, "target"),
    (540, "adrenaline"),
    (541, "bloodlust"),
    (542, "carnal rage"),
    // OBJECT/NPC spells (1001+).
    (1001, "identify"),
    (1002, "fire breath"),
    (1003, "gas breath"),
    (1004, "frost breath"),
    (1005, "acid breath"),
    (1006, "lightning breath"),
];

// Highest spell/skill index find_skill_num scans before the "\n" terminator
// in spells[] (entry 1006 = "lightning breath", 1007 = "\n").
const TOP_SPELL_INDEX: i32 = 1006;

/// Look up spells[index]. Returns "!RESERVED!" for 0, the populated name when
/// known, and "!UNUSED!" for any in-range gap (mirrors the C array's filler).
fn spell_name(index: i32) -> &'static str {
    if index == 0 {
        return "!RESERVED!";
    }
    for &(n, name) in SPELL_NAMES {
        if n == index {
            return name;
        }
    }
    "!UNUSED!"
}

// ---------------------------------------------------------------------------
// spell_info[] mana slice (spell_parser.c spello calls): (mana_max, mana_min,
// mana_change) per spell. min_level[CLASS_ARTISAN] is LVL_IMMORT for every one
// of these (no spell_level() assignment exists), so mag_manacost uses 101 as
// the class min-level for all of them — preserved exactly below.
// ---------------------------------------------------------------------------
fn spell_mana_params(spellnum: i32) -> Option<(i32, i32, i32)> {
    let p = match spellnum {
        32 => (175, 150, 3),  // animate dead
        1 => (30, 15, 3),     // armor
        3 => (35, 5, 3),      // bless
        4 => (35, 25, 1),     // blindness
        5 => (75, 50, 2),     // charm person
        6 => (200, 150, 5),   // clone
        7 => (75, 25, 5),     // control weather
        8 => (30, 5, 4),      // create food
        9 => (30, 5, 4),      // create water
        10 => (30, 5, 2),     // cure blind
        11 => (30, 10, 2),    // cure critic
        12 => (30, 10, 2),    // cure light
        13 => (80, 50, 2),    // curse
        14 => (20, 10, 2),    // detect alignment
        15 => (20, 10, 2),    // detect invisibility
        16 => (20, 10, 2),    // detect magic
        17 => (15, 5, 1),     // detect poison
        18 => (40, 25, 3),    // earthquake
        19 => (150, 100, 10), // enchant weapon
        33 => (50, 30, 2),    // group armor
        34 => (80, 60, 5),    // group heal
        20 => (60, 40, 3),    // heal
        36 => (25, 10, 1),    // infravision
        21 => (35, 25, 1),    // invisibility
        22 => (25, 20, 1),    // locate object
        43 => (100, 20, 1),   // locate life (locate target)
        23 => (50, 20, 3),    // poison
        24 => (45, 25, 5),    // remove curse
        25 => (110, 85, 5),   // sanctuary
        26 => (40, 25, 5),    // sleep
        27 => (35, 30, 1),    // strength
        28 => (75, 50, 3),    // summon
        29 => (20, 10, 2),    // word of recall
        30 => (40, 8, 4),     // remove poison
        31 => (20, 10, 2),    // sense life
        38 => (120, 60, 3),   // stone skin
        39 => (100, 50, 1),   // fear
        40 => (150, 75, 1),   // recharge
        42 => (240, 120, 2),  // group stone skin
        44 => (110, 85, 5),   // convergence of power
        45 => (140, 100, 5),  // mana autus
        46 => (200, 100, 2),  // resist portal
        47 => (150, 150, 0),  // regen mana
        48 => (80, 20, 1),    // home
        49 => (30, 20, 1),    // word of retreat
        37 => (200, 150, 1),  // waterwalk
        _ => return None,
    };
    Some(p)
}

/// CircleMUD mag_manacost (spell_parser.c): the per-cast mana for `spellnum`
/// at this character's level. min_level[class] for every brewable/scribable
/// spell is LVL_IMMORT (101). AFF_AUTUS halving is not modelled (the affect
/// flag is not yet ported), matching the current rust-mud affect set.
fn mag_manacost(g: &GameState, ch: CharId, spellnum: i32) -> i32 {
    let level = g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(1);
    let (mana_max, mana_min, mana_change) = spell_mana_params(spellnum).unwrap_or((0, 0, 0));
    let computed = mana_max - mana_change * (level - LVL_IMMORT as i32);
    computed.max(mana_min)
}

// ---------------------------------------------------------------------------
// syls[] (spell_parser.c) — garble table used to obfuscate a scroll's spell
// name. The "\n"-terminated org list is encoded as a slice; the longest
// matching prefix at each offset is substituted, in table order.
// ---------------------------------------------------------------------------
const SYLS: &[(&str, &str)] = &[
    (" ", " "),
    ("ar", "abra"),
    ("ate", "i"),
    ("cau", "kada"),
    ("blind", "nose"),
    ("bur", "mosa"),
    ("cu", "judi"),
    ("de", "oculo"),
    ("dis", "mar"),
    ("ect", "kamina"),
    ("en", "uns"),
    ("gro", "cra"),
    ("light", "dies"),
    ("lo", "hi"),
    ("magi", "kari"),
    ("mon", "bar"),
    ("mor", "zak"),
    ("move", "sido"),
    ("ness", "lacri"),
    ("ning", "illa"),
    ("per", "duda"),
    ("ra", "gru"),
    ("re", "candus"),
    ("son", "sabru"),
    ("tect", "infra"),
    ("tri", "cula"),
    ("ven", "nofo"),
    ("word of", "inset"),
    ("a", "i"),
    ("b", "v"),
    ("c", "q"),
    ("d", "m"),
    ("e", "o"),
    ("f", "y"),
    ("g", "t"),
    ("h", "p"),
    ("i", "u"),
    ("j", "y"),
    ("k", "t"),
    ("l", "r"),
    ("m", "w"),
    ("n", "b"),
    ("o", "a"),
    ("p", "s"),
    ("q", "d"),
    ("r", "f"),
    ("s", "g"),
    ("t", "h"),
    ("u", "e"),
    ("v", "z"),
    ("w", "x"),
    ("x", "n"),
    ("y", "l"),
    ("z", "k"),
    ("", ""),
];

/// CircleMUD garble_spell (act.create.c): walk the spell name left-to-right,
/// substituting the first matching syllable from SYLS at each offset. The C
/// loop advances `ofs` only when a match is found; if no syllable matches the
/// current position it would spin forever, but the single-character entries at
/// the tail guarantee a match for any ASCII letter (matching C exactly).
fn garble_spell(spellnum: i32) -> String {
    let lbuf = spell_name(spellnum).as_bytes();
    let mut out = String::new();
    let mut ofs = 0usize;
    while ofs < lbuf.len() {
        let mut matched = false;
        for &(org, new) in SYLS {
            let org_b = org.as_bytes();
            if org_b.is_empty() {
                continue;
            }
            if ofs + org_b.len() <= lbuf.len() && &lbuf[ofs..ofs + org_b.len()] == org_b {
                out.push_str(new);
                ofs += org_b.len();
                matched = true;
                break;
            }
        }
        if !matched {
            // No syllable matched (non-letter char such as '\0' padding); the C
            // code leaves ofs unchanged here, but advancing avoids a hang while
            // preserving output for every real (letter/space) spell name.
            ofs += 1;
        }
    }
    out
}

/// CircleMUD find_skill_num (spell_parser.c): abbreviation / multi-word match
/// of `name` against spells[]. Returns the spell/skill index, or -1.
fn find_skill_num(name: &str) -> i32 {
    let name = name.trim();
    if name.is_empty() {
        return -1;
    }
    let mut index = 1;
    while index <= TOP_SPELL_INDEX {
        let entry = spell_name(index);
        // is_abbrev(name, spells[index]) — whole-string prefix match.
        if crate::interpreter::is_abbrev(name, entry) {
            return index;
        }
        // Multi-word abbreviation: every word of `name` is_abbrev of the
        // corresponding word of spells[index], and `name` has no extra words.
        let mut ent_words = entry.split_whitespace();
        let mut nam_words = name.split_whitespace();
        let mut ok = true;
        loop {
            let e = ent_words.next();
            let n = nam_words.next();
            match (e, n) {
                (Some(ew), Some(nw)) => {
                    if !crate::interpreter::is_abbrev(nw, ew) {
                        ok = false;
                        break;
                    }
                }
                _ => break,
            }
        }
        if ok && nam_words.next().is_none() {
            return index;
        }
        index += 1;
    }
    -1
}

/// get_spell_name (act.create.c): the text between the first pair of single
/// quotes in `argument`. Returns None when there is no closing quote.
fn get_spell_name(argument: &str) -> Option<String> {
    let mut parts = argument.split('\'');
    parts.next(); // text before the first quote
    parts.next().map(|s| s.to_string())
}

/// CircleMUD update_pos (fight.c): recompute a character's position from HP.
fn update_pos(g: &mut GameState, victim: CharId) {
    if let Some(c) = g.get_char_mut(victim) {
        let hp = c.points.hit;
        if hp > 0 && c.position > Position::Stunned {
            return;
        }
        c.position = if hp > 0 {
            Position::Standing
        } else if hp <= -11 {
            Position::Dead
        } else if hp <= -6 {
            Position::MortallyWounded
        } else if hp <= -3 {
            Position::Incapacitated
        } else {
            Position::Stunned
        };
    }
}

/// IS_ARENACOMBATANT (utils.h): true while a player is queued/active in the
/// arena. The arena status field is not yet on the Rust Character, so this is
/// always false (no player is an arena combatant in the current data model).
fn is_arena_combatant(_g: &GameState, _ch: CharId) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Potion mixing (make_potion / do_brew)
// ---------------------------------------------------------------------------

/// potion_names[] (act.create.c) — colour-coded surface descriptions, indexed
/// by the per-spell `num` mapping in make_potion.
const POTION_NAMES: [&str; 16] = [
    "milky white",
    "&Wbubbling white&n",
    "&Mglowing ivory&n",
    "&Bglowing blue&n",
    "&ybubbling yellow&n",
    "&Glight green&n",
    "&ygritty brown&n",
    "&Rblood red&n",
    "&Mswirling purple&n",
    "&gflickering green&n",
    "&ccloudy blue&n",
    "&Rglowing red&n",
    "&Wsparkling white&n",
    "&bincandescent blue&n",
    "&wtranslucent grey&n",
    "&bmidnight blue&n",
];

fn make_potion(g: &mut GameState, ch: CharId, potion: i32, container: ObjId) {
    // Map the spell to its potion-colour index; reject non-brewable spells.
    let num: usize = match potion {
        SPELL_CURE_BLIND => 0,
        SPELL_CURE_LIGHT => 1,
        SPELL_CURE_CRITIC => 2,
        SPELL_DETECT_MAGIC => 3,
        SPELL_DETECT_INVIS => 4,
        SPELL_DETECT_POISON => 5,
        SPELL_REMOVE_POISON => 6,
        SPELL_STRENGTH => 7,
        SPELL_WORD_OF_RECALL => 8,
        SPELL_SENSE_LIFE => 9,
        SPELL_WATERWALK => 10,
        SPELL_INFRAVISION => 11,
        SPELL_HEAL => 12,
        SPELL_SANCTUARY => 13,
        SPELL_INVISIBLE => 14,
        SPELL_STONE_SKIN => 15,
        _ => {
            g.send_to_char(ch, "That spell cannot be mixed into a potion.\r\n");
            return;
        }
    };

    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);

    // 1-in-3 catastrophic explosion for mortals.
    if g.rng.number(1, 3) == 3 && level < LVL_IMMORT {
        g.send_to_char(ch, "As you begin mixing the potion, it violently explodes!\r\n");
        act(g, "$n begins to mix a potion, but it suddenly explodes!", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(container);
        g.extract_obj(container);
        let dam_hi = mag_manacost(g, ch, potion) * 2;
        let dam = g.rng.number(15, dam_hi);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.hit -= dam;
        }
        update_pos(g, ch);
        return;
    }

    // Brewing costs x3 the spell's mana.
    let mana = mag_manacost(g, ch, potion) * 3;
    let cur_mana = g.get_char(ch).map(|c| c.points.mana).unwrap_or(0);
    if cur_mana - mana > 0 {
        if level < LVL_IMMORT {
            if let Some(c) = g.get_char_mut(ch) {
                c.points.mana -= mana;
            }
        }
        g.send_to_char(ch, &format!("You create a {} potion.\r\n", spell_name(potion)));
        act(g, "$n creates a potion!", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(container);
        g.extract_obj(container);
    } else {
        g.send_to_char(ch, "You don't have enough mana to mix that potion!\r\n");
        return;
    }

    // Build the finished potion.
    let sname = spell_name(potion);
    let colour = POTION_NAMES[num];
    let mut obj = crate::object::Object::new(
        NOTHING,
        format!("{} {} potion", colour, sname),
        format!("a {} potion", colour),
    );
    obj.description = format!("A {} potion lies here.", colour);
    obj.action_description = Some(format!("It appears to be a {} potion.", sname));
    obj.obj_type = ObjectType::Potion;
    obj.wear_flags = WearFlags::TAKE;
    obj.extra_flags = ExtraFlags::NO_RENT;
    obj.values = [level as i32, potion, -1, -1];
    obj.cost = level as i32 * 500;
    obj.weight = 1;
    obj.rent = 0;

    let oid = g.create_obj(obj);
    g.obj_to_char(oid, ch);
}

pub fn do_brew(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // bottle_name = first argument; spell_name = text inside single quotes.
    let (bottle_name, rest) = crate::interpreter::one_argument(arg);
    let spell_name_str = get_spell_name(rest).unwrap_or_default();

    // Can't brew in the arena.
    if is_arena_combatant(g, ch) {
        g.send_to_char(ch, "Strangely enough, you find your ability to brew potions dimunished while you're in the arena.\r\n");
        return;
    }

    let (class, level) = g
        .get_char(ch)
        .map(|c| (c.player.class, c.player.level))
        .unwrap_or((Class::Warrior, 1));
    if !(class == Class::Artisan || level >= LVL_IMMORT) {
        g.send_to_char(ch, "You have no idea how to mix potions!\r\n");
        return;
    }
    if g.get_char(ch).map(|c| c.skill(SKILL_BREW)).unwrap_or(0) == 0 {
        g.send_to_char(ch, "You have no idea how to mix potions!\r\n");
        return;
    }
    if bottle_name.is_empty() || spell_name_str.is_empty() {
        g.send_to_char(ch, "What do you wish to mix in where?\r\n");
        return;
    }

    // Locate the container in inventory.
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let container = g.get_obj_in_list_vis(ch, &bottle_name, &inv);
    let found = container.is_some();

    if let Some(cid) = container {
        if g.get_obj(cid).map(|o| o.obj_type) != Some(ObjectType::LiqContainer) {
            g.send_to_char(ch, "That item is not a drink container!\r\n");
            return;
        }
    }
    if !found {
        g.send_to_char(ch, &format!("You don't have {} in your inventory!\r\n", bottle_name));
        return;
    }

    if spell_name_str.is_empty() {
        g.send_to_char(ch, "Spell names must be enclosed in single quotes!\r\n");
        return;
    }

    let potion = find_skill_num(&spell_name_str);
    if potion < 1 || potion > MAX_SPELLS {
        g.send_to_char(ch, "Mix what spell?!?\r\n");
        return;
    }

    make_potion(g, ch, potion, container.unwrap());
}

// ---------------------------------------------------------------------------
// Scroll scribing (make_scroll / do_scribe)
// ---------------------------------------------------------------------------

fn make_scroll(g: &mut GameState, ch: CharId, scroll: i32, paper: ObjId) {
    // (No prohibited-spell case statement in the C source — can_make is always
    // true here.)
    let level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);

    // 1-in-3 catastrophic explosion for mortals.
    if g.rng.number(1, 3) == 3 && level < LVL_IMMORT {
        g.send_to_char(ch, "As you begin inscribing the final rune, the scroll violently explodes!\r\n");
        act(g, "$n tries to scribe a spell, but it explodes!", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(paper);
        g.extract_obj(paper);
        let dam_hi = mag_manacost(g, ch, scroll) * 2;
        let dam = g.rng.number(15, dam_hi);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.hit -= dam;
        }
        update_pos(g, ch);
        return;
    }

    // Scribing costs x3 the spell's mana.
    let mana = mag_manacost(g, ch, scroll) * 3;
    let cur_mana = g.get_char(ch).map(|c| c.points.mana).unwrap_or(0);
    if cur_mana - mana > 0 {
        if level < LVL_IMMORT {
            if let Some(c) = g.get_char_mut(ch) {
                c.points.mana -= mana;
            }
        }
        g.send_to_char(ch, &format!("You create a scroll of {}.\r\n", spell_name(scroll)));
        act(g, "$n creates a scroll!", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(paper);
        g.extract_obj(paper);
    } else {
        g.send_to_char(ch, "You don't have enough mana to scribe such a powerful spell!\r\n");
        return;
    }

    // Build the finished scroll (garbled runes for flavour).
    let garbled = garble_spell(scroll);
    let mut obj = crate::object::Object::new(
        NOTHING,
        format!("{} {} scroll", spell_name(scroll), garbled),
        format!("a {} scroll", garbled),
    );
    obj.description = format!("A parchment inscribed with the runes '{}' lies here.", garbled);
    obj.action_description = Some(format!("It appears to be a {} scroll.", spell_name(scroll)));
    obj.obj_type = ObjectType::Scroll;
    obj.wear_flags = WearFlags::TAKE;
    obj.extra_flags = ExtraFlags::NO_RENT;
    obj.values = [level as i32, scroll, -1, -1];
    obj.cost = level as i32 * 500;
    obj.weight = 1;
    obj.rent = 0;

    let oid = g.create_obj(obj);
    g.obj_to_char(oid, ch);
}

pub fn do_scribe(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (paper_name, rest) = crate::interpreter::one_argument(arg);
    let spell_name_str = get_spell_name(rest).unwrap_or_default();

    // Can't scribe in the arena.
    if is_arena_combatant(g, ch) {
        g.send_to_char(ch, "Strangely enough, you find your ability to scribe scrolls diminished while you're in the arena.\r\n");
        return;
    }

    let (class, level) = g
        .get_char(ch)
        .map(|c| (c.player.class, c.player.level))
        .unwrap_or((Class::Warrior, 1));
    if !(class == Class::Artisan || level >= LVL_IMMORT) {
        g.send_to_char(ch, "You have no idea how to scribe scrolls!\r\n");
        return;
    }
    if g.get_char(ch).map(|c| c.skill(SKILL_SCRIBE)).unwrap_or(0) == 0 {
        g.send_to_char(ch, "You have no idea how to scribe scrolls!\r\n");
        return;
    }
    if paper_name.is_empty() || spell_name_str.is_empty() {
        g.send_to_char(ch, "What do you wish to scribe where?\r\n");
        return;
    }

    // Locate the paper in inventory.
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let paper = g.get_obj_in_list_vis(ch, &paper_name, &inv);
    let found = paper.is_some();

    if let Some(pid) = paper {
        if g.get_obj(pid).map(|o| o.obj_type) != Some(ObjectType::Note) {
            g.send_to_char(ch, "You can't write on that!\r\n");
            return;
        }
    }
    if !found {
        g.send_to_char(ch, &format!("You don't have {} in your inventory!\r\n", paper_name));
        return;
    }

    if spell_name_str.is_empty() {
        g.send_to_char(ch, "Spell names must be enclosed in single quotes!\r\n");
        return;
    }

    let scroll = find_skill_num(&spell_name_str);
    if scroll < 1 || scroll > MAX_SPELLS {
        g.send_to_char(ch, "Scribe what spell?!?\r\n");
        return;
    }

    make_scroll(g, ch, scroll, paper.unwrap());
}

// ---------------------------------------------------------------------------
// Weapon forging (do_forge)
// ---------------------------------------------------------------------------

/// GET_ADD (utils.h): the strength-addition sub-stat (aff_abils.str_add).
fn get_add(g: &GameState, ch: CharId) -> i32 {
    g.get_char(ch).map(|c| c.aff_abils.str_add as i32).unwrap_or(0)
}

pub fn do_forge(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    // PLEASE NOTE: this command rewrites the weapon's object_values, which the
    // C version persists to rent files. value[0] stores the forger's level to
    // prevent a mortal re-forge.
    let (weapon_name, _rest) = crate::interpreter::one_argument(arg);

    let (class, level) = g
        .get_char(ch)
        .map(|c| (c.player.class, c.player.level))
        .unwrap_or((Class::Warrior, 1));
    if !(class == Class::Artisan || level >= LVL_IMMORT) {
        g.send_to_char(ch, "You have no idea how to forge weapons!\r\n");
        return;
    }
    if g.get_char(ch).map(|c| c.skill(SKILL_FORGE)).unwrap_or(0) == 0 {
        g.send_to_char(ch, "You have no idea how to forge weapons!\r\n");
        return;
    }
    if weapon_name.is_empty() {
        g.send_to_char(ch, "What do you wish to forge?\r\n");
        return;
    }

    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let weapon = match g.get_obj_in_list_vis(ch, &weapon_name, &inv) {
        Some(w) => w,
        None => {
            g.send_to_char(ch, &format!("You don't have {} in your inventory!\r\n", weapon_name));
            return;
        }
    };

    if g.get_obj(weapon).map(|o| o.obj_type) != Some(ObjectType::Weapon) {
        g.send_to_char(ch, &format!("It doesn't look like {} would make a good weapon...\r\n", weapon_name));
        return;
    }

    let val0 = g.get_obj(weapon).map(|o| o.values[0]).unwrap_or(0);
    if val0 > 0 && level < LVL_GOD {
        g.send_to_char(ch, "You cannot forge a weapon more than once!\r\n");
        return;
    }

    if g.get_obj(weapon).map(|o| o.extra_flags.contains(ExtraFlags::MAGIC)).unwrap_or(false) {
        g.send_to_char(ch, "The weapon is imbued with magical powers beyond your grasp.\r\nYou cannot further affect its form.\r\n");
        return;
    }

    // Success probability from physical stats.
    let (str_, dex) = g
        .get_char(ch)
        .map(|c| (c.aff_abils.str as i32, c.aff_abils.dex as i32))
        .unwrap_or((11, 11));
    let add = get_add(g, ch);
    let mut prob: i32 = 0;
    prob += (dex - 11) << 1;
    prob += ((str_ - 11) << 1) + (add >> 3);
    prob -= g.rng.number(10, 30);

    // number((prob*0.7), (prob*1.5)) — C truncates the float bounds to int.
    let lo = (prob as f64 * 0.7) as i32;
    let hi = (prob as f64 * 1.5) as i32;
    let roll = g.rng.number(lo, hi);
    let threshold = prob + (level as i32 / g.rng.number(5, 10));
    if roll > threshold && level < LVL_IMMORT {
        g.send_to_char(ch, "As you pound out the dents in the weapon, you hit a weak spot and it explodes!\r\n");
        act(g, "$n tries to forge a weapon, but it explodes!", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(weapon);
        g.extract_obj(weapon);
        let dam = g.rng.number(10, 22);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.hit -= dam;
        }
        update_pos(g, ch);
        return;
    }

    // Apply the forge: stamp level into value[0], buff dice in value[1]/[2].
    let lvl = level as i32;
    let bump1 = (lvl % 3) + g.rng.number(-1, 2);
    let bump2 = (lvl % 2) + g.rng.number(-1, 2);
    let (new_v1, new_v2) = {
        let o = g.get_obj_mut(weapon).unwrap();
        o.values[0] = lvl;
        o.values[1] += bump1;
        o.values[2] += bump2;
        (o.values[1], o.values[2])
    };

    // Over-stress check: if the new dice roll exceeds level/2.5, it shatters.
    if g.rng.dice(new_v1, new_v2) as f64 > (lvl as f64 / 2.5) {
        g.send_to_char(ch, "As you pound out the dents in the weapon, you stress it beyond it's ability and it explodes!\r\n");
        act(g, "$n tries to forge a weapon, but it explodes!", false, ch, None, ActArg::None, To::Room);
        g.obj_from_anywhere(weapon);
        g.extract_obj(weapon);
        let dam = g.rng.number(10, 50);
        if let Some(c) = g.get_char_mut(ch) {
            c.points.hit -= dam;
        }
        update_pos(g, ch);
        return;
    }

    // Survived: re-rent the weapon.
    let mult = g.rng.number(2, 5);
    if let Some(o) = g.get_obj_mut(weapon) {
        o.rent += lvl << 3;
        o.rent *= mult;
    }

    g.send_to_char(ch, "You have forged new life into the weapon!\r\n");
    act(g, "$n vigorously pounds on a weapon!", false, ch, None, ActArg::None, To::Room);
}
