use crate::character::{Character, Affect};
use crate::types::*;
use crate::combat::{Combat, DamageType};
use std::sync::Arc;
use parking_lot::RwLock;
use std::collections::HashMap;
use lazy_static::lazy_static;
use rand::Rng;

// Spell numbers
pub const SPELL_ARMOR: i32 = 1;
pub const SPELL_TELEPORT: i32 = 2;
pub const SPELL_BLESS: i32 = 3;
pub const SPELL_BLINDNESS: i32 = 4;
pub const SPELL_BURNING_HANDS: i32 = 5;
pub const SPELL_CHARM: i32 = 7;
pub const SPELL_CHILL_TOUCH: i32 = 8;
pub const SPELL_COLOR_SPRAY: i32 = 10;
pub const SPELL_CREATE_FOOD: i32 = 12;
pub const SPELL_CREATE_WATER: i32 = 13;
pub const SPELL_CURE_BLIND: i32 = 14;
pub const SPELL_CURE_CRITIC: i32 = 15;
pub const SPELL_CURE_LIGHT: i32 = 16;
pub const SPELL_CURSE: i32 = 17;
pub const SPELL_DETECT_INVIS: i32 = 19;
pub const SPELL_DETECT_MAGIC: i32 = 20;
pub const SPELL_EARTHQUAKE: i32 = 23;
pub const SPELL_FIREBALL: i32 = 26;
pub const SPELL_HARM: i32 = 27;
pub const SPELL_HEAL: i32 = 28;
pub const SPELL_INVISIBILITY: i32 = 29;
pub const SPELL_LIGHTNING_BOLT: i32 = 30;
pub const SPELL_MAGIC_MISSILE: i32 = 32;
pub const SPELL_POISON: i32 = 33;
pub const SPELL_SANCTUARY: i32 = 36;
pub const SPELL_SLEEP: i32 = 38;
pub const SPELL_STRENGTH: i32 = 39;
pub const SPELL_WORD_OF_RECALL: i32 = 42;
pub const SPELL_IDENTIFY: i32 = 53;

// Skill numbers
pub const SKILL_BACKSTAB: i32 = 131;
pub const SKILL_BASH: i32 = 132;
pub const SKILL_HIDE: i32 = 133;
pub const SKILL_KICK: i32 = 134;
pub const SKILL_PICK_LOCK: i32 = 135;
pub const SKILL_RESCUE: i32 = 137;
pub const SKILL_SNEAK: i32 = 138;
pub const SKILL_STEAL: i32 = 139;
pub const SKILL_TRACK: i32 = 140;
pub const SKILL_DISARM: i32 = 141;

// Affect locations
pub const APPLY_NONE: i32 = 0;
pub const APPLY_STR: i32 = 1;
pub const APPLY_DEX: i32 = 2;
pub const APPLY_INT: i32 = 3;
pub const APPLY_WIS: i32 = 4;
pub const APPLY_CON: i32 = 5;
pub const APPLY_CHA: i32 = 6;
pub const APPLY_AGE: i32 = 8;
pub const APPLY_WEIGHT: i32 = 10;
pub const APPLY_HEIGHT: i32 = 11;
pub const APPLY_MANA: i32 = 12;
pub const APPLY_HIT: i32 = 13;
pub const APPLY_MOVE: i32 = 14;
pub const APPLY_AC: i32 = 17;
pub const APPLY_HITROLL: i32 = 18;
pub const APPLY_DAMROLL: i32 = 19;

// Affect flags
pub const AFF_BLIND: i64 = 1 << 0;
pub const AFF_INVISIBLE: i64 = 1 << 1;
pub const AFF_DETECT_EVIL: i64 = 1 << 2;
pub const AFF_DETECT_INVIS: i64 = 1 << 3;
pub const AFF_DETECT_MAGIC: i64 = 1 << 4;
pub const AFF_SANCTUARY: i64 = 1 << 7;
pub const AFF_POISON: i64 = 1 << 10;
pub const AFF_SLEEP: i64 = 1 << 12;
pub const AFF_SNEAK: i64 = 1 << 15;
pub const AFF_HIDE: i64 = 1 << 16;
pub const AFF_CHARM: i64 = 1 << 18;

// Spell info structure
#[derive(Clone)]
pub struct SpellInfo {
    pub name: &'static str,
    pub min_position: Position,
    pub min_mana: i32,
    pub targets: TargetFlags,
    pub violent: bool,
    pub routine: SpellFunction,
    pub wear_off_msg: &'static str,
}

