// spell_parser.rs — the top-level magic entry points (CircleMUD
// spell_parser.c), ported 1:1. This module owns:
//   * the SPELL_*/SKILL_* numeric constants (spells.h),
//   * the spell_info[] table (mana cost, position, targets, violent, routines)
//     built by the spello()/skillo() macros in mag_assign_spells,
//   * the spells[] name table + find_skill_num/skill_name/get_spell_name,
//   * say_spell (the garbled spoken-words broadcast),
//   * mag_manacost,
//   * and the full do_cast pipeline (cast_spell + do_cast).
//
// magic.rs and spells.rs import the SPELL_*/MAG_*/TAR_* constants and the
// SpellFn dispatch from here. The whole casting core is one coherent author:
// the routine dispatch lives in magic::call_magic, which this file calls at
// the end of cast_spell.

use crate::act::{act, ActArg, To};
use crate::interpreter::{any_one_arg, is_abbrev, one_argument};
use crate::state::GameState;
use crate::types::*;

// ===========================================================================
// Magic routine bits (spells.h MAG_*).
// ===========================================================================
pub const MAG_DAMAGE: i32 = 1 << 0;
pub const MAG_AFFECTS: i32 = 1 << 1;
pub const MAG_UNAFFECTS: i32 = 1 << 2;
pub const MAG_POINTS: i32 = 1 << 3;
pub const MAG_ALTER_OBJS: i32 = 1 << 4;
pub const MAG_GROUPS: i32 = 1 << 5;
pub const MAG_MASSES: i32 = 1 << 6;
pub const MAG_AREAS: i32 = 1 << 7;
pub const MAG_SUMMONS: i32 = 1 << 8;
pub const MAG_CREATIONS: i32 = 1 << 9;
pub const MAG_MANUAL: i32 = 1 << 10;

// Default item levels (spells.h).
pub const DEFAULT_STAFF_LVL: i32 = 12;
pub const DEFAULT_WAND_LVL: i32 = 12;

pub const TYPE_UNDEFINED: i32 = -1;

// ===========================================================================
// Target bits (spells.h TAR_*).
// ===========================================================================
pub const TAR_IGNORE: i32 = 1;
pub const TAR_CHAR_ROOM: i32 = 2;
pub const TAR_CHAR_WORLD: i32 = 4;
pub const TAR_FIGHT_SELF: i32 = 8;
pub const TAR_FIGHT_VICT: i32 = 16;
pub const TAR_SELF_ONLY: i32 = 32;
pub const TAR_NOT_SELF: i32 = 64;
pub const TAR_OBJ_INV: i32 = 128;
pub const TAR_OBJ_ROOM: i32 = 256;
pub const TAR_OBJ_WORLD: i32 = 512;
pub const TAR_OBJ_EQUIP: i32 = 1024;

// ===========================================================================
// Position numeric values (structs.h POS_*); used as min_position gates.
// ===========================================================================
pub const POS_SLEEPING: i32 = 4;
pub const POS_RESTING: i32 = 5;
pub const POS_SITTING: i32 = 7;
pub const POS_FIGHTING: i32 = 8;
pub const POS_STANDING: i32 = 9;

// ===========================================================================
// Spell numbers (spells.h). DO NOT CHANGE — they index the playerfile.
// ===========================================================================
pub const SPELL_RESERVED_DBC: i32 = 0;

pub const SPELL_ARMOR: i32 = 1;
pub const SPELL_TELEPORT: i32 = 2;
pub const SPELL_BLESS: i32 = 3;
pub const SPELL_BLINDNESS: i32 = 4;
pub const SPELL_CHARM: i32 = 5;
pub const SPELL_CLONE: i32 = 6;
pub const SPELL_CONTROL_WEATHER: i32 = 7;
pub const SPELL_CREATE_FOOD: i32 = 8;
pub const SPELL_CREATE_WATER: i32 = 9;
pub const SPELL_CURE_BLIND: i32 = 10;
pub const SPELL_CURE_CRITIC: i32 = 11;
pub const SPELL_CURE_LIGHT: i32 = 12;
pub const SPELL_CURSE: i32 = 13;
pub const SPELL_DETECT_ALIGN: i32 = 14;
pub const SPELL_DETECT_INVIS: i32 = 15;
pub const SPELL_DETECT_MAGIC: i32 = 16;
pub const SPELL_DETECT_POISON: i32 = 17;
pub const SPELL_EARTHQUAKE: i32 = 18;
pub const SPELL_ENCHANT_WEAPON: i32 = 19;
pub const SPELL_HEAL: i32 = 20;
pub const SPELL_INVISIBLE: i32 = 21;
pub const SPELL_LOCATE_OBJECT: i32 = 22;
pub const SPELL_POISON: i32 = 23;
pub const SPELL_REMOVE_CURSE: i32 = 24;
pub const SPELL_SANCTUARY: i32 = 25;
pub const SPELL_SLEEP: i32 = 26;
pub const SPELL_STRENGTH: i32 = 27;
pub const SPELL_SUMMON: i32 = 28;
pub const SPELL_WORD_OF_RECALL: i32 = 29;
pub const SPELL_REMOVE_POISON: i32 = 30;
pub const SPELL_SENSE_LIFE: i32 = 31;
pub const SPELL_ANIMATE_DEAD: i32 = 32;
pub const SPELL_GROUP_ARMOR: i32 = 33;
pub const SPELL_GROUP_HEAL: i32 = 34;
pub const SPELL_GROUP_RECALL: i32 = 35;
pub const SPELL_INFRAVISION: i32 = 36;
pub const SPELL_WATERWALK: i32 = 37;
pub const SPELL_STONE_SKIN: i32 = 38;
pub const SPELL_FEAR: i32 = 39;
pub const SPELL_RECHARGE: i32 = 40;
pub const SPELL_PORTAL: i32 = 41;
pub const SPELL_GROUP_STONE_SKIN: i32 = 42;
pub const SPELL_LOCATE_TARGET: i32 = 43;
pub const SPELL_CONVERGENCE: i32 = 44;
pub const SPELL_AUTUS: i32 = 45;
pub const SPELL_RESIST_PORTAL: i32 = 46;
pub const SPELL_REGEN_MANA: i32 = 47;
pub const SPELL_HOME: i32 = 48;
pub const SPELL_WORD_OF_RETREAT: i32 = 49;
pub const SKILL_CHAIN_FOOTING: i32 = 50;
pub const SPELL_REDIRECT_CHARGE: i32 = 51;

pub const MAX_SPELLS: i32 = 500;

