//! do_stat + the per-target stat renderers (room / object / character).
//!
//! Split out of cmd_wizard.rs (phase 6); `use super::*` inherits the
//! module's imports and private helpers.

use super::*;

pub(crate) fn do_stat_room(g: &mut GameState, ch: CharId) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    };
    let room_vnum = g.room(rnum).number;
    if authority.authority < i32::from(LVL_IMMORT) && !can_edit_zone(g, ch, real_zone(g, room_vnum))
    {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }

    let (name, sector, vnum, zone_num, light, flags, has_desc, desc, exits) = {
        let rm = g.room(rnum);
        let zone_num = g
            .zones
            .get(rm.zone as usize)
            .map(|z| z.number)
            .unwrap_or(rm.zone);
        (
            rm.name.clone(),
            rm.sector_type as i32,
            rm.number,
            zone_num,
            rm.light,
            rm.room_flags.bits() as i64,
            !rm.description.is_empty(),
            rm.description.clone(),
            rm.exits.clone(),
        )
    };

    g.send_to_char(ch, &format!("Room name: &c{}&n\r\n", name));
    let sectstr = sprinttype(sector, constants::SECTOR_TYPES);
    g.send_to_char(
        ch,
        &format!(
            "Zone: [{:3}], VNum: [&g{:5}&n], RNum: [{:5}], Type: {} Light: [{:2}]\r\n",
            zone_num, vnum, rnum, sectstr, light
        ),
    );
    let flagstr = sprintbit(flags, constants::ROOM_BITS);
    // C act.wizard.c:473-474: (rm->func == NULL) ? "None" : "Exists".
    let room_spec = crate::spec_assign::get_room_spec(g, room_vnum).is_some();
    g.send_to_char(
        ch,
        &format!(
            "SpecProc: {}, Flags: {}\r\n",
            if room_spec { "Exists" } else { "None" },
            flagstr
        ),
    );

    g.send_to_char(ch, "Description:\r\n");
    if has_desc {
        g.send_to_char(ch, &desc);
    } else {
        g.send_to_char(ch, "  None.\r\n");
    }

    // Extra descs.
    let extra: Vec<String> = g
        .room(rnum)
        .extra_descriptions
        .iter()
        .map(|(k, _)| k.clone())
        .collect();
    if !extra.is_empty() {
        let mut line = String::from("Extra descs:&c");
        for k in &extra {
            line.push(' ');
            line.push_str(k);
        }
        line.push_str("&n\r\n");
        g.send_to_char(ch, &line);
    }

    // Chars present.
    let people = g.room(rnum).people.clone();
    let mut buf = String::from("Chars present:&y");
    let mut found = 0;
    let mut idx = 0usize;
    let total = people.len();
    for k in people {
        if !g.can_see(ch, k) {
            idx += 1;
            continue;
        }
        let kind = if !is_npc(g, k) {
            "PC"
        } else if g.get_char(k).map(|c| c.nr == NOBODY).unwrap_or(true) {
            "NPC"
        } else {
            "MOB"
        };
        let nm = name_of(g, k);
        buf.push_str(&format!(
            "{} {}({})",
            if found > 0 { "," } else { "" },
            nm,
            kind
        ));
        found += 1;
        if buf.len() >= 62 {
            if idx + 1 < total {
                buf.push_str(",\r\n");
            } else {
                buf.push_str("\r\n");
            }
            g.send_to_char(ch, &buf);
            buf.clear();
            found = 0;
        }
        idx += 1;
    }
    if !buf.is_empty() {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }
    g.send_to_char(ch, "&n");

    // Contents.
    let contents = g.room(rnum).contents.clone();
    if !contents.is_empty() {
        let mut buf = String::from("Contents:&g");
        let mut found = 0;
        let mut idx = 0usize;
        let total = contents.len();
        for j in contents {
            if !can_see_obj(g, ch, j) {
                idx += 1;
                continue;
            }
            let short = g
                .get_obj(j)
                .map(|o| o.short_description.clone())
                .unwrap_or_default();
            buf.push_str(&format!("{} {}", if found > 0 { "," } else { "" }, short));
            found += 1;
            if buf.len() >= 62 {
                if idx + 1 < total {
                    buf.push_str(",\r\n");
                } else {
                    buf.push_str("\r\n");
                }
                g.send_to_char(ch, &buf);
                buf.clear();
                found = 0;
            }
            idx += 1;
        }
        if !buf.is_empty() {
            buf.push_str("\r\n");
            g.send_to_char(ch, &buf);
        }
        g.send_to_char(ch, "&n");
    }

    // Exits.
    for (i, exo) in exits.iter().enumerate() {
        if let Some(ex) = exo {
            let to_str = if ex.to_room == NOWHERE {
                " &cNONE&n".to_string()
            } else {
                format!("&c{:5}&n", ex.to_room)
            };
            let exinfo = sprintbit(ex.exit_info as i64, constants::EXIT_BITS);
            let kw = ex.keyword.clone().unwrap_or_else(|| "None".to_string());
            g.send_to_char(
                ch,
                &format!(
                    "Exit &c{:<5}&n:  To: [{}], Key: [{:5}], Keywrd: {}, Type: {}\r\n ",
                    DIR_NAMES[i], to_str, ex.key, kw, exinfo
                ),
            );
            match &ex.description {
                Some(d) if !d.is_empty() => g.send_to_char(ch, d),
                _ => g.send_to_char(ch, "  No exit description.\r\n"),
            }
        }
    }
    // do_sstat_room: DG-script room trigger listing.
    do_sstat(g, ch, ScriptKey::Room(rnum));
}

