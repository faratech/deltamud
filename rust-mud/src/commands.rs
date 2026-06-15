// Tier-0 command handlers, signature fn(&mut GameState, CharId, &str, i32),
// matching CircleMUD ACMD(ch, argument, cmd, subcmd). Output strings track
// the C source; byte-parity is tuned against the live C MUD by the harness.

use crate::act::{act, ActArg, To};
use crate::combat;
use crate::constants;
use crate::object::{ObjLoc, WearFlags};
use crate::room::EX_CLOSED;
use crate::state::GameState;
use crate::types::*;

// ---------------------------------------------------------------------------
// Looking
// ---------------------------------------------------------------------------

/// Render a room to a character (CircleMUD look_at_room).
pub fn look_at_room(g: &mut GameState, ch: CharId, _ignore_brief: bool) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "You see nothing but infinite darkness...\r\n");
            return;
        }
    };

    // Room name.
    let name = g.room(rnum).name.clone();
    let is_imm = g.get_char(ch).map(|c| c.is_immortal()).unwrap_or(false);
    if is_imm {
        let vnum = g.room(rnum).number;
        g.send_to_char(ch, &format!("&c[{}] {}&n\r\n", vnum, name));
    } else {
        g.send_to_char(ch, &format!("&c{}&n\r\n", name));
    }

    // Description.
    let desc = g.room(rnum).description.clone();
    let desc = normalize_lines(&desc);
    g.send_to_char(ch, &desc);
    if !desc.ends_with("\r\n") {
        g.send_to_char(ch, "\r\n");
    }

    // Ground objects (their room description line).
    let contents = g.room(rnum).contents.clone();
    for oid in contents {
        if let Some(obj) = g.get_obj(oid) {
            if !obj.description.is_empty() {
                let line = format!("{}\r\n", obj.description);
                g.send_to_char(ch, &line);
            }
        }
    }

    // Other people / mobs.
    let people = g.room(rnum).people.clone();
    for pid in people {
        if pid == ch {
            continue;
        }
        if !g.can_see(ch, pid) {
            continue;
        }
        let line = g.get_char(pid).map(|p| p.display_in_room()).unwrap_or_default();
        if !line.is_empty() {
            g.send_to_char(ch, &format!("{}\r\n", line));
        }
    }
}

pub fn do_look(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        look_at_room(g, ch, true);
        return;
    }
    // look <target>: try a character, then an object in room/inventory.
    if let Some(target) = g.get_char_room_vis(ch, arg) {
        look_at_char(g, ch, target);
        return;
    }
    let in_room = g.get_char(ch).and_then(|c| c.in_room);
    if let Some(rnum) = in_room {
        let contents = g.room(rnum).contents.clone();
        if let Some(oid) = g.get_obj_in_list_vis(ch, arg, &contents) {
            look_at_obj(g, ch, oid);
            return;
        }
    }
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    if let Some(oid) = g.get_obj_in_list_vis(ch, arg, &inv) {
        look_at_obj(g, ch, oid);
        return;
    }
    g.send_to_char(ch, "You do not see that here.\r\n");
}

fn look_at_char(g: &mut GameState, ch: CharId, target: CharId) {
    let desc = g.get_char(target).map(|t| t.player.description.clone()).unwrap_or_default();
    if desc.is_empty() {
        act(g, "You see nothing special about $M.", false, ch, None, ActArg::Char(target), To::Char);
    } else {
        let d = normalize_lines(&desc);
        g.send_to_char(ch, &d);
    }
    // Tell the target they were looked at.
    act(g, "$n looks at you.", true, ch, None, ActArg::Char(target), To::Vict);
    act(g, "$n looks at $N.", true, ch, None, ActArg::Char(target), To::NotVict);
}

fn look_at_obj(g: &mut GameState, ch: CharId, oid: ObjId) {
    let action = g.get_obj(oid).and_then(|o| o.action_description.clone());
    let short = g.get_obj(oid).map(|o| o.short_description.clone()).unwrap_or_default();
    match action {
        Some(a) if !a.is_empty() => {
            let a = normalize_lines(&a);
            g.send_to_char(ch, &a);
        }
        _ => g.send_to_char(ch, &format!("You see nothing special about {}.\r\n", short)),
    }
}

