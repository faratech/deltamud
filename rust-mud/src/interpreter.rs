// Command interpreter (CircleMUD interpreter.c): argument tokenisers and the
// data-driven command_interpreter that abbreviation-matches against the
// 343-entry CMD_INFO table, gates on level/position, and dispatches.

use crate::command_table::{HandlerId, CMD_INFO};
use crate::flags::{AFF_HIDE, PLR_FROZEN, PRF2_INTANGIBLE, PRF2_LOCKOUT, PRF2_MBUILDING};
use crate::state::GameState;
use crate::types::*;

const CMD_LOCKOUT_MSG: &str =
    "Your terminal is currently locked!\r\nTo unlock please type 'unlock <yourpassword>'\r\n";
const CMD_FROZEN_MSG: &str = "You try, but the mind-numbing cold prevents you...\r\n";
const CMD_MBUILDING_BLOCK_MSG: &str = "You may not use that command in build mode.\r\n";
const CMD_INTANGIBLE_BLOCK_MSG: &str = "The intangible have no need for such abilities.\r\n";

const CMDS_MORTAL_BUILDERS_CANT_USE: &str =
    "sacrifice trigedit tlist tstat auction brew buy carve cast deposit \
donate drink drop eat fill fillet forge give group mount quaff recite sacrafice scribe school \
sell sip steal take tan taste train use withdraw nosummon chain";

const CMDS_DEAD_CAN_USE: &str = "qui quit look who say north south east west up down ' emote";

/// Fill words skipped by one_argument (CircleMUD fill[]).
const FILL_WORDS: &[&str] = &["in", "from", "with", "the", "on", "at", "to", "of"];

fn is_fill_word(w: &str) -> bool {
    FILL_WORDS.iter().any(|f| f.eq_ignore_ascii_case(w))
}

/// any_one_arg: first whitespace-delimited token (lowercased) + the rest
/// (original case, leading space trimmed). Does NOT skip fill words.
pub fn any_one_arg(argument: &str) -> (String, &str) {
    let s = argument.trim_start();
    match s.find(char::is_whitespace) {
        Some(pos) => (s[..pos].to_lowercase(), s[pos..].trim_start()),
        None => (s.to_lowercase(), ""),
    }
}

/// Command word split with CircleMUD's one-character non-alphanumeric special
/// case. This keeps shorthand commands like `'hello` as command `'` plus
/// argument `hello` instead of the literal word `'hello`.
fn command_arg_line(argument: &str) -> (String, &str) {
    let s = argument.trim_start();
    match s.chars().next() {
        Some(ch) if !ch.is_ascii_alphabetic() => {
            let len = ch.len_utf8();
            (ch.to_string(), s[len..].trim_start())
        }
        Some(_) => any_one_arg(s),
        None => (String::new(), ""),
    }
}

/// one_argument: like any_one_arg but skips leading fill words.
pub fn one_argument(argument: &str) -> (String, &str) {
    let mut rest = argument;
    loop {
        let (word, r) = any_one_arg(rest);
        if word.is_empty() {
            return (String::new(), "");
        }
        if is_fill_word(&word) {
            rest = r;
            continue;
        }
        return (word, r);
    }
}

/// half_chop: split into the first word (lowercased) and the remainder.
pub fn half_chop(argument: &str) -> (String, String) {
    let (first, rest) = any_one_arg(argument);
    (first, rest.to_string())
}

/// is_abbrev: true if `arg` is a non-empty case-insensitive prefix of `full`.
pub fn is_abbrev(arg: &str, full: &str) -> bool {
    !arg.is_empty() && full.to_lowercase().starts_with(&arg.to_lowercase())
}

/// Search a slice of names for a prefix match; returns the index (search_block,
/// exact=false semantics).
pub fn search_block(arg: &str, list: &[&str]) -> Option<usize> {
    let arg = arg.to_lowercase();
    list.iter().position(|s| s.to_lowercase().starts_with(&arg))
}

/// The central dispatcher (CircleMUD command_interpreter).
pub fn command_interpreter(g: &mut GameState, ch: CharId, input: &str) {
    run_command(g, ch, input);
}

/// The position-refusal text C emits when `GET_POS(ch) < minimum_position`
/// (interpreter.c:828-853). An empty string means "no message" (positions that
/// are never below any command minimum, e.g. Standing). Shared so socials —
/// which C gates through the same interpreter minimum_position path — refuse
/// identically.
pub fn position_refusal_msg(pos: Position) -> &'static str {
    match pos {
        Position::Dead => "Lie still; you are DEAD!!! :-(\r\n",
        Position::Incapacitated | Position::MortallyWounded => {
            "You are in a pretty bad shape, unable to do anything!\r\n"
        }
        Position::Stunned => "All you can do right now is think about the stars!\r\n",
        Position::Sleeping => "In your dreams, or what?\r\n",
        Position::Resting => "Nah... You feel too relaxed to do that..\r\n",
        Position::Sitting => "Maybe you should get on your feet first?\r\n",
        Position::Fighting => "No way!  You're fighting for your life!\r\n",
        Position::Meditating | Position::Standing => "",
    }
}

