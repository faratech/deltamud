use crate::character::Character;
use crate::room::Room;
use crate::types::*;
use std::sync::Arc;
use parking_lot::RwLock;
use rand::Rng;

/// Signal emitted by combat when a victim drops to death in-round.
/// Game::process_combat collects these while holding a world *read* lock,
/// then processes them after releasing it (corpse creation needs write).
pub struct DeathResult {
    pub victim: Arc<RwLock<Character>>,
    pub room: Arc<RwLock<Room>>,
    pub is_npc: bool,
}

// Combat-related constants
pub const PULSE_VIOLENCE: u64 = 3;  // 3 seconds between combat rounds
pub const WEAR_WIELD: usize = 16;

// Damage types
#[derive(Debug, Clone, Copy)]
pub enum DamageType {
    Hit,
    Slash,
    Pierce,
    Bludgeon,
    Fire,
    Cold,
    Lightning,
    Acid,
    Poison,
}

// Attack types for messages
#[derive(Debug, Clone, Copy)]
pub struct AttackType {
    pub singular: &'static str,
    pub plural: &'static str,
}

pub const ATTACK_TYPES: &[AttackType] = &[
    AttackType { singular: "hit", plural: "hits" },
    AttackType { singular: "slash", plural: "slashes" },
    AttackType { singular: "pierce", plural: "pierces" },
    AttackType { singular: "pound", plural: "pounds" },
    AttackType { singular: "claw", plural: "claws" },
    AttackType { singular: "bite", plural: "bites" },
    AttackType { singular: "sting", plural: "stings" },
    AttackType { singular: "crush", plural: "crushes" },
];

pub struct Combat;

impl Combat {
    pub fn start_fighting(attacker: Arc<RwLock<Character>>, victim: Arc<RwLock<Character>>) {
        let victim_weak = Arc::downgrade(&victim);
        let attacker_weak = Arc::downgrade(&attacker);
        
        // Set attacker's target
        {
            let mut att = attacker.write();
            if att.fighting.is_none() {
                att.fighting = Some(victim_weak);
                att.position = Position::Fighting;
            }
        }
        
        // Set victim's target if not already fighting
        {
            let mut vic = victim.write();
            if vic.fighting.is_none() {
                vic.fighting = Some(attacker_weak);
                vic.position = Position::Fighting;
            }
        }
    }
    
    pub fn stop_fighting(character: &mut Character) {
        character.fighting = None;
        if character.position == Position::Fighting {
            character.position = Position::Standing;
        }
    }
    
    pub fn perform_violence(attacker: Arc<RwLock<Character>>) -> (Vec<String>, Option<DeathResult>) {
        let mut messages = Vec::new();
        let mut death: Option<DeathResult> = None;

        // Get fighting target
        let target = {
            let att = attacker.read();
            match &att.fighting {
                Some(weak) => weak.upgrade(),
                None => return (messages, death),
            }
        };

        if let Some(victim) = target {
            // Check if victim is still alive and in same room
            let can_attack = {
                let att = attacker.read();
                let vic = victim.read();

                vic.position != Position::Dead &&
                att.in_room.as_ref().and_then(|r| r.upgrade()).map(|r| r.read().number) ==
                vic.in_room.as_ref().and_then(|r| r.upgrade()).map(|r| r.read().number)
            };

            if can_attack {
                let damage_msg = Combat::hit(attacker.clone(), victim.clone());
                messages.push(damage_msg);

                // Check if victim died
                let victim_dead = victim.read().points.hit <= 0;
                if victim_dead {
                    messages.push(Combat::death_cry(victim.clone()));
                    // Capture the victim's current room BEFORE we mutate
                    // anything (Combat::die clears fighting but not in_room).
                    let room_arc = victim.read().in_room.as_ref().and_then(|w| w.upgrade());
                    let is_npc = victim.read().is_npc;
                    Combat::die(victim.clone());
                    Combat::stop_fighting(&mut attacker.write());
                    if let Some(room) = room_arc {
                        death = Some(DeathResult { victim, room, is_npc });
                    }
                }
            } else {
                Combat::stop_fighting(&mut attacker.write());
            }
        } else {
            Combat::stop_fighting(&mut attacker.write());
        }

        (messages, death)
    }
    
    pub fn hit(attacker: Arc<RwLock<Character>>, victim: Arc<RwLock<Character>>) -> String {
        let mut rng = rand::thread_rng();
        
        // Calculate hit chance
        let thac0 = Combat::calculate_thac0(&attacker.read());
        let ac = victim.read().points.armor;
        let hitroll = attacker.read().points.hitroll;
        
        let roll = rng.gen_range(1..=20);
        let needed = thac0 - ac;
        
        if roll == 1 || (roll < 20 && roll < needed - hitroll) {
            // Miss
            Combat::damage_message(&attacker.read(), &victim.read(), 0, DamageType::Hit)
        } else {
            // Hit - calculate damage
            let damage = Combat::calculate_damage(attacker.clone());
            Combat::do_damage(attacker, victim, damage, DamageType::Hit)
        }
    }
    