pub fn do_examine(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Examine what?\r\n");
        return;
    }
    do_look(g, ch, arg, 0);
    // Containers also show contents — Tier-0 stops at the look.
}

pub fn do_exits(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let mut out = String::from("Obvious exits:\r\n");
    let mut any = false;
    for dir in 0..NUM_OF_DIRS {
        if let Some(exit) = g.room(rnum).exits[dir].as_ref() {
            if exit.exit_info & EX_CLOSED != 0 {
                continue;
            }
            if let Some(to_rnum) = g.real_room(exit.to_room) {
                any = true;
                let rn = capitalize(DIR_NAMES[dir]);
                let dest = g.room(to_rnum).name.clone();
                out.push_str(&format!("{:<5} - {}\r\n", rn, dest));
            }
        }
    }
    if !any {
        out.push_str(" None.\r\n");
    }
    g.send_to_char(ch, &out);
}

// ---------------------------------------------------------------------------
// Movement
// ---------------------------------------------------------------------------

pub fn do_move(g: &mut GameState, ch: CharId, _arg: &str, subcmd: i32) {
    // SCMD_NORTH(1)..SCMD_DOWN(6) -> direction index 0..5.
    let dir = (subcmd - 1) as usize;
    if dir >= NUM_OF_DIRS {
        return;
    }
    perform_move(g, ch, dir);
}

fn perform_move(g: &mut GameState, ch: CharId, dir: usize) {
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let exit = match g.room(rnum).exits[dir].clone() {
        Some(e) => e,
        None => {
            g.send_to_char(ch, "Alas, you cannot go that way...\r\n");
            return;
        }
    };
    if exit.exit_info & EX_CLOSED != 0 {
        let door = exit.keyword.as_deref().unwrap_or("door");
        g.send_to_char(ch, &format!("The {} seems to be closed.\r\n", first_word(door)));
        return;
    }
    let to_rnum = match g.real_room(exit.to_room) {
        Some(r) => r,
        None => {
            g.send_to_char(ch, "Alas, you cannot go that way...\r\n");
            return;
        }
    };

    // Movement cost from sector loss table (avg of from/to).
    let from_sect = g.room(rnum).sector_type as usize;
    let to_sect = g.room(to_rnum).sector_type as usize;
    let loss = |s: usize| constants::MOVEMENT_LOSS.get(s).copied().unwrap_or(1);
    let need = (loss(from_sect) + loss(to_sect)) / 2;
    let need = need.max(1);

    let (cur_move, is_imm) = g
        .get_char(ch)
        .map(|c| (c.points.move_points, c.is_immortal()))
        .unwrap_or((0, false));
    if cur_move < need && !is_imm {
        g.send_to_char(ch, "You are too exhausted.\r\n");
        return;
    }

    // Leave message to old room.
    act(g, &format!("$n leaves {}.", DIR_NAMES[dir]), true, ch, None, ActArg::None, To::Room);

    if !is_imm {
        if let Some(c) = g.get_char_mut(ch) {
            c.points.move_points -= need;
        }
    }

    g.char_from_room(ch);
    g.char_to_room(ch, to_rnum);

    // Arrival message to new room.
    act(g, "$n has arrived.", true, ch, None, ActArg::None, To::Room);

    look_at_room(g, ch, false);
}

// ---------------------------------------------------------------------------
// Position changes
// ---------------------------------------------------------------------------

fn change_position(
    g: &mut GameState,
    ch: CharId,
    target: Position,
    self_msg: &str,
    room_msg: &str,
    already: &str,
) {
    let cur = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing);
    if cur == target {
        g.send_to_char(ch, already);
        return;
    }
    if cur == Position::Fighting {
        g.send_to_char(ch, "You are already fighting for your life!\r\n");
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.position = target;
    }
    g.send_to_char(ch, self_msg);
    act(g, room_msg, true, ch, None, ActArg::None, To::Room);
}