// Player skills (spells.h).
pub const SKILL_BACKSTAB: i32 = 501;
pub const SKILL_BASH: i32 = 502;
pub const SKILL_HIDE: i32 = 503;
pub const SKILL_KICK: i32 = 504;
pub const SKILL_PICK_LOCK: i32 = 505;
pub const SKILL_PUNCH: i32 = 506;
pub const SKILL_RESCUE: i32 = 507;
pub const SKILL_SNEAK: i32 = 508;
pub const SKILL_STEAL: i32 = 509;
pub const SKILL_TRACK: i32 = 510;
pub const SKILL_FORAGE: i32 = 511;
pub const SKILL_SCAN: i32 = 512;
pub const SKILL_BREW: i32 = 513;
pub const SKILL_FORGE: i32 = 514;
pub const SKILL_SCRIBE: i32 = 515;
pub const SKILL_SPEED: i32 = 516;
pub const SKILL_BERSERK: i32 = 517;
pub const SKILL_CAMOUFLAGE: i32 = 518;
pub const SKILL_BLANKET: i32 = 519;
pub const SKILL_RAM_DOOR: i32 = 520;
pub const SKILL_MOUNT: i32 = 521;
pub const SKILL_RIDING: i32 = 522;
pub const SKILL_TAME: i32 = 523;
pub const SKILL_SECOND_ATTACK: i32 = 524;
pub const SKILL_THIRD_ATTACK: i32 = 525;
pub const SKILL_LISTEN: i32 = 526;
pub const SKILL_MEDITATE: i32 = 527;
pub const SKILL_REPAIR: i32 = 528;
pub const SKILL_TAN: i32 = 529;
pub const SKILL_FILLET: i32 = 530;
pub const SKILL_CARVE: i32 = 531;
pub const SKILL_DODGE: i32 = 532;
pub const SKILL_PARRY: i32 = 533;
pub const SKILL_AVOID: i32 = 534;
pub const SKILL_RIPOSTE: i32 = 535;
pub const SKILL_CIRCLE: i32 = 536;
pub const SKILL_TRIP: i32 = 537;
pub const SKILL_DISARM: i32 = 538;
pub const SKILL_TARGET: i32 = 539;
pub const SKILL_ADRENALINE: i32 = 540;
pub const SKILL_BLOODLUST: i32 = 541;
pub const SKILL_CARNALRAGE: i32 = 542;

// Object/NPC spells (spells.h).
pub const SPELL_IDENTIFY: i32 = 1001;
pub const SPELL_FIRE_BREATH: i32 = 1002;
pub const SPELL_GAS_BREATH: i32 = 1003;
pub const SPELL_FROST_BREATH: i32 = 1004;
pub const SPELL_ACID_BREATH: i32 = 1005;
pub const SPELL_LIGHTNING_BREATH: i32 = 1006;

pub const TOP_SPELL_DEFINE: i32 = 1099;

// NUM_CLASSES (structs.h): MagicUser/Cleric/Thief/Warrior/Artisan.
pub const NUM_CLASSES: usize = 5;

// AFF_AUTUS (structs.h 1<<20) — halves mana in mag_manacost. Not in the shared
// flags.rs subset; transcribed here to match the C value exactly.
const AFF_AUTUS: i64 = 1 << 20;

// ===========================================================================
// spell_info[] — built once by mag_assign_spells (lazily via OnceLock).
// ===========================================================================
#[derive(Clone, Copy)]
pub struct SpellInfoType {
    pub min_position: i32,
    pub mana_min: i32,
    pub mana_max: i32,
    pub mana_change: i32,
    pub min_level: [i32; NUM_CLASSES],
    pub routines: i32,
    pub violent: bool,
    pub targets: i32,
}

impl SpellInfoType {
    const fn unused() -> Self {
        // unused_spell(): min_level = LVL_IMPL + 1 for every class.
        SpellInfoType {
            min_position: 0,
            mana_min: 0,
            mana_max: 0,
            mana_change: 0,
            min_level: [LVL_IMPL as i32 + 1; NUM_CLASSES],
            routines: 0,
            violent: false,
            targets: 0,
        }
    }
}

use std::sync::OnceLock;
static SPELL_INFO: OnceLock<Vec<SpellInfoType>> = OnceLock::new();

/// Access spell_info[spellnum]; out-of-range yields the unused() sentinel.
pub fn spell_info(spellnum: i32) -> SpellInfoType {
    let table = SPELL_INFO.get_or_init(build_spell_info);
    if spellnum < 0 || spellnum as usize >= table.len() {
        SpellInfoType::unused()
    } else {
        table[spellnum as usize]
    }
}

/// spello() — fill one spell_info entry (min_level defaults to LVL_IMMORT for
/// every class, exactly as spell_parser.c::spello does before class.c assigns).
fn spello(
    table: &mut [SpellInfoType],
    spl: i32,
    max_mana: i32,
    min_mana: i32,
    mana_change: i32,
    minpos: i32,
    targets: i32,
    violent: bool,
    routines: i32,
) {
    let e = &mut table[spl as usize];
    e.min_level = [LVL_IMMORT as i32; NUM_CLASSES];
    e.mana_max = max_mana;
    e.mana_min = min_mana;
    e.mana_change = mana_change;
    e.min_position = minpos;
    e.targets = targets;
    e.violent = violent;
    e.routines = routines;
}

/// skillo(skill) — spello(skill, 0,0,0,0,0,0,0): just makes immortals able.
fn skillo(table: &mut [SpellInfoType], skill: i32) {
    spello(table, skill, 0, 0, 0, 0, 0, false, 0);
}

fn spell_level(table: &mut [SpellInfoType], spell: i32, class: usize, level: i32) {
    if spell >= 0 && (spell as usize) < table.len() && class < NUM_CLASSES {
        table[spell as usize].min_level[class] = level;
    }
}

