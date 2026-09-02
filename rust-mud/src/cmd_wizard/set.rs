//! do_set / perform_set: the immortal `set` field editor.
//!
//! Split out of cmd_wizard.rs (phase 6); `use super::*` inherits the
//! module's imports and private helpers.

use super::*;

pub(crate) fn range_i32(low: i32, high: i32, value: i32) -> i32 {
    value.clamp(low, high)
}

/// parse_class(c): first-letter class parse (class.c). -1 == CLASS_UNDEFINED.
pub(crate) fn parse_class(c: char) -> i32 {
    match c.to_ascii_lowercase() {
        'm' => Class::MagicUser as i32,
        'c' => Class::Cleric as i32,
        't' => Class::Thief as i32,
        'w' => Class::Warrior as i32,
        'a' => Class::Artisan as i32,
        _ => -1,
    }
}

/// parse_race(c): first-letter race parse. -1 == RACE_UNDEFINED.
pub(crate) fn parse_race(c: char) -> i32 {
    // C act.wizard.c:3400 uses races.c parse_race (menu letters a..i); the
    // Rust copy invented name-initial letters and could not set Goblin/Drow.
    crate::races::parse_race(c)
}

/// Player level, trust, and command grants form one durable authority record
/// and must be committed atomically by `advance`; `set` may still change an
/// NPC's runtime level because NPC authority is not persistent account state.
pub(crate) fn set_field_changes_player_authority(field: &SetField) -> bool {
    matches!(field.switchnum, 34 | 52 | 103..=108) || field.cmd.starts_with("cmd")
}