pub fn do_stand(g: &mut GameState, ch: CharId, _a: &str, _s: i32) {
    change_position(g, ch, Position::Standing, "You clamber to your feet.\r\n", "$n clambers to $s feet.", "You are already standing.\r\n");
}
pub fn do_sit(g: &mut GameState, ch: CharId, _a: &str, _s: i32) {
    change_position(g, ch, Position::Sitting, "You sit down.\r\n", "$n sits down.", "You're sitting already.\r\n");
}
pub fn do_rest(g: &mut GameState, ch: CharId, _a: &str, _s: i32) {
    change_position(g, ch, Position::Resting, "You rest your tired bones.\r\n", "$n sits down and rests.", "You are already resting.\r\n");
}
pub fn do_sleep(g: &mut GameState, ch: CharId, _a: &str, _s: i32) {
    change_position(g, ch, Position::Sleeping, "You go to sleep.\r\n", "$n lies down and falls asleep.", "You are already sound asleep.\r\n");
}
pub fn do_wake(g: &mut GameState, ch: CharId, _a: &str, _s: i32) {
    let pos = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing);
    if pos != Position::Sleeping {
        g.send_to_char(ch, "You are already awake...\r\n");
        return;
    }
    if let Some(c) = g.get_char_mut(ch) {
        c.position = Position::Sitting;
    }
    g.send_to_char(ch, "You wake, and sit up.\r\n");
    act(g, "$n awakens.", true, ch, None, ActArg::None, To::Room);
}

// ---------------------------------------------------------------------------
// Communication
// ---------------------------------------------------------------------------

pub fn do_say(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Yes, but WHAT do you want to say?\r\n");
        return;
    }
    g.send_to_char(ch, &format!("You say, '{}'\r\n", arg));
    let msg = format!("$n says, '{}'", arg);
    act(g, &msg, false, ch, None, ActArg::None, To::Room);
}

pub fn do_tell(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let (target_name, message) = crate::interpreter::half_chop(arg);
    if target_name.is_empty() || message.is_empty() {
        g.send_to_char(ch, "Who do you wish to tell what??\r\n");
        return;
    }
    let target = g
        .find_player_by_name(&target_name)
        .filter(|&t| g.can_see(ch, t) && t != ch);
    let target = match target {
        Some(t) => t,
        None => {
            g.send_to_char(ch, "No-one by that name here.\r\n");
            return;
        }
    };
    let to_vict = format!("$n tells you, '{}'", message);
    act(g, &to_vict, false, ch, None, ActArg::Char(target), To::Vict);
    g.send_to_char(ch, &format!("You tell {}, '{}'\r\n", g.get_char(target).map(|c| c.player.name.clone()).unwrap_or_default(), message));
    if let Some(c) = g.get_char_mut(target) {
        c.last_tell = Some(ch);
    }
}

// holler/shout/gossip/auction/grats via subcmd.
pub fn do_gen_comm(g: &mut GameState, ch: CharId, arg: &str, subcmd: i32) {
    let arg = arg.trim();
    // (self_verb, other_verb) per SCMD: HOLLER0 SHOUT1 GOSSIP2 AUCTION3 GRATZ4
    let (you, name) = match subcmd {
        0 => ("holler", "hollers"),
        1 => ("shout", "shouts"),
        3 => ("auction", "auctions"),
        4 => ("congratulate", "congratulates"),
        _ => ("gossip", "gossips"),
    };
    if arg.is_empty() {
        g.send_to_char(ch, &format!("Yes, {} we must, but WHAT?\r\n", you));
        return;
    }
    g.send_to_char(ch, &format!("You {}, '{}'\r\n", you, arg));
    // Broadcast to all other players (channel scope refined in Batch 2/3).
    let me = ch;
    let speaker = g.get_char(ch).map(|c| c.player.name.clone()).unwrap_or_default();
    let line = format!("{} {}, '{}'\r\n", speaker, name, arg);
    let players: Vec<CharId> = g.players_by_name.values().copied().collect();
    for p in players {
        if p == me {
            continue;
        }
        if g.can_see(p, me) {
            g.send_to_char(p, &line);
        }
    }
}