/// init_spell_levels() (class.c): assign the mortal class/level table after
/// mag_assign_spells has initialized every castable spell to LVL_IMMORT.
fn init_spell_levels(table: &mut [SpellInfoType]) {
    const MAG: usize = 0;
    const CLE: usize = 1;
    const THE: usize = 2;
    const WAR: usize = 3;
    const ART: usize = 4;

    for &(spell, class, level) in &[
        // Magic users.
        (SKILL_MOUNT, MAG, 2),
        (SKILL_RIDING, MAG, 3),
        (SPELL_DETECT_INVIS, MAG, 4),
        (SPELL_DETECT_MAGIC, MAG, 5),
        (SPELL_INFRAVISION, MAG, 8),
        (SPELL_INVISIBLE, MAG, 9),
        (SPELL_ARMOR, MAG, 10),
        (SPELL_LOCATE_OBJECT, MAG, 13),
        (SPELL_STRENGTH, MAG, 15),
        (SKILL_MEDITATE, MAG, 17),
        (SPELL_SLEEP, MAG, 19),
        (SKILL_TAME, MAG, 21),
        (SPELL_BLINDNESS, MAG, 23),
        (SPELL_DETECT_POISON, MAG, 24),
        (SPELL_CURSE, MAG, 27),
        (SPELL_POISON, MAG, 29),
        (SPELL_FEAR, MAG, 29),
        (SPELL_CLONE, MAG, 31),
        (SPELL_CHARM, MAG, 33),
        (SPELL_RECHARGE, MAG, 33),
        (SPELL_SENSE_LIFE, MAG, 34),
        (SPELL_ENCHANT_WEAPON, MAG, 35),
        (SPELL_PORTAL, MAG, 35),
        (SPELL_STONE_SKIN, MAG, 37),
        (SPELL_GROUP_STONE_SKIN, MAG, 40),
        (SKILL_DODGE, MAG, 42),
        (SPELL_REDIRECT_CHARGE, MAG, 60),
        (SPELL_AUTUS, MAG, 75),
        (SPELL_CONVERGENCE, MAG, 80),
        (SPELL_RESIST_PORTAL, MAG, 85),
        (SPELL_HOME, MAG, 90),
        (SPELL_WATERWALK, MAG, 95),
        // Clerics.
        (SPELL_CURE_LIGHT, CLE, 1),
        (SPELL_ARMOR, CLE, 3),
        (SKILL_MOUNT, CLE, 5),
        (SKILL_RIDING, CLE, 7),
        (SPELL_CREATE_FOOD, CLE, 9),
        (SPELL_CREATE_WATER, CLE, 11),
        (SPELL_DETECT_POISON, CLE, 13),
        (SPELL_DETECT_ALIGN, CLE, 15),
        (SPELL_CURE_BLIND, CLE, 17),
        (SPELL_BLESS, CLE, 19),
        (SPELL_SENSE_LIFE, CLE, 21),
        (SPELL_DETECT_INVIS, CLE, 23),
        (SKILL_MEDITATE, CLE, 25),
        (SPELL_BLINDNESS, CLE, 27),
        (SPELL_INFRAVISION, CLE, 29),
        (SKILL_TAME, CLE, 33),
        (SPELL_POISON, CLE, 35),
        (SPELL_GROUP_ARMOR, CLE, 37),
        (SPELL_CURE_CRITIC, CLE, 39),
        (SPELL_SUMMON, CLE, 41),
        (SPELL_TELEPORT, MAG, 46),
        (SPELL_REMOVE_POISON, CLE, 43),
        (SPELL_WORD_OF_RECALL, CLE, 45),
        (SPELL_EARTHQUAKE, CLE, 47),
        (SPELL_SANCTUARY, CLE, 53),
        (SPELL_HEAL, CLE, 57),
        (SPELL_CONTROL_WEATHER, CLE, 59),
        (SPELL_GROUP_HEAL, CLE, 63),
        (SKILL_DODGE, CLE, 65),
        (SPELL_REMOVE_CURSE, CLE, 67),
        (SPELL_ANIMATE_DEAD, CLE, 69),
        (SPELL_STONE_SKIN, CLE, 71),
        (SPELL_GROUP_STONE_SKIN, CLE, 73),
        (SPELL_PORTAL, CLE, 77),
        (SPELL_RECHARGE, CLE, 79),
        (SPELL_LOCATE_TARGET, CLE, 85),
        (SPELL_REGEN_MANA, CLE, 91),
        (SPELL_WATERWALK, CLE, 93),
        (SPELL_WORD_OF_RETREAT, CLE, 95),
        // Thieves.
        (SKILL_SNEAK, THE, 1),
        (SKILL_MOUNT, THE, 3),
        (SKILL_RIDING, THE, 5),
        (SKILL_PICK_LOCK, THE, 7),
        (SKILL_BACKSTAB, THE, 9),
        (SKILL_STEAL, THE, 11),
        (SKILL_HIDE, THE, 13),
        (SKILL_TRACK, THE, 15),
        (SKILL_TAME, THE, 17),
        (SKILL_SECOND_ATTACK, THE, 19),
        (SKILL_LISTEN, THE, 21),
        (SKILL_FORAGE, THE, 23),
        (SKILL_SCAN, THE, 25),
        (SKILL_CAMOUFLAGE, THE, 27),
        (SKILL_BLANKET, THE, 29),
        (SKILL_CIRCLE, THE, 31),
        (SKILL_TRIP, THE, 33),
        (SKILL_DISARM, THE, 35),
        (SKILL_TARGET, THE, 37),
        (SKILL_AVOID, THE, 39),
        // Warriors.
        (SKILL_KICK, WAR, 1),
        (SKILL_MOUNT, WAR, 3),
        (SKILL_RIDING, WAR, 5),
        (SKILL_RESCUE, WAR, 7),
        (SKILL_TAME, WAR, 9),
        (SKILL_TRACK, WAR, 11),
        (SKILL_SECOND_ATTACK, WAR, 13),
        (SKILL_BASH, WAR, 15),
        (SKILL_FORAGE, WAR, 17),
        (SKILL_SCAN, WAR, 19),
        (SKILL_ADRENALINE, WAR, 20),
        (SKILL_THIRD_ATTACK, WAR, 21),
        (SKILL_RIPOSTE, WAR, 23),
        (SKILL_SPEED, WAR, 25),
        (SKILL_BERSERK, WAR, 27),
        (SKILL_RAM_DOOR, WAR, 29),
        (SKILL_PARRY, WAR, 31),
        (SKILL_CHAIN_FOOTING, WAR, 50),
        (SKILL_BLOODLUST, WAR, 60),
        (SKILL_CARNALRAGE, WAR, 75),
        // Artisans.
        (SKILL_MOUNT, ART, 1),
        (SKILL_RIDING, ART, 3),
        (SKILL_SCRIBE, ART, 5),
        (SKILL_TAME, ART, 7),
        (SKILL_BREW, ART, 11),
        (SKILL_REPAIR, ART, 21),
        (SKILL_TAN, ART, 25),
        (SKILL_FILLET, ART, 31),
        (SKILL_CARVE, ART, 33),
        (SKILL_FORGE, ART, 35),
        (SKILL_DODGE, ART, 37),
    ] {
        spell_level(table, spell, class, level);
    }
}

