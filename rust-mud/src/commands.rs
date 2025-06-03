use crate::character::Character;
use crate::world::World;
use crate::types::*;
use crate::combat::Combat;
use crate::magic::{SPELL_INFO, can_cast};
use crate::object::Object;
use std::sync::Arc;
use parking_lot::RwLock;

pub struct Commands;

impl Commands {
    // Communication commands
    pub fn do_say(ch: &Character, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Say what?".to_string());
            return messages;
        }
        
        messages.push(format!("You say, '{}'", args));
        
        // Send to room
        if let Some(room_weak) = &ch.in_room {
            if let Some(room) = room_weak.upgrade() {
                let room = room.read();
                for other_weak in &room.people {
                    if let Some(other) = other_weak.upgrade() {
                        let other = other.read();
                        if other.id != ch.id {
                            // Message would be sent to other player's connection
                        }
                    }
                }
            }
        }
        
        messages
    }
    
    pub fn do_tell(_ch: &Character, world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        let parts: Vec<&str> = args.split_whitespace().collect();
        
        if parts.len() < 2 {
            messages.push("Tell whom what?".to_string());
            return messages;
        }
        
        let target_name = parts[0];
        let message = parts[1..].join(" ");
        
        if let Some(target) = world.find_character_by_name(target_name) {
            let target = target.read();
            messages.push(format!("You tell {}, '{}'", target.get_name(), message));
            // Send to target: format!("{} tells you, '{}'", ch.get_name(), message)
        } else {
            messages.push("They aren't here.".to_string());
        }
        
        messages
    }
    
    pub fn do_shout(ch: &Character, world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Shout what?".to_string());
            return messages;
        }
        
        messages.push(format!("You shout, '{}'", args));
        
        // Send to all players in the world
        for (_, other_ch) in &world.characters {
            let other = other_ch.read();
            if other.id != ch.id && !other.is_npc {
                // Send: format!("{} shouts, '{}'", ch.get_name(), args)
            }
        }
        
        messages
    }
    
    // Information commands
    pub fn do_who(_ch: &Character, world: &World, _args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        messages.push("Players Online:".to_string());
        messages.push("--------------".to_string());
        
        let mut count = 0;
        for (_, other_ch) in &world.characters {
            let other = other_ch.read();
            if !other.is_npc {
                let level_str = if other.is_immortal() { "IMM" } else { &format!("{:3}", other.player.level) };
                messages.push(format!("[{}] {} {}", 
                    level_str,
                    other.class_abbrev(),
                    other.get_title()
                ));
                count += 1;
            }
        }
        
        messages.push(format!("\n{} player{} online.", count, if count == 1 { "" } else { "s" }));
        messages
    }
    
    pub fn do_score(ch: &Character, _world: &World, _args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        messages.push(format!("You are {} (level {}).", ch.get_title(), ch.player.level));
        messages.push(format!("Race: {:?}, Class: {:?}, Sex: {:?}", 
            ch.player.race, ch.player.class, ch.player.sex));
        messages.push(format!("Hit: {}/{}, Mana: {}/{}, Move: {}/{}",
            ch.points.hit, ch.points.max_hit,
            ch.points.mana, ch.points.max_mana,
            ch.points.move_points, ch.points.max_move
        ));
        messages.push(format!("Str: {}, Int: {}, Wis: {}, Dex: {}, Con: {}, Cha: {}",
            ch.aff_abils.str, ch.aff_abils.int, ch.aff_abils.wis,
            ch.aff_abils.dex, ch.aff_abils.con, ch.aff_abils.cha
        ));
        messages.push(format!("AC: {}, Hitroll: {}, Damroll: {}",
            ch.points.armor, ch.points.hitroll, ch.points.damroll
        ));
        messages.push(format!("Gold: {}, Experience: {}", ch.points.gold, ch.points.exp));
        
        messages
    }
    
    pub fn do_inventory(ch: &Character, _world: &World, _args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if ch.carrying.is_empty() {
            messages.push("You are carrying nothing.".to_string());
        } else {
            messages.push("You are carrying:".to_string());
            for obj in &ch.carrying {
                let obj = obj.read();
                messages.push(format!("  {}", obj.short_description));
            }
        }
        
        messages
    }
    
    pub fn do_equipment(ch: &Character, _world: &World, _args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        messages.push("You are using:".to_string());
        
        let wear_slots = [
            (WEAR_LIGHT, "<used as light>      "),
            (WEAR_FINGER_R, "<worn on finger>     "),
            (WEAR_FINGER_L, "<worn on finger>     "),
            (WEAR_NECK_1, "<worn around neck>   "),
            (WEAR_NECK_2, "<worn around neck>   "),
            (WEAR_BODY, "<worn on body>       "),
            (WEAR_HEAD, "<worn on head>       "),
            (WEAR_LEGS, "<worn on legs>       "),
            (WEAR_FEET, "<worn on feet>       "),
            (WEAR_HANDS, "<worn on hands>      "),
            (WEAR_ARMS, "<worn on arms>       "),
            (WEAR_SHIELD, "<worn as shield>     "),
            (WEAR_ABOUT, "<worn about body>    "),
            (WEAR_WAIST, "<worn about waist>   "),
            (WEAR_WRIST_R, "<worn around wrist>  "),
            (WEAR_WRIST_L, "<worn around wrist>  "),
            (WEAR_WIELD, "<wielded>            "),
            (WEAR_HOLD, "<held>               "),
            (WEAR_FLOAT, "<floating nearby>    "),
            (WEAR_FACE, "<worn on face>       "),
        ];
        
        for (pos, desc) in &wear_slots {
            if let Some(obj) = &ch.equipment[*pos] {
                let obj = obj.read();
                messages.push(format!("{}{}", desc, obj.short_description));
            }
        }
        
        messages
    }
    
    // Object manipulation
    pub fn do_get(ch: &mut Character, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Get what?".to_string());
            return messages;
        }
        
        // Find object in room
        if let Some(room_weak) = &ch.in_room {
            if let Some(room) = room_weak.upgrade() {
                let mut room = room.write();
                
                // Simple name matching (should be improved)
                let obj_index = room.contents.iter().position(|obj| {
                    obj.read().name.to_lowercase().contains(&args.to_lowercase())
                });
                
                if let Some(index) = obj_index {
                    let obj = room.contents.remove(index);
                    messages.push(format!("You get {}.", obj.read().short_description));
                    ch.carrying.push(obj);
                } else {
                    messages.push("You don't see that here.".to_string());
                }
            }
        }
        
        messages
    }
    
    pub fn do_drop(ch: &mut Character, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Drop what?".to_string());
            return messages;
        }
        
        // Find object in inventory
        let obj_index = ch.carrying.iter().position(|obj| {
            obj.read().name.to_lowercase().contains(&args.to_lowercase())
        });
        
        if let Some(index) = obj_index {
            let obj = ch.carrying.remove(index);
            messages.push(format!("You drop {}.", obj.read().short_description));
            
            // Add to room
            if let Some(room_weak) = &ch.in_room {
                if let Some(room) = room_weak.upgrade() {
                    room.write().add_object(obj);
                }
            }
        } else {
            messages.push("You don't have that.".to_string());
        }
        
        messages
    }
    
    pub fn do_wear(ch: &mut Character, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Wear what?".to_string());
            return messages;
        }
        
        // Find object in inventory
        let obj_index = ch.carrying.iter().position(|obj| {
            obj.read().name.to_lowercase().contains(&args.to_lowercase())
        });
        
        if let Some(index) = obj_index {
            let obj = ch.carrying[index].clone();
            let obj_read = obj.read();
            
            // Find appropriate wear position
            let wear_pos = Commands::find_eq_pos(ch, &obj_read);
            
            if let Some(pos) = wear_pos {
                drop(obj_read);
                
                // Remove from inventory
                ch.carrying.remove(index);
                
                // Remove old equipment if any
                if let Some(old_eq) = ch.equipment[pos].take() {
                    ch.carrying.push(old_eq);
                }
                
                // Wear new equipment
                ch.equipment[pos] = Some(obj);
                messages.push("Ok.".to_string());
            } else {
                messages.push("You can't wear that.".to_string());
            }
        } else {
            messages.push("You don't have that.".to_string());
        }
        
        messages
    }
    
    pub fn do_remove(ch: &mut Character, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Remove what?".to_string());
            return messages;
        }
        
        // Find equipped item
        let mut found = false;
        for i in 0..NUM_WEARS {
            if let Some(obj) = &ch.equipment[i] {
                if obj.read().name.to_lowercase().contains(&args.to_lowercase()) {
                    let obj = ch.equipment[i].take().unwrap();
                    messages.push(format!("You stop using {}.", obj.read().short_description));
                    ch.carrying.push(obj);
                    found = true;
                    break;
                }
            }
        }
        
        if !found {
            messages.push("You're not wearing that.".to_string());
        }
        
        messages
    }
    
    // Combat commands
    pub fn do_kill(ch: Arc<RwLock<Character>>, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if args.is_empty() {
            messages.push("Kill who?".to_string());
            return messages;
        }
        
        // Find target in room
        let ch_read = ch.read();
        if let Some(room_weak) = &ch_read.in_room {
            if let Some(room) = room_weak.upgrade() {
                let room = room.read();
                
                for person_weak in &room.people {
                    if let Some(person) = person_weak.upgrade() {
                        let person_read = person.read();
                        if person_read.id != ch_read.id && 
                           person_read.player.name.to_lowercase().contains(&args.to_lowercase()) {
                            
                            // Check if can attack
                            if let Err(msg) = Combat::can_kill(&ch_read, &person_read) {
                                messages.push(msg);
                                return messages;
                            }
                            
                            drop(person_read);
                            drop(ch_read);
                            drop(room);
                            
                            Combat::start_fighting(ch.clone(), person.clone());
                            messages.push("You attack!".to_string());
                            
                            // Perform first attack
                            let combat_msgs = Combat::perform_violence(ch);
                            messages.extend(combat_msgs);
                            
                            return messages;
                        }
                    }
                }
            }
        }
        
        messages.push("They aren't here.".to_string());
        messages
    }
    
    pub fn do_flee(ch: &mut Character, _world: &World, _args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        
        if ch.fighting.is_none() {
            messages.push("You're not fighting anyone!".to_string());
            return messages;
        }
        
        // Find a random exit
        if let Some(room_weak) = &ch.in_room {
            if let Some(room) = room_weak.upgrade() {
                let room = room.read();
                
                let mut exits = Vec::new();
                for (i, exit) in room.exits.iter().enumerate() {
                    if exit.is_some() {
                        exits.push(i);
                    }
                }
                
                if !exits.is_empty() {
                    use rand::seq::SliceRandom;
                    let mut rng = rand::thread_rng();
                    if let Some(&_dir) = exits.choose(&mut rng) {
                        Combat::stop_fighting(ch);
                        messages.push("You flee in panic!".to_string());
                        // TODO: Actually move character
                        return messages;
                    }
                }
            }
        }
        
        messages.push("PANIC! You couldn't escape!".to_string());
        messages
    }
    
    // Magic commands
    pub fn do_cast(ch: Arc<RwLock<Character>>, _world: &World, args: &str) -> Vec<String> {
        let mut messages = Vec::new();
        let parts: Vec<&str> = args.split_whitespace().collect();
        
        if parts.is_empty() {
            messages.push("Cast which spell?".to_string());
            return messages;
        }
        
        let spell_name = parts[0];
        let target_name = parts.get(1).copied();
        
        // Find spell
        let spell_num = SPELL_INFO.iter()
            .find(|(_, info)| info.name.starts_with(spell_name))
            .map(|(num, _)| *num);
        
        if let Some(spell_num) = spell_num {
            let ch_read = ch.read();
            
            // Check if can cast
            if let Err(msg) = can_cast(&ch_read, spell_num) {
                messages.push(msg);
                return messages;
            }
            
            let spell_info = &SPELL_INFO[&spell_num];
            
            // Find target if needed
            let target = if spell_info.targets.contains(crate::magic::TargetFlags::TAR_CHAR_ROOM) {
                if let Some(_name) = target_name {
                    // Find target in room
                    None // TODO: Implement target finding
                } else if spell_info.targets.contains(crate::magic::TargetFlags::TAR_SELF_ONLY) {
                    Some(ch.clone())
                } else {
                    messages.push("Cast on whom?".to_string());
                    return messages;
                }
            } else {
                None
            };
            
            drop(ch_read);
            
            // Deduct mana
            ch.write().points.mana -= spell_info.min_mana;
            
            // Cast spell
            let level = ch.read().player.level;
            let result = (spell_info.routine)(level, ch.clone(), target);
            
            messages.push(format!("You cast {}.", spell_info.name));
            if !result.is_empty() {
                messages.push(result);
            }
        } else {
            messages.push("You don't know that spell!".to_string());
        }
        
        messages
    }
    
    // Utility functions
    fn find_eq_pos(ch: &Character, obj: &Object) -> Option<usize> {
        use crate::object::WearFlags;
        
        if obj.wear_flags.contains(WearFlags::FINGER) {
            if ch.equipment[WEAR_FINGER_R].is_none() {
                return Some(WEAR_FINGER_R);
            } else if ch.equipment[WEAR_FINGER_L].is_none() {
                return Some(WEAR_FINGER_L);
            }
        }
        
        if obj.wear_flags.contains(WearFlags::NECK) {
            if ch.equipment[WEAR_NECK_1].is_none() {
                return Some(WEAR_NECK_1);
            } else if ch.equipment[WEAR_NECK_2].is_none() {
                return Some(WEAR_NECK_2);
            }
        }
        
        if obj.wear_flags.contains(WearFlags::WRIST) {
            if ch.equipment[WEAR_WRIST_R].is_none() {
                return Some(WEAR_WRIST_R);
            } else if ch.equipment[WEAR_WRIST_L].is_none() {
                return Some(WEAR_WRIST_L);
            }
        }
        
        // Single-slot positions
        let positions = [
            (WearFlags::BODY, WEAR_BODY),
            (WearFlags::HEAD, WEAR_HEAD),
            (WearFlags::LEGS, WEAR_LEGS),
            (WearFlags::FEET, WEAR_FEET),
            (WearFlags::HANDS, WEAR_HANDS),
            (WearFlags::ARMS, WEAR_ARMS),
            (WearFlags::SHIELD, WEAR_SHIELD),
            (WearFlags::ABOUT, WEAR_ABOUT),
            (WearFlags::WAIST, WEAR_WAIST),
            (WearFlags::WIELD, WEAR_WIELD),
            (WearFlags::HOLD, WEAR_HOLD),
            (WearFlags::FACE, WEAR_FACE),
        ];
        
        for (flag, pos) in &positions {
            if obj.wear_flags.contains(*flag) && ch.equipment[*pos].is_none() {
                return Some(*pos);
            }
        }
        
        None
    }
}

impl Character {
    pub fn class_abbrev(&self) -> &'static str {
        match self.player.class {
            Class::MagicUser => "Mag",
            Class::Cleric => "Cle",
            Class::Thief => "Thi",
            Class::Warrior => "War",
            Class::Artisan => "Art",
        }
    }
}