pub fn do_who(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let mut out = String::from("Players\r\n-------\r\n");
    let mut count = 0;
    let mut players: Vec<CharId> = g.players_by_name.values().copied().collect();
    players.sort_by_key(|c| g.get_char(*c).map(|x| std::cmp::Reverse(x.player.level)).unwrap());
    for pid in players {
        if !g.can_see(ch, pid) {
            continue;
        }
        if let Some(p) = g.get_char(pid) {
            let lvl = if p.is_immortal() { "IMM".to_string() } else { format!("{:3}", p.player.level) };
            out.push_str(&format!("[{} {}] {}\r\n", lvl, p.class_abbrev(), p.get_title()));
            count += 1;
        }
    }
    out.push_str(&format!("\r\n{} player{} displayed.\r\n", count, if count == 1 { "" } else { "s" }));
    g.send_to_char(ch, &out);
}

// ---------------------------------------------------------------------------
// Information
// ---------------------------------------------------------------------------

pub fn do_score(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let c = match g.get_char(ch) {
        Some(c) => c.clone(),
        None => return,
    };
    let mut out = String::new();
    out.push_str(&format!("You are {}.\r\n", c.get_title()));
    out.push_str(&format!(
        "Level: {}  Race: {:?}  Class: {:?}  Sex: {:?}\r\n",
        c.player.level, c.player.race, c.player.class, c.player.sex
    ));
    out.push_str(&format!(
        "Hit points: {}/{}   Mana: {}/{}   Move: {}/{}\r\n",
        c.points.hit, c.points.max_hit, c.points.mana, c.points.max_mana, c.points.move_points, c.points.max_move
    ));
    out.push_str(&format!(
        "Str: {}  Int: {}  Wis: {}  Dex: {}  Con: {}  Cha: {}\r\n",
        c.aff_abils.str, c.aff_abils.intel, c.aff_abils.wis, c.aff_abils.dex, c.aff_abils.con, c.aff_abils.cha
    ));
    out.push_str(&format!("Armor Class: {}   Alignment: {}\r\n", c.points.armor / 10, c.alignment));
    out.push_str(&format!("Gold: {}   Bank: {}   Experience: {}\r\n", c.points.gold, c.points.bank_gold, c.points.exp));
    out.push_str(&format!("Position: {}\r\n", position_name(c.position)));
    g.send_to_char(ch, &out);
}

pub fn do_inventory(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    g.send_to_char(ch, "You are carrying:\r\n");
    if inv.is_empty() {
        g.send_to_char(ch, " Nothing.\r\n");
        return;
    }
    for oid in inv {
        let s = g.get_obj(oid).map(|o| o.short_description.clone()).unwrap_or_default();
        g.send_to_char(ch, &format!("{}\r\n", s));
    }
}

pub fn do_equipment(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    g.send_to_char(ch, "You are using:\r\n");
    let mut any = false;
    for pos in 0..NUM_WEARS {
        let oid = g.get_char(ch).and_then(|c| c.equipment[pos]);
        if let Some(oid) = oid {
            any = true;
            let label = constants::WHERE.get(pos).copied().unwrap_or("<worn>");
            let s = g.get_obj(oid).map(|o| o.short_description.clone()).unwrap_or_default();
            g.send_to_char(ch, &format!("{}{}\r\n", label, s));
        }
    }
    if !any {
        g.send_to_char(ch, " Nothing.\r\n");
    }
}

// ---------------------------------------------------------------------------
// Object manipulation
// ---------------------------------------------------------------------------

pub fn do_get(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Get what?\r\n");
        return;
    }
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    let contents = g.room(rnum).contents.clone();
    let oid = match g.get_obj_in_list_vis(ch, arg, &contents) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You don't see that here.\r\n");
            return;
        }
    };
    // Takeable?
    let takeable = g.get_obj(oid).map(|o| o.wear_flags.contains(WearFlags::TAKE)).unwrap_or(false);
    if !takeable {
        act(g, "$p: you can't take that!", false, ch, Some(oid), ActArg::None, To::Char);
        return;
    }
    g.obj_from_anywhere(oid);
    g.obj_to_char(oid, ch);
    act(g, "You get $p.", false, ch, Some(oid), ActArg::None, To::Char);
    act(g, "$n gets $p.", true, ch, Some(oid), ActArg::None, To::Room);
}