/// mag_assign_spells (spell_parser.c) — the full spello/skillo table.
fn build_spell_info() -> Vec<SpellInfoType> {
    let mut t = vec![SpellInfoType::unused(); (TOP_SPELL_DEFINE + 1) as usize];

    spello(
        &mut t,
        SPELL_ANIMATE_DEAD,
        175,
        150,
        3,
        POS_STANDING,
        TAR_OBJ_ROOM,
        false,
        MAG_SUMMONS,
    );
    spello(
        &mut t,
        SPELL_ARMOR,
        30,
        15,
        3,
        POS_FIGHTING,
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_BLESS,
        35,
        5,
        3,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_OBJ_INV,
        false,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    );
    spello(
        &mut t,
        SPELL_BLINDNESS,
        35,
        25,
        1,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_NOT_SELF,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_CHARM,
        75,
        50,
        2,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_NOT_SELF,
        true,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_CLONE,
        200,
        150,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_SUMMONS,
    );
    spello(
        &mut t,
        SPELL_CONTROL_WEATHER,
        75,
        25,
        5,
        POS_STANDING,
        TAR_IGNORE,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_CREATE_FOOD,
        30,
        5,
        4,
        POS_STANDING,
        TAR_IGNORE,
        false,
        MAG_CREATIONS,
    );
    spello(
        &mut t,
        SPELL_CREATE_WATER,
        30,
        5,
        4,
        POS_STANDING,
        TAR_OBJ_INV | TAR_OBJ_EQUIP,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_CURE_BLIND,
        30,
        5,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM,
        false,
        MAG_UNAFFECTS,
    );
    spello(
        &mut t,
        SPELL_CURE_CRITIC,
        30,
        10,
        2,
        POS_FIGHTING,
        TAR_CHAR_ROOM,
        false,
        MAG_POINTS,
    );
    spello(
        &mut t,
        SPELL_CURE_LIGHT,
        30,
        10,
        2,
        POS_FIGHTING,
        TAR_CHAR_ROOM,
        false,
        MAG_POINTS,
    );
    spello(
        &mut t,
        SPELL_CURSE,
        80,
        50,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_OBJ_INV,
        true,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    );
    spello(
        &mut t,
        SPELL_DETECT_ALIGN,
        20,
        10,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_DETECT_INVIS,
        20,
        10,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_DETECT_MAGIC,
        20,
        10,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_DETECT_POISON,
        15,
        5,
        1,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_EARTHQUAKE,
        40,
        25,
        3,
        POS_FIGHTING,
        TAR_IGNORE,
        true,
        MAG_AREAS,
    );
    spello(
        &mut t,
        SPELL_ENCHANT_WEAPON,
        150,
        100,
        10,
        POS_STANDING,
        TAR_OBJ_INV | TAR_OBJ_EQUIP,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_GROUP_ARMOR,
        50,
        30,
        2,
        POS_STANDING,
        TAR_IGNORE,
        false,
        MAG_GROUPS,
    );
    spello(
        &mut t,
        SPELL_GROUP_HEAL,
        80,
        60,
        5,
        POS_STANDING,
        TAR_IGNORE,
        false,
        MAG_GROUPS,
    );
    spello(
        &mut t,
        SPELL_HEAL,
        60,
        40,
        3,
        POS_FIGHTING,
        TAR_CHAR_ROOM,
        false,
        MAG_POINTS | MAG_AFFECTS | MAG_UNAFFECTS,
    );
    spello(
        &mut t,
        SPELL_INFRAVISION,
        25,
        10,
        1,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_INVISIBLE,
        35,
        25,
        1,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    );
    spello(
        &mut t,
        SPELL_LOCATE_OBJECT,
        25,
        20,
        1,
        POS_STANDING,
        TAR_OBJ_WORLD,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_LOCATE_TARGET,
        100,
        20,
        1,
        POS_STANDING,
        TAR_CHAR_WORLD,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_POISON,
        50,
        20,
        3,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_NOT_SELF | TAR_OBJ_INV,
        true,
        MAG_AFFECTS | MAG_ALTER_OBJS,
    );
    spello(
        &mut t,
        SPELL_REMOVE_CURSE,
        45,
        25,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_OBJ_INV,
        false,
        MAG_UNAFFECTS | MAG_ALTER_OBJS,
    );
    spello(
        &mut t,
        SPELL_SANCTUARY,
        110,
        85,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_SLEEP,
        40,
        25,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM,
        true,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_STRENGTH,
        35,
        30,
        1,
        POS_STANDING,
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_SUMMON,
        75,
        50,
        3,
        POS_STANDING,
        TAR_CHAR_WORLD | TAR_NOT_SELF,
        false,
        MAG_MANUAL,
    );
    // Finish-the-game activation: SPELL_TELEPORT and SPELL_GROUP_RECALL had
    // handlers (spells.c:173 / magic.c:559) but were never spello()'d in C,
    // leaving them uncastable (registered in COMPATIBILITY.md).
    // Teleport: virtuous escape — random destination within the real rooms
    // (map cells excluded by the handler).
    spello(
        &mut t,
        SPELL_TELEPORT,
        75,
        25,
        2,
        POS_STANDING,
        TAR_SELF_ONLY,
        false,
        MAG_SUMMONS,
    );
    // Group recall: pulls the caster's whole group back to the recall point.
    spello(
        &mut t,
        SPELL_GROUP_RECALL,
        80,
        40,
        2,
        POS_STANDING,
        TAR_SELF_ONLY,
        false,
        MAG_GROUPS,
    );
    spello(
        &mut t,
        SPELL_WORD_OF_RECALL,
        20,
        10,
        2,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_REMOVE_POISON,
        40,
        8,
        4,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_UNAFFECTS | MAG_ALTER_OBJS,
    );
    spello(
        &mut t,
        SPELL_SENSE_LIFE,
        20,
        10,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_STONE_SKIN,
        120,
        60,
        3,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_FEAR,
        100,
        50,
        1,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_NOT_SELF,
        true,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_RECHARGE,
        150,
        75,
        1,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_NOT_SELF | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_PORTAL,
        170,
        100,
        5,
        POS_STANDING,
        TAR_CHAR_WORLD | TAR_NOT_SELF,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_GROUP_STONE_SKIN,
        240,
        120,
        2,
        POS_STANDING,
        TAR_IGNORE,
        false,
        MAG_GROUPS,
    );
    spello(
        &mut t,
        SPELL_CONVERGENCE,
        110,
        85,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_AUTUS,
        140,
        100,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_RESIST_PORTAL,
        200,
        100,
        2,
        POS_STANDING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_REGEN_MANA,
        150,
        150,
        0,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_NOT_SELF,
        false,
        MAG_POINTS,
    );
    spello(
        &mut t,
        SPELL_HOME,
        80,
        20,
        1,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_WORD_OF_RETREAT,
        30,
        20,
        1,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_MANUAL,
    );
    spello(
        &mut t,
        SPELL_WATERWALK,
        200,
        150,
        1,
        POS_FIGHTING,
        TAR_CHAR_ROOM | TAR_SELF_ONLY,
        false,
        MAG_AFFECTS,
    );
    spello(
        &mut t,
        SPELL_REDIRECT_CHARGE,
        200,
        100,
        5,
        POS_STANDING,
        TAR_CHAR_ROOM,
        false,
        MAG_AFFECTS,
    );

    // Non-castable spells.
    spello(
        &mut t,
        SPELL_IDENTIFY,
        0,
        0,
        0,
        0,
        TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM,
        false,
        MAG_MANUAL,
    );

    // Skills — just make immortals able by default.
    for &sk in &[
        SKILL_BACKSTAB,
        SKILL_BASH,
        SKILL_HIDE,
        SKILL_KICK,
        SKILL_BERSERK,
        SKILL_PICK_LOCK,
        SKILL_RAM_DOOR,
        SKILL_PUNCH,
        SKILL_RESCUE,
        SKILL_SNEAK,
        SKILL_CAMOUFLAGE,
        SKILL_BLANKET,
        SKILL_STEAL,
        SKILL_TRACK,
        SKILL_FORAGE,
        SKILL_SCAN,
        SKILL_BREW,
        SKILL_FORGE,
        SKILL_SCRIBE,
        SKILL_SPEED,
        SKILL_MOUNT,
        SKILL_RIDING,
        SKILL_TAME,
        SKILL_SECOND_ATTACK,
        SKILL_THIRD_ATTACK,
        SKILL_LISTEN,
        SKILL_MEDITATE,
        SKILL_REPAIR,
        SKILL_TAN,
        SKILL_FILLET,
        SKILL_CARVE,
        SKILL_DODGE,
        SKILL_PARRY,
        SKILL_AVOID,
        SKILL_RIPOSTE,
        SKILL_CIRCLE,
        SKILL_TRIP,
        SKILL_TARGET,
        SKILL_DISARM,
        SKILL_CHAIN_FOOTING,
        SKILL_ADRENALINE,
        SKILL_BLOODLUST,
        SKILL_CARNALRAGE,
    ] {
        skillo(&mut t, sk);
    }

    init_spell_levels(&mut t);

    t
}