/// perform_set: apply one set field. Returns true on a change that should be
/// saved. `ch` is None for recursive setall-style calls (no permission echo).
pub(crate) fn perform_set(
    g: &mut GameState,
    ch: Option<CharId>,
    vict: CharId,
    mode: usize,
    val_arg: &str,
) -> bool {
    let field = &SET_FIELDS[mode];
    let switchmode = field.switchnum;
    let mut on = false;
    let mut off = false;
    let mut value = 0i32;
    let mut output = String::new();

    if !is_npc(g, vict) && set_field_changes_player_authority(field) {
        if let Some(cch) = ch {
            g.send_to_char(
                cch,
                "Player authority changes must use 'advance <player> <level>' so they are durably committed.\r\n",
            );
        }
        return false;
    }

    if ch.is_none() {
        if val_arg == "on" || val_arg == "yes" {
            on = true;
        } else if val_arg == "off" || val_arg == "no" {
            off = true;
        }
    } else {
        let cch = ch.unwrap();
        let Some(caller) = authenticated_player_authority(g, cch) else {
            g.send_to_char(cch, "You are not godly enough for that!\r\n");
            return false;
        };
        let vnpc = is_npc(g, vict);
        let victim_authority = if vnpc {
            None
        } else {
            let Some(target) = exact_player_authority(g, vict) else {
                g.send_to_char(cch, "Maybe that's not such a great idea...\r\n");
                return false;
            };
            Some(target)
        };
        if caller.authority < i32::from(LVL_IMPL)
            && victim_authority.is_some_and(|target| {
                caller.principal != target.principal && caller.authority <= target.authority
            })
        {
            g.send_to_char(cch, "Maybe that's not such a great idea...\r\n");
            return false;
        }
        if caller.authority < i32::from(field.level) {
            g.send_to_char(cch, "You are not godly enough for that!\r\n");
            return false;
        }
        // PC/NPC correctness.
        if vnpc && (field.pcnpc & NPC) == 0 {
            g.send_to_char(cch, "You can't do that to a beast!\r\n");
            return false;
        }
        if !vnpc && (field.pcnpc & PC) == 0 {
            g.send_to_char(cch, "That can only be done to a beast!\r\n");
            return false;
        }
        let cname = name_of(g, caller.principal);
        let vname = name_of(g, vict);
        let m = LVL_GOD.max(invis_lev(g, cch) as u8);
        match field.typ {
            T_BINARY => {
                if val_arg == "on" || val_arg == "yes" {
                    on = true;
                } else if val_arg == "off" || val_arg == "no" {
                    off = true;
                }
                if !(on || off) {
                    g.send_to_char(cch, "Value must be 'on' or 'off'.\r\n");
                    return false;
                }
                mudlog(
                    g,
                    &format!(
                        "(GC) {} set {} {} for {}.",
                        cname,
                        field.cmd,
                        onoff(on),
                        vname
                    ),
                    BRF,
                    m,
                );
                output = format!("{}'s {} set {}.", vname, field.cmd, onoff(on));
            }
            T_NUMBER => {
                value = match crate::text::parse_i32_atoi(val_arg) {
                    Ok(value) => value,
                    Err(crate::text::ParseIntError::Overflow) => {
                        g.send_to_char(cch, "That number is outside the supported range.\r\n");
                        return false;
                    }
                    Err(_) => unreachable!("parse_i32_atoi maps nonnumeric input to zero"),
                };
                mudlog(
                    g,
                    &format!("(GC) {} set {}'s {} to {}.", cname, vname, field.cmd, value),
                    BRF,
                    m,
                );
                output = format!("{}'s {} set to {}.", vname, field.cmd, value);
            }
            _ => {
                output = "Okay.".to_string();
            }
        }
    }

    // Apply.
    let set_or_remove_act = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.act_flags |= flag;
            } else if off {
                v.act_flags &= !flag;
            }
        }
    };
    let set_or_remove_prf = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.prf_flags |= flag;
            } else if off {
                v.prf_flags &= !flag;
            }
        }
    };
    let set_or_remove_prf2 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.prf2_flags |= flag;
            } else if off {
                v.prf2_flags &= !flag;
            }
        }
    };
    // SET_OR_REMOVE over the per-player god-command bitvectors (godcmds1..3).
    let set_or_remove_gcmd1 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.godcmds1 |= flag;
            } else if off {
                v.godcmds1 &= !flag;
            }
        }
    };
    let set_or_remove_gcmd2 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.godcmds2 |= flag;
            } else if off {
                v.godcmds2 &= !flag;
            }
        }
    };
    let set_or_remove_gcmd3 = |g: &mut GameState, flag: i64| {
        if let Some(v) = g.get_char_mut(vict) {
            if on {
                v.godcmds3 |= flag;
            } else if off {
                v.godcmds3 &= !flag;
            }
        }
    };

    let vict_immortal = !is_npc(g, vict)
        && exact_player_authority(g, vict)
            .is_some_and(|target| target.authority >= i32::from(LVL_IMMORT));

    match switchmode {
        0 => set_or_remove_prf(g, PRF_BRIEF),
        1 => set_or_remove_act(g, PLR_INVSTART),
        2 => {
            // set_title.
            if let Some(v) = g.get_char_mut(vict) {
                v.player.title = if val_arg.is_empty() {
                    None
                } else {
                    Some(val_arg.to_string())
                };
            }
            let vname = name_of(g, vict);
            let title = g
                .get_char(vict)
                .map(|c| c.get_title())
                .unwrap_or(vname.clone());
            output = format!("{}'s title is now: {}", vname, title);
        }
        3 => {
            set_or_remove_prf(g, PRF_SUMMONABLE);
            output = format!("Nosummon {} for {}.\r\n", onoff(!on), name_of(g, vict));
        }
        4 => {
            let nv = range_i32(1, 5000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.max_hit = nv;
            }
            g.affect_total(vict);
        }
        5 => {
            let nv = range_i32(1, 5000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.max_mana = nv;
            }
            g.affect_total(vict);
        }
        6 => {
            let nv = range_i32(1, 5000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.max_move = nv;
            }
            g.affect_total(vict);
        }
        7 => {
            let mh = g.get_char(vict).map(|c| c.points.max_hit).unwrap_or(0);
            let nv = range_i32(-9, mh, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.hit = nv;
            }
            g.affect_total(vict);
        }
        8 => {
            let mm = g.get_char(vict).map(|c| c.points.max_mana).unwrap_or(0);
            let nv = range_i32(0, mm, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.mana = nv;
            }
            g.affect_total(vict);
        }
        9 => {
            let mm = g.get_char(vict).map(|c| c.points.max_move).unwrap_or(0);
            let nv = range_i32(0, mm, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.move_points = nv;
            }
            g.affect_total(vict);
        }
        10 => {
            let nv = range_i32(-1000, 1000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.alignment = nv;
            }
            g.affect_total(vict);
        }
        11 => {
            let hi = if is_npc(g, vict) || vict_immortal {
                MAX_STAT
            } else {
                MAX_PLAYER_STAT
            };
            let nv = range_i32(3, hi as i32, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.str = nv;
                v.real_abils.str_add = 0;
            }
            g.affect_total(vict);
        }
        12 => {
            let nv = range_i32(0, 100, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.str_add = nv as i8;
                if value > 0 {
                    v.real_abils.str = MAX_PLAYER_STAT;
                }
            }
            g.affect_total(vict);
        }
        13 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.intel = nv;
            }
            g.affect_total(vict);
        }
        14 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.wis = nv;
            }
            g.affect_total(vict);
        }
        15 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.dex = nv;
            }
            g.affect_total(vict);
        }
        16 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.con = nv;
            }
            g.affect_total(vict);
        }
        17 => {
            let hi = stat_hi(g, vict);
            let nv = range_i32(3, hi, value) as i8;
            if let Some(v) = g.get_char_mut(vict) {
                v.real_abils.cha = nv;
            }
            g.affect_total(vict);
        }
        18 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.defense = nv;
            }
            g.affect_total(vict);
        }
        19 => {
            let nv = range_i32(0, 100_000_000, value);
            if let Some(v) = g.get_char_mut(vict) {
                crate::gold::set(v, crate::gold::Account::Carried, i64::from(nv));
            }
        }
        20 => {
            let nv = range_i32(0, 100_000_000, value);
            if let Some(v) = g.get_char_mut(vict) {
                crate::gold::set(v, crate::gold::Account::Bank, i64::from(nv));
            }
        }
        21 => {
            let nv = range_i32(0, 50_000_000, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.points.exp = nv as i64;
            }
        }
        22 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.power = nv;
            }
            g.affect_total(vict);
        }
        23 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.mdefense = nv;
            }
            g.affect_total(vict);
        }
        24 => {
            let Some(cch) = ch else {
                return false;
            };
            let (Some(caller), Some(target)) = (
                authenticated_player_authority(g, cch),
                exact_player_authority(g, vict),
            ) else {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            };
            if caller.authority < i32::from(LVL_IMPL) && caller.principal != target.principal {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            }
            let nv = range_i32(0, target.authority, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.invis_level = nv;
            }
        }
        25 => {
            let Some(cch) = ch else {
                return false;
            };
            let (Some(caller), Some(target)) = (
                authenticated_player_authority(g, cch),
                exact_player_authority(g, vict),
            ) else {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            };
            if caller.authority < i32::from(LVL_IMPL) && caller.principal != target.principal {
                g.send_to_char(cch, "You aren't godly enough for that!\r\n");
                return false;
            }
            set_or_remove_prf(g, PRF_NOHASSLE);
        }
        26 => {
            if let Some(cch) = ch {
                if cch == vict {
                    g.send_to_char(cch, "Better not -- could be a long winter!\r\n");
                    return false;
                }
            }
            set_or_remove_act(g, PLR_FROZEN);
        }
        27 | 28 => {
            let nv = range_i32(0, 100, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.spells_to_learn = nv;
            }
        }
        29 => {
            let nv = range_i32(-100, 24, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.conditions[DRUNK] = nv as i8;
            }
        }
        30 => {
            let nv = range_i32(-100, 24, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.conditions[FULL] = nv as i8;
            }
        }
        31 => {
            let nv = range_i32(-100, 24, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.conditions[THIRST] = nv as i8;
            }
        }
        32 => set_or_remove_act(g, PLR_KILLER),
        33 => set_or_remove_act(g, PLR_THIEF),
        34 => {
            let ch_trust = ch
                .and_then(|caller| authenticated_player_authority(g, caller))
                .map(|caller| caller.authority)
                .unwrap_or(i32::from(LVL_IMPL));
            if value > ch_trust || value > i32::from(LVL_IMPL) {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "You can't do that.\r\n");
                }
                return false;
            }
            let nv = range_i32(0, LVL_IMPL as i32, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.player.level = nv as u8;
            }
        }
        35 => match g.real_room(value) {
            Some(i) => {
                g.char_from_room(vict);
                g.char_to_room(vict, i);
            }
            None => {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "No room exists with that number.\r\n");
                }
                return false;
            }
        },
        36 => set_or_remove_prf(g, PRF_ROOMFLAGS),
        37 => set_or_remove_act(g, PLR_SITEOK),
        38 => set_or_remove_act(g, PLR_DELETED),
        39 => {
            let i = parse_class(val_arg.chars().next().unwrap_or(' '));
            if i < 0 {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "That is not a class.\r\n");
                }
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.player.class = Class::from_u8(i as u8);
            }
        }
        40 => set_or_remove_act(g, PLR_NOWIZLIST),
        41 => set_or_remove_prf2(g, PRF2_QCHAN),
        42 => {
            if is_number(val_arg) {
                value = match crate::text::parse_i32_strict(val_arg) {
                    Ok(value) => value,
                    Err(_) => {
                        if let Some(cch) = ch {
                            g.send_to_char(
                                cch,
                                "Must be a room's virtual number in the supported range.\r\n",
                            );
                        }
                        return false;
                    }
                };
                if g.real_room(value).is_some() || value == -1 {
                    if let Some(v) = g.get_char_mut(vict) {
                        v.load_room = value;
                    }
                    let vname = name_of(g, vict);
                    if value == -1 {
                        output = format!("{}'s loadroom turned off.", vname);
                    } else {
                        output = format!("{} will enter at room #{}.", vname, value);
                    }
                } else {
                    if let Some(cch) = ch {
                        g.send_to_char(cch, "That room does not exist!\r\n");
                    }
                    return false;
                }
            } else {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "Must be a room's virtual number.\r\n");
                }
                return false;
            }
        }
        43 => set_or_remove_prf(g, PRF_COLOR_1 | PRF_COLOR_2),
        44 => {
            // idnum: an Implementor role may change NPC runtime identities.
            // Durable player id 1 is historical data, not an authorization
            // credential; trusting it let a mortal impostor bypass this gate.
            let caller_is_implementor = ch.is_some_and(|caller| {
                target_principal_authority(g, caller)
                    .is_some_and(|principal| principal.authority >= i32::from(LVL_IMPL))
            });
            if !caller_is_implementor || !is_npc(g, vict) {
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.idnum = value as i64;
            }
        }
        45 => {
            let Some(target) = exact_player_authority(g, vict) else {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "You cannot change that.\r\n");
                }
                return false;
            };
            if target.authority >= i32::from(LVL_GRGOD) {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "You cannot change that.\r\n");
                }
                return false;
            }
            if !(3..=crate::password::MAX_PASSWORD_INPUT_BYTES).contains(&val_arg.len()) {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "Password must be between 3 and 64 bytes.\r\n");
                }
                return false;
            }
            let Some(authorization) =
                ch.and_then(|caller| crate::interpreter::authenticated_command_request(g, caller))
            else {
                return false;
            };
            let Some((idnum, name)) = g
                .get_char(vict)
                .filter(|victim| !victim.is_npc && victim.idnum > 0)
                .map(|victim| (victim.idnum, victim.get_name().to_string()))
            else {
                g.send_to_char(
                    authorization.requester_body,
                    "That player has no durable identity.\r\n",
                );
                return false;
            };
            g.queue_password_update(authorization, vict, idnum, &name, val_arg.to_owned());
            output = format!("Password change for {} queued.", name);
        }
        46 => set_or_remove_act(g, PLR_NODELETE),
        47 => {
            let sex = if val_arg.eq_ignore_ascii_case("male") {
                Gender::Male
            } else if val_arg.eq_ignore_ascii_case("female") {
                Gender::Female
            } else if val_arg.eq_ignore_ascii_case("neutral") {
                Gender::Neutral
            } else {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "Must be 'male', 'female', or 'neutral'.\r\n");
                }
                return false;
            };
            if let Some(v) = g.get_char_mut(vict) {
                v.player.sex = sex;
            }
        }
        49 => set_or_remove_prf(g, PRF_AFK),
        50 => {
            let i = parse_race(val_arg.chars().next().unwrap_or(' '));
            if i < 0 {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "That is not a race.\r\n");
                }
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.player.race = Race::from_u8(i as u8);
            }
        }
        51 => {
            // hometown: parse_town(*val_arg) — the home-town menu letter (a..c).
            let i = crate::class::parse_town(val_arg.chars().next().unwrap_or(' '));
            if i == -1 {
                if let Some(cch) = ch {
                    g.send_to_char(cch, "That is not a hometown.\r\n");
                }
                return false;
            }
            if let Some(v) = g.get_char_mut(vict) {
                v.player.hometown = i;
            }
        }
        52 => {
            if let Some(cch) = ch {
                g.send_to_char(
                    cch,
                    "Player authority changes must use 'advance <player> <level>' so they are durably committed.\r\n",
                );
            }
            return false;
        }
        53 => {
            let nv = range_i32(0, 100, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.spells_to_learn = nv;
            }
        }
        // 54..=108: per-player god-command bitvectors (godcmds1..4). Each cmd*
        // field flips one GCMD bit; the grant/revoke aggregates (103..=108) set
        // or recursively walk the set_fields table.
        54 => set_or_remove_gcmd1(g, GCMD_GEN),
        55 => set_or_remove_gcmd1(g, GCMD_ADVANCE),
        56 => set_or_remove_gcmd1(g, GCMD_AT),
        57 => set_or_remove_gcmd1(g, GCMD_BAN),
        58 => set_or_remove_gcmd1(g, GCMD_DC),
        59 => set_or_remove_gcmd1(g, GCMD_ECHO),
        60 => set_or_remove_gcmd1(g, GCMD_FORCE),
        61 => set_or_remove_gcmd1(g, GCMD_FREEZE),
        62 => set_or_remove_gcmd1(g, GCMD_HCONTROL),
        63 => set_or_remove_gcmd1(g, GCMD_LOAD),
        64 => set_or_remove_gcmd1(g, GCMD_MUTE),
        65 => set_or_remove_gcmd1(g, GCMD_SYSLOG),
        66 => set_or_remove_gcmd1(g, GCMD_PARDON),
        67 => set_or_remove_gcmd1(g, GCMD_PURGE),
        68 => set_or_remove_gcmd1(g, GCMD_RELOAD),
        69 => set_or_remove_gcmd1(g, GCMD_REROLL),
        70 => set_or_remove_gcmd1(g, GCMD_RESTORE),
        71 => set_or_remove_gcmd1(g, GCMD_SEND),
        72 => set_or_remove_gcmd1(g, GCMD_SET),
        73 => set_or_remove_gcmd1(g, GCMD_SHUTDOWN),
        74 => set_or_remove_gcmd1(g, GCMD_SKILLSET),
        75 => set_or_remove_gcmd1(g, GCMD_AUCTIONEER),
        76 => set_or_remove_gcmd1(g, GCMD_SLOWNS),
        77 => set_or_remove_gcmd1(g, GCMD_SNOOP),
        78 => set_or_remove_gcmd1(g, GCMD_SWITCH),
        79 => set_or_remove_gcmd1(g, GCMD_PLAGUE),
        80 => set_or_remove_gcmd1(g, GCMD_TRANS),
        81 => set_or_remove_gcmd1(g, GCMD_UNAFFECT),
        82 => set_or_remove_gcmd1(g, GCMD_WIZLOCK),
        83 => set_or_remove_gcmd1(g, GCMD_ISAY),
        84 => {
            set_or_remove_gcmd3(g, GCMD3_ADDSNOW);
            set_or_remove_gcmd3(g, GCMD3_DELSNOW);
        }
        85 => set_or_remove_gcmd2(g, GCMD2_OLC),
        86 => set_or_remove_gcmd2(g, GCMD2_INVIS),
        87 => set_or_remove_gcmd2(g, GCMD2_MCASTERS),
        88 => set_or_remove_gcmd2(g, GCMD2_MUDHEAL),
        89 => set_or_remove_gcmd2(g, GCMD2_REWIZ),
        90 => set_or_remove_gcmd2(g, GCMD2_GECHO),
        92 => set_or_remove_gcmd2(g, GCMD2_REWWW),
        93 => set_or_remove_gcmd2(g, GCMD2_NOTITLE),
        94 => set_or_remove_gcmd2(g, GCMD2_PAGE),
        95 => set_or_remove_gcmd2(g, GCMD2_QECHO),
        96 => set_or_remove_gcmd2(g, GCMD2_ZRESET),
        97 => set_or_remove_gcmd2(g, GCMD2_SETREBOOT),
        98 => set_or_remove_gcmd2(g, GCMD2_TMOBDIE),
        99 => set_or_remove_gcmd2(g, GCMD2_WRESTRICT),
        100 => set_or_remove_gcmd2(g, GCMD2_ATTACH),
        101 => set_or_remove_gcmd2(g, GCMD2_USERS),
        102 => set_or_remove_gcmd2(g, GCMD2_ALOAD),
        103 => {
            // imp: grant everything (bar GCMD_CMDSET) or revoke everything.
            if val_arg.eq_ignore_ascii_case("on") {
                if let Some(v) = g.get_char_mut(vict) {
                    v.godcmds1 = (!GCMD_CMDSET) | v.godcmds1;
                    v.godcmds2 = !0;
                    v.godcmds3 = !0;
                    v.godcmds4 = !0;
                }
            } else if val_arg.eq_ignore_ascii_case("off") {
                if let Some(v) = g.get_char_mut(vict) {
                    v.godcmds1 = 0;
                    v.godcmds2 = 0;
                    v.godcmds3 = 0;
                    v.godcmds4 = 0;
                }
            }
        }
        104 => {
            if val_arg.eq_ignore_ascii_case("on") {
                if let Some(v) = g.get_char_mut(vict) {
                    for i in 0..=32 {
                        v.godcmds1 |= 1i64 << i;
                        v.godcmds2 |= 1i64 << i;
                        v.godcmds3 |= 1i64 << i;
                    }
                }
            } else {
                grant_cmd_tier(g, vict, LVL_IMPL, val_arg);
            }
        }
        105 => grant_cmd_tier(g, vict, LVL_DEMIGOD, val_arg),
        106 => grant_cmd_tier(g, vict, LVL_GOD, val_arg),
        107 | 108 => grant_cmd_tier(g, vict, LVL_GRGOD, val_arg),
        109 => {}
        110 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.wins = value as u8;
            }
        }
        111 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.losses = value as u8;
            }
        }
        112 => set_or_remove_gcmd2(g, GCMD2_RESPEC),
        113 => set_or_remove_prf2(g, PRF2_LOCKOUT),
        114 => {
            if on {
                if let Some(cch) = ch {
                    g.send_to_char(
                        cch,
                        "Sorry. But setting QUESTOR flag ON for a player will cause problems.\r\n",
                    );
                }
                return false;
            }
            set_or_remove_act(g, PLR_QUESTOR);
            if let Some(v) = g.get_char_mut(vict) {
                v.quest_mob = 0;
                v.quest_obj = 0;
            }
        }
        115 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.next_quest = value;
            }
        }
        116 => {
            if let Some(v) = g.get_char_mut(vict) {
                v.quest_points = value;
            }
        }
        117 => set_or_remove_gcmd2(g, GCMD2_QUESTMOBS),
        118 => set_or_remove_gcmd2(g, GCMD2_REWARD),
        119 => set_or_remove_act(g, PLR_MULTIOK),
        120 => set_or_remove_gcmd3(g, GCMD3_PEACE),
        121 => set_or_remove_gcmd3(g, GCMD3_IMPOLC),
        122 => {
            // C: RANGE(1,7); citizen = value - 1 (stored 0..6).
            let nv = range_i32(1, 7, value);
            if let Some(v) = g.get_char_mut(vict) {
                v.citizen = (nv - 1) as u8;
            }
        }
        123 => set_or_remove_act(g, PLR_MBUILDER),
        124 => set_or_remove_gcmd3(g, GCMD3_MAP),
        125 => set_or_remove_gcmd3(g, GCMD3_LWEATHER),
        126 => set_or_remove_gcmd3(g, GCMD3_PFILECLEAN),
        127 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.mpower = nv;
            }
            g.affect_total(vict);
        }
        128 => {
            let nv = range_i32(-750, 750, value) as i16;
            if let Some(v) = g.get_char_mut(vict) {
                v.points.technique = nv;
            }
            g.affect_total(vict);
        }
        129 => set_or_remove_prf2(g, PRF2_INTANGIBLE),
        130 => set_or_remove_gcmd3(g, GCMD3_REBALANCE),
        _ => {
            if let Some(cch) = ch {
                g.send_to_char(cch, "Can't set that!\r\n");
            }
            return false;
        }
    }

    output.push_str("\r\n");
    if let Some(cch) = ch {
        g.send_to_char(cch, &cap(&output));
    }
    true
}