pub fn do_drop(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Drop what?\r\n");
        return;
    }
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let oid = match g.get_obj_in_list_vis(ch, arg, &inv) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You don't seem to have that.\r\n");
            return;
        }
    };
    let rnum = match g.get_char(ch).and_then(|c| c.in_room) {
        Some(r) => r,
        None => return,
    };
    g.obj_from_anywhere(oid);
    g.obj_to_room(oid, rnum);
    act(g, "You drop $p.", false, ch, Some(oid), ActArg::None, To::Char);
    act(g, "$n drops $p.", true, ch, Some(oid), ActArg::None, To::Room);
}

pub fn do_wear(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    wear_or_wield(g, ch, arg, None);
}
pub fn do_wield(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    wear_or_wield(g, ch, arg, Some(WEAR_WIELD));
}
pub fn do_grab(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    wear_or_wield(g, ch, arg, Some(WEAR_HOLD));
}

fn wear_or_wield(g: &mut GameState, ch: CharId, arg: &str, force_pos: Option<usize>) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Wear what?\r\n");
        return;
    }
    let inv = g.get_char(ch).map(|c| c.carrying.clone()).unwrap_or_default();
    let oid = match g.get_obj_in_list_vis(ch, arg, &inv) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You don't seem to have that.\r\n");
            return;
        }
    };
    let pos = match force_pos.or_else(|| wear_position(g, ch, oid)) {
        Some(p) => p,
        None => {
            g.send_to_char(ch, "You can't wear that.\r\n");
            return;
        }
    };
    // Slot occupied?
    if g.get_char(ch).and_then(|c| c.equipment[pos]).is_some() {
        g.send_to_char(ch, "You're already using something there.\r\n");
        return;
    }
    g.obj_from_anywhere(oid);
    g.equip_char(ch, oid, pos);
    let (self_m, room_m) = wear_messages(pos);
    act(g, self_m, false, ch, Some(oid), ActArg::None, To::Char);
    act(g, room_m, true, ch, Some(oid), ActArg::None, To::Room);
}

pub fn do_remove(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Remove what?\r\n");
        return;
    }
    let eq: Vec<ObjId> = g
        .get_char(ch)
        .map(|c| c.equipment.iter().flatten().copied().collect())
        .unwrap_or_default();
    let oid = match g.get_obj_in_list_vis(ch, arg, &eq) {
        Some(o) => o,
        None => {
            g.send_to_char(ch, "You're not using that.\r\n");
            return;
        }
    };
    let pos = match g.get_obj(oid).map(|o| o.loc) {
        Some(ObjLoc::Worn(_, p)) => p,
        _ => return,
    };
    g.unequip_char(ch, pos);
    g.obj_to_char(oid, ch);
    act(g, "You stop using $p.", false, ch, Some(oid), ActArg::None, To::Char);
    act(g, "$n stops using $p.", true, ch, Some(oid), ActArg::None, To::Room);
}

// ---------------------------------------------------------------------------
// Combat / magic (Tier-0 entry points; rounds run in the heartbeat)
// ---------------------------------------------------------------------------

pub fn do_kill(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    let arg = arg.trim();
    if arg.is_empty() {
        g.send_to_char(ch, "Kill who?\r\n");
        return;
    }
    let victim = match g.get_char_room_vis(ch, arg) {
        Some(v) => v,
        None => {
            g.send_to_char(ch, "They aren't here.\r\n");
            return;
        }
    };
    if victim == ch {
        g.send_to_char(ch, "You hit yourself...OUCH!.\r\n");
        return;
    }
    if let Err(msg) = combat::can_kill(g, ch, victim) {
        g.send_to_char(ch, &msg);
        return;
    }
    act(g, "You attack $N!", false, ch, None, ActArg::Char(victim), To::Char);
    act(g, "$n attacks you!", false, ch, None, ActArg::Char(victim), To::Vict);
    act(g, "$n attacks $N!", true, ch, None, ActArg::Char(victim), To::NotVict);
    combat::set_fighting(g, ch, victim);
}