// ===========================================================================
// spells[] name table (spell_parser.c). Index == spell/skill number; entries
// not listed are "!UNUSED!"; index 0 is "!RESERVED!".
// ===========================================================================
const SPELL_NAMES: &[(i32, &str)] = &[
    (SPELL_ARMOR, "armor"),
    (SPELL_TELEPORT, "teleport"),
    (SPELL_BLESS, "bless"),
    (SPELL_BLINDNESS, "blindness"),
    (SPELL_CHARM, "charm person"),
    (SPELL_CLONE, "clone"),
    (SPELL_CONTROL_WEATHER, "control weather"),
    (SPELL_CREATE_FOOD, "create food"),
    (SPELL_CREATE_WATER, "create water"),
    (SPELL_CURE_BLIND, "cure blind"),
    (SPELL_CURE_CRITIC, "cure critic"),
    (SPELL_CURE_LIGHT, "cure light"),
    (SPELL_CURSE, "curse"),
    (SPELL_DETECT_ALIGN, "detect alignment"),
    (SPELL_DETECT_INVIS, "detect invisibility"),
    (SPELL_DETECT_MAGIC, "detect magic"),
    (SPELL_DETECT_POISON, "detect poison"),
    (SPELL_EARTHQUAKE, "earthquake"),
    (SPELL_ENCHANT_WEAPON, "enchant weapon"),
    (SPELL_HEAL, "heal"),
    (SPELL_INVISIBLE, "invisibility"),
    (SPELL_LOCATE_OBJECT, "locate object"),
    (SPELL_POISON, "poison"),
    (SPELL_REMOVE_CURSE, "remove curse"),
    (SPELL_SANCTUARY, "sanctuary"),
    (SPELL_SLEEP, "sleep"),
    (SPELL_STRENGTH, "strength"),
    (SPELL_SUMMON, "summon"),
    (SPELL_WORD_OF_RECALL, "word of recall"),
    (SPELL_REMOVE_POISON, "remove poison"),
    (SPELL_SENSE_LIFE, "sense life"),
    (SPELL_ANIMATE_DEAD, "animate dead"),
    (SPELL_GROUP_ARMOR, "group armor"),
    (SPELL_GROUP_HEAL, "group heal"),
    (SPELL_GROUP_RECALL, "group recall"),
    (SPELL_INFRAVISION, "infravision"),
    (SPELL_WATERWALK, "waterwalk"),
    (SPELL_STONE_SKIN, "stone skin"),
    (SPELL_FEAR, "fear"),
    (SPELL_RECHARGE, "recharge"),
    (SPELL_PORTAL, "portal"),
    (SPELL_GROUP_STONE_SKIN, "group stone skin"),
    (SPELL_LOCATE_TARGET, "locate life"),
    (SPELL_CONVERGENCE, "convergence of power"),
    (SPELL_AUTUS, "mana autus"),
    (SPELL_RESIST_PORTAL, "resist portal"),
    (SPELL_REGEN_MANA, "regen mana"),
    (SPELL_HOME, "home"),
    (SPELL_WORD_OF_RETREAT, "word of retreat"),
    (SKILL_CHAIN_FOOTING, "chain footing"),
    (SPELL_REDIRECT_CHARGE, "redirect charge"),
    (SKILL_BACKSTAB, "backstab"),
    (SKILL_BASH, "bash"),
    (SKILL_HIDE, "hide"),
    (SKILL_KICK, "kick"),
    (SKILL_PICK_LOCK, "pick lock"),
    (SKILL_PUNCH, "punch"),
    (SKILL_RESCUE, "rescue"),
    (SKILL_SNEAK, "sneak"),
    (SKILL_STEAL, "steal"),
    (SKILL_TRACK, "track"),
    (SKILL_FORAGE, "forage"),
    (SKILL_SCAN, "scan"),
    (SKILL_BREW, "brew"),
    (SKILL_FORGE, "forge"),
    (SKILL_SCRIBE, "scribe"),
    (SKILL_SPEED, "speed"),
    (SKILL_BERSERK, "berserk"),
    (SKILL_CAMOUFLAGE, "camouflage"),
    (SKILL_BLANKET, "blanket"),
    (SKILL_RAM_DOOR, "ram"),
    (SKILL_MOUNT, "mount"),
    (SKILL_RIDING, "riding"),
    (SKILL_TAME, "tame"),
    (SKILL_SECOND_ATTACK, "second attack"),
    (SKILL_THIRD_ATTACK, "third attack"),
    (SKILL_LISTEN, "listen"),
    (SKILL_MEDITATE, "meditate"),
    (SKILL_REPAIR, "repair"),
    (SKILL_TAN, "tan"),
    (SKILL_FILLET, "fillet"),
    (SKILL_CARVE, "carve"),
    (SKILL_DODGE, "dodge"),
    (SKILL_PARRY, "parry"),
    (SKILL_AVOID, "avoid"),
    (SKILL_RIPOSTE, "riposte"),
    (SKILL_CIRCLE, "circle"),
    (SKILL_TRIP, "trip"),
    (SKILL_DISARM, "disarm"),
    (SKILL_TARGET, "target"),
    (SKILL_ADRENALINE, "adrenaline"),
    (SKILL_BLOODLUST, "bloodlust"),
    (SKILL_CARNALRAGE, "carnal rage"),
    (SPELL_IDENTIFY, "identify"),
    (SPELL_FIRE_BREATH, "fire breath"),
    (SPELL_GAS_BREATH, "gas breath"),
    (SPELL_FROST_BREATH, "frost breath"),
    (SPELL_ACID_BREATH, "acid breath"),
    (SPELL_LIGHTNING_BREATH, "lightning breath"),
];

// Highest populated index in spells[] before the "\n" terminator (1006).
const TOP_SPELL_INDEX: i32 = SPELL_LIGHTNING_BREATH;

/// skill_name(num) (spell_parser.c): the player-facing name of a spell/skill,
/// "UNUSED"/"UNDEFINED" for the reserved/out-of-range cases, "!UNUSED!" filler
/// for an in-range gap.
pub fn skill_name(num: i32) -> &'static str {
    if num <= 0 {
        return if num == -1 { "UNUSED" } else { "UNDEFINED" };
    }
    if num > TOP_SPELL_INDEX {
        return "UNDEFINED";
    }
    for &(n, name) in SPELL_NAMES {
        if n == num {
            return name;
        }
    }
    "!UNUSED!"
}

/// get_spell_name(spellnum): the raw spells[] entry, used by say_spell garble.
fn raw_spell_name(spellnum: i32) -> &'static str {
    if spellnum == 0 {
        return "!RESERVED!";
    }
    skill_name(spellnum)
}

/// find_skill_num(name) (spell_parser.c): abbreviation / multi-word match of
/// `name` against spells[]. Returns the spell/skill index, or -1.
pub fn find_skill_num(name: &str) -> i32 {
    // C spell_parser.c:956: 'cast \'\'' reaches find_skill_num with an empty
    // name whose is_abbrev("", entry) matches the FIRST entry (armor), then
    // fails the GET_SKILL check with 'You are unfamiliar with that spell.'
    // The early bail made Rust print 'Cast what?!?' instead (#256).
    let name = name.trim();
    let mut index = 1;
    while index <= TOP_SPELL_INDEX {
        let entry = raw_spell_name(index);
        // is_abbrev(name, spells[index]) — whole-string prefix match.
        if is_abbrev(name, entry) {
            return index;
        }
        // Multi-word: every word of name is_abbrev of the corresponding word of
        // spells[index], and name supplies no extra words.
        let mut ent = entry;
        let mut nam = name;
        let mut ok = true;
        loop {
            let (first, ent_rest) = any_one_arg(ent);
            let (first2, nam_rest) = any_one_arg(nam);
            if first.is_empty() || first2.is_empty() || !ok {
                break;
            }
            if !is_abbrev(&first2, &first) {
                ok = false;
            }
            ent = ent_rest;
            nam = nam_rest;
        }
        // after the loop, first2 (remaining name word) must be empty.
        let (rem_name, _) = any_one_arg(nam);
        if ok && rem_name.is_empty() {
            return index;
        }
        index += 1;
    }
    -1
}