pub(crate) fn do_stat_object(g: &mut GameState, ch: CharId, j: ObjId) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    };
    let obj_vnum = g.get_obj(j).map(|o| o.item_number).unwrap_or(NOTHING);
    if authority.authority < i32::from(LVL_IMMORT) && !can_edit_zone(g, ch, real_zone(g, obj_vnum))
    {
        g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
        return;
    }
    let (
        vnum,
        short,
        namelist,
        ldesc,
        otype,
        wear,
        bitvector,
        extra,
        weight,
        cost,
        rent,
        timer,
        minlvl,
        loc,
        contained_in,
        carried_by,
        worn_by,
        values,
        contains,
        affects,
        curr_slots,
        total_slots,
    ) = {
        let o = match g.get_obj(j) {
            Some(o) => o,
            None => return,
        };
        (
            o.item_number,
            if o.short_description.is_empty() {
                "<None>".to_string()
            } else {
                o.short_description.clone()
            },
            o.name.clone(),
            if o.description.is_empty() {
                "None".to_string()
            } else {
                o.description.clone()
            },
            o.obj_type as i32,
            o.wear_flags.bits() as i64,
            o.bitvector,
            o.extra_flags.bits() as i64,
            o.weight,
            o.cost,
            o.rent,
            o.timer,
            o.level,
            o.loc,
            match o.loc {
                ObjLoc::Contained(c) => Some(c),
                _ => None,
            },
            match o.loc {
                ObjLoc::Carried(c) => Some(c),
                _ => None,
            },
            match o.loc {
                ObjLoc::Worn(c, _) => Some(c),
                _ => None,
            },
            o.values,
            o.contains.clone(),
            o.affects.clone(),
            o.curr_slots,  // GET_OBJ_CSLOTS
            o.total_slots, // GET_OBJ_TSLOTS
        )
    };

    g.send_to_char(
        ch,
        &format!("Name: '&y{}&n', Aliases: {}\r\n", short, namelist),
    );
    let typestr = sprinttype(otype, constants::ITEM_TYPES);
    // C act.wizard.c:605-610: obj_index[GET_OBJ_RNUM(j)].func ? "Exists" : "None".
    let obj_spec = crate::spec_assign::get_obj_spec(g, vnum).is_some();
    let rnum = if obj_spec { "Exists" } else { "None" };
    g.send_to_char(
        ch,
        &format!(
            "VNum: [&g{:5}&n], RNum: [{:5}], Type: {}, SpecProc: {}\r\n",
            vnum, -1, typestr, rnum
        ),
    );
    g.send_to_char(ch, &format!("L-Des: {}\r\n", ldesc));

    g.send_to_char(ch, "Can be worn on: ");
    g.send_to_char(
        ch,
        &format!("{}\r\n", sprintbit(wear, constants::WEAR_BITS)),
    );
    g.send_to_char(ch, "Set char bits : ");
    g.send_to_char(
        ch,
        &format!("{}\r\n", sprintbit(bitvector, constants::AFFECTED_BITS)),
    );
    g.send_to_char(ch, "Extra flags   : ");
    g.send_to_char(
        ch,
        &format!("{}\r\n", sprintbit(extra, constants::EXTRA_BITS)),
    );

    g.send_to_char(
        ch,
        &format!(
            "Weight: {}, Value: {}, Cost/day: {}, Timer: {} Level: {}\r\n",
            weight, cost, rent, timer, minlvl
        ),
    );

    let mut line = String::from("In room: ");
    match loc {
        ObjLoc::Room(r) => line.push_str(&g.room(r).number.to_string()),
        _ => line.push_str("Nowhere"),
    }
    line.push_str(", In object: ");
    line.push_str(&match contained_in {
        Some(c) => g
            .get_obj(c)
            .map(|o| o.short_description.clone())
            .unwrap_or_else(|| "None".to_string()),
        None => "None".to_string(),
    });
    line.push_str(", Carried by: ");
    line.push_str(&match carried_by {
        Some(c) => name_of(g, c),
        None => "Nobody".to_string(),
    });
    line.push_str(", Worn by: ");
    line.push_str(&match worn_by {
        Some(c) => name_of(g, c),
        None => "Nobody".to_string(),
    });
    line.push_str("\r\n");
    g.send_to_char(ch, &line);

    // Type-specific values block.
    let detail = match ObjectType::from_i32(otype) {
        ObjectType::Light => {
            if values[2] == -1 {
                "Hours left: Infinite".to_string()
            } else {
                format!("Hours left: [{}]", values[2])
            }
        }
        ObjectType::Scroll | ObjectType::Potion => format!(
            "Spells: (Level {}) {}, {}, {}",
            values[0],
            skill_name(values[1]),
            skill_name(values[2]),
            skill_name(values[3])
        ),
        ObjectType::Wand | ObjectType::Staff => format!(
            "Spell: {} at level {}, {} (of {}) charges remaining",
            skill_name(values[3]),
            values[0],
            values[2],
            values[1]
        ),
        ObjectType::Weapon => format!(
            "Todam: {}d{} (avg-dmg {:.1}), Message type: {}",
            values[1],
            values[2],
            ((values[2] + 1) as f64 / 2.0) * values[1] as f64,
            values[3]
        ),
        ObjectType::Armor => format!("Defense-app: [{}]", values[0]),
        ObjectType::Container => format!(
            "Weight capacity: {}, Lock Type: {}, Key Num: {}, Corpse: {}",
            values[0],
            sprintbit(values[1] as i64, constants::CONTAINER_BITS),
            values[2],
            yesno(values[3] != 0)
        ),
        ObjectType::LiqContainer | ObjectType::Fountain => format!(
            "Capacity: {}, Contains: {}, Poisoned: {}, Liquid: {}",
            values[0],
            values[1],
            yesno(values[3] != 0),
            sprinttype(values[2], constants::DRINKS)
        ),
        ObjectType::Note => format!("Tongue: {}", values[0]),
        ObjectType::Key => String::new(),
        ObjectType::Food => format!(
            "Makes full: {}, Poisoned: {}",
            values[0],
            yesno(values[3] != 0)
        ),
        ObjectType::Money => format!("Coins: {}", values[0]),
        _ => format!(
            "Values 0-3: [{}] [{}] [{}] [{}]",
            values[0], values[1], values[2], values[3]
        ),
    };
    // act.wizard.c do_stat_object: "Quality: [%d] [%d]" with
    // GET_OBJ_CSLOTS / GET_OBJ_TSLOTS (obj_flags.curr_slots / total_slots).
    g.send_to_char(
        ch,
        &format!(
            "{}\r\nQuality: [{}] [{}]\r\n",
            detail, curr_slots, total_slots
        ),
    );

    // Contents.
    if !contains.is_empty() {
        let mut buf = String::from("\r\nContents:&g");
        let mut found = 0;
        let mut idx = 0usize;
        let total = contains.len();
        for j2 in contains {
            let short = g
                .get_obj(j2)
                .map(|o| o.short_description.clone())
                .unwrap_or_default();
            buf.push_str(&format!("{} {}", if found > 0 { "," } else { "" }, short));
            found += 1;
            if buf.len() >= 62 {
                if idx + 1 < total {
                    buf.push_str(",\r\n");
                } else {
                    buf.push_str("\r\n");
                }
                g.send_to_char(ch, &buf);
                buf.clear();
                found = 0;
            }
            idx += 1;
        }
        if !buf.is_empty() {
            buf.push_str("\r\n");
            g.send_to_char(ch, &buf);
        }
        g.send_to_char(ch, "&n");
    }

    // Affections.
    g.send_to_char(ch, "Affections:");
    let mut found = 0;
    for a in &affects {
        if a.modifier != 0 {
            let loc = sprinttype(a.location, constants::APPLY_TYPES);
            g.send_to_char(
                ch,
                &format!(
                    "{} {:+} to {}",
                    if found > 0 { "," } else { "" },
                    a.modifier,
                    loc
                ),
            );
            found += 1;
        }
    }
    if found == 0 {
        g.send_to_char(ch, " None");
    }
    g.send_to_char(ch, "\r\n");
    // do_sstat_object: DG-script object trigger listing.
    do_sstat(g, ch, ScriptKey::Obj(j));
}

