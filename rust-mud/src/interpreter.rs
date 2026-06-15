// Command interpreter (CircleMUD interpreter.c): argument tokenisers and the
// data-driven command_interpreter that abbreviation-matches against the
// 343-entry CMD_INFO table, gates on level/position, and dispatches.

use crate::command_table::{HandlerId, CMD_INFO};
use crate::state::GameState;
use crate::types::*;

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
    dispatch_with_alias(g, ch, input, 0);
}

/// Alias-aware entry: expand a leading alias (C perform_alias) before
/// dispatching, then run the real interpreter on the (possibly multi-command)
/// result. `depth` bounds alias recursion so a self-referential alias cannot
/// loop forever; the C input-queue model is naturally bounded per-pulse.
fn dispatch_with_alias(g: &mut GameState, ch: CharId, input: &str, depth: u32) {
    if depth < 16 {
        if let Some(expanded) = crate::alias::alias_replace(g, ch, input) {
            // A COMPLEX alias may expand to several ';'-separated commands,
            // returned joined by '\n'; run each in order (Rust analogue of C
            // pushing them onto the front of the input queue). A SIMPLE alias
            // yields a single line. Each expanded command is itself alias-
            // checked (depth-bounded) to mirror queue re-entry.
            for line in expanded.split('\n') {
                if !g.char_exists(ch) {
                    return;
                }
                dispatch_with_alias(g, ch, line, depth + 1);
            }
            return;
        }
    }
    run_command(g, ch, input);
}

/// The central dispatcher body (CircleMUD command_interpreter proper), run on
/// input that has already passed alias expansion.
fn run_command(g: &mut GameState, ch: CharId, input: &str) {
    let (level, is_npc, pos) = match g.get_char(ch) {
        Some(c) => (c.player.level, c.is_npc, c.position),
        None => return,
    };

    let input = input.trim_start();
    if input.is_empty() {
        return;
    }

    // First token = command word (lowercased); remainder keeps original case.
    let (arg, line) = any_one_arg(input);
    if arg.is_empty() {
        return;
    }

    // DG command triggers (room/mob/obj) get first refusal on ANY typed word
    // (dg_triggers command_*trigger). A consuming trigger ends the command.
    if !is_npc
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

    // Abbreviation match: first table entry whose name is prefixed by `arg`
    // and whose min_level the actor meets. Table order is load-bearing.
    let mut found = None;
    for e in CMD_INFO {
        if e.name == "\n" {
            break;
        }
        if e.name.starts_with(arg.as_str()) && level >= e.min_level {
            found = Some(*e);
            break;
        }
    }

    let entry = match found {
        Some(e) => e,
        None => {
            // Socials are loaded dynamically and are not in the static CMD_INFO
            // table (C splices them in via create_command_list()); try them
            // before giving up.
            if let Some(name) = crate::cmd_social::find_social(&arg) {
                crate::cmd_social::do_action_named(g, ch, name, line);
            } else {
                g.send_to_char(ch, "Huh?!?\r\n");
            }
            return;
        }
    };

    if is_npc && entry.min_level >= LVL_IMMORT {
        g.send_to_char(ch, "You can't use immortal commands while switched.\r\n");
        return;
    }

    if pos < entry.min_position {
        let msg = match pos {
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
        };
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
    use crate::{alias, arena, auction, ban, clan, cmd_comm, cmd_create, cmd_informative,
        cmd_item, cmd_movement, cmd_offensive, cmd_other, cmd_social, cmd_wizard,
        graph, house, mail, maputils, misc, olc, quest, shop, spell_parser};
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
        DoMasound => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "masound", arg); }
        DoMat => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mat", arg); }
        DoMecho => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mecho", arg); }
        DoMechoaround => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mechoaround", arg); }
        DoMexp => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mexp", arg); }
        DoMforce => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mforce", arg); }
        DoMforget => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mforget", arg); }
        DoMgold => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mgold", arg); }
        DoMgoto => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mgoto", arg); }
        DoMhunt => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mhunt", arg); }
        DoMjunk => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mjunk", arg); }
        DoMkill => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mkill", arg); }
        DoMload => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mload", arg); }
        DoMpurge => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mpurge", arg); }
        DoMremember => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mremember", arg); }
        DoMsend => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "msend", arg); }
        DoMteleport => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mteleport", arg); }
        DoMtransform => { crate::dg_mobcmd::dispatch_mob_command(g, ch, "mtransform", arg); }

        // --- maputils (maputils.c) — surface world map ---
        DoMap => maputils::do_map(g, ch, arg, subcmd),
        Pweather => maputils::pweather(g, ch, arg, subcmd),
        Lweather => maputils::lweather(g, ch, arg, subcmd),
        _ => {
            g.send_to_char(ch, "Sorry, that command hasn't been implemented yet.
");
        }
    }
}