// ===========================================================================
// syls[] garble table (spell_parser.c) — used by say_spell.
// ===========================================================================
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
];

/// Garble a spell name for hearers of a different class (spell_parser.c).
fn garble_spell(spellnum: i32) -> String {
    let lbuf = raw_spell_name(spellnum).as_bytes();
    let mut out = String::new();
    let mut ofs = 0usize;
    while ofs < lbuf.len() {
        let mut matched = false;
        for &(org, new) in SYLS {
            let ob = org.as_bytes();
            if ofs + ob.len() <= lbuf.len() && &lbuf[ofs..ofs + ob.len()] == ob {
                out.push_str(new);
                ofs += ob.len();
                matched = true;
                break;
            }
        }
        if !matched {
            ofs += 1;
        }
    }
    out
}

/// mag_manacost(ch, spellnum) (spell_parser.c).
pub fn mag_manacost(g: &GameState, ch: CharId, spellnum: i32) -> i32 {
    let si = spell_info(spellnum);
    let (level, class, autus) = match g.get_char(ch) {
        Some(c) => (
            c.player.level as i32,
            c.player.class as usize,
            c.affect_flags & AFF_AUTUS != 0,
        ),
        None => return si.mana_min,
    };
    let class = class.min(NUM_CLASSES - 1);
    let mut mana = (si.mana_max - si.mana_change * (level - si.min_level[class])).max(si.mana_min);
    if autus {
        mana >>= 1;
    }
    mana
}

/// say_spell(ch, spellnum, tch, tobj) (spell_parser.c): broadcast the spoken
/// words to the room — garbled for hearers of a different class.
pub fn say_spell(
    g: &mut GameState,
    ch: CharId,
    spellnum: i32,
    tch: Option<CharId>,
    tobj: Option<ObjId>,
) {
    let real = raw_spell_name(spellnum).to_string();
    let garbled = garble_spell(spellnum);

    let ch_room = g.get_char(ch).and_then(|c| c.in_room);
    let tch_same_room =
        tch.and_then(|t| g.get_char(t).and_then(|c| c.in_room)) == ch_room && tch.is_some();
    let tobj_here = tobj
        .map(|o| {
            let here = g.get_obj(o).map(|x| match x.loc {
                crate::object::ObjLoc::Room(r) => Some(r) == ch_room,
                crate::object::ObjLoc::Carried(c) => c == ch,
                _ => false,
            });
            here.unwrap_or(false)
        })
        .unwrap_or(false);

    // The template, with %s for the spell words.
    let template = if tch_same_room {
        if tch == Some(ch) {
            "$n closes $s eyes and utters the words, '{}'."
        } else {
            "$n stares at $N and utters the words, '{}'."
        }
    } else if tobj_here {
        "$n stares at $p and utters the words, '{}'."
    } else {
        "$n utters the words, '{}'."
    };

    let buf1 = template.replace("{}", &real); // same-class hearers
    let buf2 = template.replace("{}", &garbled); // other-class hearers

    // Per-recipient render: same class hears the real words, otherwise garble.
    let ch_class = g
        .get_char(ch)
        .map(|c| c.player.class)
        .unwrap_or(Class::Warrior);
    let people = ch_room
        .map(|r| g.room(r).people.clone())
        .unwrap_or_default();
    let vict_arg = match tch {
        Some(v) => ActArg::Char(v),
        None => ActArg::None,
    };
    for i in people {
        if i == ch || Some(i) == tch {
            continue;
        }
        // i must have a connection and be awake (act() handles awake/desc, but
        // the per-class branch needs to mirror the C `if (... || !AWAKE) continue`).
        let awake = g
            .get_char(i)
            .map(|c| c.desc.is_some() && c.position > Position::Sleeping)
            .unwrap_or(false);
        if !awake {
            continue;
        }
        let same_class = g.get_char(i).map(|c| c.player.class) == Some(ch_class);
        let msg = if same_class { &buf1 } else { &buf2 };
        // act() To::Char renders for `i` as the speaker-position viewer; we want
        // i as recipient of an $n-message about ch. Use a one-off render by
        // sending To a temporary: emulate perform_act(buf, ch, tobj, tch, i)
        // by sending only to i via a room-scoped act excluding everyone else.
        render_to(g, msg, ch, tobj, &vict_arg, i);
    }

    // To the target (if a different char in the room): "stares at you".
    if let Some(t) = tch {
        if t != ch && tch_same_room {
            let t_same_class = g.get_char(t).map(|c| c.player.class) == Some(ch_class);
            let words = if t_same_class { &real } else { &garbled };
            let buf = format!("$n stares at you and utters the words, '{}'.", words);
            act(g, &buf, false, ch, None, ActArg::Char(t), To::Vict);
        }
    }
}

/// perform_act for a single explicit viewer (CircleMUD perform_act). act() in
/// this port only exposes To::Char/Vict/Room/NotVict; say_spell needs to render
/// a $n-message to an arbitrary bystander with per-viewer substitution, so we
/// re-implement the minimal substitution inline and send directly.
fn render_to(
    g: &mut GameState,
    msg: &str,
    ch: CharId,
    obj: Option<ObjId>,
    vict: &ActArg,
    viewer: CharId,
) {
    let text = perform_act_str(g, msg, ch, obj, vict, viewer);
    g.send_to_char(viewer, &text);
}

/// Minimal $-code substitution mirroring act.rs::perform_act for one viewer.
fn perform_act_str(
    g: &GameState,
    orig: &str,
    ch: CharId,
    obj: Option<ObjId>,
    vict: &ActArg,
    viewer: CharId,
) -> String {
    let mut out = String::with_capacity(orig.len() + 16);
    let mut chars = orig.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let code = match chars.next() {
            Some(x) => x,
            None => break,
        };
        let repl: String = match code {
            'n' => pers(g, viewer, ch),
            'N' => match vict {
                ActArg::Char(v) => pers(g, viewer, *v),
                _ => String::new(),
            },
            's' => hshr(g, ch).to_string(),
            'S' => match vict {
                ActArg::Char(v) => hshr(g, *v).to_string(),
                _ => String::new(),
            },
            'o' | 'p' => match obj {
                Some(o) => obj_disp(g, o, code == 'o'),
                None => String::new(),
            },
            '$' => "$".to_string(),
            _ => String::new(),
        };
        out.push_str(&repl);
    }
    if let Some(f) = out.chars().next() {
        if f.is_ascii_lowercase() {
            let up = f.to_ascii_uppercase();
            out.replace_range(0..f.len_utf8(), &up.to_string());
        }
    }
    out.push_str("\r\n");
    out
}

fn pers(g: &GameState, viewer: CharId, cid: CharId) -> String {
    if g.can_see(viewer, cid) {
        if let Some(c) = g.get_char(cid) {
            return if c.is_npc {
                c.short_desc
                    .clone()
                    .unwrap_or_else(|| c.player.name.clone())
            } else {
                c.player.name.clone()
            };
        }
    }
    "someone".to_string()
}