pub(crate) fn do_stat_character(g: &mut GameState, ch: CharId, k: CharId) {
    let Some(authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "You find yourself unable to.\r\n");
        return;
    };
    if !is_npc(g, k) {
        let Some(target) = exact_player_authority(g, k) else {
            g.send_to_char(ch, PLAYER_INSPECTION_DENIED);
            return;
        };
        if !authorize_player_inspection(g, ch, target.authority) {
            return;
        }
        if authority.authority < i32::from(LVL_IMMORT) {
            g.send_to_char(ch, "You find yourself unable to.\r\n");
            return;
        }
    } else if authority.authority < i32::from(LVL_IMMORT) {
        // Mortal builders may stat a mob whose zone they own (can_edit_zone of
        // real_zone(GET_MOB_VNUM)).
        let mob_vnum = g.get_char(k).map(|c| c.nr).unwrap_or(NOBODY);
        if !can_edit_zone(g, ch, real_zone(g, mob_vnum)) {
            g.send_to_char(ch, "You don't have permissions to that zone.\r\n");
            return;
        }
    }

    // Snapshot everything we need before any send.
    let kc = match g.get_char(k) {
        Some(c) => c,
        None => return,
    };
    let sex = kc.player.sex;
    let npc = kc.is_npc;
    let is_mob = npc && kc.nr != NOBODY;
    let kname = kc.player.name.clone();
    let idnum = kc.idnum;
    let in_room_vnum = kc.in_room.map(|r| g.rooms[r].number).unwrap_or(NOWHERE);
    let loadroom = kc.load_room;
    let mob_vnum = kc.nr;
    let title = kc.player.title.clone();
    let trust = kc.trust;
    // C act.wizard.c:828 prints IS_GOD(k) — the granted-command bits.
    let gcmd = is_god(g, k);
    let long_descr = kc.long_desc.clone();
    let class = kc.player.class as i32;
    let klevel = kc.player.level;
    let exp = kc.points.exp;
    let align = kc.alignment;
    let citizen = kc.citizen as i32; // GET_CITIZEN (Cstat is GET_CITIZEN+1).
    let abils = kc.aff_abils;
    let points = kc.points.clone();
    let position = kc.position as i32;
    let fighting = kc.fighting;
    let default_pos = kc.position as i32; // mob_specials.default_pos not modelled separately
    let timer = kc.timer;
    let act_flags = kc.act_flags;
    let prf_flags = kc.prf_flags;
    let prf2_flags = kc.prf2_flags;
    let affect_flags = kc.affect_flags;
    let carry_weight = kc.carry_weight;
    let carry_items = kc.carry_items;
    let n_inv = kc.carrying.len();
    let n_eq = kc.equipment.iter().flatten().count();
    let conditions = kc.conditions;
    let master = kc.master;
    let followers = kc.followers.clone();
    let affected = kc.affected.clone();
    let connected = kc.desc.is_some();
    // C sprinttype(k->desc->connected, connected_types) — the real state.
    let conn_state = kc
        .desc
        .and_then(|conn| g.descriptors.get(&conn))
        .map(|d| d.state);
    let hometown = kc.player.hometown;
    let talks = kc.talks;
    let clan = kc.clan;
    let time_birth = kc.player.time_birth;
    let time_played = kc.player.time_played.max(0);
    let last_logon = kc.last_logon.timestamp();
    let practices = kc.spells_to_learn;
    let next_quest = kc.next_quest;
    let countdown = kc.quest_countdown;
    let quest_mob = kc.quest_mob;
    let quest_obj = kc.quest_obj;
    let wins = kc.wins;
    let losses = kc.losses;
    let damnodice = g
        .mob_protos
        .get(&mob_vnum)
        .map(|m| m.damnodice)
        .unwrap_or(0);
    let damsizedice = g
        .mob_protos
        .get(&mob_vnum)
        .map(|m| m.damsizedice)
        .unwrap_or(0);
    // C act.wizard.c:1000-1002: mob_index[GET_MOB_RNUM(k)].func ? "Exists" : "None".
    let mob_spec = crate::spec_assign::get_mob_spec(g, mob_vnum).is_some();
    // C act.wizard.c:908: attack_hit_text[k->mob_specials.attack_type].singular.
    let attack_type = g
        .mob_protos
        .get(&mob_vnum)
        .map(|m| m.attack_type)
        .unwrap_or(0);
    let attack_word = constants::ATTACK_HIT_TEXT
        .get(attack_type.clamp(0, constants::ATTACK_HIT_TEXT.len() as i32 - 1) as usize)
        .map(|(s, _)| (*s).to_string())
        .unwrap_or_else(|| "hit".to_string());
    // C act.wizard.c:847-850 / 870-873 / 887-890: MaxWeapon, practices-per,
    // and the hit/mana/move regen rates.
    let maxweapon = constants::LVL_MAXDMG_WEAPON
        .get(klevel as usize)
        .copied()
        .unwrap_or(0);
    let learn_per = constants::INT_APP
        .get((abils.intel as i32).clamp(0, constants::INT_APP.len() as i32 - 1) as usize)
        .map(|a| a.learn)
        .unwrap_or(0);
    let nstl = constants::WIS_APP
        .get((abils.wis as i32).clamp(0, constants::WIS_APP.len() as i32 - 1) as usize)
        .map(|a| a.bonus)
        .unwrap_or(0);
    let (hit_regen, mana_regen, move_regen) = (
        crate::limits::hit_gain(g, k),
        crate::limits::mana_gain(g, k),
        crate::limits::move_gain(g, k),
    );

    let sexstr = match sex {
        Gender::Neutral => "NEUTRAL-SEX",
        Gender::Male => "MALE",
        Gender::Female => "FEMALE",
    };
    let kind = if !npc {
        "PC"
    } else if mob_vnum == NOBODY {
        "NPC"
    } else {
        "MOB"
    };
    let mut hdr = format!(
        "{} {} '{}'  IDNum: [{:5}], In room [{:5}]",
        sexstr, kind, kname, idnum, in_room_vnum
    );
    if !npc {
        hdr.push_str(&format!(", LoadRoom: [{:5}]", loadroom));
    }
    hdr.push_str("\r\n");
    g.send_to_char(ch, &hdr);

    if is_mob {
        g.send_to_char(
            ch,
            &format!(
                "Alias: {}, VNum: [{:5}], RNum: [{:5}]\r\n",
                kname, mob_vnum, -1
            ),
        );
    }

    g.send_to_char(
        ch,
        &format!(
            "Title: {}     Trust: {}     God-Commands: {}",
            title.clone().unwrap_or_else(|| "<None>".to_string()),
            trust,
            if gcmd { "&YYes&n\r\n" } else { "No\r\n" }
        ),
    );

    g.send_to_char(
        ch,
        &format!(
            "L-Des: {}",
            long_descr.unwrap_or_else(|| "<None>\r\n".to_string())
        ),
    );

    let classstr = if npc {
        sprinttype(class, constants::NPC_CLASS_TYPES)
    } else {
        // pc_class_types live in class.c (not surfaced); use abbreviation set.
        match Class::from_u8(class as u8) {
            Class::MagicUser => "Magic User".to_string(),
            Class::Cleric => "Cleric".to_string(),
            Class::Thief => "Thief".to_string(),
            Class::Warrior => "Warrior".to_string(),
            Class::Artisan => "Artisan".to_string(),
        }
    };
    let class_label = if npc { "Monster Class: " } else { "Class: " };
    let lvl_line = if klevel < LVL_IMMORT {
        format!(
            "{}{}, Lev: [&y{:2}&n], XP: [&y{:7}&n], Align: [{:4}], MaxWeapon: [{}], Cstat: [{}]\r\n",
            class_label,
            classstr,
            klevel,
            exp,
            align,
            maxweapon,
            citizen + 1
        )
    } else {
        format!(
            "{}{}, Lev: [&y{:2}&n], XP: [&y{:7}&n], Align: [{:4}], Cstat: [{}]\r\n",
            class_label,
            classstr,
            klevel,
            exp,
            align,
            citizen + 1
        )
    };
    g.send_to_char(ch, &lvl_line);

    if !npc {
        let created = ctime(time_birth);
        let last = ctime(last_logon);
        let played_hours = time_played / 3600;
        let played_minutes = (time_played % 3600) / 60;
        let (age_years, _, _, _) = mud_age_parts(time_birth);
        g.send_to_char(
            ch,
            &format!(
                "Created: [{}], Last Logon: [{}], Played [{}h {}m], Age [{}]\r\n",
                created.chars().take(10).collect::<String>(),
                last.chars().take(10).collect::<String>(),
                played_hours,
                played_minutes,
                age_years
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "Hometown: [{}], Speaks: [{}/{}/{}], (STL[{}]/per[{}]/NSTL[{}]) Clan: [{}]\r\n",
                hometown,
                talks[0] as i32,
                talks[1] as i32,
                talks[2] as i32,
                practices,
                learn_per,
                nstl,
                clan
            ),
        );
    }

    g.send_to_char(
        ch,
        &format!(
            "Str: [&c{}/{}&n]  Int: [&c{}&n]  Wis: [&c{}&n]  Dex: [&c{}&n]  Con: [&c{}&n]  Cha: [&c{}&n]\r\n",
            abils.str, abils.str_add, abils.intel, abils.wis, abils.dex, abils.con, abils.cha
        ),
    );

    g.send_to_char(
        ch,
        &format!(
            "Hit p.:[&g{}/{}+{}&n]  Mana p.:[&g{}/{}+{}&n]  Move p.:[&g{}/{}+{}&n]\r\n",
            points.hit,
            points.max_hit,
            hit_regen,
            points.mana,
            points.max_mana,
            mana_regen,
            points.move_points,
            points.max_move,
            move_regen
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "Coins: [{:9}], Bank: [{:9}] (Total: {})\r\n",
            points.gold,
            points.bank_gold,
            i64::from(points.gold) + i64::from(points.bank_gold)
        ),
    );
    g.send_to_char(
        ch,
        &format!(
            "Defense: [{}], Magic Defense: [{:2}], Power: [{:2}], Magic Power: [{}] Technique: [{:2}]\r\n",
            points.defense, points.mdefense, points.power, points.mpower, points.technique
        ),
    );

    let posstr = sprinttype(position, constants::POSITION_TYPES);
    let mut buf = format!(
        "Pos: {}, Fighting: {}",
        posstr,
        match fighting {
            Some(f) => name_of(g, f),
            None => "Nobody".to_string(),
        }
    );
    if is_mob {
        buf.push_str(&format!(", Attack type: {}", attack_word));
    }
    if let Some(st) = conn_state {
        buf.push_str(&format!(
            ", Connected: {}",
            sprinttype(conn_state_index(st), constants::CONNECTED_TYPES)
        ));
    }
    if !npc {
        // Arena status block (GET_ARENASTAT), matching act.wizard.c do_stat_character.
        let stat = crate::arena::arena_stat(&g, k);
        buf.push_str("\r\nArena: ");
        match stat {
            crate::arena::ARENA_NOT => buf.push_str("[NO]"),
            crate::arena::ARENA_COMBATANT1 => buf.push_str("[COMBAT1]"),
            crate::arena::ARENA_COMBATANT1W => buf.push_str("[COMBAT1W]"),
            crate::arena::ARENA_COMBATANT2 => buf.push_str("[COMBAT2]"),
            crate::arena::ARENA_COMBATANT3 => buf.push_str("[COMBAT3]"),
            crate::arena::ARENA_COMBATANTZ => buf.push_str("[COMBATZ]"),
            crate::arena::ARENA_OBSERVER => {
                buf.push_str("[OBSERV]");
                match crate::arena::arena_observing(g, k) {
                    Some(t) => buf.push_str(&format!(", Observing: [{}]", name_of(g, t))),
                    None => buf.push_str(", Observing: [NOBODY]"),
                }
            }
            _ => buf.push_str("[UNKNOWN]"),
        }
        buf.push_str(&format!(", Wins: [{}]", wins));
        buf.push_str(&format!(", Losses: [{}]", losses));
        if connected {
            let ft = crate::arena::arena_flee_timer(g, k);
            if ft > 0 {
                let lf = match crate::arena::arena_last_fighting(g, k) {
                    Some(o) => name_of(g, o),
                    None => String::new(),
                };
                buf.push_str(&format!(", Fled-a-match: {} [timer {}]", lf, ft));
            }
        }
    }
    buf.push_str("\r\n");
    g.send_to_char(ch, &buf);

    let dposstr = sprinttype(default_pos, constants::POSITION_TYPES);
    g.send_to_char(
        ch,
        &format!(
            "Default position: {}, Idle Timer (in tics) [{}]\r\n",
            dposstr, timer
        ),
    );

    if npc {
        let flagstr = sprintbit(act_flags, constants::ACTION_BITS);
        g.send_to_char(ch, &format!("NPC flags: &c{}&n\r\n", flagstr));
    } else {
        let mut qline = format!(
            "Quest Next: [{}], Quest Timeleft: [{}]",
            next_quest, countdown
        );
        if quest_mob > 0 {
            qline.push_str(&format!(", On Quest for Mob: [{}]", quest_mob));
        }
        if quest_mob < 0 {
            qline.push_str(&format!(
                ", Killed target mob of level: [{}]",
                quest_mob.abs()
            ));
        }
        if quest_obj > 0 {
            qline.push_str(&format!(", On Quest for Obj: [{}]", quest_obj));
        }
        qline.push_str("\r\n");
        g.send_to_char(ch, &qline);

        g.send_to_char(
            ch,
            &format!(
                "PLR : &c{}&n\r\n",
                sprintbit(act_flags, constants::PLAYER_BITS)
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "PRF : &g{}&n\r\n",
                sprintbit(prf_flags, constants::PREFERENCE_BITS)
            ),
        );
        g.send_to_char(
            ch,
            &format!(
                "PRF2: &g{}&n\r\n",
                sprintbit(prf2_flags, constants::PREFERENCE2_BITS)
            ),
        );
    }

    if is_mob {
        g.send_to_char(
            ch,
            &format!(
                "Mob Spec-Proc: {}, NPC Bare Hand Dam: {}d{}\r\n",
                if mob_spec { "Exists" } else { "None" },
                damnodice,
                damsizedice
            ),
        );
    }

    g.send_to_char(
        ch,
        &format!(
            "Carried: weight: {}, items: {}; Items in: inventory: {}, eq: {}\r\n",
            carry_weight, carry_items, n_inv, n_eq
        ),
    );

    g.send_to_char(
        ch,
        &format!(
            "Hunger: {}, Thirst: {}, Drunk: {}\r\n",
            conditions[FULL], conditions[THIRST], conditions[DRUNK]
        ),
    );

    // Master / followers.
    let mut buf = format!(
        "Master is: {}, Followers are:",
        match master {
            Some(m) => name_of(g, m),
            None => "<none>".to_string(),
        }
    );
    let mut found = 0;
    let total = followers.len();
    for (i, fol) in followers.iter().enumerate() {
        let pers = if g.can_see(ch, *fol) {
            name_of(g, *fol)
        } else {
            "someone".to_string()
        };
        buf.push_str(&format!("{} {}", if found > 0 { "," } else { "" }, pers));
        found += 1;
        if buf.len() >= 62 {
            if i + 1 < total {
                buf.push_str(",\r\n");
            } else {
                buf.push_str("\r\n");
            }
            g.send_to_char(ch, &buf);
            buf.clear();
            found = 0;
        }
    }
    if !buf.is_empty() {
        buf.push_str("\r\n");
        g.send_to_char(ch, &buf);
    }

    // AFF bitvector.
    g.send_to_char(
        ch,
        &format!(
            "AFF: &y{}&n\r\n",
            sprintbit(affect_flags, constants::AFFECTED_BITS)
        ),
    );

    // Active spell affects.
    for aff in &affected {
        if aff.spell_type == -1 && aff.duration == -1 {
            let bits = sprintbit(aff.bitvector, constants::AFFECTED_BITS);
            g.send_to_char(ch, &format!("SPL: (&YO&nPERM) &c{:<21}&n \r\n", bits));
            continue;
        }
        let spell = skill_name(aff.spell_type);
        let mut line = format!("SPL: ({:3}hr) &c{:<21}&n ", aff.duration + 1, spell);
        let mut had_mod = false;
        if aff.modifier != 0 {
            line.push_str(&format!(
                "{:+} to {}",
                aff.modifier,
                sprinttype(aff.location, constants::APPLY_TYPES)
            ));
            had_mod = true;
        }
        if aff.bitvector != 0 {
            line.push_str(if had_mod { ", sets " } else { "sets " });
            line.push_str(&sprintbit(aff.bitvector, constants::AFFECTED_BITS));
        }
        line.push_str("\r\n");
        g.send_to_char(ch, &line);
    }
    // do_sstat_character: DG-script trigger listing (mobs carry triggers).
    do_sstat(g, ch, ScriptKey::Mob(k));
}

