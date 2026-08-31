// quest.rs — the LJ "autoquest" system (CircleMUD/DeltaMUD quest.c), ported
// 1:1 to the id-indexed GameState.
//
// Provenance: "Automated Quest code written by Vassago of MOONGATE … Quest
// Code (c) 1996 Ryan Addams", adapted to Circle 3.0 by DSW. This module ports
// the whole file: do_autoquest (the `autoquest`/`quest` command and all its
// subcommands info/points/time/list/buy/request/complete), generate_quest,
// complete_quest (inlined into the `complete` arm exactly as C does), the
// quest item/mob selection, qchance, and quest_update (the per-minute pulse).
//
// Borrow discipline: the C reads ch/questman fields freely; here every value
// is copied/cloned into locals before any send_to_char/act, and entities are
// re-looked-up by id after a mutation. act() is used for the room/char/vict
// broadcasts; the questman's `do_tell(...)` lines are rendered through the
// same "$n tells you, '…'" path the real tell uses (see questman_tell).
//
// Not-yet-ported dependencies degrade exactly as C does:
//   * MOB_QUESTMASTER / MOB_QUEST live in a mob's act_flags. The world loader
//     does not yet populate NPC act_flags from the .mob files, so no room
//     contains a questmaster and no prototype is flagged MOB_QUEST. The C code
//     in that situation already prints "You can't do that here." (no questman)
//     and generate_quest already yields "I don't have any quests for you now"
//     (no MOB_QUEST mob). We reproduce those exact paths; the moment act_flags
//     are populated the full logic activates with no further changes here.
//   * The kill-side hook (fight.c sets GET_QUESTMOB(ch) negative when the
//     quest target dies) is exposed as `quest_on_kill`, for the combat module
//     to call. See boot_hooks.
//   * GET_QUESTGIVER is a `struct char_data *` in C; Character has no such
//     field and this module may not add one, so the questgiver↔player binding
//     is held in a module-static table (QUEST_GIVERS), cleared at boot.
//
// On-disk format: the autoquest system has NO data file of its own. All quest
// state is per-player (questpoints/nextquest/countdown/questobj/questmob),
// persisted with the player record by the (future) player save. boot_quest
// therefore only resets the runtime questgiver table.

use crate::act::{act, ActArg, To};
use crate::state::GameState;
use crate::types::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Flag / vnum constants mirroring quest.c's #defines and structs.h.
// ---------------------------------------------------------------------------

// structs.h player / mob flags (act_flags column).
const PLR_QUESTOR: i64 = 1 << 16;
const MOB_QUESTMASTER: i64 = 1 << 18;
const MOB_QUEST: i64 = 1 << 19;

// Object vnums for the quest reward shop (quest.c QUEST_ITEM*).
const QUEST_ITEM1: ObjVnum = 9002; // Sword of the Gods         500,000qp
const QUEST_ITEM2: ObjVnum = 9003; // Midgaard Hero Vest         30,000qp
const QUEST_ITEM3: ObjVnum = 3082; // Divine Bag of Holding      25,000qp
const QUEST_ITEM4: ObjVnum = 3163; // Necklace of Recall         20,000qp
const QUEST_ITEM5: ObjVnum = 8620; // Pendant of Age             17,000qp
const QUEST_ITEM6: ObjVnum = 3161; // Hat of Eternal Nourish     15,000qp
const QUEST_ITEM7: ObjVnum = 3160; // Belt of Invisibility       10,000qp
const QUEST_ITEM8: ObjVnum = 9000; // Black-eyed Ringstone        5,000qp
const QUEST_ITEM9: ObjVnum = 9001; // Black-eyed Infinite Loop     4,000qp
const QUEST_ITEM10: ObjVnum = 6814; // A golden claw               3,000qp
                                    // items 11..13 are gold / training / practices (no object)
const QUEST_ITEM14: ObjVnum = 18702; // Boots of Water Walking       800qp
const QUEST_ITEM15: ObjVnum = 9010; // A gold brick (50,000)          500qp

// Object vnums for the throwaway object-quest 'tokens' (quest.c QUEST_OBJQUEST*).
const QUEST_OBJQUEST1: ObjVnum = 9005;
const QUEST_OBJQUEST2: ObjVnum = 9006;
const QUEST_OBJQUEST3: ObjVnum = 9007;
const QUEST_OBJQUEST4: ObjVnum = 9008;
const QUEST_OBJQUEST5: ObjVnum = 9009;

// quest.c char_to_room(vsearch, real_room(1204)) "trash" room used to dispose
// of the candidate mob that is read purely to probe difficulty.
const QUEST_TRASH_ROOM: RoomVnum = 1204;

// ---------------------------------------------------------------------------
// Runtime questgiver table (substitute for ch->questgiver pointer).
// ---------------------------------------------------------------------------

static QUEST_GIVERS: OnceLock<Mutex<HashMap<CharId, CharId>>> = OnceLock::new();