fn hshr(g: &GameState, cid: CharId) -> &'static str {
    match g.get_char(cid).map(|c| c.player.sex) {
        Some(Gender::Male) => "his",
        Some(Gender::Female) => "her",
        _ => "its",
    }
}

fn obj_disp(g: &GameState, oid: ObjId, fname_only: bool) -> String {
    match g.get_obj(oid) {
        Some(o) => {
            if fname_only {
                o.name
                    .split_whitespace()
                    .next()
                    .unwrap_or("something")
                    .to_string()
            } else {
                o.short_description.clone()
            }
        }
        None => "something".to_string(),
    }
}

// ===========================================================================
// World-wide finders (handler.c get_char_vis / get_obj_vis). The id-indexed
// GameState exposes only room-scoped finders, so the casting core supplies its
// own world scan over char_list / obj_list (matching C newest-first order).
// ===========================================================================
pub fn get_char_world_vis(g: &GameState, observer: CharId, arg: &str) -> Option<CharId> {
    // First try the room (C get_char_vis checks the local room first).
    if let Some(c) = g.get_char_room_vis(observer, arg) {
        return Some(c);
    }
    let (mut count, name) = crate::handler::get_number(arg);
    if count == 0 {
        return None;
    }
    for cid in g.char_ids() {
        let ch = match g.get_char(cid) {
            Some(c) => c,
            None => continue,
        };
        if crate::handler::isname(&name, &ch.player.name) && g.can_see(observer, cid) {
            count -= 1;
            if count == 0 {
                return Some(cid);
            }
        }
    }
    None
}

pub fn get_obj_world_vis(g: &GameState, _observer: CharId, arg: &str) -> Option<ObjId> {
    let (mut count, name) = crate::handler::get_number(arg);
    if count == 0 {
        return None;
    }
    for oid in g.obj_ids() {
        let obj = match g.get_obj(oid) {
            Some(o) => o,
            None => continue,
        };
        if crate::handler::isname(&name, &obj.name) {
            count -= 1;
            if count == 0 {
                return Some(oid);
            }
        }
    }
    None
}

// ===========================================================================
// cast_spell / do_cast (spell_parser.c).
// ===========================================================================

const OK: &str = "Ok.\r\n";

/// cast_spell(ch, tch, tobj, spellnum) (spell_parser.c): checks restrictions,
/// prints the words, calls call_magic. Returns call_magic's result.
pub fn cast_spell(
    g: &mut GameState,
    ch: CharId,
    tch: Option<CharId>,
    tobj: Option<ObjId>,
    spellnum: i32,
) -> i32 {
    if !(0..=TOP_SPELL_DEFINE).contains(&spellnum) {
        return 0;
    }
    let si = spell_info(spellnum);

    let pos = g
        .get_char(ch)
        .map(|c| c.position)
        .unwrap_or(Position::Standing);
    if (pos as i32) < si.min_position {
        let msg = match pos {
            Position::Sleeping => "You dream about great magical powers.\r\n",
            Position::Resting => "You cannot concentrate while resting.\r\n",
            Position::Sitting => "You can't do this sitting!\r\n",
            Position::Fighting => "Impossible!  You can't concentrate enough!\r\n",
            _ => "You can't do much of anything like this!\r\n",
        };
        g.send_to_char(ch, msg);
        return 0;
    }

    // Charmed casters won't hurt their master.
    let charmed_master = g
        .get_char(ch)
        .map(|c| c.affect_flags & AFF_CHARM != 0 && c.master == tch && tch.is_some())
        .unwrap_or(false);
    if charmed_master {
        g.send_to_char(ch, "You are afraid you might hurt your master!\r\n");
        return 0;
    }
    if tch != Some(ch) && si.targets & TAR_SELF_ONLY != 0 {
        g.send_to_char(ch, "You can only cast this spell upon yourself!\r\n");
        return 0;
    }
    if tch == Some(ch) && si.targets & TAR_NOT_SELF != 0 {
        g.send_to_char(ch, "You cannot cast this spell upon yourself!\r\n");
        return 0;
    }
    if si.routines & MAG_GROUPS != 0
        && g.get_char(ch)
            .map(|c| c.affect_flags & AFF_GROUP == 0)
            .unwrap_or(true)
    {
        g.send_to_char(
            ch,
            "You can't cast this spell if you're not in a group!\r\n",
        );
        return 0;
    }
    g.send_to_char(ch, OK);
    say_spell(g, ch, spellnum, tch, tobj);

    let level = g.get_char(ch).map(|c| c.player.level as i32).unwrap_or(1);
    crate::magic::call_magic(g, ch, tch, tobj, spellnum, level)
}

const AFF_GROUP: i64 = 1 << 8;
const AFF_CHARM: i64 = 1 << 21;

