use crate::object::{ExtraFlags, ObjectType, WearFlags};
use crate::room::{Exit, Room, RoomFlags, SectorType};
use crate::state::GameState;
use crate::types::*;
use crate::world::{MAX_ZONE_NUMBER, MobileProto, ObjectProto, ResetCmd, Zone, zone_vnum_bounds};
use anyhow::{Context, Result, anyhow};
use log::{info, warn};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// structs.h MAX_OBJ_AFFECT — number of stat-apply slots an object carries.
const MAX_OBJ_AFFECT: usize = 6;

pub struct FileLoader;

impl FileLoader {
    /// Parse a present numeric field exactly; only a genuinely absent optional
    /// field receives its legacy default. Malformed or overflowing text is a
    /// record error, so it cannot silently shift/default another field.
    fn numeric_field<T>(raw: Option<&str>, default: T, field: &str) -> Result<T>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        match raw {
            Some(raw) => raw
                .parse::<T>()
                .map_err(|error| anyhow!("invalid {field} {raw:?}: {error}")),
            None => Ok(default),
        }
    }

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
            let zone_num: i32 = Self::numeric_field(Some(hdr[1..].trim()), 0, "zone number")?;
            let (zone_start, _) = zone_vnum_bounds(zone_num).ok_or_else(|| {
                anyhow!(
                    "zone number {zone_num} is outside the supported range 0..={MAX_ZONE_NUMBER}"
                )
            })?;
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
            let top: i32 = Self::numeric_field(parts.first().copied(), 0, "zone top")?;
            if top < zone_start {
                return Err(anyhow!(
                    "zone {zone_num} top {top} is below its first vnum {zone_start}"
                ));
            }
            let lifespan: i32 = Self::numeric_field(parts.get(1).copied(), 30, "zone lifespan")?;
            let reset_mode: i32 = Self::numeric_field(parts.get(2).copied(), 2, "zone reset mode")?;
            i += 1;

            // Level/status line: lvl1 lvl2 status_mode (required in DeltaMUD).
            let lvl_parts: Vec<&str> = lines
                .get(i)
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();
            let lvl1: i32 =
                Self::numeric_field(lvl_parts.first().copied(), 0, "zone minimum level")?;
            let lvl2: i32 =
                Self::numeric_field(lvl_parts.get(1).copied(), 50, "zone maximum level")?;
            let status_mode: i32 =
                Self::numeric_field(lvl_parts.get(2).copied(), 0, "zone status mode")?;
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
                match Self::parse_reset_command(raw) {
                    Ok(Some(cmd)) => reset_commands.push(cmd),
                    Ok(None) => warn!("zone {} unparseable reset cmd: {:?}", zone_num, raw),
                    Err(error) => {
                        warn!("zone {} rejected reset cmd {:?}: {error:#}", zone_num, raw)
                    }
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

    fn parse_reset_command(raw: &str) -> Result<Option<ResetCmd>> {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(None);
        }
        let Some(cmd) = parts[0].chars().next() else {
            return Ok(None);
        };
        let i32_at =
            |idx: usize, field: &str| Self::numeric_field(parts.get(idx).copied(), 0i32, field);
        let required = |idx: usize, field: &str| -> Result<i32> {
            let raw = parts
                .get(idx)
                .copied()
                .ok_or_else(|| anyhow!("missing {field}"))?;
            Self::numeric_field(Some(raw), 0i32, field)
        };
        let if_flag = i32_at(1, "reset if flag")? != 0;
        let parsed = match cmd {
            // arg4 (split index 5) is the load-chance for M/O/E/P; for G it's
            // arg3 (index 4). Absent => 0 (always loads), matching legacy zones.
            'M' => Some(ResetCmd::LoadMob {
                if_flag,
                mob_vnum: required(2, "mobile vnum")?,
                max_count: required(3, "mobile max count")?,
                room_vnum: required(4, "mobile room vnum")?,
                load_chance: i32_at(5, "mobile load chance")?,
            }),
            'O' => Some(ResetCmd::LoadObjInRoom {
                if_flag,
                obj_vnum: required(2, "object vnum")?,
                max_count: required(3, "object max count")?,
                room_vnum: required(4, "object room vnum")?,
                load_chance: i32_at(5, "object load chance")?,
            }),
            'G' => Some(ResetCmd::GiveObjToMob {
                if_flag,
                obj_vnum: required(2, "give object vnum")?,
                max_count: required(3, "give object max count")?,
                load_chance: i32_at(4, "give object load chance")?,
            }),
            'E' => Some(ResetCmd::EquipMob {
                if_flag,
                obj_vnum: required(2, "equipment object vnum")?,
                max_count: required(3, "equipment max count")?,
                wear_pos: usize::try_from(required(4, "equipment wear position")?)
                    .context("negative equipment wear position")?,
                load_chance: i32_at(5, "equipment load chance")?,
            }),
            'P' => Some(ResetCmd::PutObjInObj {
                if_flag,
                obj_vnum: required(2, "put object vnum")?,
                max_count: required(3, "put object max count")?,
                container_vnum: required(4, "container vnum")?,
                load_chance: i32_at(5, "put object load chance")?,
            }),
            'R' => Some(ResetCmd::RemoveObj {
                if_flag,
                room_vnum: required(2, "remove room vnum")?,
                obj_vnum: required(3, "remove object vnum")?,
            }),
            'D' => Some(ResetCmd::Door {
                if_flag,
                room_vnum: required(2, "door room vnum")?,
                direction: usize::try_from(required(3, "door direction")?)
                    .context("negative door direction")?,
                state: required(4, "door state")?,
            }),
            _ => None,
        };
        Ok(parsed)
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
                let flags = match parts.get(1) {
                    Some(raw) => u32::try_from(Self::asciiflag_conv(raw)?)
                        .context("room flags exceed the u32 storage range")?,
                    None => 0,
                };
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
                                if lt.starts_with('T') {
                                    let trigger_vnum: i32 = Self::numeric_field(
                                        Some(lt[1..].trim()),
                                        0,
                                        "room trigger vnum",
                                    )?;
                                    crate::dg_db_scripts::parse_trigger_line(
                                        2,
                                        vnum,
                                        &format!("T {trigger_vnum}"),
                                    );
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
                            let raw_dir: i32 = Self::numeric_field(
                                Some(line[1..].trim()),
                                NUM_OF_DIRS as i32,
                                "room exit direction",
                            )?;
                            let dir =
                                usize::try_from(raw_dir).context("negative room exit direction")?;
                            // Read exit description
                            let exit_desc = Self::read_tilde_buf(&mut reader)?;
                            // Read keywords
                            let keywords = Self::read_tilde_buf(&mut reader)?;
                            // Read door info: exit_info key to_room
                            line.clear();
                            reader.read_line(&mut line)?;
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            let raw_flag: i32 =
                                Self::numeric_field(parts.first().copied(), 0, "room exit flag")?;
                            let key =
                                Self::numeric_field(parts.get(1).copied(), -1, "room exit key")?;
                            let to_room = Self::numeric_field(
                                parts.get(2).copied(),
                                0,
                                "room exit destination",
                            )?;
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
                            let raw_flag: i32 = Self::numeric_field(
                                parts.first().copied(),
                                0,
                                "special exit flag",
                            )?;
                            let key =
                                Self::numeric_field(parts.get(1).copied(), -1, "special exit key")?;
                            let to_room = Self::numeric_field(
                                parts.get(2).copied(),
                                0,
                                "special exit destination",
                            )?;
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
        let first_word = desc.split_whitespace().next().unwrap_or("").to_lowercase();
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
        let act_flags = i64::try_from(Self::asciiflag_conv(flag_parts[0])?)
            .context("mobile action flags exceed the i64 storage range")?
            | crate::flags::MOB_ISNPC;
        let affect_flags = i64::try_from(Self::asciiflag_conv(flag_parts[1])?)
            .context("mobile affect flags exceed the i64 storage range")?;
        let alignment: i32 = Self::numeric_field(Some(flag_parts[2]), 0, "mobile alignment")?;
        let letter = flag_parts[3]
            .chars()
            .next()
            .unwrap_or('S')
            .to_ascii_uppercase();
        if !matches!(letter, 'S' | 'E') {
            return Err(anyhow!("unsupported mobile type {letter:?}"));
        }

        // Stats line: either classic (9 numbers with dice) or X-prefixed
        // (DeltaMUD extended power/mpower/defense/mdefense/technique).
        let stats_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing stats line"))?;
        let stats = Self::stat_numbers(stats_line)?;
        // The on-disk level is an i32 in C while Character stores a byte-sized
        // level. Preserve the established loader policy by explicitly
        // clamping syntactically valid values instead of allowing a cast to
        // wrap.
        let level = stats[0].clamp(0, 200) as u8;
        let xstats = Self::parse_combat_stats(stats_line, &stats);

        // Gold + experience line.
        let ge_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing gold/exp line"))?;
        let ge: Vec<&str> = ge_line.split_whitespace().collect();
        let gold: i32 = Self::numeric_field(ge.first().copied(), 0, "mobile gold")?;
        let experience: i64 = Self::numeric_field(ge.get(1).copied(), 100, "mobile experience")?;

        // Position / default_pos / sex.
        let pos_line = Self::next_content_line(lines, i)
            .ok_or_else(|| anyhow::anyhow!("missing position line"))?;
        let pos_parts: Vec<&str> = pos_line.split_whitespace().collect();
        // Position and sex enums retain the established documented clamps,
        // but a present malformed/overflowing token is a record error.
        let position: i32 = Self::numeric_field(pos_parts.first().copied(), 8, "mobile position")?;
        let default_pos: i32 =
            Self::numeric_field(pos_parts.get(1).copied(), 8, "mobile default position")?;
        let sex: i32 = Self::numeric_field(pos_parts.get(2).copied(), 0, "mobile sex")?;
        let position = position.clamp(0, 9) as u8;
        let default_pos = default_pos.clamp(0, 9) as u8;
        let sex = sex.clamp(0, 2) as u8;

        // Hitpoints + damage dice from the stats line, format-aware.
        let ((hp_nd, hp_sd, hp_bonus), damnodice, damsizedice) =
            Self::parse_mob_dice(stats_line, &stats);

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
                Self::interpret_espec(t, &mut ab, &mut attack_type)?;
            }
            abilities = Some(ab);
        }

        // Attach DG triggers declared by trailing 'T <vnum>' lines (MOB_TRIGGER=0).
        while *i < lines.len() {
            let t = lines[*i].trim();
            if !t.starts_with('T') {
                break;
            }
            let trigger_vnum: i32 =
                Self::numeric_field(Some(t[1..].trim()), 0, "mobile trigger vnum")?;
            crate::dg_db_scripts::parse_trigger_line(0, vnum, &format!("T {trigger_vnum}"));
            *i += 1;
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
    fn interpret_espec(
        line: &str,
        ab: &mut crate::character::Abilities,
        attack_type: &mut i32,
    ) -> Result<()> {
        let (key, val) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (line.trim(), ""),
        };
        if !matches!(
            key,
            "BareHandAttack" | "Str" | "StrAdd" | "Int" | "Wis" | "Dex" | "Con" | "Cha"
        ) {
            warn!("unrecognized espec keyword {:?}", key);
            return Ok(());
        }
        let num: i32 = Self::numeric_field(Some(val), 0, "mobile espec value")
            .with_context(|| format!("espec {key:?}"))?;
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
            _ => {}
        }
        Ok(())
    }

    /// Parse the DeltaMUD `X`-format combat stats from a stats line. Returns
    /// (power, mpower, defense, mdefense, technique); all zero for the classic
    /// (non-X) format. Mirrors parse_simple_mob's X branch in db.c.
    fn parse_combat_stats(stats_line: &str, nums: &[i32]) -> (i16, i16, i16, i16, i16) {
        let trimmed = stats_line.trim();
        if !trimmed.starts_with('X') && !trimmed.starts_with('x') {
            return (0, 0, 0, 0, 0);
        }
        // nums[0] is level; combat stats follow.
        // These fields are `sh_int` in the C oracle; explicitly clamp after
        // checked i32 parsing instead of letting a narrowing cast wrap.
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
    fn parse_mob_dice(stats_line: &str, nums: &[i32]) -> ((i32, i32, i32), i32, i32) {
        let trimmed = stats_line.trim();
        let g = |idx: usize| -> i32 { nums.get(idx).copied().unwrap_or(0) };
        if trimmed.starts_with('X') || trimmed.starts_with('x') {
            // t0=lvl t1..t5=combat t6=hit t7=mana t8=move t9=damnodice t10=damsizedice
            ((g(6), g(7), g(8)), g(9).max(1), g(10).max(1))
        } else {
            // t0=lvl t1=thac0 t2=ac t3=hit t4=mana t5=move t6=damnodice t7=damsizedice t8=damroll
            ((g(3), g(4), g(5)), g(6).max(1), g(7).max(1))
        }
    }

    /// Flatten a stats line into a list of integers, splitting dice tokens on
    /// 'd'/'+' and dropping a leading 'X'/'x' marker. So `X5 1 2 3d4+5` yields
    /// [5,1,2,3,4,5]. Every component must be a valid i32 and the flattened
    /// field count must match the C `sscanf` format; invalid pieces cannot be
    /// dropped and shift later fields into their place.
    fn stat_numbers(stats_line: &str) -> Result<Vec<i32>> {
        let trimmed = stats_line.trim();
        let extended = trimmed.starts_with('X') || trimmed.starts_with('x');
        let mut out = Vec::new();
        for (idx, tok) in trimmed.split_whitespace().enumerate() {
            let tok = if idx == 0 && extended {
                // `X` is ASCII, so this one-byte prefix slice is a UTF-8
                // boundary even when later content is non-ASCII.
                &tok[1..]
            } else {
                tok
            };
            for piece in tok.split(['d', '+']) {
                if piece.is_empty() {
                    return Err(anyhow!("empty numeric component in mobile stats {tok:?}"));
                }
                out.push(Self::numeric_field(
                    Some(piece),
                    0i32,
                    "mobile stats component",
                )?);
            }
        }
        let expected = if extended { 11 } else { 9 };
        if out.len() != expected {
            return Err(anyhow!(
                "mobile stats have {} numeric fields, expected {expected}",
                out.len()
            ));
        }
        Ok(out)
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
    fn asciiflag_conv(flag: &str) -> Result<u64> {
        let flag = flag.trim();
        if flag.is_empty() {
            return Err(anyhow!("empty flag field"));
        }
        if flag.bytes().all(|byte| byte.is_ascii_digit()) {
            return flag
                .parse::<u64>()
                .with_context(|| format!("numeric flag {flag:?} is outside the u64 range"));
        }
        let mut bits = 0u64;
        for c in flag.chars() {
            if c.is_ascii_lowercase() {
                bits |= 1 << (c as u64 - 'a' as u64);
            } else if c.is_ascii_uppercase() {
                bits |= 1 << (26 + c as u64 - 'A' as u64);
            } else {
                return Err(anyhow!("invalid character {c:?} in flag field {flag:?}"));
            }
        }
        Ok(bits)
    }

    /// Object files persist `0` for Generic and `1..=NUM_CLASSES` for the
    /// concrete classes. Anything else used to survive as an invalid internal
    /// index and could later panic OEDIT autoset. Keep the record loadable, but
    /// normalize the malformed class to Generic with a diagnostic.
    fn normalize_object_class(raw: &str, vnum: ObjVnum) -> i32 {
        match raw.parse::<i32>() {
            Ok(value) if (0..=crate::class::NUM_CLASSES as i32).contains(&value) => value - 1,
            Ok(value) => {
                warn!(
                    "object #{} has invalid persisted class {}; using Generic",
                    vnum, value
                );
                -1
            }
            Err(error) => {
                warn!(
                    "object #{} has invalid persisted class {:?}: {}; using Generic",
                    vnum, raw, error
                );
                -1
            }
        }
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
            let obj_type: i32 = Self::numeric_field(parts.first().copied(), 12, "object type")?;
            let extra_flags = match parts.get(1) {
                Some(raw) => Self::asciiflag_conv(raw).context("object extra flags")?,
                None => 0,
            };
            let wear_flags = match parts.get(2) {
                Some(raw) => u32::try_from(Self::asciiflag_conv(raw).context("object wear flags")?)
                    .context("object wear flags exceed the u32 storage range")?,
                None => 1,
            };

            // values line: up to 6 numbers. value[0..4] are obj values;
            // value[4]/value[5] become curr_slots/total_slots when in 0..=100.
            line.clear();
            reader.read_line(&mut line)?;
            let vparts: Vec<&str> = line.split_whitespace().collect();
            let mut values = [0i32; 4];
            for (i, v) in values.iter_mut().enumerate() {
                *v = Self::numeric_field(vparts.get(i).copied(), 0, &format!("object value {i}"))?;
            }
            let v4: i32 = Self::numeric_field(vparts.get(4).copied(), 0, "object current slots")?;
            let v5: i32 = Self::numeric_field(vparts.get(5).copied(), 0, "object total slots")?;
            let (curr_slots, total_slots) = if (0..=100).contains(&v4) && (0..=100).contains(&v5) {
                (v4, v5)
            } else {
                (0, 0)
            };

            // weight, cost, rent.
            line.clear();
            reader.read_line(&mut line)?;
            let wparts: Vec<&str> = line.split_whitespace().collect();
            let weight = Self::numeric_field(wparts.first().copied(), 1, "object weight")?;
            let cost = Self::numeric_field(wparts.get(1).copied(), 0, "object cost")?;
            let rent = Self::numeric_field(wparts.get(2).copied(), 0, "object rent")?;

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
                            let location = Self::numeric_field(
                                ap.first().copied(),
                                0,
                                "object affect location",
                            )?;
                            let modifier = Self::numeric_field(
                                ap.get(1).copied(),
                                0,
                                "object affect modifier",
                            )?;
                            affects.push(crate::object::ObjectAffect { location, modifier });
                        }
                    }
                    'c' => {
                        // On disk 0 is Generic and 1..=5 are the PC classes.
                        obj_class =
                            Self::normalize_object_class(line.trim_start()[1..].trim(), vnum);
                    }
                    'L' => {
                        min_level = Self::numeric_field(
                            Some(line.trim_start()[1..].trim()),
                            0,
                            "object minimum level",
                        )?;
                    }
                    'B' => {
                        // Only 'BV' is meaningful (affect bitvector).
                        let body = line.trim_start();
                        if body.as_bytes().get(1) == Some(&b'V') {
                            bitvector = Self::numeric_field(
                                Some(body[2..].trim()),
                                0,
                                "object affect bitvector",
                            )?;
                        }
                    }
                    'T' => {
                        // Object DG trigger (kind = OBJ_TRIGGER = 1).
                        let lt = line.trim();
                        let trigger_vnum: i32 =
                            Self::numeric_field(Some(lt[1..].trim()), 0, "object trigger vnum")?;
                        crate::dg_db_scripts::parse_trigger_line(
                            1,
                            vnum,
                            &format!("T {trigger_vnum}"),
                        );
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

#[cfg(test)]
mod tests {
    use super::FileLoader;
    use crate::config::Config;
    use crate::state::GameState;
    use crate::world::{MAX_ZONE_NUMBER, ResetCmd, zone_vnum_bounds};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture_path(label: &str, contents: &str) -> PathBuf {
        let serial = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "deltamud-file-loader-{}-{}-{label}",
            std::process::id(),
            serial
        ));
        std::fs::write(&path, contents).expect("write loader fixture");
        path
    }

    fn load_mobile_fixture(contents: &str) -> GameState {
        let path = fixture_path("mobile.mob", contents);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_mobile_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        result.expect("mobile fixture should remain loadable");
        world
    }

    fn object_record(
        vnum: i32,
        type_flags: &str,
        values: &str,
        weight_cost_rent: &str,
        extensions: &str,
    ) -> String {
        format!(
            "#{vnum}\népée blade~\nan épée~\népée rests here.~\n动作说明~\n{type_flags}\n{values}\n{weight_cost_rent}\n{extensions}"
        )
    }

    #[test]
    fn checked_i32_bounds_reject_overflow_instead_of_aliasing_to_default() {
        assert_eq!(
            FileLoader::numeric_field::<i32>(Some("-2147483648"), 7, "test").unwrap(),
            i32::MIN
        );
        assert_eq!(
            FileLoader::numeric_field::<i32>(Some("2147483647"), 7, "test").unwrap(),
            i32::MAX
        );
        for raw in ["-2147483649", "2147483648", "999999999999999999999"] {
            assert!(
                FileLoader::numeric_field::<i32>(Some(raw), 7, "test").is_err(),
                "{raw} must not become the default"
            );
        }
        assert_eq!(
            FileLoader::numeric_field::<i32>(None, 7, "test").unwrap(),
            7
        );
        assert_eq!(FileLoader::asciiflag_conv("aZ").unwrap(), 1 | (1 << 51));
        for raw in ["18446744073709551616", "a?", "é"] {
            assert!(FileLoader::asciiflag_conv(raw).is_err(), "bad flag {raw}");
        }
        assert!(FileLoader::parse_reset_command("D 0 10 -1 0").is_err());
    }

    #[test]
    fn real_mobile_loader_rejects_bad_numeric_records_without_field_shifting() {
        let fixture = r#"
#99
bad flags~
a bad flags mob~
Bad flags waits here.~
Bad flags.~
9223372036854775808 0 0 S
X1 0 0 0 0 0 1d1+1 1d1
0 0
9 9 0
#100
bad alignment~
a bad alignment~
Bad alignment waits here.~
Bad alignment.~
0 0 2147483648 S
X1 0 0 0 0 0 1d1+1 1d1
0 0
9 9 0
#101
bad stats~
a bad stats mob~
Bad stats waits here.~
Bad stats.~
0 0 0 S
X1 0 invalid 0 0 0 1d1+1 1d1
0 0
9 9 0
#102
bad gold~
a bad gold mob~
Bad gold waits here.~
Bad gold.~
0 0 0 S
X1 0 0 0 0 0 1d1+1 1d1
not-a-number 123
9 9 0
#103
bad position~
a bad position mob~
Bad position waits here.~
Bad position.~
0 0 0 S
X1 0 0 0 0 0 1d1+1 1d1
0 0
9 2147483648 0
#104
bad espec~
a bad espec mob~
Bad espec waits here.~
Bad espec.~
0 0 0 E
X1 0 0 0 0 0 1d1+1 1d1
0 0
9 9 0
Str: 2147483648
E
#105
bad trigger~
a bad trigger mob~
Bad trigger waits here.~
Bad trigger.~
0 0 0 S
X1 0 0 0 0 0 1d1+1 1d1
0 0
9 9 0
T 2147483648
#106
gardien épée~
an élite gardien~
Élite gardien waits here.~
守護者の説明。~
a b 5 E
X7 1 2 3 4 5 2d8+9 3d4
2147483647 9223372036854775807
9 9 1
Str: 25
E
$~
"#;

        let world = load_mobile_fixture(fixture);
        assert_eq!(world.mob_protos.len(), 1);
        let mob = world.mob_protos.get(&106).expect("valid mobile retained");
        assert_eq!(mob.name, "gardien épée");
        assert_eq!(mob.description, "守護者の説明。");
        assert_eq!(mob.gold, i32::MAX);
        assert_eq!(mob.experience, i64::MAX);
        assert_eq!(mob.level, 7);
        assert_eq!(mob.hit_dice, (2, 8, 9));
        assert_eq!(
            (mob.power, mob.mpower, mob.defense, mob.mdefense),
            (1, 2, 3, 4)
        );
    }

    #[test]
    fn real_object_loader_rejects_present_invalid_numeric_fields() {
        let cases = [
            ("type", "2147483648 0 1", "0 0 0 0 0 0", "1 0 0", ""),
            (
                "extra flag overflow",
                "12 18446744073709551616 1",
                "0 0 0 0 0 0",
                "1 0 0",
                "",
            ),
            (
                "wear flag overflow",
                "12 0 4294967296",
                "0 0 0 0 0 0",
                "1 0 0",
                "",
            ),
            ("value", "12 0 1", "invalid 0 0 0 0 0", "1 0 0", ""),
            ("slot", "12 0 1", "0 0 0 0 2147483648 0", "1 0 0", ""),
            ("weight", "12 0 1", "0 0 0 0 0 0", "invalid 0 0", ""),
            ("affect", "12 0 1", "0 0 0 0 0 0", "1 0 0", "A\n0 invalid\n"),
            ("level", "12 0 1", "0 0 0 0 0 0", "1 0 0", "L invalid\n"),
            (
                "bitvector",
                "12 0 1",
                "0 0 0 0 0 0",
                "1 0 0",
                "BV 9223372036854775808\n",
            ),
            (
                "trigger",
                "12 0 1",
                "0 0 0 0 0 0",
                "1 0 0",
                "T 2147483648\n",
            ),
        ];

        for (label, type_flags, values, weight, extensions) in cases {
            let contents = format!(
                "{}$~\n",
                object_record(200, type_flags, values, weight, extensions)
            );
            let path = fixture_path(label, &contents);
            let mut world = GameState::new(Config::default());
            let result = FileLoader::load_object_file(&mut world, &path);
            let _ = std::fs::remove_file(path);
            assert!(result.is_err(), "{label} should reject the object record");
            assert!(
                !world.obj_protos.contains_key(&200),
                "{label} must not publish a partial object"
            );
        }
    }

    #[test]
    fn real_object_loader_normalizes_only_invalid_persisted_classes() {
        let persisted = [
            (300, "0", -1),
            (301, "1", 0),
            (302, "2", 1),
            (303, "3", 2),
            (304, "4", 3),
            (305, "5", 4),
            (306, "9", -1),
            (307, "-1", -1),
            (308, "2147483648", -1),
        ];
        let mut contents = String::new();
        for (vnum, raw_class, _) in persisted {
            contents.push_str(&object_record(
                vnum,
                "12 a b",
                "1 2 3 4 5 6",
                "7 8 9",
                &format!("c {raw_class}\n"),
            ));
        }
        contents.push_str("$~\n");

        let path = fixture_path("object-classes.obj", &contents);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_object_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        result.expect("invalid class values should normalize without losing the object");

        for (vnum, _, expected) in persisted {
            let object = world.obj_protos.get(&vnum).expect("object loaded");
            assert_eq!(object.obj_class, expected, "object #{vnum}");
            assert_eq!(object.name, "épée blade");
            assert_eq!(object.action_description, "动作说明");
        }
    }

    #[test]
    fn real_zone_loader_rejects_overflowing_fields_and_bad_reset_only() {
        let bad_header = "#1\nZone~\nBuilders~\n2147483648 30 2\n0 50 0\nS\n$\n";
        let path = fixture_path("bad-zone-header.zon", bad_header);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_zone_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        assert!(result.is_err());
        assert!(world.zones.is_empty());

        let mixed = concat!(
            "#2\nZone Unicode Ω~\nBuilders~\n299 30 2\n0 50 0\n",
            "M 0 2147483648 1 200\n",
            "M 0 10 1 200 100\n",
            "S\n$\n"
        );
        let path = fixture_path("mixed-zone.zon", mixed);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_zone_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        result.expect("one rejected reset must not discard the zone");
        assert_eq!(world.zones.len(), 1);
        assert_eq!(world.zones[0].name, "Zone Unicode Ω");
        assert_eq!(world.zones[0].reset_commands.len(), 1);
        assert!(matches!(
            world.zones[0].reset_commands[0],
            ResetCmd::LoadMob { mob_vnum: 10, .. }
        ));
    }

    #[test]
    fn real_zone_loader_enforces_checked_zone_number_bounds() {
        for number in [0, MAX_ZONE_NUMBER] {
            let (_, top) = zone_vnum_bounds(number).expect("boundary zone must be valid");
            let contents =
                format!("#{number}\nBoundary Zone~\nBuilders~\n{top} 30 2\n0 50 0\nS\n$\n");
            let path = fixture_path("zone-number-boundary.zon", &contents);
            let mut world = GameState::new(Config::default());
            let result = FileLoader::load_zone_file(&mut world, &path);
            let _ = std::fs::remove_file(path);

            result.expect("valid boundary zone should load");
            assert_eq!(world.zones.len(), 1);
            assert_eq!(world.zones[0].number, number);
            assert_eq!(
                world.zones[0].vnum_start(),
                zone_vnum_bounds(number).map(|(first, _)| first)
            );
        }

        // The first value is just beyond the C persistence contract.  The
        // latter values exercise numbers whose hundred-vnum calculations
        // would overflow even though the zone header itself still fits i32,
        // plus an integer that does not fit the parsed type at all.
        for raw_number in [
            (MAX_ZONE_NUMBER + 1).to_string(),
            "21474836".to_string(),
            i32::MAX.to_string(),
            "2147483648".to_string(),
            "-1".to_string(),
        ] {
            let contents = format!(
                "#{raw_number}\nRejected Zone~\nBuilders~\n{} 30 2\n0 50 0\nS\n$\n",
                i32::MAX
            );
            let path = fixture_path("bad-zone-number.zon", &contents);
            let mut world = GameState::new(Config::default());
            let result = FileLoader::load_zone_file(&mut world, &path);
            let _ = std::fs::remove_file(path);

            assert!(result.is_err(), "zone #{raw_number} must be rejected");
            assert!(
                world.zones.is_empty(),
                "zone #{raw_number} must not publish a partial Zone"
            );
        }
    }

    #[test]
    fn real_room_loader_preserves_utf8_and_rejects_flag_overflow() {
        let valid = "#10\nÉcole du monde~\n四文字の説明~\n0 a 0\nS\n$\n";
        let path = fixture_path("unicode-room.wld", valid);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_room_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        result.expect("valid Unicode room should load");
        assert_eq!(world.rooms.len(), 1);
        assert_eq!(world.rooms[0].name, "École du monde");
        assert_eq!(world.rooms[0].description, "四文字の説明");

        let overflow = "#11\nOverflow room~\nRejected description~\n0 4294967296 0\nS\n$\n";
        let path = fixture_path("overflow-room.wld", overflow);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_room_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        assert!(result.is_err());
        assert!(world.rooms.is_empty());

        let trigger_overflow = concat!(
            "#12\nTrigger room~\nRejected description~\n0 0 0\nS\n",
            "T 2147483648\n$\n"
        );
        let path = fixture_path("overflow-room-trigger.wld", trigger_overflow);
        let mut world = GameState::new(Config::default());
        let result = FileLoader::load_room_file(&mut world, &path);
        let _ = std::fs::remove_file(path);
        assert!(result.is_err());
        assert!(world.rooms.is_empty());
    }

    #[test]
    fn shipped_mob_and_object_files_have_no_numeric_record_rejections() {
        let world_root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../lib/world"));
        if !world_root.join("mob/index").exists() {
            return;
        }

        for kind in ["mob", "obj"] {
            let dir = world_root.join(kind);
            let index = std::fs::read_to_string(dir.join("index")).expect("read world index");
            for filename in index.lines().map(str::trim).take_while(|line| *line != "$") {
                if filename.is_empty() {
                    continue;
                }
                let path = dir.join(filename);
                let contents = std::fs::read_to_string(&path).expect("read world data file");
                let expected: HashSet<i32> = contents
                    .lines()
                    .filter_map(|line| line.trim().strip_prefix('#'))
                    .filter_map(|raw| raw.parse::<i32>().ok())
                    .collect();
                let mut world = GameState::new(Config::default());
                if kind == "mob" {
                    FileLoader::load_mobile_file(&mut world, &path).expect("read mobile file");
                    assert_eq!(
                        world.mob_protos.len(),
                        expected.len(),
                        "{filename} contains a mobile record rejected by checked parsing"
                    );
                } else {
                    FileLoader::load_object_file(&mut world, &path).unwrap_or_else(|error| {
                        panic!("{filename} failed checked parsing: {error:#}")
                    });
                    assert_eq!(
                        world.obj_protos.len(),
                        expected.len(),
                        "{filename} contains an object record rejected by checked parsing"
                    );
                }
            }
        }
    }
}