fn givers() -> &'static Mutex<HashMap<CharId, CharId>> {
    QUEST_GIVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_questgiver(ch: CharId, questman: Option<CharId>) {
    let mut g = givers().lock().unwrap();
    match questman {
        Some(q) => {
            g.insert(ch, q);
        }
        None => {
            g.remove(&ch);
        }
    }
}

fn get_questgiver(ch: CharId) -> Option<CharId> {
    givers().lock().unwrap().get(&ch).copied()
}

/// Integrator boot hook. No data file (quest state is per-player); just clears
/// the runtime questgiver bindings so a fresh boot starts clean.
pub fn boot_quest(_lib_path: &str) {
    givers().lock().unwrap().clear();
}

// ---------------------------------------------------------------------------
// Small ports of helpers quest.c reaches into utils.c / handler.c for.
// ---------------------------------------------------------------------------

#[inline]
fn is_questor(g: &GameState, ch: CharId) -> bool {
    g.get_char(ch)
        .map(|c| c.act_flags & PLR_QUESTOR != 0)
        .unwrap_or(false)
}

/// quest.c qchance(n): true (1..100) <= n.
fn qchance(g: &mut GameState, num: i32) -> bool {
    g.rng.number(1, 100) <= num
}

/// create_object(vnum, dummy): read a fresh object prototype instance, or None
/// if the vnum has no prototype. (The `dummy`/level arg is unused in C.)
fn create_object(g: &mut GameState, vnum: ObjVnum) -> Option<ObjId> {
    g.load_object(vnum)
}

/// estimate_difficulty(ch, victim) from act.informative.c: a composite of the
/// level + combat-modifier deltas. Used to bound quest-mob selection.
fn estimate_difficulty(g: &GameState, ch: CharId, victim: CharId) -> i32 {
    let c = match g.get_char(ch) {
        Some(c) => c,
        None => return 0,
    };
    let v = match g.get_char(victim) {
        Some(v) => v,
        None => return 0,
    };
    (v.player.level as i32 - c.player.level as i32)
        + (v.points.defense as i32 - c.points.defense as i32)
        + (v.points.mdefense as i32 - c.points.mdefense as i32)
        + (v.points.power as i32 - c.points.power as i32)
        + (v.points.mpower as i32 - c.points.mpower as i32)
        + (v.points.technique as i32 - c.points.technique as i32)
}

/// Render a questman→player private message exactly like do_tell(questman,…):
/// the player hears "$n tells you, '<msg>'" from the questman. The C passes
/// "%s <text>" with GET_NAME(ch) as the leading token (the tell target); only
/// the text after that name is the spoken sentence, so we send <text> as-is.
fn questman_tell(g: &mut GameState, questman: CharId, player: CharId, msg: &str) {
    let line = format!("$n tells you, '{}'", msg);
    act(
        g,
        &line,
        false,
        questman,
        None,
        ActArg::Char(player),
        To::Vict,
    );
}

/// two_arguments: first two space-delimited tokens, lowercased like the C
/// two_arguments() (which lowercases via one_argument's any_one_arg path on
/// the matched keywords). arg1 is the subcommand, arg2 the buy target.
fn two_arguments(argument: &str) -> (String, String) {
    let (a1, rest) = crate::interpreter::any_one_arg(argument);
    let (a2, _) = crate::interpreter::any_one_arg(rest);
    (a1, a2)
}