/// do_cast (spell_parser.c): the PC casting entry point.
pub fn do_cast(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    if g.get_char(ch).map(|c| c.is_npc).unwrap_or(false) {
        return;
    }

    // strtok over single quotes, faithful to C do_cast. CircleMUD's
    // `argument` keeps the leading space the interpreter left after the command
    // word, so strtok's FIRST token is that space (or the pre-quote text) and
    // the SECOND token is the spell name. Emulated here:
    //   s1 = first '-delimited token (text before the 1st quote, may be empty);
    //   s2 = second '-delimited token (the spell name);
    //   t  = the rest after the 2nd quote.
    let arg_t = arg.trim_start();
    if arg_t.is_empty() {
        // strtok(argument,"'") == NULL  ->  "Cast what where?"
        g.send_to_char(ch, "Cast what where?\r\n");
        return;
    }
    // Split on single quotes into the strtok token stream.
    let mut tokens = arg.split('\'');
    let _pre = tokens.next(); // text before the first quote (strtok s1)
    let s = match tokens.next() {
        // s2 == NULL  ->  no closing context, "must be enclosed".
        None => {
            g.send_to_char(
                ch,
                "Spell names must be enclosed in the Holy Magic Symbols: '\r\n",
            );
            return;
        }
        Some(name) => name,
    };
    // t = strtok(NULL,"\0"): everything after the 2nd quote.
    let target_str: &str = match arg.find('\'') {
        Some(q1) => match arg[q1 + 1..].find('\'') {
            Some(q2) => arg[q1 + 1 + q2 + 1..].trim_start(),
            None => "",
        },
        None => "",
    };

    let spellnum = find_skill_num(s);
    if spellnum < 1 || spellnum > MAX_SPELLS {
        g.send_to_char(ch, "Cast what?!?\r\n");
        return;
    }
    let si = spell_info(spellnum);

    // Class min-level gate.
    let (level, class) = g
        .get_char(ch)
        .map(|c| (c.player.level as i32, c.player.class as usize))
        .unwrap_or((1, 0));
    let class = class.min(NUM_CLASSES - 1);
    if level < si.min_level[class] {
        g.send_to_char(ch, "You do not know that spell!\r\n");
        return;
    }
    if g.get_char(ch)
        .map(|c| c.skill(spellnum as u16))
        .unwrap_or(0)
        == 0
    {
        g.send_to_char(ch, "You are unfamiliar with that spell.\r\n");
        return;
    }

    // Resolve the target. `t` is one_argument(target_str) skip_spaces'd.
    let (t_arg, _) = one_argument(target_str);
    let t = t_arg.trim();

    let mut tch: Option<CharId> = None;
    let mut tobj: Option<ObjId> = None;
    let mut target = false;

    if si.targets & TAR_IGNORE != 0 {
        target = true;
    } else if !t.is_empty() {
        if !target && si.targets & TAR_CHAR_ROOM != 0 {
            if let Some(v) = g.get_char_room_vis(ch, t) {
                tch = Some(v);
                target = true;
            }
        }
        if !target && si.targets & TAR_CHAR_WORLD != 0 {
            if let Some(v) = get_char_world_vis(g, ch, t) {
                tch = Some(v);
                target = true;
            }
        }
        if !target && si.targets & TAR_OBJ_INV != 0 {
            let inv = g
                .get_char(ch)
                .map(|c| c.carrying.clone())
                .unwrap_or_default();
            if let Some(o) = g.get_obj_in_list_vis(ch, t, &inv) {
                tobj = Some(o);
                target = true;
            }
        }
        if !target && si.targets & TAR_OBJ_EQUIP != 0 {
            let eq: Vec<ObjId> = g
                .get_char(ch)
                .map(|c| c.equipment.iter().flatten().copied().collect())
                .unwrap_or_default();
            if let Some(o) = g.get_obj_in_list_vis(ch, t, &eq) {
                tobj = Some(o);
                target = true;
            }
        }
        if !target && si.targets & TAR_OBJ_ROOM != 0 {
            let contents = g
                .get_char(ch)
                .and_then(|c| c.in_room)
                .map(|r| g.room(r).contents.clone())
                .unwrap_or_default();
            if let Some(o) = g.get_obj_in_list_vis(ch, t, &contents) {
                tobj = Some(o);
                target = true;
            }
        }
        if !target && si.targets & TAR_OBJ_WORLD != 0 {
            if let Some(o) = get_obj_world_vis(g, ch, t) {
                tobj = Some(o);
                target = true;
            }
        }
    } else {
        // Empty target string.
        let fighting = g.get_char(ch).and_then(|c| c.fighting);
        if !target && si.targets & TAR_FIGHT_SELF != 0 && fighting.is_some() {
            tch = Some(ch);
            target = true;
        }
        if !target && si.targets & TAR_FIGHT_VICT != 0 {
            if let Some(v) = fighting {
                tch = Some(v);
                target = true;
            }
        }
        if !target && si.targets & TAR_CHAR_ROOM != 0 && !si.violent {
            tch = Some(ch);
            target = true;
        }
        if !target {
            let which = if si.targets & (TAR_OBJ_ROOM | TAR_OBJ_INV | TAR_OBJ_WORLD) != 0 {
                "what"
            } else {
                "who"
            };
            g.send_to_char(ch, &format!("Upon {} should the spell be cast?\r\n", which));
            return;
        }
    }

    if target && tch == Some(ch) && si.violent {
        g.send_to_char(
            ch,
            "You shouldn't cast that on yourself -- could be bad for your health!\r\n",
        );
        return;
    }
    if !target {
        g.send_to_char(ch, "Cannot find the target of your spell!\r\n");
        return;
    }

    let mana = mag_manacost(g, ch, spellnum);
    let (cur_mana, is_imm) = g
        .get_char(ch)
        .map(|c| (c.points.mana, c.player.level >= LVL_IMMORT))
        .unwrap_or((0, false));
    if mana > 0 && cur_mana < mana && !is_imm {
        g.send_to_char(ch, "You haven't the energy to cast that spell!\r\n");
        return;
    }

    // Throw the dice — 101 is total failure.
    let skill = g
        .get_char(ch)
        .map(|c| c.skill(spellnum as u16) as i32)
        .unwrap_or(0);
    if g.rng.number(0, 101) > skill {
        // Lost concentration.
        g.set_wait_state(ch, PULSE_VIOLENCE as i32);
        g.send_to_char(ch, "You lost your concentration!\r\n");
        if mana > 0 {
            if let Some(c) = g.get_char_mut(ch) {
                let max = c.points.max_mana;
                c.points.mana = (c.points.mana - (mana >> 1)).clamp(0, max);
            }
        }
        if si.violent {
            if let Some(v) = tch {
                if g.get_char(v).map(|c| c.is_npc).unwrap_or(false) {
                    crate::combat::hit(g, v, ch);
                }
            }
        }
    } else if cast_spell(g, ch, tch, tobj, spellnum) != 0 {
        g.set_wait_state(ch, PULSE_VIOLENCE as i32);
        if mana > 0 {
            if let Some(c) = g.get_char_mut(ch) {
                let max = c.points.max_mana;
                c.points.mana = (c.points.mana - mana).clamp(0, max);
            }
        }
    }
}

/// do_skillset (modify.c): immortal command to set a player's skill/spell
/// proficiency. Syntax: skillset <name> '<skill>' <value>.
pub fn do_skillset(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (name, rest) = crate::interpreter::one_argument(arg);
    if name.is_empty() {
        g.send_to_char(ch, "Syntax: skillset <name> '<skill>' <value>\r\n");
        return;
    }
    let vict = match g.find_player_by_name(&name) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "No one by that name is online.\r\n");
            return;
        }
    };
    // Skill name is the text between the first pair of single quotes.
    let skillname = match rest.find('\'').and_then(|q1| {
        rest[q1 + 1..]
            .find('\'')
            .map(|q2| (&rest[q1 + 1..q1 + 1 + q2], &rest[q1 + 1 + q2 + 1..]))
    }) {
        Some((sk, _)) => sk.to_string(),
        None => {
            g.send_to_char(ch, "Skill name must be enclosed in: '\r\n");
            return;
        }
    };
    let after = rest.rsplit('\'').next().unwrap_or("");
    let value: i32 = after
        .trim()
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1);
    let spellnum = find_skill_num(&skillname);
    if spellnum <= 0 {
        g.send_to_char(ch, "Unrecognized skill.\r\n");
        return;
    }
    if !(0..=100).contains(&value) {
        g.send_to_char(ch, "Learned value must be 0-100.\r\n");
        return;
    }
    if let Some(c) = g.get_char_mut(vict) {
        c.set_skill(spellnum as u16, value as u8);
    }
    g.send_to_char(
        ch,
        &format!(
            "You change {}'s {} to {}.\r\n",
            name,
            skill_name(spellnum),
            value
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spell_levels_are_assigned_for_mortal_spellcasters() {
        let armor = spell_info(SPELL_ARMOR);
        assert_eq!(armor.min_level[Class::MagicUser as usize], 10);
        assert_eq!(armor.min_level[Class::Cleric as usize], 3);

        let cure_light = spell_info(SPELL_CURE_LIGHT);
        assert_eq!(cure_light.min_level[Class::Cleric as usize], 1);
        assert_eq!(
            cure_light.min_level[Class::MagicUser as usize],
            LVL_IMMORT as i32
        );
    }

    #[test]
    fn skill_levels_are_assigned_for_mortal_classes() {
        let kick = spell_info(SKILL_KICK);
        assert_eq!(kick.min_level[Class::Warrior as usize], 1);

        let sneak = spell_info(SKILL_SNEAK);
        assert_eq!(sneak.min_level[Class::Thief as usize], 1);

        let scribe = spell_info(SKILL_SCRIBE);
        assert_eq!(scribe.min_level[Class::Artisan as usize], 5);
    }
}