pub fn do_flee(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    combat::do_flee(g, ch);
}

pub fn do_cast(g: &mut GameState, ch: CharId, arg: &str, _subcmd: i32) {
    crate::magic::do_cast(g, ch, arg);
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

pub fn do_quit(g: &mut GameState, ch: CharId, _arg: &str, _subcmd: i32) {
    let pos = g.get_char(ch).map(|c| c.position).unwrap_or(Position::Standing);
    if pos == Position::Fighting {
        g.send_to_char(ch, "No way!  You're fighting for your life!\r\n");
        return;
    }
    g.send_to_char(ch, "Goodbye, friend.. Come back soon!\r\n");
    act(g, "$n has left the game.", true, ch, None, ActArg::None, To::Room);
    // The game loop observes the Close request and persists the player.
    if let Some(conn) = g.get_char(ch).and_then(|c| c.desc) {
        if let Some(d) = g.descriptors.get_mut(&conn) {
            d.state = crate::connection::ConState::Close;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn wear_position(g: &GameState, ch: CharId, oid: ObjId) -> Option<usize> {
    let wf = g.get_obj(oid)?.wear_flags;
    let occupied = |p: usize| g.get_char(ch).map(|c| c.equipment[p].is_some()).unwrap_or(true);
    if wf.contains(WearFlags::FINGER) {
        if !occupied(WEAR_FINGER_R) { return Some(WEAR_FINGER_R); }
        if !occupied(WEAR_FINGER_L) { return Some(WEAR_FINGER_L); }
    }
    if wf.contains(WearFlags::NECK) {
        if !occupied(WEAR_NECK_1) { return Some(WEAR_NECK_1); }
        if !occupied(WEAR_NECK_2) { return Some(WEAR_NECK_2); }
    }
    if wf.contains(WearFlags::WRIST) {
        if !occupied(WEAR_WRIST_R) { return Some(WEAR_WRIST_R); }
        if !occupied(WEAR_WRIST_L) { return Some(WEAR_WRIST_L); }
    }
    let single = [
        (WearFlags::BODY, WEAR_BODY), (WearFlags::HEAD, WEAR_HEAD), (WearFlags::LEGS, WEAR_LEGS),
        (WearFlags::FEET, WEAR_FEET), (WearFlags::HANDS, WEAR_HANDS), (WearFlags::ARMS, WEAR_ARMS),
        (WearFlags::SHIELD, WEAR_SHIELD), (WearFlags::ABOUT, WEAR_ABOUT), (WearFlags::WAIST, WEAR_WAIST),
        (WearFlags::FACE, WEAR_FACE),
    ];
    for (flag, p) in single {
        if wf.contains(flag) {
            return Some(p);
        }
    }
    None
}

fn wear_messages(pos: usize) -> (&'static str, &'static str) {
    match pos {
        WEAR_WIELD => ("You wield $p.", "$n wields $p."),
        WEAR_HOLD => ("You grab $p.", "$n grabs $p."),
        WEAR_LIGHT => ("You light $p and hold it.", "$n lights $p and holds it."),
        _ => ("You wear $p.", "$n wears $p."),
    }
}

fn position_name(p: Position) -> &'static str {
    match p {
        Position::Dead => "dead",
        Position::MortallyWounded => "mortally wounded",
        Position::Incapacitated => "incapacitated",
        Position::Stunned => "stunned",
        Position::Sleeping => "sleeping",
        Position::Resting => "resting",
        Position::Meditating => "meditating",
        Position::Sitting => "sitting",
        Position::Fighting => "fighting",
        Position::Standing => "standing",
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

/// Normalise bare \n to \r\n for telnet output (world files use \n).
fn normalize_lines(s: &str) -> String {
    let unified = s.replace("\r\n", "\n");
    unified.replace('\n', "\r\n")
}