/// The level-tier god-command grant/revoke aggregates (set cmddemigod/cmdgod/
/// cmdgreatergod/cmdimpcmds-off, do_set cases 104..=108). Walks the set_fields
/// table and recursively perform_set's every `cmd*` field at `tier`, skipping
/// the aggregate switches (104..=108) and the two multi-bit fields (54 cmdgeneral
/// / 84 cmdsnow) exactly as C does. `val_arg` ("on"/"off") drives each flip.
pub(crate) fn grant_cmd_tier(g: &mut GameState, vict: CharId, tier: u8, val_arg: &str) {
    for i in 0..SET_FIELDS.len() {
        let f = &SET_FIELDS[i];
        if f.level == tier
            && f.cmd.starts_with("cmd")
            && !(104..=108).contains(&f.switchnum)
            && f.switchnum != 54
            && f.switchnum != 84
        {
            perform_set(g, None, vict, i, val_arg);
        }
    }
}

/// Stat ceiling for int/wis/dex/con/cha: NPC or >= GRGOD gets MAX_STAT.
pub(crate) fn stat_hi(g: &GameState, vict: CharId) -> i32 {
    if is_npc(g, vict)
        || exact_player_authority(g, vict)
            .is_some_and(|target| target.authority >= i32::from(LVL_GRGOD))
    {
        MAX_STAT as i32
    } else {
        MAX_PLAYER_STAT as i32
    }
}

