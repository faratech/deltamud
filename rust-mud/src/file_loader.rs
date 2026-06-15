use crate::world::{Zone, MobileProto, ObjectProto, ResetCmd};
use crate::room::{Room, Exit, RoomFlags, SectorType};
use crate::object::{WearFlags, ExtraFlags, ObjectType};
use crate::state::GameState;
use crate::types::*;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use anyhow::Result;
use log::{info, warn};

pub struct FileLoader;

impl FileLoader {
    pub async fn load_world(world: &mut GameState, base_path: &str) -> Result<()> {
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
    
    fn load_zones(world: &mut GameState, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }

            let zone_file = path.join(&line);
            if let Err(e) = FileLoader::load_zone_file(world, &zone_file) {
                warn!("Failed to load zone {:?}: {}", zone_file.file_name().unwrap_or_default(), e);
            }
        }

        Ok(())
    }
    
    fn load_zone_file(world: &mut GameState, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>()?;
        let mut i = 0;

        while i < lines.len() {
            let hdr = lines[i].trim();
            if !hdr.starts_with('#') {
                i += 1;
                continue;
            }
            let zone_num: i32 = match hdr[1..].trim().parse() {
                Ok(v) => v,
                Err(_) => { i += 1; continue; }
            };
            i += 1;

            // Zone name (may span single line, ~-terminated)
            let name = lines.get(i).map(|s| s.trim_end_matches('~').trim().to_string())
                .unwrap_or_default();
            i += 1;

            // Optional "builders" line — tilde-terminated in most formats but
            // some DeltaMUD zones skip it. Peek: if the next line looks
            // numeric (header of 3 numbers), treat it as the data line.
            let maybe_builders = lines.get(i).map(|s| s.trim());
            let builders_is_data = maybe_builders
                .map(|s| s.split_whitespace().all(|t| t.parse::<i32>().is_ok()) && s.split_whitespace().count() >= 3)
                .unwrap_or(false);
            if !builders_is_data {
                i += 1;
            }

            // Zone header: top lifespan reset_mode [optional levels/status]
            let parts: Vec<&str> = lines.get(i).map(|s| s.split_whitespace().collect())
                .unwrap_or_default();
            let top: i32 = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let lifespan: i32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
            let reset_mode: i32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
            i += 1;

            // Optional level-range line (not always present). Skip if it
            // looks like numbers without a trailing reset command.
            if let Some(next) = lines.get(i) {
                let first = next.trim().chars().next();
                let is_reset = matches!(first, Some(c) if "MOGEPRD*".contains(c));
                if !is_reset && !next.trim().is_empty() && !next.trim().starts_with('S') && !next.trim().starts_with('$') {
                    // Probably the level/status line — skip.
                    i += 1;
                }
            }

            // Reset commands until 'S' or '$'.
            let mut reset_commands = Vec::new();
            while i < lines.len() {
                let raw = lines[i].trim();
                i += 1;
                if raw.is_empty() || raw.starts_with('*') {
                    continue;
                }
                let first_char = raw.chars().next().unwrap();
                if first_char == 'S' || first_char == '$' {
                    break;
                }
                if let Some(cmd) = Self::parse_reset_command(raw) {
                    reset_commands.push(cmd);
                } else {
                    warn!("zone {} unparseable reset cmd: {:?}", zone_num, raw);
                }
            }

            world.zones.push(Zone {
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
                reset_commands,
            });
        }

        Ok(())
    }

    fn parse_reset_command(raw: &str) -> Option<ResetCmd> {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }
        let cmd = parts[0].chars().next()?;
        let i32_at = |idx: usize| parts.get(idx).and_then(|s| s.parse::<i32>().ok());
        let if_flag = i32_at(1).unwrap_or(0) != 0;
        match cmd {
            'M' => Some(ResetCmd::LoadMob {
                if_flag,
                mob_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                room_vnum: i32_at(4)?,
            }),
            'O' => Some(ResetCmd::LoadObjInRoom {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                room_vnum: i32_at(4)?,
            }),
            'G' => Some(ResetCmd::GiveObjToMob {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
            }),
            'E' => Some(ResetCmd::EquipMob {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                wear_pos: i32_at(4)? as usize,
            }),
            'P' => Some(ResetCmd::PutObjInObj {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                container_vnum: i32_at(4)?,
            }),
            'R' => Some(ResetCmd::RemoveObj {
                if_flag,
                room_vnum: i32_at(2)?,
                obj_vnum: i32_at(3)?,
            }),
            'D' => Some(ResetCmd::Door {
                if_flag,
                room_vnum: i32_at(2)?,
                direction: i32_at(3)? as usize,
                state: i32_at(4)?,
            }),
            _ => None,
        }
    }
    
    fn load_rooms(world: &mut GameState, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }

            let room_file = path.join(&line);
            if let Err(e) = FileLoader::load_room_file(world, &room_file) {
                warn!("Failed to load rooms {:?}: {}", room_file.file_name().unwrap_or_default(), e);
            }
        }

        Ok(())
    }
    
    fn load_room_file(world: &mut GameState, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        
        while reader.read_line(&mut line)? > 0 {
            if line.starts_with('#') {
                let vnum: RoomVnum = line[1..].trim().parse()?;
                
                // Read room name (tilde-terminated; strip trailing newline first)
                line.clear();
                reader.read_line(&mut line)?;
                let name = line.trim_end().trim_end_matches('~').to_string();

                // Read room description
                let mut description = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    if let Some(p) = line.find('~') {
                        description.push_str(&line[..p]);
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
                room.sector_type = SectorType::from_i32(sector as i32);
                
                // Read exits
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    
                    if line.trim() == "S" {
                        break;
                    }

                    // Room DG trigger: exactly 'T <number>' (WLD_TRIGGER=2);
                    // avoids matching description lines like "The...".
                    let lt = line.trim();
                    if lt.starts_with("T ") && lt[2..].trim().parse::<i32>().is_ok() {
                        crate::dg_db_scripts::parse_trigger_line(2, vnum, lt);
                        continue;
                    }

                    if line.starts_with('D') {
                        let dir = line[1..].trim().parse::<usize>()?;
                        if dir < NUM_OF_DIRS {
                            // Read exit description
                            let mut exit_desc = String::new();
                            loop {
                                line.clear();
                                reader.read_line(&mut line)?;
                                if let Some(p) = line.find('~') {
                                    exit_desc.push_str(&line[..p]);
                                    break;
                                }
                                exit_desc.push_str(&line);
                            }

                            // Read keywords
                            line.clear();
                            reader.read_line(&mut line)?;
                            let keywords = line.trim_end().trim_end_matches('~').to_string();
                            
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
    
    fn load_mobiles(world: &mut GameState, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        
        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }
            
            let mob_file = path.join(&line);
            if let Err(e) = FileLoader::load_mobile_file(world, &mob_file) {
                warn!("Failed to load mobs {:?}: {}", mob_file.file_name().unwrap_or_default(), e);
            }
        }
        
        Ok(())
    }
    
    /// Parse a mobile file in DeltaMUD/CircleMUD format.
    /// Mirrors C reference /web/deltamud/src/db.c:1043-1340
    /// (parse_simple_mob, parse_enhanced_mob, parse_mobile).
    ///
    /// File layout per mob block:
    ///   #VNUM
    ///   keyword(s)~
    ///   short-desc~
    ///   long-desc lines
    ///   ~
    ///   detailed description lines
    ///   ~
    ///   ACTION_FLAGS AFF_FLAGS ALIGNMENT LETTER   (LETTER = S or E)
    ///   <stats line>                              (classic 9-number OR `X`-prefixed 11-number)
    ///   GOLD EXP
    ///   POS DEFAULT_POS SEX
    ///   [optional espec keyword lines, terminated by a lone 'E']
    ///   [optional T... trigger lines, skipped for Tier-0]
    ///
    /// Per-mob errors log and skip instead of aborting the whole file,
    /// so one bad entry doesn't sink the zone.
    fn load_mobile_file(world: &mut GameState, path: &Path) -> Result<()> {
        let contents = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = contents.lines().collect();
        let mut i = 0;
        let mut parsed = 0usize;
        let mut failed = 0usize;

        while i < lines.len() {
            let trimmed = lines[i].trim();
            // Terminator for the whole file.
            if trimmed == "$~" || trimmed == "$" {
                break;
            }
            if !trimmed.starts_with('#') {
                i += 1;
                continue;
            }
            let vnum: MobVnum = match trimmed[1..].trim().parse() {
                Ok(v) => v,
                Err(_) => { i += 1; continue; }
            };
            i += 1;
            let start = i;
            match Self::parse_single_mob(vnum, &lines, &mut i) {
                Ok(proto) => {
                    world.mob_protos.insert(vnum, proto);
                    parsed += 1;
                }
                Err(e) => {
                    warn!("mob #{} in {:?} skipped: {}", vnum, path.file_name().unwrap_or_default(), e);
                    failed += 1;
                    // Advance to the next '#' or end — parse may have left
                    // the cursor anywhere.
                    if i <= start { i = start; }
                    while i < lines.len() {
                        let t = lines[i].trim();
                        if t.starts_with('#') || t == "$" || t == "$~" { break; }
                        i += 1;
                    }
                }
            }
        }

        if parsed + failed > 0 {
            info!("{:?}: {} mobs parsed, {} failed", path.file_name().unwrap_or_default(), parsed, failed);
        }
        Ok(())
    }

    fn parse_single_mob(vnum: MobVnum, lines: &[&str], i: &mut usize) -> Result<MobileProto> {
        let name = Self::read_tilde_string(lines, i)?;
        let short_desc = Self::read_tilde_string(lines, i)?;
        let long_desc = Self::read_tilde_string(lines, i)?;
        let description = Self::read_tilde_string(lines, i)?;

        // Flag line: ACTION_FLAGS AFF_FLAGS ALIGNMENT LETTER
        // We don't need flags for Tier-0; we just need the type letter to
        // know whether an espec block follows.
        let flag_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing flag line"))?;
        let flag_parts: Vec<&str> = flag_line.split_whitespace().collect();
        if flag_parts.len() < 4 {
            return Err(anyhow::anyhow!("flag line has {} fields, need 4", flag_parts.len()));
        }
        let letter = flag_parts[3].chars().next().unwrap_or('S').to_ascii_uppercase();

        // Stats line: either classic (9 numbers with dice) or X-prefixed
        // (11 numbers, DeltaMUD extended). The only field we currently
        // persist is level.
        let stats_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing stats line"))?;
        let level = Self::extract_level(stats_line)?;

        // Gold + experience line.
        let ge_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing gold/exp line"))?;
        let ge: Vec<i64> = ge_line.split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let gold = *ge.get(0).unwrap_or(&0) as i32;
        let experience = *ge.get(1).unwrap_or(&100);

        // Position / default_pos / sex.
        let pos_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing position line"))?;
        let pos_parts: Vec<i32> = pos_line.split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let position = (*pos_parts.get(0).unwrap_or(&8)).clamp(0, 9) as u8;
        let default_pos = (*pos_parts.get(1).unwrap_or(&8)).clamp(0, 9) as u8;
        let sex = (*pos_parts.get(2).unwrap_or(&0)).clamp(0, 2) as u8;

        // Hitpoints: when the stats line has Hd+H notation, C stores the
        // base of that dice roll in points.hit. Extract approximately.
        let hitpoints = Self::extract_hitpoints(stats_line).unwrap_or(10);

        // Enhanced: skip until a lone 'E' line (end of espec section).
        // We don't persist espec values for Tier-0; that's a later polish.
        if letter == 'E' {
            while *i < lines.len() {
                let t = lines[*i].trim();
                *i += 1;
                if t == "E" { break; }
                if t.starts_with('#') || t == "$" || t == "$~" {
                    // Ran off the end of the mob without an E — recover.
                    *i -= 1;
                    break;
                }
            }
        }

        // Attach DG triggers declared by trailing 'T <vnum>' lines (MOB_TRIGGER=0).
        while *i < lines.len() {
            let t = lines[*i].trim();
            if t.starts_with("T ") && t[2..].trim().parse::<i32>().is_ok() {
                crate::dg_db_scripts::parse_trigger_line(0, vnum, t);
                *i += 1;
            } else {
                break;
            }
        }

        Ok(MobileProto {
            vnum,
            name,
            short_desc,
            long_desc,
            description,
            level,
            hitpoints,
            experience,
            gold,
            position: Position::from_u8(position),
            default_pos: Position::from_u8(default_pos),
            sex: Gender::from_u8(sex),
            // Combat fields default for Tier-0; refined in Batch 5 (fight.c).
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice: 1,
            damsizedice: 6,
        })
    }

    /// Read a tilde-terminated string block. Accepts either inline `~`
    /// (same line) or a lone `~` on a subsequent line.
    fn read_tilde_string(lines: &[&str], i: &mut usize) -> Result<String> {
        let mut out = String::new();
        while *i < lines.len() {
            let raw = lines[*i];
            *i += 1;
            if let Some(pos) = raw.find('~') {
                if !out.is_empty() { out.push('\n'); }
                out.push_str(&raw[..pos]);
                return Ok(out);
            }
            if !out.is_empty() { out.push('\n'); }
            out.push_str(raw);
        }
        Err(anyhow::anyhow!("unterminated ~-string"))
    }

    /// Next non-empty line, advancing the cursor past it.
    fn next_content_line<'a>(lines: &'a [&'a str], i: &mut usize) -> Option<&'a str> {
        while *i < lines.len() {
            let line = lines[*i];
            *i += 1;
            if !line.trim().is_empty() {
                return Some(line);
            }
        }
        None
    }

    /// Extract the mob level from either classic or X-prefixed stats line.
    /// Both formats put level first: classic `LEVEL thac0 ac ...` or
    /// `XLEVEL power mpower defense mdefense technique ...`.
    fn extract_level(stats_line: &str) -> Result<u8> {
        let first = stats_line.trim().split_whitespace().next()
            .ok_or_else(|| anyhow::anyhow!("empty stats line"))?;
        let digits = first.trim_start_matches('X').trim_start_matches('x');
        let level: i32 = digits.parse()
            .map_err(|_| anyhow::anyhow!("bad level token {:?}", first))?;
        Ok(level.clamp(0, 200) as u8)
    }

    /// Pull the hit-point base out of a stats line's Hd+H dice field.
    /// Both classic and X formats end with `... Hd+H ...`; we try the
    /// first dice-notation token and read the `+N` or `Nd` value.
    fn extract_hitpoints(stats_line: &str) -> Option<i32> {
        for tok in stats_line.split_whitespace() {
            if let Some(_d_pos) = tok.find('d') {
                // Parse NdM+K — use K as HP base if present, else NxM as rough.
                let (n_part, rest) = tok.split_once('d')?;
                let (m_part, plus_part) = match rest.split_once('+') {
                    Some((m, p)) => (m, Some(p)),
                    None => (rest, None),
                };
                let n: i32 = n_part.parse().ok()?;
                let m: i32 = m_part.parse().ok()?;
                let k: i32 = plus_part.and_then(|p| p.parse().ok()).unwrap_or(0);
                return Some(k + n * (m.max(1) + 1) / 2);
            }
        }
        None
    }
    
    fn load_objects(world: &mut GameState, path: &Path) -> Result<()> {
        let index_path = path.join("index");
        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        
        for line in reader.lines() {
            let line = line?;
            if line == "$" {
                break;
            }
            
            let obj_file = path.join(&line);
            if let Err(e) = FileLoader::load_object_file(world, &obj_file) {
                warn!("Failed to load objs {:?}: {}", obj_file.file_name().unwrap_or_default(), e);
            }
        }
        
        Ok(())
    }
    
    /// Read a tilde-terminated (possibly multi-line) string from a reader.
    fn read_tilde_buf(reader: &mut BufReader<File>) -> Result<String> {
        let mut out = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            if let Some(p) = line.find('~') {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&line[..p]);
                return Ok(out);
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line.trim_end_matches(['\r', '\n']));
        }
        Ok(out)
    }

    /// CircleMUD asciiflag_conv: a flag field is either a plain integer or a
    /// string of letters (a-z = bits 0-25, A-Z = bits 26-51).
    fn asciiflag_conv(flag: &str) -> u64 {
        let flag = flag.trim();
        if let Ok(n) = flag.parse::<u64>() {
            return n;
        }
        let mut bits = 0u64;
        for c in flag.chars() {
            if c.is_ascii_lowercase() {
                bits |= 1 << (c as u64 - 'a' as u64);
            } else if c.is_ascii_uppercase() {
                bits |= 1 << (26 + c as u64 - 'A' as u64);
            }
        }
        bits
    }

    fn load_object_file(world: &mut GameState, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        while reader.read_line(&mut line)? > 0 {
            if line.starts_with('#') {
                let vnum: ObjVnum = match line[1..].trim().parse() {
                    Ok(v) => v,
                    Err(_) => {
                        line.clear();
                        continue;
                    }
                };

                // Four tilde-terminated strings (each may span lines).
                let keywords = Self::read_tilde_buf(&mut reader)?;
                let short_desc = Self::read_tilde_buf(&mut reader)?;
                let long_desc = Self::read_tilde_buf(&mut reader)?;
                let action_desc = Self::read_tilde_buf(&mut reader)?;
                let _ = action_desc;

                // type, extra flags, wear flags (flags may be ascii letters).
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                let obj_type = parts.first().and_then(|s| s.parse::<i32>().ok()).unwrap_or(9);
                let extra_flags = parts.get(1).map(|s| Self::asciiflag_conv(s)).unwrap_or(0);
                let wear_flags = parts.get(2).map(|s| Self::asciiflag_conv(s) as u32).unwrap_or(1);

                // values (DeltaMUD writes up to 6; Object keeps the first 4).
                line.clear();
                reader.read_line(&mut line)?;
                let vparts: Vec<&str> = line.split_whitespace().collect();
                let mut values = [0i32; 4];
                for (i, v) in values.iter_mut().enumerate() {
                    *v = vparts.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
                }

                // weight, cost, rent.
                line.clear();
                reader.read_line(&mut line)?;
                let wparts: Vec<&str> = line.split_whitespace().collect();
                let weight = wparts.first().and_then(|s| s.parse().ok()).unwrap_or(1);
                let cost = wparts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let rent = wparts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

                world.obj_protos.insert(
                    vnum,
                    ObjectProto {
                        vnum,
                        name: keywords,
                        short_desc,
                        description: long_desc,
                        obj_type: ObjectType::from_i32(obj_type),
                        wear_flags: WearFlags::from_bits_truncate(wear_flags),
                        extra_flags: ExtraFlags::from_bits_truncate(extra_flags),
                        weight,
                        cost,
                        rent,
                        values,
                    },
                );
            }
            line.clear();
        }

        Ok(())
    }
}