// ===========================================================================
// do_stat dispatcher
// ===========================================================================
pub fn do_stat(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (kind, rest) = half_chop(arg);
    if kind.is_empty() {
        g.send_to_char(ch, "Stats on who or what?\r\n");
        return;
    }
    if is_abbrev(&kind, "room") {
        do_stat_room(g, ch);
    } else if is_abbrev(&kind, "mob") {
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which mobile?\r\n");
        } else if let Some(v) = get_char_vis(g, ch, &rest) {
            do_stat_character(g, ch, v);
        } else {
            g.send_to_char(ch, "No such mobile around.\r\n");
        }
    } else if is_abbrev(&kind, "player") {
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which player?\r\n");
        } else {
            let online = get_player_vis(g, ch, &rest);
            if let Some(v) = online {
                let Some(target) = exact_player_authority(g, v) else {
                    g.send_to_char(ch, PLAYER_INSPECTION_DENIED);
                    return;
                };
                if !authorize_player_inspection(g, ch, target.authority) {
                    return;
                }
                do_stat_character(g, ch, v);
            } else {
                if let Some(target) = g.player_index(&rest)
                    && !authorize_player_inspection(g, ch, target.trust)
                {
                    return;
                }
                if try_defer_offline(
                    g,
                    ch,
                    &rest,
                    &format!("stat player {}", rest),
                    OfflineOpAuthority::InspectPlayer,
                ) {
                    // The replay repeats this trust check after the DB load.
                } else {
                    g.send_to_char(ch, "No such player around.\r\n");
                }
            }
        }
    } else if is_abbrev(&kind, "file") {
        // stat file <name>: retrieve_player_entry() loads an OFFLINE player's
        // full record. The real read is an async DB query (database::load_player)
        // unreachable from this sync path, so when the named player exists in the
        // index we defer through the async bridge (game.rs loads + replays +
        // extracts). We replay as `stat player <name>` so the replayed pass takes
        // the online get_player_vis branch (the char is now in the world) instead
        // of re-entering this `file` branch and deferring forever.
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which player?\r\n");
        } else {
            // C act.wizard.c:1140-1143: retrieve_player_entry(), then refuse a
            // target whose authority exceeds the requester's — "Sorry, you
            // can't do that." — before any record is rendered. Target trust
            // comes from the live character when online, else the persistent
            // player index (C's player_table, which retrieve_player_entry
            // walks).
            let online = get_player_vis(g, ch, &rest);
            let target_trust = match online {
                Some(v) => exact_player_authority(g, v).map(|target| target.authority),
                None => g.player_index(&rest).map(|p| p.trust),
            };
            match target_trust {
                None => g.send_to_char(ch, "There is no such player.\r\n"),
                Some(trust) => {
                    if !authorize_player_inspection(g, ch, trust) {
                        return;
                    }
                    match online {
                        Some(v) => do_stat_character(g, ch, v),
                        // Offline: the async bridge loads the record, replays
                        // `stat player <name>` so the online path renders it, then
                        // saves + extracts (C retrieve_player_entry/insert_player_entry).
                        None => {
                            try_defer_offline(
                                g,
                                ch,
                                &rest,
                                &format!("stat player {}", rest),
                                OfflineOpAuthority::InspectPlayer,
                            );
                        }
                    }
                }
            }
        }
    } else if is_abbrev(&kind, "object") {
        if rest.is_empty() {
            g.send_to_char(ch, "Stats on which object?\r\n");
        } else if let Some(o) = get_obj_vis(g, ch, &rest) {
            do_stat_object(g, ch, o);
        } else {
            g.send_to_char(ch, "No such object around.\r\n");
        }
    } else {
        // Bareword: equipment, inventory, room chars, room objs, world char/obj.
        let eq: Vec<ObjId> = g
            .get_char(ch)
            .map(|c| c.equipment.iter().flatten().copied().collect())
            .unwrap_or_default();
        if let Some(o) = g.get_obj_in_list_vis(ch, &kind, &eq) {
            do_stat_object(g, ch, o);
            return;
        }
        let inv: Vec<ObjId> = g
            .get_char(ch)
            .map(|c| c.carrying.clone())
            .unwrap_or_default();
        if let Some(o) = g.get_obj_in_list_vis(ch, &kind, &inv) {
            do_stat_object(g, ch, o);
            return;
        }
        if let Some(v) = g.get_char_room_vis(ch, &kind) {
            do_stat_character(g, ch, v);
            return;
        }
        let room_objs: Vec<ObjId> = g
            .get_char(ch)
            .and_then(|c| c.in_room)
            .map(|r| g.rooms[r].contents.clone())
            .unwrap_or_default();
        if let Some(o) = g.get_obj_in_list_vis(ch, &kind, &room_objs) {
            do_stat_object(g, ch, o);
            return;
        }
        if let Some(v) = get_char_vis(g, ch, &kind) {
            do_stat_character(g, ch, v);
            return;
        }
        if let Some(o) = get_obj_vis(g, ch, &kind) {
            do_stat_object(g, ch, o);
            return;
        }
        g.send_to_char(ch, "Nothing around by that name.\r\n");
    }
}

// ===========================================================================
// do_shutdown
// ===========================================================================