pub fn do_set(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (mut name, mut rest) = half_chop(arg);
    let mut is_player = false;
    let mut is_mob = false;

    let Some(ch_authority) = authenticated_player_authority(g, ch) else {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    };
    let principal_has_god_commands = g.get_char(ch_authority.principal).is_some_and(|principal| {
        principal.godcmds1 != 0
            || principal.godcmds2 != 0
            || principal.godcmds3 != 0
            || principal.godcmds4 != 0
    });
    // C act.wizard.c:3895: `if (!IS_GOD(ch) && GET_LEVEL(ch) < LVL_IMMORT)`.
    // IS_GOD is the granted-command test, so a sub-immortal holding bits is
    // admitted where a plain trust check would reject them. Both properties
    // come from the authenticated principal, never the active body's level.
    if !principal_has_god_commands && ch_authority.authority < i32::from(LVL_IMMORT) {
        g.send_to_char(ch, "Huh?!?\r\n");
        return;
    }

    if name == "file" {
        // set file <name> <field> <value>: load-edit-save an OFFLINE player
        // (retrieve_player_entry + save_char). The load + write-back are async
        // DB ops (database::load_player / save_player), unreachable from this
        // sync path — so when the named player exists in the index, defer the
        // command through the async bridge (game.rs loads the player into the
        // world, replays it, saves + extracts). We replay as `set player <…>`
        // rather than `set file <…>` so the replayed pass takes the online
        // get_player_vis path (the char is now present) instead of re-entering
        // this `file` branch and deferring forever.
        let (fname, frest) = half_chop(&rest);
        if !fname.is_empty()
            && g.find_player_by_name(&fname).is_none()
            && g.get_id_by_name(&fname).is_some()
        {
            g.send_to_char(
                ch,
                &format!("[ Loading {} from the player file... ]\r\n", fname),
            );
            g.queue_offline_op(
                ch,
                &fname,
                &format!("set player {} {}", fname, frest),
                OfflineOpAuthority::ReplayHandler,
            );
            return;
        }
        g.send_to_char(ch, "There is no such player.\r\n");
        return;
    } else if name.eq_ignore_ascii_case("player") {
        is_player = true;
        let (n, r) = half_chop(&rest);
        name = n;
        rest = r;
    } else if name.eq_ignore_ascii_case("mob") {
        is_mob = true;
        let (n, r) = half_chop(&rest);
        name = n;
        rest = r;
    } else if name.eq_ignore_ascii_case("Legal_PKS")
        && ch_authority.authority >= i32::from(LVL_GRGOD)
    {
        // C act.wizard.c:3914-3921: this really flips the pk_allowed global that
        // do_hit/do_kill/murder, fight.c's killer flagging and the PvP spell
        // guards all read.
        let (mode, _r) = half_chop(&rest);
        let mut allowed = g.pk_allowed;
        if mode.eq_ignore_ascii_case("OFF") {
            allowed = false;
        }
        if mode.eq_ignore_ascii_case("ON") {
            allowed = true;
        }
        g.pk_allowed = allowed;
        g.send_to_char(
            ch,
            &format!(
                "Legal PKs are now {}.\r\n",
                if allowed { "Allowed" } else { "Disallowed" }
            ),
        );
        return;
    }
    let _ = is_mob;

    let (field, val_arg) = half_chop(&rest);

    if name.is_empty() || field.is_empty() {
        let mut buf = String::from("Usage: set <victim> <field> <value>\r\nFields:\r\n");
        let mut k = 0;
        for f in SET_FIELDS {
            if i32::from(f.level) > ch_authority.authority {
                continue;
            }
            k += 1;
            if f.cmd.starts_with("cmd") {
                buf.push_str(&format!("&Ycmd&n{:<12}", &f.cmd[3..]));
            } else {
                buf.push_str(&format!("{:<15}", f.cmd));
            }
            if k % 5 == 0 {
                buf.push_str("\r\n");
            }
        }
        buf.push_str(&format!(
            "\r\nThere are {} set fields available to you.\r\n",
            k
        ));
        g.send_to_char(ch, &buf);
        return;
    }

    // Find target. An offline player is resolved through the async bridge: if
    // the name isn't in the world but IS in the player_table, defer the WHOLE
    // command (game.rs loads the player, replays this verbatim so the online
    // path below runs against the now-in-world char, then saves + extracts).
    let vict = if is_player {
        match get_player_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                if try_defer_offline(
                    g,
                    ch,
                    &name,
                    &format!("set {}", arg),
                    OfflineOpAuthority::ReplayHandler,
                ) {
                    return;
                }
                g.send_to_char(ch, "There is no such player.\r\n");
                return;
            }
        }
    } else {
        match get_char_vis(g, ch, &name) {
            Some(v) => v,
            None => {
                if try_defer_offline(
                    g,
                    ch,
                    &name,
                    &format!("set {}", arg),
                    OfflineOpAuthority::ReplayHandler,
                ) {
                    return;
                }
                g.send_to_char(ch, "There is no such creature.\r\n");
                return;
            }
        }
    };

    // Find the field by prefix.
    let mut mode = SET_FIELDS.len();
    for (i, f) in SET_FIELDS.iter().enumerate() {
        if f.cmd.starts_with(&field.to_lowercase()) {
            mode = i;
            break;
        }
    }
    if mode >= SET_FIELDS.len() {
        // No match -> the C loop lands on the "\n" terminator -> default arm.
        g.send_to_char(ch, "Can't set that!\r\n");
        return;
    }

    let changed = perform_set(g, Some(ch), vict, mode, &val_arg);
    if changed
        && !is_npc(g, vict)
        && SET_FIELDS[mode].switchnum != 45
        && !set_field_changes_player_authority(&SET_FIELDS[mode])
    {
        g.request_player_save(vict);
    }
}

// ===========================================================================
// do_rewiz
// ===========================================================================
