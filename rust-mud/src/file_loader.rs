use crate::object::{ExtraFlags, ObjectType, WearFlags};
use crate::room::{Exit, Room, RoomFlags, SectorType};
use crate::state::GameState;
use crate::types::*;
use crate::world::{MobileProto, ObjectProto, ResetCmd, Zone};
use anyhow::Result;
use log::{info, warn};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// structs.h MAX_OBJ_AFFECT — number of stat-apply slots an object carries.
const MAX_OBJ_AFFECT: usize = 6;

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

        info!(
            "World loaded: {} zones, {} rooms, {} mobs, {} objects",
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
                warn!(
                    "Failed to load zone {:?}: {}",
                    zone_file.file_name().unwrap_or_default(),
                    e
                );
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
                Err(_) => {
                    i += 1;
                    continue;
                }
            };
            i += 1;

            // Zone name (single line, ~-terminated). Mirrors load_zones in db.c.
            let name = lines
                .get(i)
                .map(|s| s.split('~').next().unwrap_or("").trim().to_string())
                .unwrap_or_default();
            i += 1;

            // Builders line (single line, ~-terminated) — always present in the
            // DeltaMUD format (Z.builders = str_dup(buf)).
            let builders = lines
                .get(i)
                .map(|s| s.split('~').next().unwrap_or("").trim().to_string())
                .unwrap_or_default();
            i += 1;

            // Zone header: top lifespan reset_mode
            let parts: Vec<&str> = lines
                .get(i)
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();
            let top: i32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let lifespan: i32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
            let reset_mode: i32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
            i += 1;

            // Level/status line: lvl1 lvl2 status_mode (required in DeltaMUD).
            let lvl_parts: Vec<&str> = lines
                .get(i)
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();
            let lvl1: i32 = lvl_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let lvl2: i32 = lvl_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
            let status_mode: i32 = lvl_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            i += 1;

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
                builders,
                lifespan,
                age: 0,
                top,
                reset_mode,
                min_level: lvl1.clamp(0, 255) as Level,
                max_level: lvl2.clamp(0, 255) as Level,
                status_mode,
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
            // arg4 (split index 5) is the load-chance for M/O/E/P; for G it's
            // arg3 (index 4). Absent => 0 (always loads), matching legacy zones.
            'M' => Some(ResetCmd::LoadMob {
                if_flag,
                mob_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                room_vnum: i32_at(4)?,
                load_chance: i32_at(5).unwrap_or(0),
            }),
            'O' => Some(ResetCmd::LoadObjInRoom {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                room_vnum: i32_at(4)?,
                load_chance: i32_at(5).unwrap_or(0),
            }),
            'G' => Some(ResetCmd::GiveObjToMob {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                load_chance: i32_at(4).unwrap_or(0),
            }),
            'E' => Some(ResetCmd::EquipMob {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                wear_pos: i32_at(4)? as usize,
                load_chance: i32_at(5).unwrap_or(0),
            }),
            'P' => Some(ResetCmd::PutObjInObj {
                if_flag,
                obj_vnum: i32_at(2)?,
                max_count: i32_at(3)?,
                container_vnum: i32_at(4)?,
                load_chance: i32_at(5).unwrap_or(0),
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
                warn!(
                    "Failed to load rooms {:?}: {}",
                    room_file.file_name().unwrap_or_default(),
                    e
                );
            }
        }

        Ok(())
    }

    fn load_room_file(world: &mut GameState, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        // Carries a record header ('#...' / '$') consumed while scanning the
        // previous room's trailing trigger lines, so it isn't lost.
        let mut pending: Option<String> = None;

        loop {
            if pending.is_none() {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    break;
                }
            } else {
                line = pending.take().unwrap();
            }

            if line.starts_with('$') {
                break;
            }
            if line.starts_with('#') {
                let vnum: RoomVnum = match line[1..].trim().parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // C discrete_load (db.c:700-701): stop reading the file at
                // the first vnum >= MAX_ROOM_VNUM (500000, structs.h:583)
                // (#241).
                if vnum >= 500000 {
                    break;
                }
                // Read room name (tilde-terminated; strip trailing newline first)
                line.clear();
                reader.read_line(&mut line)?;
                let name = line.trim_end().trim_end_matches('~').to_string();

                // Read room description
                let description = Self::read_tilde_buf(&mut reader)?;

                // Read zone, flags, sector
                line.clear();
                reader.read_line(&mut line)?;
                let parts: Vec<&str> = line.split_whitespace().collect();

                let zone = parts.first().unwrap_or(&"0").parse()?;
                let flags = parts
                    .get(1)
                    .map(|s| Self::asciiflag_conv(s) as u32)
                    .unwrap_or(0);
                let sector = parts.get(2).unwrap_or(&"0").parse::<i32>()?;

                let mut room = Room::new(vnum, zone, name, description);
                room.room_flags = RoomFlags::from_bits_truncate(flags);
                room.sector_type = SectorType::from_i32(sector);

                // Read room sub-blocks until the 'S' terminator. Mirrors C
                // parse_room: 'D'<n> exits, 'O' special exit, 'E' extra descrs.
                loop {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 {
                        break;
                    }

                    let first = line.trim_start().chars().next().unwrap_or(' ');
                    match first {
                        'S' => {
                            // 'T' DG triggers follow the 'S' terminator in C.
                            loop {
                                line.clear();
                                if reader.read_line(&mut line)? == 0 {
                                    break;
                                }
                                let lt = line.trim();
                                if lt.starts_with('T') && lt[1..].trim().parse::<i32>().is_ok() {
                                    crate::dg_db_scripts::parse_trigger_line(2, vnum, lt);
                                } else {
                                    // Not a trigger — hand this header to the
                                    // outer loop so it isn't dropped.
                                    pending = Some(std::mem::take(&mut line));
                                    break;
                                }
                            }
                            break;
                        }
                        'D' => {
                            let dir = line[1..].trim().parse::<usize>().unwrap_or(NUM_OF_DIRS);
                            // Read exit description
                            let exit_desc = Self::read_tilde_buf(&mut reader)?;
                            // Read keywords
                            let keywords = Self::read_tilde_buf(&mut reader)?;
                            // Read door info: exit_info key to_room
                            line.clear();
                            reader.read_line(&mut line)?;
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            let raw_flag: i32 =
                                parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                            let key = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
                            let to_room = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                            let exit_info = Self::door_flag(raw_flag);
                            if dir < NUM_OF_DIRS {
                                room.exits[dir] = Some(Exit {
                                    description: if exit_desc.is_empty() {
                                        None
                                    } else {
                                        Some(exit_desc)
                                    },
                                    keyword: if keywords.is_empty() {
                                        None
                                    } else {
                                        Some(keywords)
                                    },
                                    exit_info,
                                    key,
                                    to_room,
                                });
                            }
                        }
                        'O' => {
                            // setup_special_dir: 4 tilde-strings then a
                            // exit_info/key/to_room line.
                            let general_description = Self::read_tilde_buf(&mut reader)?;
                            let keyword = Self::read_tilde_buf(&mut reader)?;
                            let ex_name = Self::read_tilde_buf(&mut reader)?;
                            let leave_msg = Self::read_tilde_buf(&mut reader)?;
                            line.clear();
                            reader.read_line(&mut line)?;
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            let raw_flag: i32 =
                                parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                            let key = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
                            let to_room = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                            room.special_exit = Some(crate::room::SpecialExit {
                                general_description: if general_description.is_empty() {
                                    None
                                } else {
                                    Some(general_description)
                                },
                                keyword: if keyword.is_empty() {
                                    None
                                } else {
                                    Some(keyword)
                                },
                                ex_name: if ex_name.is_empty() {
                                    None
                                } else {
                                    Some(ex_name)
                                },
                                leave_msg: if leave_msg.is_empty() {
                                    None
                                } else {
                                    Some(leave_msg)
                                },
                                exit_info: Self::door_flag(raw_flag),
                                key,
                                to_room,
                            });
                        }
                        'E' => {
                            // Extra description: keyword~ desc~
                            let keyword = Self::read_tilde_buf(&mut reader)?;
                            let descr = Self::read_tilde_buf(&mut reader)?;
                            // C db.c:828-834 PREPENDS (later duplicate keywords
                            // resolve first in find_exdesc) (#237).
                            room.extra_descriptions.insert(0, (keyword, descr));
                        }
                        _ => {
                            // Unknown line (blank / stray). Skip, matching the
                            // robust-loader behaviour of not aborting the file.
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
                warn!(
                    "Failed to load mobs {:?}: {}",
                    mob_file.file_name().unwrap_or_default(),
                    e
                );
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
                Err(_) => {
                    i += 1;
                    continue;
                }
            };
            i += 1;
            let start = i;
            match Self::parse_single_mob(vnum, &lines, &mut i) {
                Ok(proto) => {
                    // C real_mobile keeps the FIRST duplicate vnum (#241).
                    world.mob_protos.entry(vnum).or_insert(proto);
                    parsed += 1;
                }
                Err(e) => {
                    warn!(
                        "mob #{} in {:?} skipped: {}",
                        vnum,
                        path.file_name().unwrap_or_default(),
                        e
                    );
                    failed += 1;
                    // Advance to the next '#' or end — parse may have left
                    // the cursor anywhere.
                    if i <= start {
                        i = start;
                    }
                    while i < lines.len() {
                        let t = lines[i].trim();
                        if t.starts_with('#') || t == "$" || t == "$~" {
                            break;
                        }
                        i += 1;
                    }
                }
            }
        }

        if parsed + failed > 0 {
            info!(
                "{:?}: {} mobs parsed, {} failed",
                path.file_name().unwrap_or_default(),
                parsed,
                failed
            );
        }
        Ok(())
    }


    /// C db.c:1293-1296 & 1384-1388: when the article word (fname) is exactly
    /// 'a', 'an' or 'the', the short description's first character is
    /// lowercased. Objects' room descriptions additionally get their first
    /// character UPPERCASED (db.c:1391-1393) (#240).
    fn lower_article(desc: &mut String) {
        let first_word = desc
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        if matches!(first_word.as_str(), "a" | "an" | "the") {
            if let Some(c) = desc.get_mut(0..1) {
                c.make_ascii_lowercase();
            }
        }
    }

    fn upper_first(desc: &mut String) {
        if let Some(c) = desc.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
    }

    fn parse_single_mob(vnum: MobVnum, lines: &[&str], i: &mut usize) -> Result<MobileProto> {
        let name = Self::read_tilde_string(lines, i)?;
        let mut short_desc = Self::read_tilde_string(lines, i)?;
        let long_desc = Self::read_tilde_string(lines, i)?;
        let description = Self::read_tilde_string(lines, i)?;
        // C db.c:1293-1296 lowercases only the short description.
        Self::lower_article(&mut short_desc);

        // Flag line: ACTION_FLAGS AFF_FLAGS ALIGNMENT LETTER
        // (asciiflag_conv f1) (asciiflag_conv f2) (alignment) ({S|E})
        let flag_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing flag line"))?;
        let flag_parts: Vec<&str> = flag_line.split_whitespace().collect();
        if flag_parts.len() < 4 {
            return Err(anyhow::anyhow!(
                "flag line has {} fields, need 4",
                flag_parts.len()
            ));
        }
        // Action flags (f1) and affect flags (f2): asciiflag_conv, exactly as
        // db.c parse_mobile (MOB_FLAGS = asciiflag_conv(f1); SET MOB_ISNPC;
        // AFF_FLAGS = asciiflag_conv(f2)). Without this every mob had act_flags=0,
        // so MOB_SPEC/SENTINEL/SCAVENGER/AGGRESSIVE/HELPER were all inert.
        let act_flags = Self::asciiflag_conv(flag_parts[0]) as i64 | crate::flags::MOB_ISNPC;
        let affect_flags = Self::asciiflag_conv(flag_parts[1]) as i64;
        let alignment: i32 = flag_parts[2].parse().unwrap_or(0);
        let letter = flag_parts[3]
            .chars()
            .next()
            .unwrap_or('S')
            .to_ascii_uppercase();

        // Stats line: either classic (9 numbers with dice) or X-prefixed
        // (DeltaMUD extended power/mpower/defense/mdefense/technique).
        let stats_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing stats line"))?;
        let level = Self::extract_level(stats_line)?;
        let xstats = Self::parse_combat_stats(stats_line);

        // Gold + experience line.
        let ge_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing gold/exp line"))?;
        let ge: Vec<i64> = ge_line
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let gold = *ge.get(0).unwrap_or(&0) as i32;
        let experience = *ge.get(1).unwrap_or(&100);

        // Position / default_pos / sex.
        let pos_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing position line"))?;
        let pos_parts: Vec<i32> = pos_line
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let position = (*pos_parts.get(0).unwrap_or(&8)).clamp(0, 9) as u8;
        let default_pos = (*pos_parts.get(1).unwrap_or(&8)).clamp(0, 9) as u8;
        let sex = (*pos_parts.get(2).unwrap_or(&0)).clamp(0, 2) as u8;

        // Hitpoints + damage dice from the stats line, format-aware.
        let ((hp_nd, hp_sd, hp_bonus), damnodice, damsizedice) = Self::parse_mob_dice(stats_line);

        // Enhanced ('E'): parse espec ability lines until a lone 'E'.
        // Mirrors parse_enhanced_mob / interpret_espec in db.c.
        let mut abilities: Option<crate::character::Abilities> = None;
        let mut attack_type: i32 = 0;
        if letter == 'E' {
            // Start from the C default ability set (11/13) and overlay especs.
            let mut ab = crate::character::Abilities {
                str: 13,
                str_add: 0,
                intel: 13,
                wis: 13,
                dex: 13,
                con: 13,
                cha: 13,
            };
            while *i < lines.len() {
                let t = lines[*i].trim();
                *i += 1;
                if t == "E" {
                    break;
                }
                if t.starts_with('#') || t == "$" || t == "$~" {
                    // Ran off the end of the mob without an E — recover.
                    *i -= 1;
                    break;
                }
                if t.is_empty() {
                    continue;
                }
                Self::interpret_espec(t, &mut ab, &mut attack_type);
            }
            abilities = Some(ab);
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
            hitpoints: hp_nd.max(1),
            hit_dice: (hp_nd, hp_sd, hp_bonus),
            experience,
            gold,
            position: Position::from_u8(position),
            default_pos: Position::from_u8(default_pos),
            sex: Gender::from_u8(sex),
            alignment,
            act_flags,
            affect_flags,
            // Combat fields default for Tier-0; refined in Batch 5 (fight.c).
            armor: 0,
            hitroll: 0,
            damroll: 0,
            damnodice,
            damsizedice,
            power: xstats.0,
            mpower: xstats.1,
            defense: xstats.2,
            mdefense: xstats.3,
            technique: xstats.4,
            abilities,
            attack_type,
        })
    }

    /// Parse one espec keyword line ("Str: 18", "BareHandAttack: 4", ...) and
    /// apply it. Matches db.c interpret_espec/parse_espec: keyword:value split,
    /// case-sensitive keyword names, RANGE-clamped values.
    fn interpret_espec(line: &str, ab: &mut crate::character::Abilities, attack_type: &mut i32) {
        let (key, val) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (line.trim(), ""),
        };
        let num: i32 = val.parse().unwrap_or(0);
        let clamp = |n: i32, lo: i32, hi: i32| n.max(lo).min(hi);
        match key {
            "BareHandAttack" => *attack_type = clamp(num, 0, 99),
            "Str" => ab.str = clamp(num, 3, 25) as i8,
            "StrAdd" => ab.str_add = clamp(num, 0, 100) as i8,
            "Int" => ab.intel = clamp(num, 3, 25) as i8,
            "Wis" => ab.wis = clamp(num, 3, 25) as i8,
            "Dex" => ab.dex = clamp(num, 3, 25) as i8,
            "Con" => ab.con = clamp(num, 3, 25) as i8,
            "Cha" => ab.cha = clamp(num, 3, 25) as i8,
            _ => {
                warn!("unrecognized espec keyword {:?}", key);
            }
        }
    }

    /// Parse the DeltaMUD `X`-format combat stats from a stats line. Returns
    /// (power, mpower, defense, mdefense, technique); all zero for the classic
    /// (non-X) format. Mirrors parse_simple_mob's X branch in db.c.
    fn parse_combat_stats(stats_line: &str) -> (i16, i16, i16, i16, i16) {
        let trimmed = stats_line.trim();
        if !trimmed.starts_with('X') && !trimmed.starts_with('x') {
            return (0, 0, 0, 0, 0);
        }
        // X<level> power mpower defense mdefense technique ...
        let nums = Self::stat_numbers(trimmed);
        // nums[0] is level; combat stats follow.
        let g = |idx: usize| -> i16 {
            nums.get(idx)
                .copied()
                .unwrap_or(0)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16
        };
        (g(1), g(2), g(3), g(4), g(5))
    }

    /// Extract the HP dice triple + damage dice from a stats line for both
    /// formats. The HP triple is (nodice, sizedice, bonus) exactly as C's
    /// parse_simple_mob stores it (hit/mana/move of the proto); read_mobile
    /// then rolls max_hit = dice(nodice, sizedice) + bonus (#230).
    fn parse_mob_dice(stats_line: &str) -> ((i32, i32, i32), i32, i32) {
        let nums = Self::stat_numbers(stats_line);
        let trimmed = stats_line.trim();
        let g = |idx: usize| -> i32 { nums.get(idx).copied().unwrap_or(0) };
        if trimmed.starts_with('X') || trimmed.starts_with('x') {
            // t0=lvl t1..t5=combat t6=hit t7=mana t8=move t9=damnodice t10=damsizedice
            (
                (g(6), g(7), g(8)),
                g(9).max(1),
                g(10).max(1),
            )
        } else {
            // t0=lvl t1=thac0 t2=ac t3=hit t4=mana t5=move t6=damnodice t7=damsizedice t8=damroll
            (
                (g(3), g(4), g(5)),
                g(6).max(1),
                g(7).max(1),
            )
        }
    }

    /// Flatten a stats line into a list of integers, splitting dice tokens on
    /// 'd'/'+' and dropping a leading 'X'/'x' marker. So `X5 1 2 3d4+5` yields
    /// [5,1,2,3,4,5].
    fn stat_numbers(stats_line: &str) -> Vec<i32> {
        let mut out = Vec::new();
        for (idx, tok) in stats_line.trim().split_whitespace().enumerate() {
            let tok = if idx == 0 {
                tok.trim_start_matches(['X', 'x'])
            } else {
                tok
            };
            for piece in tok.split(['d', '+']) {
                if let Ok(n) = piece.parse::<i32>() {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Read a tilde-terminated string block. Accepts either inline `~`
    /// (same line) or a lone `~` on a subsequent line.
    fn read_tilde_string(lines: &[&str], i: &mut usize) -> Result<String> {
        let mut out = String::new();
        while *i < lines.len() {
            let raw = lines[*i];
            *i += 1;
            if let Some(pos) = raw.find('~') {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&raw[..pos]);
                return Ok(out);
            }
            if !out.is_empty() {
                out.push('\n');
            }
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
        let first = stats_line
            .trim()
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty stats line"))?;
        let digits = first.trim_start_matches('X').trim_start_matches('x');
        let level: i32 = digits
            .parse()
            .map_err(|_| anyhow::anyhow!("bad level token {:?}", first))?;
        Ok(level.clamp(0, 200) as u8)
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
                warn!(
                    "Failed to load objs {:?}: {}",
                    obj_file.file_name().unwrap_or_default(),
                    e
                );
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
                    out.push_str("\r\n"); // C fread_string \n -> \r\n (#239)
                }
                out.push_str(&line[..p]);
                return Ok(out);
            }
            if !out.is_empty() {
                out.push_str("\r\n"); // C fread_string \n -> \r\n (#239)
            }
            out.push_str(line.trim_end_matches(['\r', '\n']));
        }
        Ok(out)
    }

    /// Decode the door-state code from a .wld exit/special-exit line exactly
    /// as C setup_dir/setup_special_dir do: values >2 set EX_HIDDEN and drop
    /// by 3; 1 => ISDOOR, 2 => ISDOOR|PICKPROOF, else nothing.
    fn door_flag(mut t0: i32) -> i32 {
        use crate::room::{EX_HIDDEN, EX_ISDOOR, EX_PICKPROOF};
        let mut info = 0;
        if t0 > 2 {
            info = EX_HIDDEN;
            t0 -= 3;
        }
        if t0 == 1 {
            info |= EX_ISDOOR;
        } else if t0 == 2 {
            info |= EX_ISDOOR | EX_PICKPROOF;
        }
        info
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
        // When parse_object consumes the next record's '#' header (C returns
        // that line), we stash it here so the outer loop reuses it instead of
        // reading a fresh line.
        let mut pending: Option<String> = None;

        loop {
            // Acquire the current header line.
            if pending.is_none() {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    break;
                }
            } else {
                line = pending.take().unwrap();
            }

            if line.starts_with('$') {
                break;
            }
            if !line.starts_with('#') {
                continue;
            }
            let vnum: ObjVnum = match line[1..].trim().parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Four tilde-terminated strings (each may span lines).
            let keywords = Self::read_tilde_buf(&mut reader)?;
            let mut short_desc = Self::read_tilde_buf(&mut reader)?;
            let mut long_desc = Self::read_tilde_buf(&mut reader)?;
            let action_desc = Self::read_tilde_buf(&mut reader)?;
            Self::lower_article(&mut short_desc);
            Self::upper_first(&mut long_desc); // db.c:1391-1393 (#240)

            // type, extra flags, wear flags (flags may be ascii letters).
            line.clear();
            reader.read_line(&mut line)?;
            let parts: Vec<&str> = line.split_whitespace().collect();
            let obj_type = parts
                .first()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(12);
            let extra_flags = parts.get(1).map(|s| Self::asciiflag_conv(s)).unwrap_or(0);
            let wear_flags = parts
                .get(2)
                .map(|s| Self::asciiflag_conv(s) as u32)
                .unwrap_or(1);

            // values line: up to 6 numbers. value[0..4] are obj values;
            // value[4]/value[5] become curr_slots/total_slots when in 0..=100.
            line.clear();
            reader.read_line(&mut line)?;
            let vparts: Vec<&str> = line.split_whitespace().collect();
            let mut values = [0i32; 4];
            for (i, v) in values.iter_mut().enumerate() {
                *v = vparts.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            let v4: i32 = vparts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let v5: i32 = vparts.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
            let (curr_slots, total_slots) = if (0..=100).contains(&v4) && (0..=100).contains(&v5) {
                (v4, v5)
            } else {
                (0, 0)
            };

            // weight, cost, rent.
            line.clear();
            reader.read_line(&mut line)?;
            let wparts: Vec<&str> = line.split_whitespace().collect();
            let weight = wparts.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            let cost = wparts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let rent = wparts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

            // Optional E / A / c / L / BV / T blocks until '$' or next '#'.
            let mut ex_descriptions: Vec<(String, String)> = Vec::new();
            let mut affects: Vec<crate::object::ObjectAffect> = Vec::new();
            let mut obj_class: i32 = -1;
            let mut min_level: i32 = 0;
            let mut bitvector: i64 = 0;
            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 {
                    break;
                }
                let first = line.trim_start().chars().next().unwrap_or(' ');
                match first {
                    'E' => {
                        let keyword = Self::read_tilde_buf(&mut reader)?;
                        let descr = Self::read_tilde_buf(&mut reader)?;
                        // C db.c:1475-1481 prepends object extra descriptions
                        // so later duplicate keywords resolve first (#237).
                        ex_descriptions.insert(0, (keyword, descr));
                    }
                    'A' => {
                        // The 'A' marker line is followed by 'location modifier'.
                        if affects.len() < MAX_OBJ_AFFECT {
                            line.clear();
                            reader.read_line(&mut line)?;
                            let ap: Vec<&str> = line.split_whitespace().collect();
                            let location = ap.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                            let modifier = ap.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                            affects.push(crate::object::ObjectAffect { location, modifier });
                        }
                    }
                    'c' => {
                        // c <class>  -> obj_class = class - 1 (C: atoi(line+2)-1)
                        obj_class = line.trim_start()[1..].trim().parse::<i32>().unwrap_or(0) - 1;
                    }
                    'L' => {
                        min_level = line.trim_start()[1..].trim().parse().unwrap_or(0);
                    }
                    'B' => {
                        // Only 'BV' is meaningful (affect bitvector).
                        let body = line.trim_start();
                        if body.as_bytes().get(1) == Some(&b'V') {
                            bitvector = body[2..].trim().parse().unwrap_or(0);
                        }
                    }
                    'T' => {
                        // Object DG trigger (kind = OBJ_TRIGGER = 1).
                        let lt = line.trim();
                        if lt[1..].trim().parse::<i32>().is_ok() {
                            crate::dg_db_scripts::parse_trigger_line(1, vnum, lt);
                        }
                    }
                    '$' | '#' => {
                        // Hand this header to the outer loop.
                        pending = Some(std::mem::take(&mut line));
                        break;
                    }
                    _ => {
                        // Stray/blank line — skip without aborting.
                    }
                }
            }

            // C real_object keeps the FIRST duplicate vnum (#241).
            world.obj_protos.entry(vnum).or_insert_with(|| ObjectProto {
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
                curr_slots,
                total_slots,
                obj_class,
                min_level,
                bitvector,
                action_description: action_desc,
                affects,
                ex_descriptions,
            });
        }

        Ok(())
    }
}