bitflags::bitflags! {
    #[derive(Clone, Copy)]
    pub struct TargetFlags: u32 {
        const TAR_CHAR_ROOM = 1 << 0;
        const TAR_CHAR_WORLD = 1 << 1;
        const TAR_FIGHT_SELF = 1 << 2;
        const TAR_FIGHT_VICT = 1 << 3;
        const TAR_SELF_ONLY = 1 << 4;
        const TAR_NOT_SELF = 1 << 5;
        const TAR_OBJ_INV = 1 << 6;
        const TAR_OBJ_ROOM = 1 << 7;
        const TAR_IGNORE = 1 << 8;
    }
}

pub type SpellFunction = fn(level: Level, ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String;

lazy_static! {
    pub static ref SPELL_INFO: HashMap<i32, SpellInfo> = {
        let mut m = HashMap::new();
        
        m.insert(SPELL_ARMOR, SpellInfo {
            name: "armor",
            min_position: Position::Fighting,
            min_mana: 5,
            targets: TargetFlags::TAR_CHAR_ROOM,
            violent: false,
            routine: spell_armor,
            wear_off_msg: "You feel less protected.",
        });
        
        m.insert(SPELL_BLESS, SpellInfo {
            name: "bless",
            min_position: Position::Standing,
            min_mana: 5,
            targets: TargetFlags::TAR_CHAR_ROOM | TargetFlags::TAR_OBJ_INV,
            violent: false,
            routine: spell_bless,
            wear_off_msg: "You feel less blessed.",
        });
        
        m.insert(SPELL_CURE_LIGHT, SpellInfo {
            name: "cure light",
            min_position: Position::Fighting,
            min_mana: 5,
            targets: TargetFlags::TAR_CHAR_ROOM,
            violent: false,
            routine: spell_cure_light,
            wear_off_msg: "",
        });
        
        m.insert(SPELL_MAGIC_MISSILE, SpellInfo {
            name: "magic missile",
            min_position: Position::Fighting,
            min_mana: 10,
            targets: TargetFlags::TAR_CHAR_ROOM | TargetFlags::TAR_FIGHT_VICT,
            violent: true,
            routine: spell_magic_missile,
            wear_off_msg: "",
        });
        
        m.insert(SPELL_INVISIBILITY, SpellInfo {
            name: "invisibility",
            min_position: Position::Standing,
            min_mana: 10,
            targets: TargetFlags::TAR_CHAR_ROOM | TargetFlags::TAR_OBJ_INV,
            violent: false,
            routine: spell_invisibility,
            wear_off_msg: "You feel yourself exposed.",
        });
        
        m.insert(SPELL_SANCTUARY, SpellInfo {
            name: "sanctuary",
            min_position: Position::Standing,
            min_mana: 30,
            targets: TargetFlags::TAR_CHAR_ROOM,
            violent: false,
            routine: spell_sanctuary,
            wear_off_msg: "The white aura around your body fades.",
        });
        
        m.insert(SPELL_HEAL, SpellInfo {
            name: "heal",
            min_position: Position::Fighting,
            min_mana: 40,
            targets: TargetFlags::TAR_CHAR_ROOM,
            violent: false,
            routine: spell_heal,
            wear_off_msg: "",
        });
        
        m.insert(SPELL_FIREBALL, SpellInfo {
            name: "fireball",
            min_position: Position::Fighting,
            min_mana: 20,
            targets: TargetFlags::TAR_CHAR_ROOM | TargetFlags::TAR_FIGHT_VICT,
            violent: true,
            routine: spell_fireball,
            wear_off_msg: "",
        });
        
        m
    };
}

// Spell implementations
fn spell_armor(_level: Level, _ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut vic = victim.write();
        
        // Check if already affected
        if vic.affect_flags & AFF_SANCTUARY != 0 {
            return "Nothing seems to happen.".to_string();
        }
        
        let affect = Affect {
            spell_type: SPELL_ARMOR,
            duration: 24,
            modifier: -20,
            location: APPLY_AC,
            bitvector: 0,
        };
        
        vic.affected.push(affect);
        vic.points.armor -= 20;
        
        "$N is surrounded by a magical armor.".to_string()
    } else {
        "".to_string()
    }
}

fn spell_bless(_level: Level, _ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut vic = victim.write();
        
        let affect = Affect {
            spell_type: SPELL_BLESS,
            duration: 6,
            modifier: 2,
            location: APPLY_HITROLL,
            bitvector: 0,
        };
        
        vic.affected.push(affect);
        vic.points.hitroll += 2;
        
        "$N glows with divine blessing.".to_string()
    } else {
        "".to_string()
    }
}

fn spell_cure_light(level: Level, _ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut rng = rand::thread_rng();
        let heal = rng.gen_range(1..=8) + (level as i32 / 4);
        
        let mut vic = victim.write();
        vic.points.hit = (vic.points.hit + heal).min(vic.points.max_hit);
        
        "You feel better!".to_string()
    } else {
        "".to_string()
    }
}