/// The central dispatcher body (CircleMUD command_interpreter proper), run on
/// input that has already passed alias expansion.
pub(crate) fn run_command(g: &mut GameState, ch: CharId, input: &str) {
    if !crate::handler::check_perm_duration(g, ch, AFF_HIDE) {
        if let Some(c) = g.get_char_mut(ch) {
            c.affect_flags &= !AFF_HIDE;
        }
    }

    let (level, is_npc, pos, gcmds, act_flags, prf2_flags) = match g.get_char(ch) {
        Some(c) => (
            c.player.level,
            c.is_npc,
            c.position,
            // The four god-command bitvectors (godcmds1..4), indexed by
            // godcmd_set-1 in the gate below. NPCs have all-zero godcmds.
            [c.godcmds1, c.godcmds2, c.godcmds3, c.godcmds4],
            c.act_flags,
            c.prf2_flags,
        ),
        None => return,
    };

    let input = input.trim_start();
    if input.is_empty() {
        return;
    }

    // First token = command word (lowercased); remainder keeps original case.
    let (arg, line) = command_arg_line(input);
    if arg.is_empty() {
        return;
    }

    if !is_npc && prf2_flags & PRF2_LOCKOUT != 0 {
        if arg != "unlock" {
            g.send_to_char(ch, CMD_LOCKOUT_MSG);
        } else {
            crate::cmd_other::do_lockout(g, ch, line, 0);
        }
        return;
    }

    // DG command triggers (room/mob/obj) get first refusal on ANY typed word
    // (dg_triggers command_*trigger). A consuming trigger ends the command.
    // C (interpreter.c:772) gates on `GET_LEVEL(ch) < LVL_IMMORT`, NOT on PC-ness:
    // mortal NPCs DO fire command triggers, and immortals do NOT — match that.
    if level < LVL_IMMORT
        && (crate::dg_triggers::command_wtrigger(g, ch, &arg, line)
            || crate::dg_triggers::command_mtrigger(g, ch, &arg, line)
            || crate::dg_triggers::command_otrigger(g, ch, &arg, line))
    {
        return;
    }

    // Room special-exit interception (interpreter.c:799): a standing player who
    // types the room special exit's ex_name moves through it, pre-empting normal
    // dispatch (and the "Huh?!?" fallthrough). C passes the whole input line;
    // ex_name is prefix-matched.
    if pos == Position::Standing && crate::cmd_movement::special_exit_command(g, ch, input) {
        return;
    }

    // Abbreviation match: first table entry whose name is prefixed by `arg`,
    // whose min_level the actor meets, AND (for god commands) whose required
    // godcmd bit the actor holds in the matching godcmds bitvector. Table order
    // is load-bearing.
    //
    // The godcmd gate mirrors interpreter.c:786-789: a god command is usable
    // only if `(cmd.godcmd & GCMDn_FLAGS(ch))` is set in the godcmds<set>
    // bitvector named by the command's GOD_CMD/2/3/4 marker. The C OR-clause at
    // 790-791 also lets `GET_LEVEL(ch) >= LVL_IMPL` bypass the gate entirely
    // (so e.g. `citizen`/`mobdie`/`slowns`, whose godcmd bit is 0 and thus can
    // never match, remain reachable by the Implementor). A god command that
    // fails the gate is SKIPPED — the loop walks on, exactly as in C, so the
    // word falls through to a later match / "Huh?!?" rather than erroring.
    let mut found = None;
    for e in CMD_INFO {
        if e.name == "\n" {
            break;
        }
        if !(e.name.starts_with(arg.as_str()) && level >= e.min_level) {
            continue;
        }
        if e.godcmd_set != 0 && level < LVL_IMPL {
            let bits = gcmds[(e.godcmd_set - 1) as usize];
            if (e.godcmd & bits) == 0 {
                continue; // lacks the required god-command bit; keep searching
            }
        }
        found = Some(*e);
        break;
    }

    if act_flags & PLR_FROZEN != 0 && level < LVL_IMPL {
        g.send_to_char(ch, CMD_FROZEN_MSG);
        return;
    }

    let entry = match found {
        Some(e) => e,
        None => {
            // Socials are loaded dynamically and are not in the static CMD_INFO
            // table (C splices them in via create_command_list()); try them
            // before giving up. In C the resolved social IS a
            // complete_cmd_info[] entry, so the build-mode gate below runs on
            // it before the command executes — apply it here for the same
            // ordering (interpreter.c:800-804).
            if let Some(name) = crate::cmd_social::find_social(&arg) {
                let allowed = crate::cmd_social::social_min_level(&name)
                    .map(|min| level as i32 >= min)
                    .unwrap_or(false);
                if !allowed {
                    g.send_to_char(ch, "Huh?!?\r\n");
                    return;
                }
                if prf2_flags & PRF2_MBUILDING != 0
                    && crate::handler::isname(&name, CMDS_MORTAL_BUILDERS_CANT_USE)
                {
                    g.send_to_char(ch, CMD_MBUILDING_BLOCK_MSG);
                    return;
                }
                crate::cmd_social::do_action_named(g, ch, &name, line);
            } else {
                g.send_to_char(ch, "Huh?!?\r\n");
            }
            return;
        }
    };

    if prf2_flags & PRF2_MBUILDING != 0
        && crate::handler::isname(entry.name, CMDS_MORTAL_BUILDERS_CANT_USE)
    {
        g.send_to_char(ch, CMD_MBUILDING_BLOCK_MSG);
        return;
    }

    if prf2_flags & PRF2_INTANGIBLE != 0
        && prf2_flags & PRF2_MBUILDING == 0
        && !crate::handler::isname(entry.name, CMDS_DEAD_CAN_USE)
    {
        g.send_to_char(ch, CMD_INTANGIBLE_BLOCK_MSG);
        return;
    }

    if is_npc && entry.min_level >= LVL_IMMORT {
        g.send_to_char(ch, "You can't use immortal commands while switched.\r\n");
        return;
    }

    if pos < entry.min_position {
        let msg = position_refusal_msg(pos);
        if !msg.is_empty() {
            g.send_to_char(ch, msg);
        }
        return;
    }

    // Try special procedures before the normal command (C interpreter.c:856:
    // `else if (!special(ch, cmd, line)) ...`). `arg` is the typed command word
    // (lowercased); spec procs prefix-match it against canonical names. If any
    // proc consumes the command we skip normal dispatch entirely.
    if crate::spec_assign::special(g, ch, &arg, line) {
        return;
    }

    dispatch(g, ch, entry.handler, line, entry.subcmd);
}