/// Look up a live questmaster mob in the actor's room (MOB_FLAGGED QUESTMASTER).
fn find_questmaster(g: &GameState, ch: CharId) -> Option<CharId> {
    let rnum = g.get_char(ch)?.in_room?;
    for &cid in &g.rooms.get(rnum)?.people {
        let m = match g.get_char(cid) {
            Some(m) => m,
            None => continue,
        };
        if !m.is_npc {
            continue;
        }
        if m.act_flags & MOB_QUESTMASTER != 0 {
            return Some(cid);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The main quest command: do_autoquest (table: "autoquest", "quest").
// ---------------------------------------------------------------------------

pub fn do_autoquest(g: &mut GameState, ch: CharId, argument: &str, _subcmd: i32) {
    let (arg1, arg2) = two_arguments(argument);

    // ---- "info" -----------------------------------------------------------
    if arg1 == "info" {
        if is_questor(g, ch) {
            let (qmob, qobj) = match g.get_char(ch) {
                Some(c) => (c.quest_mob, c.quest_obj),
                None => return,
            };
            if qmob < 0 {
                if let Some(questman) = get_questgiver(ch) {
                    let qname = g
                        .get_char(questman)
                        .map(|c| c.display_for_others())
                        .unwrap_or_default();
                    let msg = format!(
                        "Your quest is ALMOST complete!\r\nGet back to {} before your time runs out!\r\n",
                        qname
                    );
                    g.send_to_char(ch, &msg);
                }
            } else if qobj > 0 {
                // Name of the fabled object (prototype keyword list).
                let name = g.obj_protos.get(&qobj).map(|p| p.name.clone());
                match name {
                    Some(n) => {
                        let msg = format!("You are on a quest to recover the fabled {}!\r\n", n);
                        g.send_to_char(ch, &msg);
                    }
                    None => g.send_to_char(ch, "You aren't currently on a quest.\r\n"),
                }
                return;
            } else if qmob > 0 {
                let name = g.mob_protos.get(&qmob).map(|p| p.short_desc.clone());
                match name {
                    Some(n) => {
                        let msg = format!("You are on a quest to slay the dreaded {}!\r\n", n);
                        g.send_to_char(ch, &msg);
                    }
                    None => g.send_to_char(ch, "You aren't currently on a quest.\r\n"),
                }
                return;
            }
        } else {
            g.send_to_char(ch, "You aren't currently on a quest.\r\n");
        }
        return;
    }

    // ---- "points" ---------------------------------------------------------
    if arg1 == "points" {
        let qp = g.get_char(ch).map(|c| c.quest_points).unwrap_or(0);
        let msg = format!("You have {} quest points.\r\n", qp);
        g.send_to_char(ch, &msg);
        return;
    }

    // ---- "time" -----------------------------------------------------------
    if arg1 == "time" {
        if !is_questor(g, ch) {
            g.send_to_char(ch, "You aren't currently on a quest.\r\n");
            let nq = g.get_char(ch).map(|c| c.next_quest).unwrap_or(0);
            if nq > 1 {
                let msg = format!(
                    "There are {} minutes remaining until you can go on another quest.\r\n",
                    nq
                );
                g.send_to_char(ch, &msg);
            } else if nq == 1 {
                g.send_to_char(
                    ch,
                    "There is less than a minute remaining until you can go on another quest.\r\n",
                );
            }
        } else {
            let cd = g.get_char(ch).map(|c| c.quest_countdown).unwrap_or(0);
            if cd > 0 {
                let msg = format!(
                    "Time left for current quest is approximately {} minutes\r\n",
                    cd
                );
                g.send_to_char(ch, &msg);
            }
        }
        return;
    }

    // ---- locate the questmaster (required for the remaining subcommands) ---
    let questman = match find_questmaster(g, ch) {
        Some(q) => q,
        None => {
            g.send_to_char(ch, "You can't do that here.\r\n");
            return;
        }
    };

    // Questman busy fighting?
    if g.get_char(questman).and_then(|m| m.fighting).is_some() {
        g.send_to_char(ch, "Wait until the fighting stops.\r\n");
        return;
    }

    // GET_QUESTGIVER(ch) = questman;
    set_questgiver(ch, Some(questman));

    match arg1.as_str() {
        "list" => do_quest_list(g, ch, questman),
        "buy" => do_quest_buy(g, ch, questman, &arg2),
        "request" => do_quest_request(g, ch, questman),
        "complete" => do_quest_complete(g, ch, questman),
        _ => {
            g.send_to_char(
                ch,
                "QUEST commands: POINTS INFO TIME REQUEST COMPLETE LIST BUY.\r\n",
            );
            g.send_to_char(ch, "For more information, type 'HELP QUEST'.\r\n");
        }
    }
}

// ---------------------------------------------------------------------------
// "autoquest list" — the reward catalogue.
// ---------------------------------------------------------------------------

fn do_quest_list(g: &mut GameState, ch: CharId, questman: CharId) {
    act(
        g,
        "$n asks $N for a list of quest items.",
        false,
        ch,
        None,
        ActArg::Char(questman),
        To::Room,
    );
    act(
        g,
        "You ask $N for a list of quest items.",
        false,
        ch,
        None,
        ActArg::Char(questman),
        To::Char,
    );
    let list = "Current Quest Items available for Purchase:\r\n\r\n\
         #1 - 500,000..........Sword of the Gods\r\n\
         #2 - 30,000qp.........Midgaard Hero Vest\r\n\
         #3 - 25,000qp.........Divine Bag of Holding\r\n\
         #4 - 20,000qp.........Necklace of Recall     \r\n\
         #5 - 17,000qp.........Pendant of Age \r\n\
         #6 - 15,000qp.........Hat of Eternal Nourishment \r\n\
         #7 - 10,000qp.........Belt of Invisibility\r\n\
         #8 - 5,0000qp.........Black-eyed Ringstone \r\n\
         #9 - 4,000qp..........Black-eyed Infinite Loop \r\n\
        #10 - 3,000qp..........A golden claw\r\n\
        #11 - 2,000qp..........2,000,000 gold pieces\r\n\
        #12 - 1,500qp..........1 Training Session\r\n\
        #13 - 1,000qp..........30 Practice Sessions\r\n\
        #14 - 800qp............Boots of Water Walking \r\n\
        #15 - 500qp............A gold brick worth 50,000\r\n\
        \r\nTo buy an item, type 'autoquest buy <#item>'.\r\n";
    g.send_to_char(ch, list);
}

// ---------------------------------------------------------------------------
// "autoquest buy <#item>" — spend quest points on a reward.
// ---------------------------------------------------------------------------

fn do_quest_buy(g: &mut GameState, ch: CharId, questman: CharId, arg2: &str) {
    use crate::handler::isname;

    if arg2.is_empty() {
        g.send_to_char(ch, "To buy an item, type 'autoquest buy <item>'.\r\n");
        return;
    }

    let qp = g.get_char(ch).map(|c| c.quest_points).unwrap_or(0);

    // Each catalogue entry: (keyword set, price, reward). Object rewards carry
    // a vnum; the gold/training/practices rewards (#11/#12/#13) are special.
    enum Reward {
        Obj(ObjVnum),
        Gold(i32),
        Training(i32),
        Practices(i32),
    }

    // Table order & keyword strings byte-for-byte from quest.c's isname() args.
    let catalogue: &[(&str, i32, Reward)] = &[
        ("#1 sword gods", 500000, Reward::Obj(QUEST_ITEM1)),
        ("#2 midgaard hero vest", 30000, Reward::Obj(QUEST_ITEM2)),
        ("#3 divine bag holding", 25000, Reward::Obj(QUEST_ITEM3)),
        ("#4 necklace recall", 20000, Reward::Obj(QUEST_ITEM4)),
        ("#5 pendant age", 17000, Reward::Obj(QUEST_ITEM5)),
        (
            "#6 hat nourish nourishment eternal",
            15000,
            Reward::Obj(QUEST_ITEM6),
        ),
        ("#7 belt invis invisbility", 10000, Reward::Obj(QUEST_ITEM7)),
        ("#8 ringstone", 5000, Reward::Obj(QUEST_ITEM8)),
        ("#9 infinet infinite loop", 4000, Reward::Obj(QUEST_ITEM9)),
        ("#10 golden claw", 3000, Reward::Obj(QUEST_ITEM10)),
        (
            "#14 boots water walk walking",
            800,
            Reward::Obj(QUEST_ITEM14),
        ),
        ("#13 practices prac practice", 1000, Reward::Practices(30)),
        ("#12 train training", 1500, Reward::Training(1)),
        ("#11 gold gp", 2000, Reward::Gold(2000000)),
        ("#15 brick", 500, Reward::Obj(QUEST_ITEM15)),
    ];

    // Find the matched catalogue entry (first isname hit, C if/else-if order).
    let mut matched: Option<&(&str, i32, Reward)> = None;
    for entry in catalogue {
        if isname(arg2, entry.0) {
            matched = Some(entry);
            break;
        }
    }

    let entry = match matched {
        Some(e) => e,
        None => {
            let name = g
                .get_char(ch)
                .map(|c| c.get_name().to_string())
                .unwrap_or_default();
            let _ = name; // C prepends the name as the tell-target token only.
            questman_tell(g, questman, ch, "I don't have that item, sorry.");
            return;
        }
    };

    let (_kw, price, reward) = entry;
    if qp < *price {
        questman_tell(
            g,
            questman,
            ch,
            "Sorry, but you don't have enough quest points for that.",
        );
        return;
    }

    // Deduct the points, then grant the reward.
    if let Some(c) = g.get_char_mut(ch) {
        c.quest_points -= *price;
    }

    match reward {
        Reward::Obj(vnum) => {
            // create_object + obj_to_char (after the act() lines, per C order).
            if let Some(oid) = create_object(g, *vnum) {
                act(
                    g,
                    "$N gives $p to $n.",
                    false,
                    ch,
                    Some(oid),
                    ActArg::Char(questman),
                    To::Room,
                );
                act(
                    g,
                    "$N gives you $p.",
                    false,
                    ch,
                    Some(oid),
                    ActArg::Char(questman),
                    To::Char,
                );
                g.obj_to_char(oid, ch);
            }
        }
        Reward::Practices(n) => {
            if let Some(c) = g.get_char_mut(ch) {
                c.spells_to_learn += *n;
            }
            act(
                g,
                "$N gives 30 practices to $n.",
                false,
                ch,
                None,
                ActArg::Char(questman),
                To::Room,
            );
            act(
                g,
                "$N gives you 30 practices.",
                false,
                ch,
                None,
                ActArg::Char(questman),
                To::Char,
            );
        }
        Reward::Training(n) => {
            // C quest.c:468-488: GET_TRAINING(ch) += n after spending the
            // points. The old 'field not ported' comment was stale - the
            // field exists - so 1500 qp bought nothing (#166).
            if let Some(c) = g.get_char_mut(ch) {
                c.training = c.training.saturating_add(*n as u8);
            }
            act(
                g,
                "$N gives 1 training session to $n.",
                false,
                ch,
                None,
                ActArg::Char(questman),
                To::Room,
            );
            act(
                g,
                "$N gives you 1 training session.",
                false,
                ch,
                None,
                ActArg::Char(questman),
                To::Char,
            );
        }
        Reward::Gold(amount) => {
            if let Some(c) = g.get_char_mut(ch) {
                c.points.gold += *amount;
            }
            act(
                g,
                "$N gives 2,000,000 gold pieces to $n.",
                false,
                ch,
                None,
                ActArg::Char(questman),
                To::Room,
            );
            act(
                g,
                "$N has 2,000,000 in gold transfered from $s Swiss account to your balance.",
                false,
                ch,
                None,
                ActArg::Char(questman),
                To::Char,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// "autoquest request" — ask for a new quest.
// ---------------------------------------------------------------------------

fn do_quest_request(g: &mut GameState, ch: CharId, questman: CharId) {
    act(
        g,
        "$n asks $N for a quest.",
        false,
        ch,
        None,
        ActArg::Char(questman),
        To::Room,
    );
    act(
        g,
        "You ask $N for a quest.",
        false,
        ch,
        None,
        ActArg::Char(questman),
        To::Char,
    );

    if is_questor(g, ch) {
        questman_tell(g, questman, ch, "But you're already on a quest!");
        return;
    }
    let nq = g.get_char(ch).map(|c| c.next_quest).unwrap_or(0);
    if nq > 0 {
        questman_tell(
            g,
            questman,
            ch,
            "You're very brave, but let someone else have a chance.",
        );
        questman_tell(g, questman, ch, "Come back later.");
        return;
    }

    let name = g
        .get_char(ch)
        .map(|c| c.get_name().to_string())
        .unwrap_or_default();
    let msg = format!("Thank you, brave {}!\r\n", name);
    g.send_to_char(ch, &msg);

    generate_quest(g, ch, questman);

    let (qmob, qobj) = match g.get_char(ch) {
        Some(c) => (c.quest_mob, c.quest_obj),
        None => return,
    };
    if qmob > 0 || qobj > 0 {
        let cd = g.rng.number(10, 30);
        if let Some(c) = g.get_char_mut(ch) {
            c.quest_countdown = cd;
            c.act_flags |= PLR_QUESTOR;
        }
        let msg = format!("You have {} minutes to complete this quest.\r\n", cd);
        g.send_to_char(ch, &msg);
        g.send_to_char(ch, "May the gods go with you!\r\n");
    }
}

// ---------------------------------------------------------------------------
// "autoquest complete" — turn in / claim the reward (complete_quest inlined).
// ---------------------------------------------------------------------------

fn do_quest_complete(g: &mut GameState, ch: CharId, questman: CharId) {
    act(
        g,
        "$n informs $N $e has completed $s quest.",
        false,
        ch,
        None,
        ActArg::Char(questman),
        To::Room,
    );
    act(
        g,
        "You inform $N you have completed $s quest.",
        false,
        ch,
        None,
        ActArg::Char(questman),
        To::Char,
    );

    // GET_QUESTGIVER(ch) != questman ?
    if get_questgiver(ch) != Some(questman) {
        questman_tell(
            g,
            questman,
            ch,
            "I never sent you on a quest! Perhaps you're thinking of someone else.",
        );
        return;
    }

    if is_questor(g, ch) {
        let (qmob, qobj, cd) = match g.get_char(ch) {
            Some(c) => (c.quest_mob, c.quest_obj, c.quest_countdown),
            None => return,
        };

        // --- Mob quest completed (quest_mob went negative on the kill) ------
        if qmob < 0 && cd > 0 {
            let mut reward = g.rng.number(1000, 3000);
            let mut pointreward = g.rng.number(20, 80);
            let mult = qmob.abs();
            reward *= mult;
            pointreward *= mult;

            questman_tell(g, questman, ch, "Congratulations on completing your quest!");
            let m = format!(
                "As a reward, I am giving you {} quest points, and {} gold.",
                pointreward, reward
            );
            questman_tell(g, questman, ch, &m);

            if qchance(g, 15) {
                let pracreward = g.rng.number(1, 6);
                let msg = format!("You gain {} practices!\r\n", pracreward);
                g.send_to_char(ch, &msg);
                if let Some(c) = g.get_char_mut(ch) {
                    c.spells_to_learn += pracreward;
                }
            }

            clear_quest(g, ch, 15);
            if let Some(c) = g.get_char_mut(ch) {
                c.points.gold += reward;
                c.quest_points += pointreward;
            }
            return;
        }
        // --- Object quest --------------------------------------------------
        else if qobj > 0 && cd > 0 {
            // Find the quest object in the player's inventory.
            let found: Option<ObjId> = {
                let carrying = g
                    .get_char(ch)
                    .map(|c| c.carrying.clone())
                    .unwrap_or_default();
                carrying.into_iter().find(|&oid| {
                    g.get_obj(oid)
                        .map(|o| o.item_number == qobj)
                        .unwrap_or(false)
                })
            };

            if let Some(oid) = found {
                let reward = g.rng.number(1500, 25000);
                let pointreward = g.rng.number(10, 40);

                act(
                    g,
                    "You hand $p to $N.",
                    false,
                    ch,
                    Some(oid),
                    ActArg::Char(questman),
                    To::Char,
                );
                act(
                    g,
                    "$n hands $p to $N.",
                    false,
                    ch,
                    Some(oid),
                    ActArg::Char(questman),
                    To::Room,
                );

                // obj_from_char + extract_obj.
                g.obj_from_anywhere(oid);
                if let Some(c) = g.get_char_mut(ch) {
                    c.carry_items = c.carry_items.saturating_sub(1);
                }
                g.extract_obj(oid);

                questman_tell(g, questman, ch, "Congratulations on completing your quest!");
                let m = format!(
                    "As a reward, I am giving you {} quest points, and {} gold.",
                    pointreward, reward
                );
                questman_tell(g, questman, ch, &m);

                if qchance(g, 15) {
                    let pracreward = g.rng.number(1, 6);
                    let msg = format!("You gain {} practices!\r\n", pracreward);
                    g.send_to_char(ch, &msg);
                    if let Some(c) = g.get_char_mut(ch) {
                        c.spells_to_learn += pracreward;
                    }
                }

                clear_quest(g, ch, 30);
                if let Some(c) = g.get_char_mut(ch) {
                    c.points.gold += reward;
                    c.quest_points += pointreward;
                }
                return;
            } else {
                questman_tell(
                    g,
                    questman,
                    ch,
                    "You haven't completed the quest yet, but there is still time!",
                );
                return;
            }
        }
        // --- Still in progress (positive mob/obj, time remaining) ----------
        else if (qmob > 0 || qobj > 0) && cd > 0 {
            questman_tell(
                g,
                questman,
                ch,
                "You haven't completed the quest yet, but there is still time!",
            );
            return;
        }
    }

    // Fell through: either out of time, or never requested.
    let nq = g.get_char(ch).map(|c| c.next_quest).unwrap_or(0);
    if nq > 0 {
        questman_tell(
            g,
            questman,
            ch,
            "But you didn't complete your quest in time!",
        );
    } else {
        questman_tell(g, questman, ch, "You have to REQUEST a quest first.");
    }
}

/// Reset the per-player quest state after a successful turn-in. `next_quest`
/// is the cooldown the C assigns (15 for mob quests, 30 for object quests).
fn clear_quest(g: &mut GameState, ch: CharId, next_quest: i32) {
    set_questgiver(ch, None);
    if let Some(c) = g.get_char_mut(ch) {
        c.act_flags &= !PLR_QUESTOR;
        c.quest_countdown = 0;
        c.quest_mob = 0;
        c.quest_obj = 0;
        c.next_quest = next_quest;
    }
}

// ---------------------------------------------------------------------------
// generate_quest — pick a target mob and either an object- or mob-quest.
// ---------------------------------------------------------------------------

fn generate_quest(g: &mut GameState, ch: CharId, questman: CharId) {
    // The C reads random mobs from the index up to 99 times, probing each for a
    // suitable difficulty + MOB_QUEST. We mirror that against mob_protos: a
    // candidate must be within difficulty 0..=15 (for mortals) and flagged
    // MOB_QUEST. read_mobile() in C creates a live instance to read its level /
    // combat mods; estimate_difficulty here can read the prototype-derived
    // stats directly, so no throwaway instance/QUEST_TRASH_ROOM dance is needed
    // — but the disposal room constant is retained for documentation parity.
    let _ = QUEST_TRASH_ROOM;

    let ch_level = g.get_char(ch).map(|c| c.player.level).unwrap_or(1);
    let imm = ch_level >= LVL_IMMORT;

    // Candidate vnums (stable order for determinism within a boot).
    let mut proto_vnums: Vec<MobVnum> = g.mob_protos.keys().copied().collect();
    proto_vnums.sort_unstable();
    if proto_vnums.is_empty() {
        deny_quest(g, ch, questman);
        return;
    }

    // Build a transient candidate so estimate_difficulty can use real combat
    // mods; we evaluate the prototype's stored level / combat fields.
    let mut chosen: Option<MobVnum> = None;
    for _ in 0..99 {
        let idx = g.rng.number(0, (proto_vnums.len() - 1) as i32) as usize;
        let vnum = proto_vnums[idx];
        let proto = match g.mob_protos.get(&vnum) {
            Some(p) => p,
            None => continue,
        };
        // estimate_difficulty on prototype stats (level + combat mods; protos
        // carry only level here, others default 0 like a freshly read mob).
        let level_diff = (proto.level as i32) - (ch_level as i32);
        let is_quest_mob = mob_proto_has_flag(g, vnum, MOB_QUEST);
        if !imm && (level_diff > 15 || level_diff < 0 || !is_quest_mob) {
            continue;
        }
        chosen = Some(vnum);
        break;
    }

    // Imm-only 15% scrub (the C drops the candidate 15% of the time for imms'
    // testing path); for mortals this matches `if (GET_LEVEL < LVL_IMMORT)`.
    if !imm {
        if qchance(g, 15) && chosen.is_some() {
            chosen = None;
        }
    }

    let chosen = match chosen {
        Some(v) => v,
        None => {
            deny_quest(g, ch, questman);
            return;
        }
    };

    // Locate a LIVE instance of that mob in the world (the actual victim the
    // player must hunt) — C scans world[].people for GET_MOB_VNUM == chosen.
    let victim: Option<CharId> = {
        let mut found = None;
        for cid in g.char_ids() {
            if let Some(c) = g.get_char(cid) {
                if c.is_npc && c.nr == chosen {
                    found = Some(cid);
                    break;
                }
            }
        }
        found
    };

    let victim = match victim {
        Some(v) => v,
        None => {
            deny_quest(g, ch, questman);
            return;
        }
    };

    let victim_room = match g.get_char(victim).and_then(|c| c.in_room) {
        Some(r) => r,
        None => {
            deny_quest(g, ch, questman);
            return;
        }
    };

    // Prefer item quests 50% of the time.
    if qchance(g, 50) {
        let objvnum = match g.rng.number(0, 4) {
            0 => QUEST_OBJQUEST1,
            1 => QUEST_OBJQUEST2,
            2 => QUEST_OBJQUEST3,
            3 => QUEST_OBJQUEST4,
            _ => QUEST_OBJQUEST5,
        };
        let questitem = create_object(g, objvnum);
        let oid = match questitem {
            Some(o) => o,
            None => {
                g.send_to_char(
                    ch,
                    "Error: questitem does not exist! please notify the imms\r\n",
                );
                return;
            }
        };
        g.obj_to_room(oid, victim_room);

        let item_vnum = g.get_obj(oid).map(|o| o.item_number).unwrap_or(objvnum);
        if let Some(c) = g.get_char_mut(ch) {
            c.quest_obj = item_vnum;
        }

        let short = g
            .get_obj(oid)
            .map(|o| o.short_description.clone())
            .unwrap_or_default();
        let msg = format!(
            "A rare and valuable {} has been stolen from the museum!\r\n",
            short
        );
        g.send_to_char(ch, &msg);

        let zone_name = zone_name_of_room(g, victim_room);
        let room_name = g
            .room_opt(victim_room)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let msg = format!(
            "Look in the general area of {} for {}!\r\n",
            zone_name, room_name
        );
        g.send_to_char(ch, &msg);
        return;
    }

    // Otherwise: a kill-the-mob quest.
    // C quest.c:871/881/893 uses GET_NAME - the mob's KEYWORD list - not the
    // room-display short description (#176).
    let victim_name = g
        .get_char(victim)
        .map(|c| c.get_name().to_string())
        .unwrap_or_default();
    match g.rng.number(0, 1) {
        0 => {
            let m = format!(
                "An enemy of mine, {}, is making vile threats against the crown.\r\n",
                victim_name
            );
            g.send_to_char(ch, &m);
            g.send_to_char(ch, "This threat must be eliminated!\r\n");
        }
        _ => {
            let m = format!(
                "Deltania's most heinous criminal, {}, has escaped from the dungeon!\r\n",
                victim_name
            );
            g.send_to_char(ch, &m);
            let civ = g.rng.number(2, 20);
            let m = format!(
                "Since the escape, {} has murdered {} civilians!\r\n",
                victim_name, civ
            );
            g.send_to_char(ch, &m);
        }
    }

    let room_name = g
        .room_opt(victim_room)
        .map(|r| r.name.clone())
        .unwrap_or_default();
    if !room_name.is_empty() {
        let m = format!(
            "Seek {} out somewhere in the vicinity of {}!\r\n",
            victim_name, room_name
        );
        g.send_to_char(ch, &m);
        let zone_name = zone_name_of_room(g, victim_room);
        let m = format!("That location is in the general area of {}.\r\n", zone_name);
        g.send_to_char(ch, &m);
    }

    let victim_vnum = g.get_char(victim).map(|c| c.nr).unwrap_or(NOBODY);
    if let Some(c) = g.get_char_mut(ch) {
        c.quest_mob = victim_vnum;
    }
}

/// The "Sorry, no quests for you now" failure path (generate_quest victim==NULL).
fn deny_quest(g: &mut GameState, ch: CharId, questman: CharId) {
    questman_tell(
        g,
        questman,
        ch,
        "Sorry, but I don't have any quests for you now. Try again later",
    );
    set_questgiver(ch, None);
    if let Some(c) = g.get_char_mut(ch) {
        c.next_quest = 1;
        c.quest_mob = 0;
        c.quest_obj = 0;
        c.quest_countdown = 0;
        c.act_flags &= !PLR_QUESTOR;
    }
}

/// True if a mob prototype carries `flag` in its act_flags. The world loader
/// does not yet populate NPC act_flags (see module header), so this currently
/// reads through to the live default of 0 for every prototype; it activates
/// automatically once act_flags are loaded. We probe a live instance's flags
/// if one exists (load path), else fall back to false.
fn mob_proto_has_flag(g: &GameState, vnum: MobVnum, flag: i64) -> bool {
    // Any currently-loaded instance of this prototype reflects act_flags.
    for cid in g.char_ids() {
        if let Some(c) = g.get_char(cid) {
            if c.is_npc && c.nr == vnum {
                return c.act_flags & flag != 0;
            }
        }
    }
    false
}

/// zone_table[world[room].zone].name — the human zone name for a room.
fn zone_name_of_room(g: &GameState, room: RoomRnum) -> String {
    let znum = match g.room_opt(room) {
        Some(r) => r.zone,
        None => return String::new(),
    };
    g.zones
        .iter()
        .find(|z| z.number == znum)
        .map(|z| z.name.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Kill-side hook (fight.c): when a questor kills their quest target, mark the
// quest "almost complete" by storing a negative reward multiplier in quest_mob.
// The combat module calls this from the death path. Returns true if the kill
// advanced an active quest.
// ---------------------------------------------------------------------------

pub fn quest_on_kill(g: &mut GameState, killer: CharId, victim: CharId) -> bool {
    let killer_questor = is_questor(g, killer);
    let victim_npc = g.get_char(victim).map(|c| c.is_npc).unwrap_or(false);
    if !killer_questor || !victim_npc {
        return false;
    }
    let victim_vnum = g.get_char(victim).map(|c| c.nr).unwrap_or(NOBODY);
    let quest_mob = g.get_char(killer).map(|c| c.quest_mob).unwrap_or(0);
    if quest_mob != victim_vnum {
        return false;
    }

    g.send_to_char(killer, "You have almost completed your QUEST!\n\r");
    g.send_to_char(
        killer,
        "Return to the questmaster before your time runs out!\n\r",
    );

    // questdiff = estimate_difficulty(killer, quest target); clamp; /5; negate.
    // The C re-reads the prototype as a throwaway mob; we estimate against the
    // just-killed victim (still resident at call time) for an identical number.
    let mut questdiff = estimate_difficulty(g, killer, victim);
    if questdiff <= 0 {
        questdiff = 1;
    }
    questdiff /= 5;
    if let Some(c) = g.get_char_mut(killer) {
        c.quest_mob = -questdiff;
    }
    true
}

// ---------------------------------------------------------------------------
// quest_update — per-minute pulse (comm.c: called every 60s by heartbeat).
// ---------------------------------------------------------------------------

pub fn quest_update(g: &mut GameState) {
    // Snapshot the player ids first (C walks character_list; we avoid borrow
    // conflicts by collecting ids, then mutating each in turn).
    let ids: Vec<CharId> = g.char_ids();

    for ch in ids {
        let (is_npc, nextquest, questor) = match g.get_char(ch) {
            Some(c) => (c.is_npc, c.next_quest, c.act_flags & PLR_QUESTOR != 0),
            None => continue,
        };
        if is_npc {
            continue;
        }

        if nextquest > 0 {
            let nq = nextquest - 1;
            if let Some(c) = g.get_char_mut(ch) {
                c.next_quest = nq;
            }
            if nq == 0 {
                g.send_to_char(ch, "You may now quest again.\r\n");
                // C `return`s here (a quirk that stops scanning the rest of the
                // list this tick); we preserve that exact early-out.
                return;
            }
        } else if questor {
            let cd = g.get_char(ch).map(|c| c.quest_countdown).unwrap_or(0) - 1;
            if let Some(c) = g.get_char_mut(ch) {
                c.quest_countdown = cd;
            }
            if cd <= 0 {
                set_questgiver(ch, None);
                if let Some(c) = g.get_char_mut(ch) {
                    c.next_quest = 30;
                    c.act_flags &= !PLR_QUESTOR;
                    c.quest_countdown = 0;
                    c.quest_mob = 0;
                }
                let msg = format!(
                    "You have run out of time for your quest!\r\nYou may quest again in {} minutes.\r\n",
                    30
                );
                g.send_to_char(ch, &msg);
            } else if cd > 0 && cd < 6 {
                g.send_to_char(
                    ch,
                    "Better hurry, you're almost out of time for your quest!\r\n",
                );
                // C `return`s here too (same list-scan quirk).
                return;
            }
        }
    }
}