fn spell_heal(level: Level, _ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let heal = 100 + level as i32 * 3;
        
        let mut vic = victim.write();
        vic.points.hit = (vic.points.hit + heal).min(vic.points.max_hit);
        
        "A warm feeling floods your body!".to_string()
    } else {
        "".to_string()
    }
}

fn spell_magic_missile(level: Level, ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut rng = rand::thread_rng();
        let missiles = 1 + (level as i32 - 1) / 5;
        let mut damage = 0;
        
        for _ in 0..missiles {
            damage += rng.gen_range(1..=6) + 1;
        }
        
        Combat::do_damage(ch, victim, damage, DamageType::Fire)
    } else {
        "".to_string()
    }
}

fn spell_fireball(level: Level, ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut rng = rand::thread_rng();
        let damage = rng.gen_range(1..=6) * level as i32;
        
        Combat::do_damage(ch, victim, damage, DamageType::Fire)
    } else {
        "".to_string()
    }
}

fn spell_invisibility(_level: Level, _ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut vic = victim.write();
        
        if vic.affect_flags & AFF_INVISIBLE != 0 {
            return "Nothing seems to happen.".to_string();
        }
        
        let affect = Affect {
            spell_type: SPELL_INVISIBILITY,
            duration: 12 + (vic.player.level / 4) as i32,
            modifier: -40,
            location: APPLY_AC,
            bitvector: AFF_INVISIBLE,
        };
        
        vic.affected.push(affect);
        vic.affect_flags |= AFF_INVISIBLE;
        vic.points.armor -= 40;
        
        "$N slowly fades out of existence.".to_string()
    } else {
        "".to_string()
    }
}

fn spell_sanctuary(_level: Level, _ch: Arc<RwLock<Character>>, victim: Option<Arc<RwLock<Character>>>) -> String {
    if let Some(victim) = victim {
        let mut vic = victim.write();
        
        if vic.affect_flags & AFF_SANCTUARY != 0 {
            return "Nothing seems to happen.".to_string();
        }
        
        let affect = Affect {
            spell_type: SPELL_SANCTUARY,
            duration: 4,
            modifier: 0,
            location: APPLY_NONE,
            bitvector: AFF_SANCTUARY,
        };
        
        vic.affected.push(affect);
        vic.affect_flags |= AFF_SANCTUARY;
        
        "$N is surrounded by a white aura.".to_string()
    } else {
        "".to_string()
    }
}

// Magic utility functions
pub fn affect_update(ch: &mut Character) {
    let mut to_remove = Vec::new();
    
    for (i, affect) in ch.affected.iter_mut().enumerate() {
        affect.duration -= 1;
        if affect.duration <= 0 {
            to_remove.push(i);
        }
    }
    
    // Remove expired affects
    for &i in to_remove.iter().rev() {
        let affect = ch.affected.remove(i);
        
        // Remove affect modifications
        match affect.location {
            APPLY_STR => ch.aff_abils.str -= affect.modifier as i8,
            APPLY_DEX => ch.aff_abils.dex -= affect.modifier as i8,
            APPLY_INT => ch.aff_abils.int -= affect.modifier as i8,
            APPLY_WIS => ch.aff_abils.wis -= affect.modifier as i8,
            APPLY_CON => ch.aff_abils.con -= affect.modifier as i8,
            APPLY_CHA => ch.aff_abils.cha -= affect.modifier as i8,
            APPLY_HIT => ch.points.max_hit -= affect.modifier,
            APPLY_MANA => ch.points.max_mana -= affect.modifier,
            APPLY_MOVE => ch.points.max_move -= affect.modifier,
            APPLY_AC => ch.points.armor -= affect.modifier as i16,
            APPLY_HITROLL => ch.points.hitroll -= affect.modifier as i16,
            APPLY_DAMROLL => ch.points.damroll -= affect.modifier as i16,
            _ => {}
        }
        
        // Remove bitvector flags
        ch.affect_flags &= !affect.bitvector;
    }
}

pub fn can_cast(ch: &Character, spell_num: i32) -> Result<(), String> {
    let spell = SPELL_INFO.get(&spell_num)
        .ok_or_else(|| "That is not a spell!".to_string())?;
    
    if ch.points.mana < spell.min_mana {
        return Err("You don't have enough mana!".to_string());
    }
    
    if ch.position < spell.min_position {
        return Err("You can't cast this spell in your current position!".to_string());
    }
    
    // Check if character knows the spell
    // TODO: Implement spell learning system
    
    Ok(())
}