    fn calculate_thac0(ch: &Character) -> i16 {
        // THAC0 by class and level
        let base = match ch.player.class {
            Class::MagicUser => 20 - (ch.player.level as i16 / 3),
            Class::Cleric => 20 - (ch.player.level as i16 * 2 / 3),
            Class::Thief => 20 - (ch.player.level as i16 * 2 / 3),
            Class::Warrior => 20 - ch.player.level as i16,
            Class::Artisan => 20 - (ch.player.level as i16 * 2 / 3),
        };
        base.max(0)
    }
    
    fn calculate_damage(attacker: Arc<RwLock<Character>>) -> i32 {
        let mut rng = rand::thread_rng();
        let ch = attacker.read();
        
        // Base damage from weapon or bare hands
        let (num_dice, size_dice) = if let Some(weapon) = &ch.equipment[WEAR_WIELD] {
            let obj = weapon.read();
            obj.get_damage_dice().unwrap_or((1, 3))
        } else {
            // Bare hand damage
            (1, 2)
        };
        
        // Roll damage
        let mut damage = 0;
        for _ in 0..num_dice {
            damage += rng.gen_range(1..=size_dice);
        }
        
        // Add damroll and strength bonus
        damage += ch.points.damroll as i32;
        damage += Combat::str_damage_bonus(ch.real_abils.str);
        
        damage.max(0)
    }
    
    fn str_damage_bonus(str: i8) -> i32 {
        match str {
            0..=5 => -4,
            6..=7 => -3,
            8..=9 => -2,
            10..=11 => -1,
            12..=15 => 0,
            16 => 1,
            17 => 2,
            18 => 3,
            19..=20 => 4,
            21..=22 => 5,
            23..=24 => 6,
            _ => 7,
        }
    }
    
    pub fn do_damage(
        attacker: Arc<RwLock<Character>>, 
        victim: Arc<RwLock<Character>>, 
        damage: i32,
        damage_type: DamageType
    ) -> String {
        // Apply damage
        {
            let mut vic = victim.write();
            vic.points.hit -= damage;
            vic.points.hit = vic.points.hit.max(-10);
        }
        
        // Update position based on health
        {
            let mut vic = victim.write();
            if vic.points.hit <= -10 {
                vic.position = Position::Dead;
            } else if vic.points.hit <= -3 {
                vic.position = Position::MortalllyWounded;
            } else if vic.points.hit <= 0 {
                vic.position = Position::Incapacitated;
            }
        }
        
        Combat::damage_message(&attacker.read(), &victim.read(), damage, damage_type)
    }
    
    fn damage_message(_attacker: &Character, _victim: &Character, damage: i32, _damage_type: DamageType) -> String {
        let severity = match damage {
            0 => "miss",
            1..=3 => "scratch",
            4..=6 => "bruise",
            7..=10 => "hit",
            11..=14 => "injure",
            15..=19 => "wound",
            20..=23 => "maul",
            24..=27 => "decimate",
            28..=31 => "devastate",
            32..=35 => "maim",
            36..=39 => "MUTILATE",
            40..=43 => "DISEMBOWEL",
            44..=47 => "DISMEMBER",
            48..=52 => "MASSACRE",
            53..=99 => "PULVERIZE",
            _ => "*** ANNIHILATE ***",
        };
        
        if damage == 0 {
            format!("$n tries to hit $N but misses.")
        } else {
            format!("$n {}s $N! [{}]", severity, damage)
        }
    }
    
    fn death_cry(_victim: Arc<RwLock<Character>>) -> String {
        "You hear someone's death cry.".to_string()
    }
    
    /// Clear the victim's fighting state so no one else in-round continues
    /// targeting them. The heavier work — corpse creation, extracting NPCs,
    /// respawning PCs — runs in Game::handle_death (which holds the World
    /// write lock). See /web/deltamud/src/fight.c:387-424 for the C
    /// reference (raw_kill/die/death_cry/gain_exp). Tier-0 deliberately
    /// defers: DG death trigger, EXP penalty, blood, full death_cry to
    /// adjacent rooms. Those land with DG Scripts and Tier-2 polish.
    fn die(victim: Arc<RwLock<Character>>) {
        let mut vic = victim.write();
        vic.fighting = None;
        vic.position = Position::Dead;
    }
    
    pub fn can_kill(ch: &Character, victim: &Character) -> Result<(), String> {
        use crate::room::RoomFlags;
        if ch.id == victim.id {
            return Err("You can't attack yourself!".to_string());
        }

        if victim.is_immortal() {
            return Err("You cannot attack immortals!".to_string());
        }

        // Refuse combat in PEACEFUL rooms (immortals exempt, matching C
        // /web/deltamud/src/fight.c:843,1273).
        if !ch.is_immortal() {
            if let Some(room) = ch.in_room.as_ref().and_then(|w| w.upgrade()) {
                if room.read().room_flags.contains(RoomFlags::PEACEFUL) {
                    return Err(
                        "This room just has such a peaceful, easy feeling...".to_string(),
                    );
                }
            }
        }

        Ok(())
    }
}