/// Map a HandlerId to its Rust command function. Tier-0 wires the playable
/// core; unported handlers report the CircleMUD "not implemented" message.
/// Later batches fill in arms here as their modules land.
fn dispatch(g: &mut GameState, ch: CharId, handler: HandlerId, arg: &str, subcmd: i32) {
    use crate::{
        alias, arena, auction, ban, clan, cmd_comm, cmd_create, cmd_informative, cmd_item,
        cmd_movement, cmd_offensive, cmd_other, cmd_social, cmd_wizard, graph, house, mail,
        maputils, misc, olc, quest, shop, spell_parser,
    };
    use HandlerId::*;
    match handler {
        // --- alias (interpreter.c) ---
        DoAlias => alias::do_alias(g, ch, arg, subcmd),
        // --- cmd_informative (act.informative.c) ---
        DoLook => cmd_informative::do_look(g, ch, arg, subcmd),
        DoExamine => cmd_informative::do_examine(g, ch, arg, subcmd),
        DoExits => cmd_informative::do_exits(g, ch, arg, subcmd),
        DoGold => cmd_informative::do_gold(g, ch, arg, subcmd),
        DoCheckbail => cmd_informative::do_checkbail(g, ch, arg, subcmd),
        DoMcheck => cmd_informative::do_mcheck(g, ch, arg, subcmd),
        DoWorth => cmd_informative::do_worth(g, ch, arg, subcmd),
        DoStatus => cmd_informative::do_status(g, ch, arg, subcmd),
        DoInventory => cmd_informative::do_inventory(g, ch, arg, subcmd),
        DoEquipment => cmd_informative::do_equipment(g, ch, arg, subcmd),
        DoTime => cmd_informative::do_time(g, ch, arg, subcmd),
        DoHelp => cmd_informative::do_help(g, ch, arg, subcmd),
        DoWho => cmd_informative::do_who(g, ch, arg, subcmd),
        DoUsers => cmd_informative::do_users(g, ch, arg, subcmd),
        DoGenPs => cmd_informative::do_gen_ps(g, ch, arg, subcmd),
        DoWhere => cmd_informative::do_where(g, ch, arg, subcmd),
        DoConsider => cmd_informative::do_consider(g, ch, arg, subcmd),
        DoDiagnose => cmd_informative::do_diagnose(g, ch, arg, subcmd),
        DoColor => cmd_informative::do_color(g, ch, arg, subcmd),
        DoToggle => cmd_informative::do_toggle(g, ch, arg, subcmd),
        DoCommands => cmd_informative::do_commands(g, ch, arg, subcmd),
        DoPlayers => cmd_informative::do_players(g, ch, arg, subcmd),
        DoMudheal => cmd_informative::do_mudheal(g, ch, arg, subcmd),
        DoForage => cmd_informative::do_forage(g, ch, arg, subcmd),
        DoWhois => cmd_informative::do_whois(g, ch, arg, subcmd),
        DoScan => cmd_informative::do_scan(g, ch, arg, subcmd),
        DoRnum => cmd_informative::do_rnum(g, ch, arg, subcmd),
        DoLevels => cmd_informative::do_levels(g, ch, arg, subcmd),
        // --- cmd_item (act.item.c) ---
        DoGet => cmd_item::do_get(g, ch, arg, subcmd),
        DoPut => cmd_item::do_put(g, ch, arg, subcmd),
        DoDrop => cmd_item::do_drop(g, ch, arg, subcmd),
        DoGive => cmd_item::do_give(g, ch, arg, subcmd),
        DoDrink => cmd_item::do_drink(g, ch, arg, subcmd),
        DoEat => cmd_item::do_eat(g, ch, arg, subcmd),
        DoPour => cmd_item::do_pour(g, ch, arg, subcmd),
        DoWear => cmd_item::do_wear(g, ch, arg, subcmd),
        DoWield => cmd_item::do_wield(g, ch, arg, subcmd),
        DoGrab => cmd_item::do_grab(g, ch, arg, subcmd),
        DoRemove => cmd_item::do_remove(g, ch, arg, subcmd),
        DoSac => cmd_item::do_sac(g, ch, arg, subcmd),
        DoRepair => cmd_item::do_repair(g, ch, arg, subcmd),
        // --- cmd_movement (act.movement.c) ---
        DoMove => cmd_movement::do_move(g, ch, arg, subcmd),
        DoGenDoor => cmd_movement::do_gen_door(g, ch, arg, subcmd),
        DoEnter => cmd_movement::do_enter(g, ch, arg, subcmd),
        DoLeave => cmd_movement::do_leave(g, ch, arg, subcmd),
        DoStand => cmd_movement::do_stand(g, ch, arg, subcmd),
        DoSit => cmd_movement::do_sit(g, ch, arg, subcmd),
        DoRest => cmd_movement::do_rest(g, ch, arg, subcmd),
        DoSleep => cmd_movement::do_sleep(g, ch, arg, subcmd),
        DoMeditate => cmd_movement::do_meditate(g, ch, arg, subcmd),
        DoWake => cmd_movement::do_wake(g, ch, arg, subcmd),
        DoFollow => cmd_movement::do_follow(g, ch, arg, subcmd),
        DoMount => cmd_movement::do_mount(g, ch, arg, subcmd),
        DoDismount => cmd_movement::do_dismount(g, ch, arg, subcmd),
        DoBuck => cmd_movement::do_buck(g, ch, arg, subcmd),
        DoTame => cmd_movement::do_tame(g, ch, arg, subcmd),
        // --- cmd_comm (act.comm.c) ---
        DoIgnore => cmd_comm::do_ignore(g, ch, arg, subcmd),
        DoSay => cmd_comm::do_say(g, ch, arg, subcmd),
        DoGsay => cmd_comm::do_gsay(g, ch, arg, subcmd),
        DoTell => cmd_comm::do_tell(g, ch, arg, subcmd),
        DoReply => cmd_comm::do_reply(g, ch, arg, subcmd),
        DoSpecComm => cmd_comm::do_spec_comm(g, ch, arg, subcmd),
        DoWrite => cmd_comm::do_write(g, ch, arg, subcmd),
        DoPage => cmd_comm::do_page(g, ch, arg, subcmd),
        DoGenComm => cmd_comm::do_gen_comm(g, ch, arg, subcmd),
        DoQcomm => cmd_comm::do_qcomm(g, ch, arg, subcmd),
        // --- cmd_social (act.social.c) ---
        DoInsult => cmd_social::do_insult(g, ch, arg, subcmd),
        // --- cmd_create (act.create.c) ---
        DoBrew => cmd_create::do_brew(g, ch, arg, subcmd),
        DoScribe => cmd_create::do_scribe(g, ch, arg, subcmd),
        DoForge => cmd_create::do_forge(g, ch, arg, subcmd),
        // --- cmd_other (act.other.c) ---
        DoGenTog => cmd_other::do_gen_tog(g, ch, arg, subcmd),
        DoDisplay => cmd_other::do_display(g, ch, arg, subcmd),
        DoQuit => cmd_other::do_quit(g, ch, arg, subcmd),
        DoSave => cmd_other::do_save(g, ch, arg, subcmd),
        DoPractice => cmd_other::do_practice(g, ch, arg, subcmd),
        DoVisible => cmd_other::do_visible(g, ch, arg, subcmd),
        DoTitle => cmd_other::do_title(g, ch, arg, subcmd),
        DoGroup => cmd_other::do_group(g, ch, arg, subcmd),
        DoUngroup => cmd_other::do_ungroup(g, ch, arg, subcmd),
        DoReport => cmd_other::do_report(g, ch, arg, subcmd),
        DoTrack => graph::do_track(g, ch, arg, subcmd),
        DoSplit => cmd_other::do_split(g, ch, arg, subcmd),
        DoUse => cmd_other::do_use(g, ch, arg, subcmd),
        DoWimpy => cmd_other::do_wimpy(g, ch, arg, subcmd),
        DoRecall => cmd_other::do_recall(g, ch, arg, subcmd),
        DoRetreat => cmd_other::do_retreat(g, ch, arg, subcmd),
        DoGenWrite => cmd_other::do_gen_write(g, ch, arg, subcmd),
        DoSneak => cmd_other::do_sneak(g, ch, arg, subcmd),
        DoHide => cmd_other::do_hide(g, ch, arg, subcmd),
        DoSteal => cmd_other::do_steal(g, ch, arg, subcmd),
        DoTrain => cmd_other::do_train(g, ch, arg, subcmd),
        DoSchool => cmd_other::do_school(g, ch, arg, subcmd),
        DoObserve => arena::do_observe(g, ch, arg, subcmd),
        DoLockout => cmd_other::do_lockout(g, ch, arg, subcmd),
        DoTan => cmd_other::do_tan(g, ch, arg, subcmd),
        DoFillet => cmd_other::do_fillet(g, ch, arg, subcmd),
        DoCarve => cmd_other::do_carve(g, ch, arg, subcmd),
        DoGenAtm => cmd_other::do_gen_atm(g, ch, arg, subcmd),
        DoPostbail => cmd_other::do_postbail(g, ch, arg, subcmd),
        DoNotHere => cmd_other::do_not_here(g, ch, arg, subcmd),
        DoAffected => cmd_other::do_affected(g, ch, arg, subcmd),
        DoSlist => cmd_other::do_slist(g, ch, arg, subcmd),
        DoEmail => cmd_other::do_email(g, ch, arg, subcmd),
        DoBuild => cmd_other::do_build(g, ch, arg, subcmd),
        DoMobdie => cmd_other::do_mobdie(g, ch, arg, subcmd),
        // --- cmd_offensive (act.offensive.c) ---
        DoAssist => cmd_offensive::do_assist(g, ch, arg, subcmd),
        DoHit => cmd_offensive::do_hit(g, ch, arg, subcmd),
        DoKill => cmd_offensive::do_kill(g, ch, arg, subcmd),
        DoTarget => cmd_offensive::do_target(g, ch, arg, subcmd),
        DoBackstab => cmd_offensive::do_backstab(g, ch, arg, subcmd),
        DoOrder => cmd_offensive::do_order(g, ch, arg, subcmd),
        DoFlee => cmd_offensive::do_flee(g, ch, arg, subcmd),
        DoChainFooting => cmd_offensive::do_chain_footing(g, ch, arg, subcmd),
        DoBash => cmd_offensive::do_bash(g, ch, arg, subcmd),
        DoRescue => cmd_offensive::do_rescue(g, ch, arg, subcmd),
        DoKick => cmd_offensive::do_kick(g, ch, arg, subcmd),
        DoBerserk => cmd_offensive::do_berserk(g, ch, arg, subcmd),
        DoDisarm => cmd_offensive::do_disarm(g, ch, arg, subcmd),
        DoTrip => cmd_offensive::do_trip(g, ch, arg, subcmd),
        DoCamouflage => cmd_offensive::do_camouflage(g, ch, arg, subcmd),
        DoBlanket => cmd_offensive::do_blanket(g, ch, arg, subcmd),
        // --- cmd_wizard (act.wizard.c) ---
        DoEcho => cmd_wizard::do_echo(g, ch, arg, subcmd),
        DoSend => cmd_wizard::do_send(g, ch, arg, subcmd),
        DoAt => cmd_wizard::do_at(g, ch, arg, subcmd),
        DoGoto => cmd_wizard::do_goto(g, ch, arg, subcmd),
        DoTrans => cmd_wizard::do_trans(g, ch, arg, subcmd),
        DoTeleport => cmd_wizard::do_teleport(g, ch, arg, subcmd),
        DoVnum => cmd_wizard::do_vnum(g, ch, arg, subcmd),
        DoStat => cmd_wizard::do_stat(g, ch, arg, subcmd),
        DoShutdown => cmd_wizard::do_shutdown(g, ch, arg, subcmd),
        DoSnoop => cmd_wizard::do_snoop(g, ch, arg, subcmd),
        DoSwitch => cmd_wizard::do_switch(g, ch, arg, subcmd),
        DoReturn => cmd_wizard::do_return(g, ch, arg, subcmd),
        DoLoad => cmd_wizard::do_load(g, ch, arg, subcmd),
        DoAload => cmd_wizard::do_aload(g, ch, arg, subcmd),
        DoVstat => cmd_wizard::do_vstat(g, ch, arg, subcmd),
        DoPurge => cmd_wizard::do_purge(g, ch, arg, subcmd),
        DoSyslog => cmd_wizard::do_syslog(g, ch, arg, subcmd),
        DoAdvance => cmd_wizard::do_advance(g, ch, arg, subcmd),
        DoRestore => cmd_wizard::do_restore(g, ch, arg, subcmd),
        DoInvis => cmd_wizard::do_invis(g, ch, arg, subcmd),
        DoGecho => cmd_wizard::do_gecho(g, ch, arg, subcmd),
        DoGplague => cmd_wizard::do_gplague(g, ch, arg, subcmd),
        DoGcureplague => cmd_wizard::do_gcureplague(g, ch, arg, subcmd),
        DoPoofset => cmd_wizard::do_poofset(g, ch, arg, subcmd),
        DoDc => cmd_wizard::do_dc(g, ch, arg, subcmd),
        DoWizlock => cmd_wizard::do_wizlock(g, ch, arg, subcmd),
        DoDate => cmd_wizard::do_date(g, ch, arg, subcmd),
        DoLast => cmd_wizard::do_last(g, ch, arg, subcmd),
        DoForce => cmd_wizard::do_force(g, ch, arg, subcmd),
        DoWiznet => cmd_wizard::do_wiznet(g, ch, arg, subcmd),
        DoZreset => cmd_wizard::do_zreset(g, ch, arg, subcmd),
        DoWizutil => cmd_wizard::do_wizutil(g, ch, arg, subcmd),
        DoShow => cmd_wizard::do_show(g, ch, arg, subcmd),
        DoSet => cmd_wizard::do_set(g, ch, arg, subcmd),
        DoSetreboot => cmd_wizard::do_setreboot(g, ch, arg, subcmd),
        DoRewiz => cmd_wizard::do_rewiz(g, ch, arg, subcmd),
        DoRlist => cmd_wizard::do_rlist(g, ch, arg, subcmd),
        DoMlist => cmd_wizard::do_mlist(g, ch, arg, subcmd),
        DoOlist => cmd_wizard::do_olist(g, ch, arg, subcmd),
        DoWhoupd => cmd_wizard::do_whoupd(g, ch, arg, subcmd),
        DoIsay => cmd_wizard::do_isay(g, ch, arg, subcmd),
        DoMcasters => cmd_wizard::do_mcasters(g, ch, arg, subcmd),
        DoCopyto => cmd_wizard::do_copyto(g, ch, arg, subcmd),
        DoDig => cmd_wizard::do_dig(g, ch, arg, subcmd),
        DoEsave => cmd_wizard::do_esave(g, ch, arg, subcmd),
        DoTmobdie => cmd_wizard::do_tmobdie(g, ch, arg, subcmd),
        DoWrestrict => cmd_wizard::do_wrestrict(g, ch, arg, subcmd),
        DoRespec => cmd_wizard::do_respec(g, ch, arg, subcmd),
        DoQuestmobs => cmd_wizard::do_questmobs(g, ch, arg, subcmd),
        DoReward => cmd_wizard::do_reward(g, ch, arg, subcmd),
        DoAreas => cmd_wizard::do_areas(g, ch, arg, subcmd),
        DoVwear => cmd_wizard::do_vwear(g, ch, arg, subcmd),
        DoTedit => cmd_wizard::do_tedit(g, ch, arg, subcmd),
        DoRename => cmd_wizard::do_rename(g, ch, arg, subcmd),
        DoPeace => cmd_wizard::do_peace(g, ch, arg, subcmd),
        DoCitizen => cmd_wizard::do_citizen(g, ch, arg, subcmd),
        DoAddsnow => cmd_wizard::do_addsnow(g, ch, arg, subcmd),
        DoDelsnow => cmd_wizard::do_delsnow(g, ch, arg, subcmd),
        DoLevelme => cmd_wizard::do_levelme(g, ch, arg, subcmd),
        DoCopyover => cmd_wizard::do_copyover(g, ch, arg, subcmd),
        // --- content / economy (Batch 11) ---
        DoBuy => shop::do_buy(g, ch, arg, subcmd),
        DoSell => shop::do_sell(g, ch, arg, subcmd),
        DoList => shop::do_list(g, ch, arg, subcmd),
        DoValue => shop::do_value(g, ch, arg, subcmd),
        DoAppraise => shop::do_appraise(g, ch, arg, subcmd),
        DoClan => clan::do_clan(g, ch, arg, subcmd),
        DoCsay => clan::do_csay(g, ch, arg, subcmd),
        DoAuction => auction::do_auction(g, ch, arg, subcmd),
        DoBid => auction::do_bid(g, ch, arg, subcmd),
        DoStopAuction => auction::do_stop_auction(g, ch, arg, subcmd),
        DoAuctioneer => auction::do_auctioneer(g, ch, arg, subcmd),
        DoHcontrol => house::do_hcontrol(g, ch, arg, subcmd),
        DoHouse => house::do_house(g, ch, arg, subcmd),
        DoBed => house::do_bed(g, ch, arg, subcmd),
        DoAutoquest => quest::do_autoquest(g, ch, arg, subcmd),
        DoMail => mail::do_mail(g, ch, arg, subcmd),
        // --- magic (full spell/skill system, Batch 6) ---
        DoCast => spell_parser::do_cast(g, ch, arg, subcmd),
        DoSkillset => spell_parser::do_skillset(g, ch, arg, subcmd),
        DoBan => ban::do_ban(g, ch, arg, subcmd),
        DoUnban => ban::do_unban(g, ch, arg, subcmd),
        // --- misc (leftover ACMDs from spells.c / db.c / dg_scripts.c) ---
        DoListen => crate::misc::do_listen(g, ch, arg, subcmd),
        DoSpeed => crate::misc::do_speed(g, ch, arg, subcmd),
        DoReboot => crate::misc::do_reboot(g, ch, arg, subcmd),
        DoPfileclean => crate::misc::do_pfileclean(g, ch, arg, subcmd),
        DoAttach => crate::misc::do_attach(g, ch, arg, subcmd),
        DoDetach => crate::misc::do_detach(g, ch, arg, subcmd),
        // --- OLC / trigger-admin / mob-script commands (final completion) ---
        DoOlc => olc::do_olc(g, ch, arg, subcmd),
        DoTlist => misc::do_tlist(g, ch, arg, subcmd),
        DoTstat => misc::do_tstat(g, ch, arg, subcmd),
        DoRebalance => misc::do_rebalance(g, ch, arg, subcmd),
        DoMasound => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "masound", arg);
        }
        DoMat => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mat", arg);
        }
        DoMecho => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mecho", arg);
        }
        DoMechoaround => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mechoaround", arg);
        }
        DoMexp => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mexp", arg);
        }
        DoMforce => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mforce", arg);
        }
        DoMforget => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mforget", arg);
        }
        DoMgold => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mgold", arg);
        }
        DoMgoto => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mgoto", arg);
        }
        DoMhunt => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mhunt", arg);
        }
        DoMjunk => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mjunk", arg);
        }
        DoMkill => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mkill", arg);
        }
        DoMload => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mload", arg);
        }
        DoMpurge => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mpurge", arg);
        }
        DoMremember => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mremember", arg);
        }
        DoMsend => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "msend", arg);
        }
        DoMteleport => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mteleport", arg);
        }
        DoMtransform => {
            crate::dg_mobcmd::dispatch_mob_command(g, ch, "mtransform", arg);
        }

        // --- maputils (maputils.c) — surface world map ---
        DoMap => maputils::do_map(g, ch, arg, subcmd),
        Pweather => maputils::pweather(g, ch, arg, subcmd),
        Lweather => maputils::lweather(g, ch, arg, subcmd),
        _ => {
            g.send_to_char(
                ch,
                "Sorry, that command hasn't been implemented yet.
",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{Affect, Character};
    use crate::config::Config;
    use crate::connection::{ConState, Descriptor};
    use crate::flags::{AFF_HIDE, PLR_FROZEN, PRF2_INTANGIBLE, PRF2_LOCKOUT, PRF2_MBUILDING};
    use crate::types::{Class, ConnId, Race};

    fn test_game_with_player() -> (GameState, CharId, ConnId) {
        let mut g = GameState::new(Config::default());
        let conn = ConnId(1);
        let mut ch = Character::new_player("Tester".to_string(), Class::Warrior, Race::Human);
        ch.desc = Some(conn);
        let ch_id = g.create_char(ch);
        let mut d = Descriptor::new(conn, "test".to_string());
        d.state = ConState::Playing;
        d.character = Some(ch_id);
        g.descriptors.insert(conn, d);
        (g, ch_id, conn)
    }

    fn outbuf(g: &GameState, conn: ConnId) -> &str {
        &g.descriptors.get(&conn).unwrap().outbuf
    }

    #[test]
    fn run_command_strips_temporary_hide_before_empty_input() {
        let (mut g, ch, _conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().affect_flags |= AFF_HIDE;

        command_interpreter(&mut g, ch, "");

        assert_eq!(g.get_char(ch).unwrap().affect_flags & AFF_HIDE, 0);
    }

    #[test]
    fn run_command_preserves_permanent_hide() {
        let (mut g, ch, _conn) = test_game_with_player();
        let c = g.get_char_mut(ch).unwrap();
        c.affect_flags |= AFF_HIDE;
        c.affected.push(Affect {
            spell_type: -1,
            duration: -1,
            modifier: 0,
            location: 0,
            bitvector: AFF_HIDE,
            caster: None,
        });

        command_interpreter(&mut g, ch, "");

        assert_ne!(g.get_char(ch).unwrap().affect_flags & AFF_HIDE, 0);
    }

    #[test]
    fn lockout_blocks_everything_except_unlock() {
        let (mut g, ch, conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().prf2_flags |= PRF2_LOCKOUT;

        command_interpreter(&mut g, ch, "say hello");

        assert_eq!(outbuf(&g, conn), CMD_LOCKOUT_MSG);
        assert_ne!(g.get_char(ch).unwrap().prf2_flags & PRF2_LOCKOUT, 0);
    }

    #[test]
    fn lockout_unlock_routes_to_lockout_handler() {
        let (mut g, ch, conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().prf2_flags |= PRF2_LOCKOUT;

        command_interpreter(&mut g, ch, "unlock secret");

        assert!(outbuf(&g, conn).contains("OK. Your terminal is now unlocked."));
        assert_eq!(g.get_char(ch).unwrap().prf2_flags & PRF2_LOCKOUT, 0);
    }

    #[test]
    fn frozen_blocks_resolved_and_unknown_commands() {
        let (mut g, ch, conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().act_flags |= PLR_FROZEN;

        command_interpreter(&mut g, ch, "bogus");

        assert_eq!(outbuf(&g, conn), CMD_FROZEN_MSG);
    }

    #[test]
    fn build_mode_blocks_disallowed_builder_commands() {
        let (mut g, ch, conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().prf2_flags |= PRF2_MBUILDING;

        command_interpreter(&mut g, ch, "buy bread");

        assert_eq!(outbuf(&g, conn), CMD_MBUILDING_BLOCK_MSG);
    }

    #[test]
    fn intangible_blocks_non_dead_commands() {
        let (mut g, ch, conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().prf2_flags |= PRF2_INTANGIBLE;

        command_interpreter(&mut g, ch, "cast armor");

        assert_eq!(outbuf(&g, conn), CMD_INTANGIBLE_BLOCK_MSG);
    }

    #[test]
    fn apostrophe_command_uses_one_character_command_split() {
        let (mut g, ch, conn) = test_game_with_player();

        command_interpreter(&mut g, ch, "'hello");

        assert!(!outbuf(&g, conn).contains("Huh?!?"));
    }

    /// #315: a social whose command word is in cmds_mortal_builders_cant_use is
    /// refused in build mode, exactly as C refuses it once the socials are
    /// spliced into complete_cmd_info[] (interpreter.c:800-804).
    #[test]
    fn build_mode_blocks_a_builder_blocked_social_315() {
        // The shipped socials file has no command colliding with
        // cmds_mortal_builders_cant_use, so install the real table plus one
        // extra social — "sacrafice", a blocked word that is not itself a
        // command, so the interpreter still reaches the social branch.
        let real = std::fs::read_to_string("../lib/misc/socials").unwrap();
        let mut extra = String::from("\n~sacrafice sacrafice 0 5 0 0\n");
        extra.push_str("You sacrafice a fish.\r\n"); // char_no_arg
        extra.push_str("$n sacrafices a fish.\r\n"); // others_no_arg
        for _ in 0..11 {
            extra.push_str("#\r\n"); // the remaining NULL slots
        }
        extra.push_str("$\r\n");

        let path = std::env::temp_dir().join(format!(
            "deltamud-socials-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The loader stops at the file's "$" sentinel, so append our social
        // before it rather than after it.
        let body = match real.trim_end().strip_suffix('$') {
            Some(head) => head.to_string(),
            None => real.clone(),
        };
        std::fs::write(&path, format!("{}{}\n$\n", body, extra)).unwrap();
        crate::cmd_social::boot_socials(Some(path.to_str().unwrap()));

        let (mut g, ch, conn) = test_game_with_player();
        g.get_char_mut(ch).unwrap().prf2_flags |= PRF2_MBUILDING;

        command_interpreter(&mut g, ch, "sacrafice salmon");

        assert_eq!(outbuf(&g, conn), CMD_MBUILDING_BLOCK_MSG);

        // A social outside the blocked list still runs in build mode.
        g.descriptors.get_mut(&conn).unwrap().outbuf.clear();
        command_interpreter(&mut g, ch, "smile");
        assert!(outbuf(&g, conn).contains("You smile happily."));

        // Restore the shipped socials table for the remaining tests.
        crate::cmd_social::boot_socials(Some("../lib/misc/socials"));
        let _ = std::fs::remove_file(&path);
    }

    /// #315: outside build mode the blocked social runs normally.
    #[test]
    fn builder_blocked_social_runs_outside_build_mode_315() {
        let real = std::fs::read_to_string("../lib/misc/socials").unwrap();
        let mut extra = String::from("\n~sacrafice sacrafice 0 5 0 0\n");
        extra.push_str("You sacrafice a fish.\r\n");
        extra.push_str("$n sacrafices a fish.\r\n");
        for _ in 0..11 {
            extra.push_str("#\r\n");
        }
        extra.push_str("$\r\n");

        let path = std::env::temp_dir().join(format!(
            "deltamud-socials-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The loader stops at the file's "$" sentinel, so append our social
        // before it rather than after it.
        let body = match real.trim_end().strip_suffix('$') {
            Some(head) => head.to_string(),
            None => real.clone(),
        };
        std::fs::write(&path, format!("{}{}\n$\n", body, extra)).unwrap();
        crate::cmd_social::boot_socials(Some(path.to_str().unwrap()));

        let (mut g, ch, conn) = test_game_with_player();

        command_interpreter(&mut g, ch, "sacrafice salmon");

        assert_eq!(outbuf(&g, conn), "You sacrafice a fish.\r\n");

        crate::cmd_social::boot_socials(Some("../lib/misc/socials"));
        let _ = std::fs::remove_file(&path);
    }
}
