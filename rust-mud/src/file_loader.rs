use crate::world::{World, Zone, MobileProto, ObjectProto};
use crate::room::{Room, Exit, RoomFlags};
use crate::object::{WearFlags, ExtraFlags};
use crate::types::*;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use anyhow::Result;
use log::info;

pub struct FileLoader;

impl FileLoader {
    pub async fn load_world(world: &mut World, base_path: &str) -> Result<()> {
        let world_path = Path::new(base_path).join("world");
        
        // Load zones
        FileLoader::load_zones(world, &world_path.join("zon"))?;
        
        // Load rooms
        FileLoader::load_rooms(world, &world_path.join("wld"))?;
        
        // Load mobiles
        FileLoader::load_mobiles(world, &world_path.join("mob"))?;
        
        // Load objects
        FileLoader::load_objects(world, &world_path.join("obj"))?;
        
        info!("World loaded: {} zones, {} rooms, {} mobs, {} objects",
            world.zones.len(),
            world.rooms.len(),
            world.mob_protos.len(),
            world.obj_protos.len()
        );
        
        Ok(())
    }
    
    fn load_zones(world: &mut World, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        
        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }
            
            let zone_file = path.join(&line);
            FileLoader::load_zone_file(world, &zone_file)?;
        }
        
        Ok(())
    }
    
    fn load_zone_file(world: &mut World, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        
        while reader.read_line(&mut line)? > 0 {
            if line.starts_with('#') {
                let zone_num: i32 = line[1..].trim().parse()?;
                
                // Read zone name
                line.clear();
                reader.read_line(&mut line)?;
                let name = line.trim_end_matches('~').to_string();
                
                // Read zone data
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let top = parts.get(0).unwrap_or(&"0").parse()?;
                let lifespan = parts.get(1).unwrap_or(&"30").parse()?;
                let reset_mode = parts.get(2).unwrap_or(&"2").parse()?;
                
                let zone = Zone {
                    number: zone_num,
                    name,
                    lifespan,
                    age: 0,
                    top,
                    reset_mode,
                    min_level: 0,
                    max_level: 50,
                    map_x: None,
                    map_y: None,
                };
                
                world.zones.push(zone);
            }
            line.clear();
        }
        
        Ok(())
    }
    
    fn load_rooms(world: &mut World, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        
        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }
            
            let room_file = path.join(&line);
            FileLoader::load_room_file(world, &room_file)?;
        }
        
        Ok(())
    }
    
    fn load_room_file(world: &mut World, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        
        while reader.read_line(&mut line)? > 0 {
            if line.starts_with('#') {
                let vnum: RoomVnum = line[1..].trim().parse()?;
                
                // Read room name
                line.clear();
                reader.read_line(&mut line)?;
                let name = line.trim_end_matches('~').to_string();
                
                // Read room description
                let mut description = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    if line.contains('~') {
                        description.push_str(line.trim_end_matches('~'));
                        break;
                    }
                    description.push_str(&line);
                }
                
                // Read zone, flags, sector
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let zone = parts.get(0).unwrap_or(&"0").parse()?;
                let flags = parts.get(1).unwrap_or(&"0").parse::<u32>()?;
                let sector = parts.get(2).unwrap_or(&"0").parse::<u8>()?;
                
                let mut room = Room::new(vnum, zone, name, description);
                room.room_flags = RoomFlags::from_bits_truncate(flags);
                room.sector_type = unsafe { std::mem::transmute(sector.min(10)) };
                
                // Read exits
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    
                    if line.trim() == "S" {
                        break;
                    }
                    
                    if line.starts_with('D') {
                        let dir = line[1..].trim().parse::<usize>()?;
                        if dir < NUM_OF_DIRS {
                            // Read exit description
                            let mut exit_desc = String::new();
                            loop {
                                line.clear();
                                reader.read_line(&mut line)?;
                                if line.contains('~') {
                                    exit_desc.push_str(line.trim_end_matches('~'));
                                    break;
                                }
                                exit_desc.push_str(&line);
                            }
                            
                            // Read keywords
                            line.clear();
                            reader.read_line(&mut line)?;
                            let keywords = line.trim_end_matches('~').to_string();
                            
                            // Read door info
                            line.clear();
                            reader.read_line(&mut line)?;
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            let exit_info = parts.get(0).unwrap_or(&"0").parse()?;
                            let key = parts.get(1).unwrap_or(&"-1").parse()?;
                            let to_room = parts.get(2).unwrap_or(&"0").parse()?;
                            
                            room.exits[dir] = Some(Exit {
                                description: if exit_desc.is_empty() { None } else { Some(exit_desc) },
                                keyword: if keywords.is_empty() { None } else { Some(keywords) },
                                exit_info,
                                key,
                                to_room,
                            });
                        }
                    }
                }
                
                world.add_room(room);
            }
            line.clear();
        }
        
        Ok(())
    }
    
    fn load_mobiles(world: &mut World, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        
        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }
            
            let mob_file = path.join(&line);
            FileLoader::load_mobile_file(world, &mob_file)?;
        }
        
        Ok(())
    }
    
    fn load_mobile_file(world: &mut World, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        
        while reader.read_line(&mut line)? > 0 {
            if line.starts_with('#') {
                let vnum: MobVnum = line[1..].trim().parse()?;
                
                // Read keywords
                line.clear();
                reader.read_line(&mut line)?;
                let keywords = line.trim_end_matches('~').to_string();
                
                // Read short description
                line.clear();
                reader.read_line(&mut line)?;
                let short_desc = line.trim_end_matches('~').to_string();
                
                // Read long description
                let mut long_desc = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    if line.contains('~') {
                        long_desc.push_str(line.trim_end_matches('~'));
                        break;
                    }
                    long_desc.push_str(&line);
                }
                
                // Read detailed description
                let mut description = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    if line.contains('~') {
                        description.push_str(line.trim_end_matches('~'));
                        break;
                    }
                    description.push_str(&line);
                }
                
                // Read mob stats
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let level = parts.get(2).unwrap_or(&"1").parse::<u8>()?.max(1);
                let hitpoints = parts.get(4).unwrap_or(&"10").parse()?;
                
                // Read more stats
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let gold = parts.get(0).unwrap_or(&"0").parse()?;
                let experience = parts.get(1).unwrap_or(&"100").parse()?;
                
                // Read position
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let position = parts.get(0).unwrap_or(&"8").parse::<u8>()?;
                let default_pos = parts.get(1).unwrap_or(&"8").parse::<u8>()?;
                let sex = parts.get(2).unwrap_or(&"0").parse::<u8>()?;
                
                let mob = MobileProto {
                    vnum,
                    name: keywords,
                    short_desc,
                    long_desc,
                    description,
                    level,
                    hitpoints,
                    experience,
                    gold,
                    position: unsafe { std::mem::transmute(position.min(9)) },
                    default_pos: unsafe { std::mem::transmute(default_pos.min(9)) },
                    sex: unsafe { std::mem::transmute(sex.min(2)) },
                };
                
                world.mob_protos.insert(vnum, mob);
            }
            line.clear();
        }
        
        Ok(())
    }
    
    fn load_objects(world: &mut World, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        
        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }
            
            let obj_file = path.join(&line);
            FileLoader::load_object_file(world, &obj_file)?;
        }
        
        Ok(())
    }
    
    fn load_object_file(world: &mut World, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        
        while reader.read_line(&mut line)? > 0 {
            if line.starts_with('#') {
                let vnum: ObjVnum = line[1..].trim().parse()?;
                
                // Read keywords
                line.clear();
                reader.read_line(&mut line)?;
                let keywords = line.trim_end_matches('~').to_string();
                
                // Read short description
                line.clear();
                reader.read_line(&mut line)?;
                let short_desc = line.trim_end_matches('~').to_string();
                
                // Read long description
                line.clear();
                reader.read_line(&mut line)?;
                let long_desc = line.trim_end_matches('~').to_string();
                
                // Read action description
                line.clear();
                reader.read_line(&mut line)?;
                let _action_desc = line.trim_end_matches('~').to_string();
                
                // Read type, extra flags, wear flags
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let obj_type = parts.get(0).unwrap_or(&"9").parse::<u8>()?;
                let extra_flags = parts.get(1).unwrap_or(&"0").parse::<u64>()?;
                let wear_flags = parts.get(2).unwrap_or(&"1").parse::<u32>()?;
                
                // Read values
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                let mut values = [0; 4];
                for i in 0..4 {
                    values[i] = parts.get(i).unwrap_or(&"0").parse()?;
                }
                
                // Read weight, cost, rent
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                
                let weight = parts.get(0).unwrap_or(&"1").parse()?;
                let cost = parts.get(1).unwrap_or(&"0").parse()?;
                let rent = parts.get(2).unwrap_or(&"0").parse()?;
                
                let obj = ObjectProto {
                    vnum,
                    name: keywords,
                    short_desc,
                    description: long_desc,
                    obj_type: unsafe { std::mem::transmute(obj_type.min(17)) },
                    wear_flags: WearFlags::from_bits_truncate(wear_flags),
                    extra_flags: ExtraFlags::from_bits_truncate(extra_flags),
                    weight,
                    cost,
                    rent,
                    values,
                };
                
                world.obj_protos.insert(vnum, obj);
            }
            line.clear();
        }
        
        Ok(())
    }
}