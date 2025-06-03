/* ************************************************************************
   *   File: interpreter.c                                 Part of CircleMUD *
   *  Usage: parse user commands, search for specials, call ACMD functions   *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#define __INTERPRETER_C__

#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "comm.h"
#include "interpreter.h"
#include "db.h"
#include "utils.h"
#include "spells.h"
#include "handler.h"
#include "mail.h"
#include "screen.h"
#include "olc.h"
#include "dg_scripts.h"
#include "gcmd.h"
#include "maputils.h"
#include "dbinterface.h"

extern int jail_num;            /* see config.c */
extern int newbie_room;         /* see config.c */
extern int check_multiplaying(char *hostname);
extern const struct title_type titles[NUM_CLASSES][LVL_IMPL + 1];
extern char *motd;
extern char *startup;
extern char *imotd;
extern char *background;
extern char *news;
extern char *MENU;
extern char *WELC_MESSG;
extern char *START_MESSG;
extern char *ASK_NAME;
extern char *policies;
extern struct char_data *character_list;
extern struct descriptor_data *descriptor_list;
extern struct player_index_element *player_table;
extern int top_of_p_table;
extern int circle_restrict;
extern struct index_data *mob_index;
extern struct index_data *obj_index;
extern struct room_data *world;

/* external functions */
void hedit_parse(struct descriptor_data *d, char *arg);
void do_newbie (struct char_data *vict);
void do_oldbie (struct char_data *vict);
void echo_on (struct descriptor_data *d);
void echo_off (struct descriptor_data *d);
void do_start (struct char_data *ch);
void init_char (struct char_data *ch);
void read_aliases(struct char_data *ch);
void write_aliases(struct char_data *ch);
void roll_real_abils (struct char_data *ch);
void user_cntr(struct descriptor_data *d);
int create_entry (char *name);
int special (struct char_data *ch, int cmd, char *arg);
int isbanned (char *hostname);
int Valid_Name (char *newname);
int parse_town (char arg);
void oedit_parse (struct descriptor_data *d, char *arg);
void redit_parse (struct descriptor_data *d, char *arg);
void zedit_parse (struct descriptor_data *d, char *arg);
void medit_parse (struct descriptor_data *d, char *arg);
void sedit_parse (struct descriptor_data *d, char *arg);
void trigedit_parse(struct descriptor_data *d, char *arg);
void aedit_parse(struct descriptor_data *d, char *arg);

/* prototypes for all do_x functions. */
ACMD (do_action);
ACMD (do_addsnow);
ACMD (do_advance);
ACMD (do_affected);
ACMD (do_alias);
ACMD (do_aload);
ACMD (do_areas);
ACMD (do_assist);
ACMD (do_at);
ACMD (do_auction);
ACMD (do_auctioneer);
ACMD (do_autoquest);
ACMD (do_backstab);
ACMD (do_ban);
ACMD (do_bail);
ACMD (do_bash);
ACMD (do_berserk);
ACMD (do_bed);
ACMD (do_bid);
ACMD (do_blanket);
ACMD (do_brew);
ACMD (do_buck);
ACMD (do_build);
ACMD (do_carve);
ACMD (do_cast);
ACMD (do_camouflage);
ACMD (do_checkbail);
ACMD (do_chain_footing);
ACMD (do_citizen);
ACMD (do_clan);
ACMD (do_color);
ACMD (do_commands);
ACMD (do_consider);
ACMD (do_copyover);
ACMD (do_copyto);
ACMD (do_credits);
ACMD (do_csay);
ACMD (do_date);
ACMD (do_dc);
ACMD (do_delsnow);
ACMD (do_diagnose);
ACMD (do_disarm);
ACMD (do_dismount);
ACMD (do_dig);
ACMD (do_display);
ACMD (do_drink);
ACMD (do_drop);
ACMD (do_eat);
ACMD (do_echo);
ACMD (do_email);
ACMD (do_enter);
ACMD (do_equipment);
ACMD (do_esave);
ACMD (do_examine);
ACMD (do_exit);
ACMD (do_exits);
ACMD (do_fillet);
ACMD (do_flee);
ACMD (do_follow);
ACMD (do_forage);
ACMD (do_forge);
ACMD (do_force);
ACMD (do_gecho);
ACMD (do_gplague);
ACMD (do_gcureplague);
ACMD (do_gmote);
ACMD (do_gen_atm);
ACMD (do_gen_comm);
ACMD (do_gen_door);
ACMD (do_gen_ps);
ACMD (do_gen_tog);
ACMD (do_gen_write);
ACMD (do_get);
ACMD (do_give);
ACMD (do_gold);
ACMD (do_goto);
ACMD (do_grab);
ACMD (do_group);
ACMD (do_gsay);
ACMD (do_hcontrol);
ACMD (do_help);
ACMD (do_hide);
ACMD (do_hit);
ACMD (do_house);
ACMD (do_ignore);
ACMD (do_info);
ACMD (do_insult);
ACMD (do_inventory);
ACMD (do_invis);
ACMD (do_isay);
ACMD (do_kick);
ACMD (do_kill);
ACMD (do_last);
ACMD (do_leave);
ACMD (do_levels);
ACMD (do_listen);
ACMD (do_load);
ACMD (do_look);
ACMD (do_lockout);
ACMD (do_mcheck);
ACMD (do_mcasters);
ACMD (do_meditate);
ACMD (do_mlist);
ACMD (do_move);
ACMD (do_mobdie);
ACMD(do_mount);
ACMD (do_map);
ACMD (do_mudheal);
ACMD (do_not_here);
ACMD (do_observe);
ACMD (do_offer);
ACMD (do_olc);
ACMD (do_olist);
ACMD (do_order);
ACMD (do_page);
ACMD (do_peace);
ACMD (do_pfileclean);
ACMD (do_poofset);
ACMD (do_postbail);
ACMD (do_pour);
ACMD (do_players);
ACMD (do_practice);
ACMD (do_purge);
ACMD (do_put);
ACMD (do_qcomm);
ACMD (do_quit);
ACMD (do_questmobs);
ACMD (do_ram);
ACMD (do_reboot);
ACMD (do_rebalance);
ACMD (do_recall);
ACMD (do_remove);
ACMD (do_rename);
ACMD (do_rent);
ACMD (do_repair);
ACMD (do_reply);
ACMD (do_report);
ACMD (do_respec);
ACMD (do_rescue);
ACMD (do_rest);
ACMD (do_restore);
ACMD (do_retreat);
ACMD (do_return);
ACMD (do_rewiz);
ACMD (do_reward);
ACMD (do_rlist);
ACMD (do_rnum);
ACMD (do_sac);
ACMD (do_save);
ACMD (do_say);
ACMD (do_scan);
ACMD (do_scribe);
ACMD (do_school);
ACMD (do_setreboot);
ACMD (do_speed);
ACMD (do_send);
ACMD (do_set);
ACMD (do_show);
ACMD (do_shutdown);
ACMD (do_sit);
ACMD (do_skillset);
ACMD (do_sleep);
ACMD (do_slist);
ACMD (do_sneak);
ACMD (do_snoop);
ACMD (do_spec_comm);
ACMD (do_split);
ACMD (do_stand);
ACMD (do_stat);
ACMD (do_status);
ACMD (do_steal);
ACMD (do_stop_auction);
ACMD (do_switch);
ACMD (do_syslog);
ACMD (do_tame);
ACMD (do_tan);
ACMD (do_target);
ACMD (do_tedit);
ACMD (do_teleport);
ACMD (do_tell);
ACMD (do_time);
ACMD (do_title);
ACMD (do_tmobdie);
ACMD (do_toggle);
ACMD (do_togglemap);
ACMD (do_track);
ACMD (do_trans);
ACMD (do_train);
ACMD (do_trip);
ACMD (do_unban);
ACMD (do_ungroup);
ACMD (do_use);
ACMD (do_users);
ACMD (do_vwear);
ACMD (do_visible);
ACMD (do_vnum);
ACMD (do_vstat);
ACMD (do_wake);
ACMD (do_wear);
ACMD (do_where);
ACMD (do_who);
ACMD (do_whois);
ACMD (do_whoupd);
ACMD (do_wield);
ACMD (do_wimpy);
ACMD (do_wizlock);
ACMD (do_wiznet);
ACMD (do_wizutil);
ACMD (do_worth);
ACMD (do_write);
ACMD (do_wrestrict);
ACMD (do_zreset);

/* DG Script ACMD's */
ACMD(do_attach);   
ACMD(do_detach);
ACMD(do_tlist);
ACMD(do_tstat);
ACMD(do_masound);
ACMD(do_mkill);
ACMD(do_mjunk);  
ACMD(do_mechoaround);
ACMD(do_msend);
ACMD(do_mecho);
ACMD(do_mload);
ACMD(do_mpurge);
ACMD(do_mgoto);
ACMD(do_mat);
ACMD(do_mteleport);
ACMD(do_mforce);
ACMD(do_mexp); 
ACMD(do_mgold);
ACMD(do_mhunt);  
ACMD(do_mremember);
ACMD(do_mforget);
ACMD(do_mtransform); 

/* temporary commands */
ACMD(do_trainpts);
ACMD(do_levelme);
/* Weather Stuff */
ACMD(lweather);
ACMD(pweather);

cpp_extern struct command_info *complete_cmd_info;

/* This is the Master Command List(tm).

 * You can put new commands in, take commands out, change the order
 * they appear in, etc.  You can adjust the "priority" of commands
 * simply by changing the order they appear in the command list.
 * (For example, if you want "as" to mean "assist" instead of "ask",
 * just put "assist" above "ask" in the Master Command List(tm).
 *
 * In general, utility commands such as "at" should have high priority;
 * infrequently used and dangerously destructive commands should have low
 * priority.
 */

char *cmds_mortal_builders_cant_use="sacrifice trigedit tlist tstat auction brew buy carve cast deposit
donate drink drop eat fill fillet forge give group mount quaff recite sacrafice scribe school
sell sip steal take tan taste train use withdraw nosummon chain";

char *cmds_dead_can_use="qui quit look who say north south east west up down ' emote";

cpp_extern const struct command_info cmd_info[] = {

  { "RESERVED" , "RESERVED", 0, 0, 0, 0 },      /* this must be first -- for specprocs */

  /* directions must come before other commands but after RESERVED */
  { "north"    , "n"    , POS_STANDING, do_move     , 0, SCMD_NORTH },
  { "east"     , "e"    , POS_STANDING, do_move     , 0, SCMD_EAST },
  { "south"    , "s"    , POS_STANDING, do_move     , 0, SCMD_SOUTH },
  { "west"     , "w"    , POS_STANDING, do_move     , 0, SCMD_WEST },
  { "up"       , "u"    , POS_STANDING, do_move     , 0, SCMD_UP },
  { "down"     , "d"    , POS_STANDING, do_move     , 0, SCMD_DOWN },  

  /* now, the main list */
  {"addsnow",		"adds",		POS_DEAD,		do_addsnow,		GOD_CMD3,	GCMD3_ADDSNOW,	0},
  {"at",		"at",		POS_DEAD,		do_at,			GOD_CMD,	GCMD_AT,	0},
  {"affected",          "aff",          POS_SLEEPING,           do_affected,            0,              0,              0}, 
  {"advance",		"adv",		POS_DEAD,		do_advance,		GOD_CMD,	GCMD_ADVANCE,	0},
  {"advancedmap",       "adva",          POS_DEAD,               do_gen_tog,             0,              0, SCMD_ADVANCEDMAP},
  {"aedit",		"aed",		POS_DEAD,		do_olc,			GOD_CMD3,	GCMD3_IMPOLC,	SCMD_OLC_AEDIT},
  {"afk",		"af",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_AFK},
  {"alias",		"ali",		POS_DEAD,		do_alias,		0,		0,		0},	
  {"aload",		"alo",		POS_DEAD,		do_aload,		GOD_CMD2,	GCMD2_ALOAD,	0},
  {"areas",		"area",		POS_DEAD,		do_areas,		0,		0,		0},
  {"arena",		"aren",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"ainfo",		"ain",		POS_SLEEPING,		do_gen_comm,		GOD_CMD,	GCMD_GEN,	SCMD_ARENA},
  {"assist",		"ass",		POS_FIGHTING,		do_assist,		0,		0,		0},
  {"ask",		"ask",		POS_RESTING,		do_spec_comm,		0,		0,		SCMD_ASK},
  {"auction",		"auc",		POS_SLEEPING,		do_auction,		0,		0,		0},
  {"auctioneer",	"auct",		POS_DEAD,		do_auctioneer,		GOD_CMD,	GCMD_AUCTIONEER,0},
  {"autoexit",		"autoe",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_AUTOEXIT},
  {"autosplit",		"autos",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_AUTOSPLIT},
  {"autoloot",		"autol",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_AUTOLOOT},
  {"autogold",		"autog",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_AUTOGOLD},
  {"autoquest",		"autoq",	POS_STANDING,		do_autoquest,		0,		0,		0},
  {"away",		"aw",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_AFK},
  {"backstab",		"backs",	POS_STANDING,		do_backstab,		0,		0,		0},
  {"bail",		"bai",		POS_RESTING,		do_postbail,		0,		0,		0},		
  {"ban",		"ba",		POS_DEAD,		do_ban,			GOD_CMD,	GCMD_BAN,	0},
  {"bank",		"ban",		POS_STANDING,		do_gen_atm,		0,		0,	SCMD_BANK},
  {"balance",		"bal",		POS_STANDING,		do_gen_atm,		0,		0,	SCMD_BALANCE},
  {"bash",		"bas",		POS_FIGHTING,		do_bash,		0,		0,		0},
  {"berserk",		"ber",		POS_FIGHTING,		do_berserk,		0,		0,		0},
  {"bed",		"bed",		POS_STANDING,		do_bed,			0,		0,		0},
  {"bid",		"bid",		POS_SLEEPING,		do_bid,			0,		0,		0},
  {"blanket",		"blank",	POS_FIGHTING,		do_blanket,		0,		0,		0},
  {"brew",		"bre",		POS_STANDING,		do_brew,		0,		0,		0},
  {"brief",		"bri",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_BRIEF},
  {"buck",		"buck",		POS_STANDING,		do_buck,		0,		0,		0},
  {"build",		"build",	POS_STANDING,		do_build,		0,		0,		0},
  {"buy",		"buy",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"bug",		"bug",		POS_DEAD,		do_gen_write,		0,		0,		SCMD_BUG},
  {"camp",		"camp",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"carve",		"car",		POS_STANDING,		do_carve,		0,		0,		0},
  {"cast",		"cast",		POS_SITTING,		do_cast,		0,		0,		0},
  {"camouflage",	"camo",		POS_FIGHTING,		do_camouflage,		0,		0,		0},
  {"chain",		"cha",		POS_FIGHTING,		do_chain_footing,	0,		0,		0},		
  {"check",		"chec",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"checkbail",		"check",	POS_RESTING,		do_checkbail,		0,		0,		0},		
  {"circlemud",		"circle",	POS_DEAD,		do_gen_ps,		0,		0,		SCMD_CIRCLEMUD},
  {"citizen",		"cit",		POS_DEAD,		do_citizen,		GOD_CMD3,	0,		0},
  {"clan",		"clan",		POS_SLEEPING,		do_clan,		0,		0,		0},
  {"clear",		"clea",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_CLEAR},
  {"close",		"clos",		POS_SITTING,		do_gen_door,		0,		0,		SCMD_CLOSE},
  {"cls",		"cls",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_CLEAR},
  {"consider",		"con",		POS_RESTING,		do_consider,		0,		0,		0},
  {"copyto",		"copyto",	POS_DEAD,		do_copyto,		GOD_CMD2,	GCMD2_OLC,	0},
  {"color",		"col",		POS_DEAD,		do_color,		0,		0,		0},
  {"commands",		"com",		POS_DEAD,		do_commands,		0,		0,		SCMD_COMMANDS},
  {"compact",		"comp",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_COMPACT},
  {"copyover",		"copyo",	POS_DEAD,		do_copyover,		GOD_CMD3,	GCMD3_COPYOVER,	0},
  {"credits",		"cred",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_CREDITS},
  {"csay",		"cs",		POS_SLEEPING,		do_csay,		0,		0,		0},
  {"dc",		"dc",		POS_DEAD,		do_dc,			GOD_CMD,	GCMD_DC,	0},
  {"deathblow",         "death",        POS_STANDING,           do_hit,                 0,              0, SCMD_DEATHBLOW},
  {"delsnow",		"dels",		POS_DEAD,		do_delsnow,		GOD_CMD3,	GCMD3_DELSNOW,	0},
  {"deposit",		"dep",		POS_STANDING,		do_gen_atm,		0,		0,	SCMD_DEPOSIT},
  {"diagnose",		"dia",		POS_RESTING,		do_diagnose,		0,		0,		0},
  {"dismount",		"dism",		POS_STANDING,		do_dismount,		0,		0,		0},
  {"dig",		"dig",		POS_DEAD,		do_dig,			GOD_CMD2,	GCMD2_OLC,	0},
  {"disarm",		"disa",		POS_FIGHTING,		do_disarm,		0,		0,		0},
  {"display",		"disp",		POS_DEAD,		do_display,		0,		0,		0},
  {"donate",		"don",		POS_RESTING,		do_drop,		0,		0,		SCMD_DONATE},
  {"drink",		"dri",		POS_RESTING,		do_drink,		0,		0,		SCMD_DRINK},
  {"drop",		"dro",		POS_RESTING,		do_drop,		0,		0,		SCMD_DROP},
  {"eat",		"ea",		POS_RESTING,		do_eat,			0,		0,		SCMD_EAT},
  {"echo",		"ech",		POS_SLEEPING,		do_echo,		GOD_CMD,	GCMD_GEN,	SCMD_ECHO},
  {"esave",		"esav",		POS_DEAD,		do_esave,		GOD_CMD2,	GCMD2_OLC,	0},
  {"emote",		"emot",		POS_RESTING,		do_echo,		0,		0,		SCMD_EMOTE},
  {":",			":",		POS_RESTING,		do_echo,		0,		0,		SCMD_EMOTE},
  {"email",		"ema",		POS_DEAD,		do_email,		0,		0,		0},
  {"enter",		"ent",		POS_STANDING,		do_enter,		0,		0,		0},
  {"equipment",		"equip",	POS_SLEEPING,		do_equipment,		0,		0,		0},
  {"exits",		"exi",		POS_RESTING,		do_exits,		0,		0,		0},
  {"examine",		"exam",		POS_SITTING,		do_examine,		0,		0,		0},
  {"force",		"for",		POS_SLEEPING,		do_force,		GOD_CMD,	GCMD_FORCE,	0},
  {"fill",		"fil",		POS_STANDING,		do_pour,		0,		0,		SCMD_FILL},
  {"fillet",		"fille",	POS_STANDING,		do_fillet,		0,		0,		0},
  {"finger",		"fin",		POS_DEAD,		do_whois,		0,		0,		0},
  {"flee",		"fle",		POS_FIGHTING,		do_flee,		0,		0,		0},
  {"follow",		"fol",		POS_RESTING,		do_follow,		0,		0,		0},
  {"forage",		"fora",		POS_STANDING,		do_forage,		0,		0,		0},
  {"forge",		"forg",		POS_STANDING,		do_forge,		0,		0,		0},
  {"freeze",		"freeze",	POS_DEAD,		do_wizutil,		GOD_CMD,	GCMD_FREEZE,	SCMD_FREEZE},
  {"get",		"g",		POS_RESTING,		do_get,			0,		0,		0},
  {"gecho",		"gech",		POS_DEAD,		do_gecho,		GOD_CMD2,	GCMD2_GECHO,	0},
  {"gplague",		"gpla",		POS_DEAD,		do_gplague,		GOD_CMD,	GCMD_PLAGUE,	0},
  {"gdeplague",		"gdep",		POS_DEAD,		do_gcureplague,		GOD_CMD,	GCMD_PLAGUE,	0},
  {"gemote",		"gem",		POS_SLEEPING,		do_gen_comm,		0,		0,		SCMD_GMOTE},
  {"give",		"giv",		POS_RESTING,		do_give,		0,		0,		0},
  {"goto",		"got",		POS_SLEEPING,		do_goto,		GOD_CMD,	GCMD_GEN,	0},
  {"gold",		"gol",		POS_RESTING,		do_gold,		0,		0,		0},
  {".",			".",		POS_SLEEPING,		do_gen_comm,		0,		0,		SCMD_GOSSIP},
  {"gossip",		"gos",		POS_SLEEPING,		do_gen_comm,		0,		0,		SCMD_GOSSIP},
  {"group",		"grou",		POS_RESTING,		do_group,		0,		0,		0},
  {"grab",		"grab",		POS_RESTING,		do_grab,		0,		0,		0},
  {"grats",		"grat",		POS_SLEEPING,		do_gen_comm,		0,		0,		SCMD_GRATZ},
  {"gsay",		"gs",		POS_SLEEPING,		do_gsay,		0,		0,		0},
  {"gtell",		"gt",		POS_SLEEPING,		do_gsay,		0,		0,		0},
  {"hedit",		"hedit",	POS_DEAD,		do_olc,			GOD_CMD3,       GCMD3_IMPOLC,	SCMD_OLC_HEDIT},
  {"help",		"hel",		POS_DEAD,		do_help,		0,		0,		0},
  {"handbook",		"hand",		POS_DEAD,		do_gen_ps,		GOD_CMD,	GCMD_GEN,	SCMD_HANDBOOK},
  {"hcontrol",		"hcon",		POS_DEAD,		do_hcontrol,		GOD_CMD,	GCMD_HCONTROL,	0},
  {"hide",		"hid",		POS_RESTING,		do_hide,		0,		0,		0},
  {"hit",		"h",		POS_FIGHTING,		do_hit,			0,		0,		SCMD_HIT},
  {"hold",		"hold",		POS_RESTING,		do_grab,		0,		0,		0},
  {"holler",		"holl",		POS_RESTING,		do_gen_comm,		0,		0,		SCMD_HOLLER},
  {"holylight",		"holy",		POS_DEAD,		do_gen_tog,		GOD_CMD,	GCMD_GEN,	SCMD_HOLYLIGHT},
  {"house",		"hous",		POS_RESTING,		do_house,		0,		0,		0},
  {"inventory",		"i",		POS_DEAD,		do_inventory,		0,		0,		0},
  {"ignore",		"ignore",	POS_DEAD,		do_ignore,		0,		0,		0},
  {"idea",		"id",		POS_DEAD,		do_gen_write,		0,		0,		SCMD_IDEA},
  {"imotd",		"imotd",	POS_DEAD,		do_gen_ps,		0,		0,		SCMD_IMOTD},
  {"immlist",		"imm",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_IMMLIST},
  {"info",		"inf",		POS_SLEEPING,		do_gen_ps,		0,		0,		SCMD_INFO},
  {"insult",		"insul",	POS_RESTING,		do_insult,		0,		0,		0},
  {"invis",		"inv",		POS_DEAD,		do_invis,		GOD_CMD2,	GCMD2_INVIS,	0},
  {"isay",		"isay",		POS_DEAD,		do_isay,		GOD_CMD,	GCMD_ISAY,	0},
  {"junk",		"j",		POS_RESTING,		do_drop,		0,		0,		SCMD_JUNK},
  {"kill",		"k",		POS_FIGHTING,		do_kill,		0,		0,		0},
  {"kick",		"ki",		POS_FIGHTING,		do_kick,		0,		0,		0},
  {"look",		"l",		POS_RESTING,		do_look,		0,		0,		SCMD_LOOK},
  {"last",		"la",		POS_DEAD,		do_last,		GOD_CMD2,	GCMD2_USERS,	0},
  {"leave",		"lea",		POS_STANDING,		do_leave,		0,		0,		0},
  {"levels",		"lev",		POS_DEAD,		do_levels,		0,		0,		0},
  {"list",		"li",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"listen",		"lis",		POS_RESTING,		do_listen,		0,		0,		0},
  {"lock",		"lo",		POS_SITTING,		do_gen_door,		0,		0,		SCMD_LOCK},
  {"lockout",		"lock",		POS_RESTING,		do_lockout,		0,		0,		0},
  {"load",		"loa",		POS_DEAD,		do_load,		GOD_CMD,	GCMD_LOAD,	0},
  {"lweather",		"lweat",	POS_DEAD,		lweather,		GOD_CMD3,	GCMD3_LWEATHER,	0},  		
  {"medit",		"medi",		POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_MEDIT},
  {"motd",		"mot",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_MOTD},
  {"mail",		"ma",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"map",		"map",		POS_DEAD,		do_map,			GOD_CMD3,	GCMD3_MAP,	0},
  {"mcheck",		"mcheck",	POS_DEAD,		do_mcheck,		0,		0,		0},
  {"mcasters",		"mcast",	POS_DEAD,		do_mcasters,		GOD_CMD2,	GCMD2_MCASTERS,	0},
  {"meditate",		"med",		POS_DEAD,		do_meditate,		0,		0,		0},
  {"mercy",             "mer",          POS_DEAD,               do_gen_tog,             0,              0,     SCMD_MERCY},
  {"mlist",		"mlis",		POS_DEAD,		do_mlist,		0,		0,		0},
  {"mount",		"mou",		POS_STANDING,		do_mount,		0,		0,		0},
  {"mudheal",		"mudh",		POS_DEAD,		do_mudheal,		GOD_CMD2,	GCMD2_MUDHEAL,	0},
  {"mute",		"mut",		POS_DEAD,		do_wizutil,		GOD_CMD,	GCMD_MUTE,	SCMD_SQUELCH},
  {"murder",		"mur",		POS_FIGHTING,		do_hit,			0,		0,		SCMD_MURDER},
  {"mobdie",		"mobd",		POS_DEAD,		do_mobdie,		GOD_CMD,	0,		0},
  {"news",		"new",		POS_SLEEPING,		do_gen_ps,		0,		0,		SCMD_NEWS},
  {"noarena",		"noar",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOARENA},
  {"noauction",		"noauc",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOAUCTION},
  {"nogossip",		"nogos",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOGOSSIP},
  {"nograts",		"nogra",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOGRATZ},
  {"nohassle",		"nohas",	POS_DEAD,		do_gen_tog,		GOD_CMD,	GCMD_GEN,	SCMD_NOHASSLE},
  {"nomap",		"noma",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOMAP},
  {"nomstack",		"nom",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOLOOKSTAC},
  {"norepeat",		"nor",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOREPEAT},
  {"noshout",		"nosh",		POS_SLEEPING,		do_gen_tog,		0,		0,		SCMD_DEAF},
  {"nosummon",		"nosu",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOSUMMON},
  {"notell",		"note",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOTELL},
  {"notic",		"notic",	POS_DEAD,		do_gen_tog,		0,		0,		SCMD_NOTIC},
  {"notitle",		"notit",	POS_DEAD,		do_wizutil,		GOD_CMD2,	GCMD2_NOTITLE,	SCMD_NOTITLE},
  {"nowiz",		"now",		POS_DEAD,		do_gen_tog,		GOD_CMD,	GCMD_GEN,	SCMD_NOWIZ},
  {"observe",		"obs",		POS_RESTING,		do_observe,		0,		0,		0},
  {"order",		"ord",		POS_RESTING,		do_order,		0,		0,		0},
  {"offer",		"off",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"ooc",		"ooc",		POS_SLEEPING,		do_gen_comm,		0,		0,		SCMD_GOSSIP},
  {"open",		"o",		POS_SITTING,		do_gen_door,		0,		0,		SCMD_OPEN},
  {"olc",		"olc",		POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_SAVEINFO},
  {"olist",		"olis",		POS_DEAD,		do_olist,		GOD_CMD2,	GCMD2_OLC,	0},
  {"oedit",		"oed",		POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_OEDIT},
  {"put",		"p",		POS_RESTING,		do_put,			0,		0,		0},
  {"page",		"pag",		POS_DEAD,		do_page,		GOD_CMD2,	GCMD2_PAGE,	0},
  {"pardon",		"par",		POS_DEAD,		do_wizutil,		GOD_CMD,	GCMD_PARDON,	SCMD_PARDON},
  {"peace",		"peac",		POS_DEAD,		do_peace,		GOD_CMD3,	GCMD3_PEACE,	0},
  {"pfileclean",	"pfileclean",	POS_DEAD,		do_pfileclean,		GOD_CMD3,       GCMD3_PFILECLEAN,	0},
  {"pick",		"pi",		POS_STANDING,		do_gen_door,		0,		0,		SCMD_PICK},
  {"players",		"pl",		POS_DEAD,		do_players,		0,		0,		0},
  {"policy",		"pol",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_POLICIES},
  {"poofin",		"poofi",	POS_DEAD,		do_poofset,		GOD_CMD,	GCMD_GEN,	SCMD_POOFIN},
  {"poofout",		"poofo",	POS_DEAD,		do_poofset,		GOD_CMD,	GCMD_GEN,	SCMD_POOFOUT},
  {"postbail",		"postb",	POS_RESTING,		do_postbail,		0,		0,		0},		
  {"pour",		"pou",		POS_STANDING,		do_pour,		0,		0,		SCMD_POUR},
  {"prompt",		"prom",		POS_DEAD,		do_display,		0,		0,		0},
  {"practice",		"pra",		POS_RESTING,		do_practice,		0,		0,		0},
  {"purge",		"purg",		POS_DEAD,		do_purge,		GOD_CMD,	GCMD_PURGE,	0},
  {"quaff",		"q",		POS_RESTING,		do_use,			0,		0,		SCMD_QUAFF},
  {"qecho",		"qec",		POS_DEAD,		do_qcomm,		GOD_CMD2,	GCMD2_QECHO,	SCMD_QECHO},
  {"qchan",		"qch",		POS_DEAD,		do_gen_tog,		0,		0,		SCMD_QCHAN},
  {"quest",		"quest",	POS_STANDING,		do_autoquest,		0,		0,		0},
  {"questmobs",		"questm",	POS_STANDING,		do_questmobs,		GOD_CMD2,	GCMD2_QUESTMOBS,0},
  {"qui",		"qui",		POS_DEAD,		do_quit,		0,		0,		0},
  {"quit",		"quit",		POS_DEAD,		do_quit,		0,		0,		SCMD_QUIT},
  {"qsay",		"qs",		POS_RESTING,		do_qcomm,		0,		0,		SCMD_QSAY},
  {"ram",		"ram",		POS_STANDING,		do_gen_door,		0,		0,		SCMD_RAM},
  {"reply",		"rep",		POS_SLEEPING,		do_reply,		0,		0,		0},
  {"rest",		"res",		POS_RESTING,		do_rest,		0,		0,		0},
  {"respec",		"respe",	POS_RESTING,		do_respec,		GOD_CMD2,	GCMD2_RESPEC,	0},
  {"read",		"rea",		POS_RESTING,		do_look,		0,		0,		SCMD_READ},
  {"rebalance",	"rebalance",	POS_STANDING,	do_rebalance,	GOD_CMD3,	GCMD3_REBALANCE,	0},
  {"reload",		"reloa",	POS_DEAD,		do_reboot,		GOD_CMD,	GCMD_RELOAD,	0},
  {"recall",		"reca",		POS_DEAD,		do_recall,		0,		0,		0},
  {"recite",		"reci",		POS_RESTING,		do_use,			0,		0,		SCMD_RECITE},
  {"receive",		"rece",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"remove",		"rem",		POS_RESTING,		do_remove,		0,		0,		0},
  {"rename",		"renam",	POS_DEAD,		do_rename,		GOD_CMD2,	GCMD2_IMP,	0},		
  {"rent",		"ren",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"repair",		"repa",		POS_STANDING,		do_repair,		0,		0,		0},
  {"report",		"rep",		POS_RESTING,		do_report,		0,		0,		0},
  {"reroll",		"rerol",	POS_DEAD,		do_wizutil,		GOD_CMD,	GCMD_REROLL,		SCMD_REROLL},
  {"rescue",		"resc",		POS_FIGHTING,		do_rescue,		0,		0,		0},
  {"restore",		"restor",	POS_DEAD,		do_restore,		GOD_CMD,	GCMD_RESTORE,	0},
  {"retreat",		"retr",		POS_DEAD,		do_retreat,		0,		0,		0},
  {"return",		"retur",	POS_DEAD,		do_return,		0,		0,		0},
  {"redit",		"redi",		POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_REDIT},
  {"rewiz",		"rewi",		POS_DEAD,		do_rewiz,		GOD_CMD2,	GCMD2_REWIZ,	0},
  {"reward",		"rewar",	POS_DEAD,		do_reward,		GOD_CMD2,	GCMD2_REWARD,	0},
  {"rewww",		"reww",		POS_DEAD,		do_whoupd,		GOD_CMD2,	GCMD2_REWWW,	0},
  {"rlist",		"rlis",		POS_DEAD,		do_rlist,		GOD_CMD2,	GCMD2_OLC,	0},
  {"rnumber",		"rnum",		POS_DEAD,		do_rnum,		GOD_CMD,	GCMD2_OLC,	0},
  {"roomflags",		"roomflag",	POS_DEAD,		do_gen_tog,		GOD_CMD,	GCMD_GEN,	SCMD_ROOMFLAGS},
  {"sacrifice",		"sac",		POS_STANDING,		do_sac,			0,		0,		0},
  {"say",		"s",		POS_RESTING,		do_say,			0,		0,		0},
  {"'",			"'",		POS_RESTING,		do_say,			0,		0,		0},
  {"save",		"sav",		POS_SLEEPING,		do_save,		0,		0,		0},
  {"score",		"sc",		POS_DEAD,		do_status,		0,		0,		0},
  {"scan",		"sca",		POS_STANDING,		do_scan,		0,		0,		0},
  {"scribe",		"scr",		POS_STANDING,		do_scribe,		0,		0,		0},
  {"school",		"sch",		POS_STANDING,		do_school,		0,		0,		0},
  {"sell",		"sel",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"send",		"sen",		POS_SLEEPING,		do_send,		GOD_CMD,	GCMD_SEND,	0},
  {"set",		"set",		POS_DEAD,		do_set,			GOD_CMD,	GCMD_SET,	0},
  {"setreboot",		"setreb",	POS_DEAD,		do_setreboot,		GOD_CMD2,	GCMD2_SETREBOOT,0},
  {"sedit",		"sedi",		POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_SEDIT},
  {"shout",		"shou",		POS_RESTING,		do_gen_comm,		0,		0,		SCMD_SHOUT},
  {"show",		"show",		POS_DEAD,		do_show,		GOD_CMD,	GCMD_GEN,	0},
  {"shutdow",		"shoutdow",	POS_DEAD,		do_shutdown,		GOD_CMD,	GCMD_SHUTDOWN,	0},
  {"shutdown",		"shutdown",	POS_DEAD,		do_shutdown,		GOD_CMD,	GCMD_SHUTDOWN,	SCMD_SHUTDOWN},
  {"sip",		"sip",		POS_RESTING,		do_drink,		0,		0,		SCMD_SIP},
  {"sit",		"si",		POS_RESTING,		do_sit,			0,		0,		0},
  {"skillset",		"skillse",	POS_SLEEPING,		do_skillset,		GOD_CMD,	GCMD_SKILLSET,	0},
  {"sleep",		"sle",		POS_SLEEPING,		do_sleep,		0,		0,		0},
  {"slowns",		"slown",	POS_DEAD,		do_gen_tog,		GOD_CMD,	0,		SCMD_SLOWNS},
  {"sneak",		"sne",		POS_STANDING,		do_sneak,		0,		0,		0},
  {"slist",             "sli",          POS_DEAD,               do_slist,               0,              0,              0},
  {"snoop",		"snoo",		POS_DEAD,		do_snoop,		GOD_CMD,	GCMD_SNOOP,	0},
  {"socials",		"soc",		POS_DEAD,		do_commands,		0,		0,		SCMD_SOCIALS},
  {"split",		"spl",		POS_SITTING,		do_split,		0,		0,		0},
  {"speed",		"spe",		POS_STANDING,		do_speed,		0,		0,		0},
  {"stand",		"stan",		POS_RESTING,		do_stand,		0,		0,		0},
  {"stat",		"stat",		POS_DEAD,		do_stat,		GOD_CMD2,	GCMD2_OLC,	0},
  {"status",		"statu",	POS_DEAD,		do_status,		0,		0,		0},
  {"steal",		"stea",		POS_STANDING,		do_steal,		0,		0,		0},
  {"stopauc",		"stopau",	POS_DEAD,		do_stop_auction,	GOD_CMD,	GCMD_AUCTIONEER,0},
  {"switch",		"switc",	POS_DEAD,		do_switch,		GOD_CMD,	GCMD_SWITCH,	0},
  {"syslog",		"syslo",	POS_DEAD,		do_syslog,		GOD_CMD,	GCMD_SYSLOG,	0},
  {"tedit",		"tedi",		POS_DEAD,		do_tedit,		GOD_CMD3,	GCMD3_IMPOLC,	0},
  {"tell",		"t",		POS_DEAD,		do_tell,		0,		0,		0},
  {"take",		"tak",		POS_RESTING,		do_get,			0,		0,		0},
  {"tame",		"tam",		POS_STANDING,		do_tame,		0,		0,		0},
  {"tan",		"tan",		POS_STANDING,		do_tan,			0,		0,		0},		
  {"target",		"targ",		POS_FIGHTING,		do_target,		0,		0,		0},
  {"taste",		"tas",		POS_RESTING,		do_eat,			0,		0,		SCMD_TASTE},
  {"teleport",		"telepor",	POS_DEAD,		do_teleport,		GOD_CMD,	GCMD_TRANS,	0},
  {"thaw",		"tha",		POS_DEAD,		do_wizutil,		GOD_CMD,	GCMD_FREEZE,	SCMD_THAW},
  {"title",		"tit",		POS_DEAD,		do_title,		0,		0,		0},
  {"time",		"tim",		POS_DEAD,		do_time,		0,		0,		0},
  {"tmobdie",		"tmobdi",	POS_DEAD,		do_tmobdie,		GOD_CMD2,	GCMD2_TMOBDIE,	0},
  {"toggle",		"toggl",	POS_DEAD,		do_toggle,		0,		0,		0},
  {"track",		"trac",		POS_STANDING,		do_track,		0,		0,		0},
  {"transfer",		"trans",	POS_SLEEPING,		do_trans,		GOD_CMD,	GCMD_TRANS,	0},
  {"train",		"tra",		POS_STANDING,		do_train,		0,		0,		0},
  {"trigedit",		"trige",	POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_TRIGEDIT},
  {"trip",		"tri",		POS_FIGHTING,		do_trip,		0,		0,		0},
  {"typo",		"typo",		POS_DEAD,		do_gen_write,		0,		0,		SCMD_TYPO},
  {"unlock",		"unl",		POS_SITTING,		do_gen_door,		0,		0,		SCMD_UNLOCK},
  {"ungroup",		"ung",		POS_DEAD,		do_ungroup,		0,		0,		0},
  {"unban",		"unb",		POS_DEAD,		do_unban,		GOD_CMD,	GCMD_BAN,	0},
  {"unaffect",		"unaffec",	POS_DEAD,		do_wizutil,		GOD_CMD,	GCMD_UNAFFECT,	SCMD_UNAFFECT},
  {"uptime",		"uptim",	POS_DEAD,		do_date,		GOD_CMD,	GCMD_GEN,	SCMD_UPTIME},
  {"use",		"us",		POS_SITTING,		do_use,			0,		0,		SCMD_USE},
  {"users",		"user",		POS_DEAD,		do_users,		GOD_CMD2,	GCMD2_USERS,	0},
  {"value",		"val",		POS_STANDING,		do_not_here,		0,		0,		0},
  {"version",		"ver",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_VERSION},
  {"vwear",		"vwea",		POS_DEAD,		do_vwear,		GOD_CMD,	GCMD2_OLC,	0},
  {"visible",		"vis",		POS_RESTING,		do_visible,		0,		0,		0},
  {"vnum",		"vnu",		POS_DEAD,		do_vnum,		GOD_CMD2,	GCMD2_OLC,	0},
  {"vstat",		"vsta",		POS_DEAD,		do_vstat,		GOD_CMD2,	GCMD2_OLC,	0},
  {"wake",		"wa",		POS_SLEEPING,		do_wake,		0,		0,		0},
  {"wear",		"wea",		POS_RESTING,		do_wear,		0,		0,		0},
  {"weather",		"weat",		POS_RESTING,		pweather,		0,		0,		0},
  {"who",		"w",		POS_DEAD,		do_who,			0,		0,		0},
  {"whoami",		"whoa",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_WHOAMI},
  {"whois",		"whoi",		POS_DEAD,		do_whois,		0,		0,		0},
  {"where",		"wher",		POS_RESTING,		do_where,		0,		0,		0},
  {"whisper",		"whis",		POS_RESTING,		do_spec_comm,		0,		0,		SCMD_WHISPER},
  {"wield",		"wie",		POS_RESTING,		do_wield,		0,		0,		0},
  {"wimpy",		"wim",		POS_DEAD,		do_wimpy,		0,		0,		0},
  {"withdraw",		"with",		POS_STANDING,		do_gen_atm,		0,		0,	SCMD_WITHDRAW},
  {"wiznet",		"wiz",		POS_DEAD,		do_wiznet,		GOD_CMD,	GCMD_GEN,	0},
  {";",			";",		POS_DEAD,		do_wiznet,		GOD_CMD,	GCMD_GEN,	0},
  {"wizhelp",		"wizh",		POS_SLEEPING,		do_commands,		0,		0,		SCMD_WIZHELP},
  {"wizlist",		"wizl",		POS_DEAD,		do_gen_ps,		0,		0,		SCMD_WIZLIST},
  {"wizlock",		"wizloc",	POS_DEAD,		do_wizlock,		GOD_CMD,	GCMD_WIZLOCK,	0},
  {"worth",		"wo",		POS_RESTING,		do_worth,		0,		0,		0},
  {"write",		"wr",		POS_STANDING,		do_write,		0,		0,		0},
  {"wrestrict",		"wrestri",	POS_DEAD,		do_wrestrict,		GOD_CMD2,	GCMD2_WRESTRICT,0},
  {"zedit",		"zedi",		POS_DEAD,		do_olc,			GOD_CMD2,	GCMD2_OLC,	SCMD_OLC_ZEDIT},
  {"zreset",		"zres",		POS_DEAD,		do_zreset,		GOD_CMD2,	GCMD2_ZRESET,	0},
  {"levelme",		"levelme",	POS_DEAD,		do_levelme,		0,		0,		0},
  {"attach",		"att",		POS_DEAD,		do_attach,		GOD_CMD2,	GCMD2_ATTACH,	0},		
  {"detach",		"det",		POS_DEAD,		do_detach,		GOD_CMD2,	GCMD2_ATTACH,	0},		
  {"tlist",		"tli",		POS_DEAD,		do_tlist,		GOD_CMD2,	GCMD2_OLC,	0},  
  {"tstat",		"tst",		POS_DEAD,		do_tstat,		GOD_CMD2,	GCMD2_OLC,	0},  
  {"masound",		"masound",	POS_DEAD,		do_masound,		0,		0,		0},
  {"mkill",		"mkill",	POS_STANDING,		do_mkill,		0,		0,		0},
  {"mjunk",		"mjunk",	POS_SITTING,		do_mjunk,		0,		0,		0},  		
  {"mecho",		"mecho",	POS_DEAD,		do_mecho,		0,		0,		0},		
  {"mechoaround",	"mechoaround",	POS_DEAD,		do_mechoaround,		0,		0,		0},
  {"msend",		"msend",	POS_DEAD,		do_msend,		0,		0,		0},
  {"mload",		"mload",	POS_DEAD,		do_mload,		0,		0,		0},
  {"mpurge",		"mpurge",	POS_DEAD,		do_mpurge,		0,		0,		0},
  {"mgoto",		"mgoto",	POS_DEAD,		do_mgoto,		0,		0,		0},
  {"mat",		"mat",		POS_DEAD,		do_mat,			0,		0,		0},
  {"mteleport",		"mteleport",	POS_DEAD,		do_mteleport,		0,		0,		0},
  {"mforce",		"mforce",	POS_DEAD,		do_mforce,		0,		0,		0},
  {"mexp",		"mexp",		POS_DEAD,		do_mexp,		0,		0,		0},
  {"mgold",		"mgold",	POS_DEAD,		do_mgold,		0,		0,		0},
  {"mhunt",		"mhunt",	POS_DEAD,		do_mhunt,		0,		0,		0},
  {"mremember",		"mremember",	POS_DEAD,		do_mremember,		0,		0,		0},
  {"mforget",		"mforget",	POS_DEAD,		do_mforget,		0,		0,		0},  		
  {"mtransform",	"mtransform",	POS_DEAD,		do_mtransform,		0,		0,		0},

  { "\n", "zzzzzzz", 0, 0, 0, 0 } };    /* this must be last */


char *fill[] =
{
  "in",
  "from",
  "with",
  "the",
  "on",
  "at",
  "to",
  "\n"
};

char *reserved[] =
{
  "a",
  "an",
  "self",
  "me",
  "all",
  "room",
  "someone",
  "something",
  "\n"
};

/*
 * This is the actual command interpreter called from game_loop() in comm.c
 * It makes sure you are the proper level and position to execute the command,
 * then calls the appropriate function.
 */
void 
command_interpreter (struct char_data *ch, char *argument)
{
  int cmd, length, i=-1, x=-1;
  extern int no_specials;
  char *line;
  extern int special_exit_command(struct char_data *ch, char *cmd);
  extern bool check_perm_duration(struct char_data *ch, long bitvector);
  extern int find_action(int cmd);
  extern struct social_messg *soc_mess_list;

  if (!check_perm_duration(ch, AFF_HIDE))
    REMOVE_BIT (AFF_FLAGS (ch), AFF_HIDE);

  /* just drop to next line for hitting CR */
  skip_spaces (&argument);
  if (!*argument)
    return;

  /*
   * special case to handle one-character, non-alphanumeric commands;
   * requested by many people so "'hi" or ";godnet test" is possible.
   * Patch sent by Eric Green and Stefan Wasilewski.
   */
  if (!isalpha (*argument))
    {
      arg[0] = argument[0];
      arg[1] = '\0';
      line = argument + 1;
    }
  else
    line = any_one_arg (argument, arg);

  if (!IS_NPC(ch) && PRF2_FLAGGED(ch, PRF2_LOCKOUT)){
    if (strcmp(arg, "unlock")){
      send_to_char ("Your terminal is currently locked!\r\n"
		    "To unlock please type 'unlock <yourpassword>'\r\n", ch);
      return;
    }else{
      do_lockout (ch, line, 0, 0); 
      return;
    }
  }
    
  /* otherwise, find the command */
  if ((GET_LEVEL(ch)<LVL_IMMORT) &&
      (command_wtrigger(ch, arg, line) ||
       command_mtrigger(ch, arg, line) ||
       command_otrigger(ch, arg, line)))
    return; /* command trigger took over */

//  for (length = strlen(arg), cmd = 0; *complete_cmd_info[cmd].command != '\n'; cmd++)
//    if (!strncmp(complete_cmd_info[cmd].command, arg, length))
//      if (GET_LEVEL(ch) >= complete_cmd_info[cmd].minimum_level)
//        break;
 
for (length = strlen (arg), cmd = 0; *complete_cmd_info[cmd].command != '\n'; cmd++)
    if (!strncmp (complete_cmd_info[cmd].command, arg, length))
      if ((GET_TRUST_LEVEL(ch) >= complete_cmd_info[cmd].minimum_level) &&
	  (!IS_GODCMD(cmd) || (complete_cmd_info[cmd].godcmd & GCMD_FLAGS(ch))) &&
	  (!IS_GODCMD2(cmd) || (complete_cmd_info[cmd].godcmd & GCMD2_FLAGS(ch))) &&
	  (!IS_GODCMD3(cmd) || (complete_cmd_info[cmd].godcmd & GCMD3_FLAGS(ch))) &&
	  (!IS_GODCMD4(cmd) || (complete_cmd_info[cmd].godcmd & GCMD4_FLAGS(ch))) &&
	  ((!IS_NPCCMD(cmd) || IS_NPC(ch)) ||
	  (GET_LEVEL(ch) >= LVL_IMPL)))
	break;

  if (PLR_FLAGGED(ch, PLR_FROZEN) && GET_LEVEL(ch) < LVL_IMPL) {
    send_to_char("You try, but the mind-numbing cold prevents you...\r\n", ch);
    return;
  }

  if (GET_POS(ch)==POS_STANDING)
    if (special_exit_command(ch, argument))
      return;

  if (IS_SET(PRF2_FLAGS(ch), PRF2_MBUILDING) && *complete_cmd_info[cmd].command != '\n') 
    if (is_name((char *)complete_cmd_info[cmd].command, cmds_mortal_builders_cant_use)) {
      send_to_char("You may not use that command in build mode.\r\n", ch);
      return;
    }

  if (IS_SET(PRF2_FLAGS(ch), PRF2_INTANGIBLE) && !IS_SET(PRF2_FLAGS(ch), PRF2_MBUILDING) && *complete_cmd_info[cmd].command != '\n') 
    if (!is_name((char *)complete_cmd_info[cmd].command, cmds_dead_can_use)) {
      if (complete_cmd_info[cmd].command_pointer != do_action) {
        send_to_char("The intangible have no need for such abilities.\r\n", ch);
        return;
      }
      x=find_action(cmd);
      if (x!=-1) {
        i=soc_mess_list[x].hide;
        soc_mess_list[x].hide=TRUE;
      }
    }

  if (*complete_cmd_info[cmd].command == '\n')
    send_to_char("Huh?!?\r\n", ch);
  else if (complete_cmd_info[cmd].command_pointer == NULL)
    send_to_char("Sorry, that command hasn't been implemented yet.\r\n", ch);
  else if (IS_NPC(ch) && complete_cmd_info[cmd].minimum_level >= LVL_IMMORT)
    send_to_char("You can't use immortal commands while switched.\r\n", ch);
  else if (GET_POS(ch) < complete_cmd_info[cmd].minimum_position)

    switch (GET_POS(ch)) {
    case POS_DEAD:
      send_to_char("Lie still; you are DEAD!!! :-(\r\n", ch);   
      break;  
    case POS_INCAP:
    case POS_MORTALLYW:
      send_to_char("You are in a pretty bad shape, unable to do anything!\r\n", ch);
      break;
    case POS_STUNNED:
      send_to_char("All you can do right now is think about the stars!\r\n", ch);
      break;
    case POS_SLEEPING:
      send_to_char("In your dreams, or what?\r\n", ch);
      break;
    case POS_RESTING:
      send_to_char("Nah... You feel too relaxed to do that..\r\n", ch);
      break;
    case POS_SITTING:
      send_to_char("Maybe you should get on your feet first?\r\n", ch);
      break;
    case POS_FIGHTING:
      send_to_char("No way!  You're fighting for your life!\r\n", ch);
      break;
  }
  else if (no_specials) /* If no specs are allowed in the game, just do the command */
    ((*complete_cmd_info[cmd].command_pointer) (ch, line, cmd, complete_cmd_info[cmd].subcmd));
  else if (!special(ch, cmd, line)) /* They're allowed in the game, try for one */
    ((*complete_cmd_info[cmd].command_pointer) (ch, line, cmd, complete_cmd_info[cmd].subcmd)); /* no? do the command */
  if (i!=-1 && x!=-1)
    soc_mess_list[x].hide=i;
}

/**************************************************************************
 * Routines to handle aliasing                                             *
  **************************************************************************/


struct alias *
find_alias (struct alias *alias_list, char *str)
{
  while (alias_list != NULL)
    {
      if (*str == *alias_list->alias)   /* hey, every little bit counts :-) */
	if (!strcmp (str, alias_list->alias))
	  return alias_list;

      alias_list = alias_list->next;
    }

  return NULL;
}


void 
free_alias (struct alias *a)
{
  if (a->alias)
    free (a->alias);
  if (a->replacement)
    free (a->replacement);
  free (a);
}


/* The interface to the outside world: do_alias */
ACMD (do_alias)
{
  char *repl;
  struct alias *a, *temp;

  if (IS_NPC (ch))
    return;

  repl = any_one_arg (argument, arg);

  if (!*arg)
    {                           /* no argument specified -- list currently defined aliases */
      send_to_char ("Currently defined aliases:\r\n", ch);
      if ((a = GET_ALIASES (ch)) == NULL)
	send_to_char (" None.\r\n", ch);
      else
	{
	  while (a != NULL)
	    {
	      sprintf (buf, "%-15s %s\r\n", a->alias, a->replacement);
	      send_to_char (buf, ch);
	      a = a->next;
	    }
	}
    }
  else
    {                           /* otherwise, add or remove aliases */
      /* is this an alias we've already defined? */
      if ((a = find_alias (GET_ALIASES (ch), arg)) != NULL)
	{
	  REMOVE_FROM_LIST (a, GET_ALIASES (ch), next);
	  free_alias (a);
	}
      /* if no replacement string is specified, assume we want to delete */
      if (!*repl)
	{
	  if (a == NULL)
	    send_to_char ("No such alias.\r\n", ch);
	  else
	    send_to_char ("Alias deleted.\r\n", ch);
	}
      else
	{                       /* otherwise, either add or redefine an alias */
	  if (!str_cmp (arg, "alias"))
	    {
	      send_to_char ("You can't alias 'alias'.\r\n", ch);
	      return;
	    }
	  CREATE (a, struct alias, 1);
	  a->alias = str_dup (arg);
	  delete_doubledollar (repl);
	  a->replacement = str_dup (repl);
	  if (strchr (repl, ALIAS_SEP_CHAR) || strchr (repl, ALIAS_VAR_CHAR)) {
	    a->type = ALIAS_COMPLEX;
       }
	  else
	    a->type = ALIAS_SIMPLE;
	  a->next = GET_ALIASES (ch);
	  GET_ALIASES (ch) = a;
	  send_to_char ("Alias added.\r\n", ch);
	}
    }
}

/*
 * Valid numeric replacements are only $1 .. $9 (makes parsing a little
 * easier, and it's not that much of a limitation anyway.)  Also valid
 * is "$*", which stands for the entire original line after the alias.
 * ";" is used to delimit commands.
 */
#define NUM_TOKENS       9

void 
perform_complex_alias (struct txt_q *input_q, char *orig, struct alias *a)
{
  struct txt_q temp_queue;
  char *tokens[NUM_TOKENS], *temp, *write_point;
  int num_of_tokens = 0, num;

  /* First, parse the original string */
  temp = strtok (strcpy (buf2, orig), " ");
  while (temp != NULL && num_of_tokens < NUM_TOKENS)
    {
      tokens[num_of_tokens++] = temp;
      temp = strtok (NULL, " ");
    }

  /* initialize */
  write_point = buf;
  temp_queue.head = temp_queue.tail = NULL;

  /* now parse the alias */
  for (temp = a->replacement; *temp; temp++)
    {
      if (*temp == ALIAS_SEP_CHAR)
	{
	  *write_point = '\0';
	  buf[MAX_INPUT_LENGTH - 1] = '\0';
	  write_to_q (buf, &temp_queue, 1);
	  write_point = buf;
	}
      else if (*temp == ALIAS_VAR_CHAR)
	{
	  temp++;
	  if ((num = *temp - '1') < num_of_tokens && num >= 0)
	    {
	      strcpy (write_point, tokens[num]);
	      write_point += strlen (tokens[num]);
	    }
	  else if (*temp == ALIAS_GLOB_CHAR)
	    {
	      strcpy (write_point, orig);
	      write_point += strlen (orig);
	    }
	  else if ((*(write_point++) = *temp) == '$')   /* redouble $ for act safety */
	    *(write_point++) = '$';
	}
      else
	*(write_point++) = *temp;
    }

  *write_point = '\0';
  buf[MAX_INPUT_LENGTH - 1] = '\0';
  write_to_q (buf, &temp_queue, 1);

  /* push our temp_queue on to the _front_ of the input queue */
  if (input_q->head == NULL)
    *input_q = temp_queue;
  else
    {
      temp_queue.tail->next = input_q->head;
      input_q->head = temp_queue.head;
    }
}


/*
 * Given a character and a string, perform alias replacement on it.
 *
 * Return values:
 *   0: String was modified in place; call command_interpreter immediately.
 *   1: String was _not_ modified in place; rather, the expanded aliases
 *      have been placed at the front of the character's input queue.
 */
int 
perform_alias (struct descriptor_data *d, char *orig)
{
  char first_arg[MAX_INPUT_LENGTH], *ptr;
  struct alias *a, *tmp;

  /* bail out immediately if the guy doesn't have any aliases */
  if ((tmp = GET_ALIASES (d->character)) == NULL)
    return 0;

  /* find the alias we're supposed to match */
  ptr = any_one_arg (orig, first_arg);

  /* bail out if it's null */
  if (!*first_arg)
    return 0;

  /* if the first arg is not an alias, return without doing anything */
  if ((a = find_alias (tmp, first_arg)) == NULL)
    return 0;

  if (a->type == ALIAS_SIMPLE)
    {
      strcpy (orig, a->replacement);
      return 0;
    }
  else
    {
      perform_complex_alias (&d->input, ptr, a);
      return 1;
    }
}



/***************************************************************************
 * Various other parsing utilities                                         *
 **************************************************************************/

/*
 * searches an array of strings for a target string.  "exact" can be
 * 0 or non-0, depending on whether or not the match must be exact for
 * it to be returned.  Returns -1 if not found; 0..n otherwise.  Array
 * must be terminated with a '\n' so it knows to stop searching.
 */
int search_block(char *arg, const char **list, int exact)
{
  register int i, l;

  /* Make into lower case, and get length of string */
  for (l = 0; *(arg + l); l++)
    *(arg + l) = LOWER(*(arg + l));

  if (exact) {
    for (i = 0; **(list + i) != '\n'; i++)
      if (!strcmp(arg, *(list + i)))
	return (i);
  } else {
    if (!l)
      l = 1;                    /* Avoid "" to match the first available
				 * string */
    for (i = 0; **(list + i) != '\n'; i++)
      if (!strncmp(arg, *(list + i), l))
	return (i);
  }
    
  return -1;
}

int 
is_number (const char *str)
{
  if (str[0]=='-') /* Negatives are numbers, too :) */
    str++;
  while (*str)
    if (!isdigit (*(str++)))
      return 0;

  return 1;
}

/*
 * Function to skip over the leading spaces of a string.
 */
void 
skip_spaces (char **string)
{
  for (; **string && isspace (**string); (*string)++);
}


/*
 * Given a string, change all instances of double dollar signs ($$) to
 * single dollar signs ($).  When strings come in, all $'s are changed
 * to $$'s to avoid having users be able to crash the system if the
 * inputted string is eventually sent to act().  If you are using user
 * input to produce screen output AND YOU ARE SURE IT WILL NOT BE SENT
 * THROUGH THE act() FUNCTION (i.e., do_gecho, do_title, but NOT do_say),
 * you can call delete_doubledollar() to make the output look correct.
 *
 * Modifies the string in-place.
 */
char *
delete_doubledollar (char *string)
{
  char *read, *write;

  /* If the string has no dollar signs, return immediately */
  if ((write = strchr (string, '$')) == NULL)
    return string;

  /* Start from the location of the first dollar sign */
  read = write;


  while (*read)                 /* Until we reach the end of the string... */
    if ((*(write++) = *(read++)) == '$')        /* copy one char */
      if (*read == '$')
	read++;                 /* skip if we saw 2 $'s in a row */

  *write = '\0';

  return string;
}


int 
fill_word (char *argument)
{
  return (search_block (argument, (const char **) fill, TRUE) >= 0);
}


int 
reserved_word (char *argument)
{
  return (search_block (argument, (const char **) reserved, TRUE) >= 0);
}


/*
 * copy the first non-fill-word, space-delimited argument of 'argument'
 * to 'first_arg'; return a pointer to the remainder of the string.
 */
char *
one_argument (char *argument, char *first_arg)
{
  char *begin = first_arg;

  do
    {
      skip_spaces (&argument);

      first_arg = begin;
      while (*argument && !isspace (*argument))
	{
	  *(first_arg++) = LOWER (*argument);
	  argument++;
	}

      *first_arg = '\0';
    }
  while (fill_word (begin));

  return argument;
}


/*
 * one_word is like one_argument, except that words in quotes ("") are
 * considered one word.
 */
char *
one_word (char *argument, char *first_arg)
{
  char *begin = first_arg;

  do
    {
      skip_spaces (&argument);

      first_arg = begin;

      if (*argument == '\"')
	{
	  argument++;
	  while (*argument && *argument != '\"')
	    {
	      *(first_arg++) = LOWER (*argument);
	      argument++;
	    }
	  argument++;
	}
      else
	{
	  while (*argument && !isspace (*argument))
	    {
	      *(first_arg++) = LOWER (*argument);
	      argument++;
	    }
	}

      *first_arg = '\0';
    }
  while (fill_word (begin));

  return argument;
}


/* same as one_argument except that it doesn't ignore fill words */
char *
any_one_arg (char *argument, char *first_arg)
{
  skip_spaces (&argument);

  while (*argument && !isspace (*argument))
    {
      *(first_arg++) = LOWER (*argument);
      argument++;
    }

  *first_arg = '\0';

  return argument;
}


/*
 * Same as one_argument except that it takes two args and returns the rest;
 * ignores fill words
 */
char *
two_arguments (char *argument, char *first_arg, char *second_arg)
{
  return one_argument (one_argument (argument, first_arg), second_arg);         /* :-) */
}



/*
 * determine if a given string is an abbreviation of another
 * (now works symmetrically -- JE 7/25/94)
 *
 * that was dumb.  it shouldn't be symmetrical.  JE 5/1/95
 * 
 * returnss 1 if arg1 is an abbreviation of arg2
 */
int 
is_abbrev (const char *arg1, const char *arg2)
{
  if (!arg || !arg2)
    return 0;
  if (!*arg1 || !*arg2)
    return 0;

  for (; *arg1 && *arg2; arg1++, arg2++)
    if (LOWER (*arg1) != LOWER (*arg2))
      return 0;

  if (!*arg1)
    return 1;
  else
    return 0;
}



/* return first space-delimited token in arg1; remainder of string in arg2 */
void 
half_chop (char *string, char *arg1, char *arg2)
{
  char *temp;

  temp = any_one_arg (string, arg1);
  skip_spaces (&temp);
  strcpy (arg2, temp);
}



/* Used in specprocs, mostly.  (Exactly) matches "command" to cmd number */
int find_command(const char *command)
{
  int cmd;
      
  for (cmd = 0; *complete_cmd_info[cmd].command != '\n'; cmd++)
    if (!strcmp(complete_cmd_info[cmd].command, command))
      return cmd;
    
  return -1;
}

int 
special (struct char_data *ch, int cmd, char *arg)
{
  register struct obj_data *i;
  register struct char_data *k;
  int j;

  /* special in room? */
  if (GET_ROOM_SPEC (ch->in_room) != NULL)
    if (GET_ROOM_SPEC (ch->in_room) (ch, world + ch->in_room, cmd, arg))
      return 1;

  /* special in equipment list? */
  for (j = 0; j < NUM_WEARS; j++)
    if (GET_EQ (ch, j) && GET_OBJ_SPEC (GET_EQ (ch, j)) != NULL)
      if (GET_OBJ_SPEC (GET_EQ (ch, j)) (ch, GET_EQ (ch, j), cmd, arg))
	return 1;

  /* special in inventory? */
  for (i = ch->carrying; i; i = i->next_content)
    if (GET_OBJ_SPEC (i) != NULL)
      if (GET_OBJ_SPEC (i) (ch, i, cmd, arg))
	return 1;

  /* special in mobile present? */
  for (k = world[ch->in_room].people; k; k = k->next_in_room)
    if (GET_MOB_SPEC (k) != NULL)
      if (GET_MOB_SPEC (k) (ch, k, cmd, arg))
	return 1;

  /* special in object present? */
  for (i = world[ch->in_room].contents; i; i = i->next_content)
    if (GET_OBJ_SPEC (i) != NULL)
      if (GET_OBJ_SPEC (i) (ch, i, cmd, arg))
	return 1;

  return 0;
}



/* *************************************************************************
   *  Stuff for controlling the non-playing sockets (get name, pwd etc)       *
   ************************************************************************* */


/* locate entry in p_table with entry->name == name. -1 mrks failed search */
int 
find_name (char *name)
{
  int i;

  for (i = 0; i <= top_of_p_table; i++)
    {
      if (!str_cmp ((player_table + i)->name, name))
	return i;
    }

  return -1;
}


int 
_parse_name (char *arg, char *name)
{
  int i;

  /* skip whitespaces */
  for (; isspace (*arg); arg++);

  for (i = 0; (*name = *arg); arg++, i++, name++)
    if (!isalpha (*arg))
      return 1;

  if (!i)
    return 1;

  return 0;
}


#define RECON           1
#define USURP           2
#define UNSWITCH        3

int 
perform_dupe_check (struct descriptor_data *d)
{
  struct descriptor_data *k, *next_k;
  struct char_data *target = NULL, *ch, *next_ch;
  int mode = 0;

  int id = GET_IDNUM (d->character);

  /*
   * Now that this descriptor has successfully logged in, disconnect all
   * other descriptors controlling a character with the same ID number.
   */

  for (k = descriptor_list; k; k = next_k)
    {
      next_k = k->next;

      if (k == d)
	continue;

      if (k->original && (GET_IDNUM (k->original) == id))
	{                       /* switched char */
	  SEND_TO_Q ("\r\nMultiple login detected -- disconnecting.\r\n", k);
	  STATE (k) = CON_CLOSE;
	  if (!target)
	    {
	      target = k->original;
	      mode = UNSWITCH;
	    }
	  if (k->character)
	    k->character->desc = NULL;
	  k->character = NULL;
	  k->original = NULL;
	}
      else if (k->character && (GET_IDNUM (k->character) == id))
	{
	  if (!target && STATE (k) == CON_PLAYING)
	    {
	      SEND_TO_Q ("\r\nThis body has been usurped!\r\n", k);
	      target = k->character;
	      mode = USURP;
	    }
	  k->character->desc = NULL;
	  k->character = NULL;
	  k->original = NULL;
	  SEND_TO_Q ("\r\nMultiple login detected -- disconnecting.\r\n", k);
	  STATE (k) = CON_CLOSE;
	}
    }

  /*
   * now, go through the character list, deleting all characters that
   * are not already marked for deletion from the above step (i.e., in the
   * CON_HANGUP state), and have not already been selected as a target for
   * switching into.  In addition, if we haven't already found a target,
   * choose one if one is available (while still deleting the other
   * duplicates, though theoretically none should be able to exist).
   */

  for (ch = character_list; ch; ch = next_ch)
    {
      next_ch = ch->next;

      if (IS_NPC (ch))
	continue;
      if (GET_IDNUM (ch) != id)
	continue;

      /* ignore chars with descriptors (already handled by above step) */
      if (ch->desc)
	continue;

      /* don't extract the target char we've found one already */
      if (ch == target)
	continue;

      /* we don't already have a target and found a candidate for switching */
      if (!target)
	{
	  target = ch;
	  mode = RECON;
	  continue;
	}

      /* we've found a duplicate - blow him away, dumping his eq in limbo. */
      if (ch->in_room != NOWHERE)
	char_from_room (ch);
      char_to_room (ch, 1);
      extract_char (ch);
    }

  /* no target for swicthing into was found - allow login to continue */
  if (!target)
    return 0;

  /* Okay, we've found a target.  Connect d to target. */
  free_char (d->character);     /* get rid of the old char */
  d->character = target;
  d->character->desc = d;
  d->original = NULL;
  d->character->char_specials.timer = 0;
  REMOVE_BIT (PLR_FLAGS (d->character), PLR_MAILING | PLR_WRITING);
  STATE (d) = CON_PLAYING;

  switch (mode)
    {
    case RECON:
      SEND_TO_Q ("Reconnecting.\r\n", d);

// this was annoying, removed 8/23/98 --Mulder
// if(GET_LEVEL(d->character) < LVL_IMMORT)
//{
//      sprintf (buf, "&m[&YINFO&m]&n %s has reconnected.\r\n",
//     GET_NAME (d->character));
//      send_to_all (buf);
//}
      act("$n has reconnected.", TRUE, d->character, 0, 0, TO_ROOM);
      sprintf (buf, "%s [%s] has reconnected.", GET_NAME (d->character), d->host);
      mudlog (buf, NRM, MAX (LVL_IMMORT, GET_INVIS_LEV (d->character)), TRUE);
      break;
    case USURP:
      SEND_TO_Q ("You take over your own body, already in use!\r\n", d);
      act ("$n suddenly keels over in pain, surrounded by a white aura...\r\n"
	   "$n's body has been taken over by a new spirit!",
	   TRUE, d->character, 0, 0, TO_ROOM);
      sprintf (buf, "%s has re-logged in ... disconnecting old socket.",
	       GET_NAME (d->character));
      mudlog (buf, NRM, MAX (LVL_IMMORT, GET_INVIS_LEV (d->character)), TRUE);
      break;
    case UNSWITCH:
      SEND_TO_Q ("Reconnecting to unswitched char.", d);
      sprintf (buf, "%s [%s] has reconnected.", GET_NAME (d->character), d->host);
      mudlog (buf, NRM, MAX (LVL_IMMORT, GET_INVIS_LEV (d->character)), TRUE);
      break;
    }

  return 1;
}


/* load the player, put them in the right room - used by copyover_recover too */
int enter_player_game (struct descriptor_data *d)
{
  extern long mortal_start_room[NUM_STARTROOMS + 1];
  extern long r_immort_start_room;
  extern long r_frozen_start_room;
    
    long load_room;
    int load_result;
    
	  reset_char (d->character);
	  if (PLR_FLAGGED (d->character, PLR_INVSTART))
	    GET_INVIS_LEV (d->character) = GET_LEVEL (d->character);
	  
	  /* put character to their loadroom before autoequipping them */
	  if ((load_room = GET_LOADROOM (d->character)) != NOWHERE)
	    load_room = real_room (load_room);
          if (real_room(d->character->player_specials->saved.tloadroom)!=NOWHERE) {
            load_room = real_room(d->character->player_specials->saved.tloadroom);
            d->character->player_specials->saved.tloadroom=-1;
          } /* Higher priority loadroom but temporary; used for copyovers. */
	  if (load_room != NOWHERE) // If something screws up, we're gonna crash -- you cant access world[-1] - Storm
	    if (IS_SET(ROOM_FLAGS(load_room), ROOM_IMPROOM) && GET_LEVEL(d->character) < LVL_GRGOD)
	      load_room = NOWHERE;
		  
	  if (d->character->player_specials->saved.newbie == 1 && GET_LEVEL(d->character) < 5){
	    load_room = real_room (newbie_room);
	    SET_BIT(PRF_FLAGS(d->character), PRF_AUTOEXIT);
	  }
   
	  if (find_room_by_coords(d->character->player_specials->saved.mapx,
d->character->player_specials->saved.mapy) != NOWHERE &&
d->character->player_specials->saved.mapx >= 1 && d->character->player_specials->saved.mapx <=
max_map_x && d->character->player_specials->saved.mapy >= 1 &&
d->character->player_specials->saved.mapy <= max_map_y)
	    load_room=find_room_by_coords(d->character->player_specials->saved.mapx, 
d->character->player_specials->saved.mapy);
	  d->character->player_specials->saved.mapx=-1;
	  d->character->player_specials->saved.mapy=-1;
	  /* If char was saved with NOWHERE, or real_room above failed... */
	  if (load_room == NOWHERE)
	    {
	      if (GET_LEVEL (d->character) >= LVL_IMMORT)
		load_room = r_immort_start_room;
	      else
		load_room = real_room (mortal_start_room[GET_HOME (d->character)]);
	    }
		  
	  if (!IS_NPC(d->character)
	      && PRF2_FLAGGED(d->character, PRF2_LOCKOUT))
	    REMOVE_BIT (PRF2_FLAGS (d->character), PRF2_LOCKOUT);
	   
	  if (PLR_FLAGGED (d->character, PLR_FROZEN))
	    load_room = r_frozen_start_room;
	  
	  if (PLR_FLAGGED (d->character, PLR_KILLER))
	    load_room = real_room(jail_num);

        if (PRF2_FLAGGED(d->character, PRF2_INTANGIBLE) && !PRF2_FLAGGED(d->character, PRF2_MBUILDING)) {
	    load_room = real_room(99);
        }
	  char_to_room (d->character, load_room);
	  if ((load_result = Crash_load (d->character)))
	    if (GET_LEVEL (d->character) < LVL_IMMORT &&
		!PLR_FLAGGED (d->character, PLR_FROZEN))
	      {
		char_from_room (d->character);
		char_to_room (d->character, load_room);
	      }
       /* with the copyover patch, this next line goes in enter_player_game() */
	  GET_ID(d->character) = GET_IDNUM(d->character);

	  save_char (d->character, NOWHERE);
	  d->character->next = character_list;
	  character_list = d->character;

    return load_result;
}

void start_player (struct descriptor_data * d) {
  SET_BIT (PRF_FLAGS (d->character), PRF_NOLOOKSTACK | PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE
| PRF_DISPEXP);
  SET_BIT (PRF2_FLAGS (d->character), PRF2_DISPMOB);
  create_entry (GET_NAME (d->character));
  init_char (d->character);
  save_char (d->character, NOWHERE);
  SEND_TO_Q (motd, d);
  SEND_TO_Q ("\r\n\n*** PRESS RETURN: ", d);
  STATE (d) = CON_RMOTD;
  GET_CLAN(d->character)=-1;
  GET_CLAN_RANK(d->character)=-1;
  d->character->player_specials->saved.tloadroom=-1;
  sprintf (buf, "%s [%s] new player.", GET_NAME (d->character), d->host);
  mudlog (buf, NRM, LVL_IMMORT, TRUE);
  save_char(d->character, NOWHERE);
  user_cntr(d);
/*  d->character->real_abils.str=1;
  d->character->real_abils.intel=1;
  d->character->real_abils.wis=1;
  d->character->real_abils.con=1;
  d->character->real_abils.dex=1;
  d->character->real_abils.cha=1; */
}
/* deal with newcomers and other non-playing sockets */
void 
nanny (struct descriptor_data *d, char *arg)
{
  char buf[128];
  int load_result;
  char tmp_name[MAX_INPUT_LENGTH];
  struct descriptor_data *t;
  extern const char *class_menu;
  extern const char *race_menu;
  extern const char *town_menu;
  extern const char *deity_menu;
  extern const char *ANSI;
  extern int max_bad_pws;
//  long load_room;
  int color = 0;
  int parse_class (char arg);
  int parse_race (char arg);
  int parse_deity (char arg);

  skip_spaces (&arg);

  if (d->character == NULL)
    {
      CREATE (d->character, struct char_data, 1);
      clear_char (d->character);
      CREATE (d->character->player_specials, struct player_special_data, 1);
      d->character->desc = d;
    }
  switch (STATE (d))
    {

      /*. OLC states . */
  case CON_HEDIT:
    hedit_parse(d, arg);
    break;
  case CON_OEDIT:
    oedit_parse(d, arg);
    break;
  case CON_REDIT:
    redit_parse(d, arg);
    break;
  case CON_ZEDIT:
    zedit_parse(d, arg);
    break;
  case CON_MEDIT:
    medit_parse(d, arg);
    break;
  case CON_SEDIT:
    sedit_parse(d, arg);
    break;
  case CON_AEDIT:
    aedit_parse(d, arg);
    break;
  case CON_TRIGEDIT:
    trigedit_parse(d, arg);
    break;
      /*. End of OLC states . */
    case CON_QANSI:
      if (!*arg || LOWER (*arg) == 'y')
	{
	  SET_BIT (PRF_FLAGS (d->character), PRF_COLOR_1 | PRF_COLOR_2);
	  SEND_TO_Q ("Your terminal will now receive color.\r\n\r\n\r\n", d);
	  SEND_TO_Q (startup, d);
	}
      else if (LOWER (*arg) == 'n')
	{
	  REMOVE_BIT (PRF_FLAGS (d->character), PRF_COLOR_1 | PRF_COLOR_2);
	  SEND_TO_Q ("Your terminal will not receive color.\r\n\r\n\r\n", d);
	  SEND_TO_Q (startup, d);
	}
      else
	{
	  SEND_TO_Q ("That is not a proper response.\r\n\r\n", d);
	  SEND_TO_Q (ANSI, d);
	  return;
	}
      STATE (d) = CON_GET_NAME;
      SEND_TO_Q (ASK_NAME, d);
      break;

    case CON_GET_NAME:          /* wait for input of name */
      if (!*arg)
	close_socket (d);
      else
	{
	  if ((_parse_name (arg, tmp_name)) || strlen (tmp_name) < 2 ||
	  strlen(tmp_name) > MAX_NAME_LENGTH || !Valid_Name(tmp_name) ||
	      fill_word (strcpy (buf, tmp_name)) || reserved_word (buf))
	    {
	      SEND_TO_Q ("Invalid name, please try another.\r\n"
			 "Name: ", d);
	      return;
	    }
          if (retrieve_player_entry(tmp_name, d->character))
	    {
	      if (PRF_FLAGGED (d->character, PRF_COLOR_1))
		color = 1;


	      if (PLR_FLAGGED (d->character, PLR_DELETED))
		{
		  free_char (d->character);
		  CREATE (d->character, struct char_data, 1);
		  clear_char (d->character);
		  CREATE (d->character->player_specials, struct player_special_data, 1);
		  d->character->desc = d;
		  CREATE (d->character->player.name, char, strlen (tmp_name) + 1);
		  strcpy (d->character->player.name, CAP (tmp_name));
		  if (color)
		    SET_BIT (PRF_FLAGS (d->character), PRF_COLOR_1 | PRF_COLOR_2);
		  else
		    REMOVE_BIT (PRF_FLAGS (d->character), PRF_COLOR_1 | PRF_COLOR_2);

		  sprintf (buf, "Did I get that right, %s &c(&YY&c/&YN&c)&n? ", tmp_name);
		  SEND_TO_Q (buf, d);
		  STATE (d) = CON_NAME_CNFRM;
		}
	      else
		{
		  /* undo it just in case they are set */
		  REMOVE_BIT (PLR_FLAGS (d->character),
			      PLR_WRITING | PLR_MAILING | PLR_CRYO);
		  if (color)
		    SET_BIT (PRF_FLAGS (d->character), PRF_COLOR_1 | PRF_COLOR_2);
		  else
		    REMOVE_BIT (PRF_FLAGS (d->character), PRF_COLOR_1 | PRF_COLOR_2);

		  SEND_TO_Q ("Password: ", d);
		  echo_off (d);
		  d->idle_tics = 0;
		  STATE (d) = CON_PASSWORD;
		}
	    }
	  else
	    {
	      /* player unknown -- make new character */

	      if (!Valid_Name (tmp_name))
		{
		  SEND_TO_Q ("Invalid name, please try another.\r\n", d);
		  SEND_TO_Q ("Name: ", d);
		  return;
		}
	      CREATE (d->character->player.name, char, strlen (tmp_name) + 1);
	      strcpy (d->character->player.name, CAP (tmp_name));

	      sprintf (buf, "Did I get that right, %s &c(&YY&c/&YN&c)&n? ", tmp_name);
	      SEND_TO_Q (buf, d);
	      STATE (d) = CON_NAME_CNFRM;
	    }
	}
      break;
    case CON_NAME_CNFRM:        /* wait for conf. of new name    */
      if (UPPER (*arg) == 'Y')
	{
	  if (isbanned (d->host) >= BAN_NEW)
	    {
	      sprintf (buf, "Request for new char %s denied from [%s] (siteban)",
		       GET_NAME (d->character), d->host);
	      mudlog (buf, NRM, LVL_GOD, TRUE);
	      SEND_TO_Q ("Sorry, new characters are not allowed from your site!\r\n", d);
	      STATE (d) = CON_CLOSE;
	      return;
	    }
	  if (circle_restrict)
	    {
	      SEND_TO_Q ("Sorry, new players can't be created at the moment.\r\n", d);
	      sprintf (buf, "Request for new char %s denied from [%s] (wizlock)",
		       GET_NAME (d->character), d->host);
	      mudlog (buf, NRM, LVL_GOD, TRUE);
	      STATE (d) = CON_CLOSE;
	      return;
	    }
      for (t=descriptor_list; t; t=t->next) {
        if (t==d) continue;
        if (!d->character || !t->character) continue;
        if (!GET_NAME(d->character) || !GET_NAME(t->character)) continue;
        if (!strcmp(GET_NAME (d->character), GET_NAME(t->character))) {
              sprintf (buf, "Request for new char %s denied from [%s] (dupe char in creation)",
                       GET_NAME (d->character), d->host);
              mudlog (buf, NRM, LVL_GOD, TRUE);
              SEND_TO_Q ("Sorry, someone is already creating that player!\r\n", d);
              STATE (d) = CON_CLOSE;
              return;
        }
      }
      if (!check_multiplaying(d->host) && (GET_LEVEL(d->character) < LVL_IMMORT)
       && !PLR_FLAGGED(d->character, PLR_MULTIOK)) {
       SEND_TO_Q("\r\n"
"Sorry, there is already more then one connection to the MUD from your host.\r\n"
"If you are playing from a shared connection please e-mail help@deltamud.net\r\n"
"for access.\r\n\r\n", d);

	STATE(d) = CON_CLOSE;
       sprintf(buf, "Request for new char %s denied from %s - multi-play",
	       GET_NAME(d->character), d->host);
       mudlog(buf, NRM, LVL_IMPL, TRUE);
       return;
       }
	  SEND_TO_Q ("New character.\r\n", d);
	  sprintf (buf, "Give me a password for %s: ", GET_NAME (d->character));
	  SEND_TO_Q (buf, d);
	  echo_off (d);
	  STATE (d) = CON_NEWPASSWD;
	}
      else if (*arg == 'n' || *arg == 'N')
	{
	  SEND_TO_Q ("Okay, what IS it, then? ", d);
	  free (d->character->player.name);
	  d->character->player.name = NULL;
	  STATE (d) = CON_GET_NAME;
	}
      else
	{
	  SEND_TO_Q ("Please type Yes or No: ", d);
	}
      break;
    case CON_PASSWORD:          /* get pwd for known player      */
      /*
       * To really prevent duping correctly, the player's record should
       * be reloaded from disk at this point (after the password has been
       * typed).  However I'm afraid that trying to load a character over
       * an already loaded character is going to cause some problem down the
       * road that I can't see at the moment.  So to compensate, I'm going to
       * (1) add a 15 or 20-second time limit for entering a password, and (2)
       * re-add the code to cut off duplicates when a player quits.  JE 6 Feb 96
       */

      echo_on (d);              /* turn echo back on */

      if (!*arg)
	close_socket (d);
      else
	{
	  if (strncmp (CRYPT (arg, GET_PASSWD (d->character)), GET_PASSWD (d->character), MAX_PWD_LENGTH))
	    {
	      sprintf (buf, "Bad PW: %s [%s]", GET_NAME (d->character), d->host);
	      mudlog (buf, BRF, LVL_GOD, TRUE);
	      GET_BAD_PWS (d->character)++;
	      save_char (d->character, NOWHERE);
	      if (++(d->bad_pws) >= max_bad_pws)
		{               /* 3 strikes and you're out. */
		  SEND_TO_Q ("Wrong password... disconnecting.\r\n", d);
		  STATE (d) = CON_CLOSE;
		}
	      else
		{
		  SEND_TO_Q ("Wrong password.\r\nPassword: ", d);
		  echo_off (d);
		}
	      return;
	    }

	  /* Password was correct. */
	  load_result = GET_BAD_PWS (d->character);
	  GET_BAD_PWS (d->character) = 0;
	  d->bad_pws = 0;

	  if (isbanned (d->host) == BAN_SELECT &&
	      !PLR_FLAGGED (d->character, PLR_SITEOK))
	    {
	      SEND_TO_Q ("Sorry, this char has not been cleared for login from your site!\r\n", d);
	      STATE (d) = CON_CLOSE;
	      sprintf (buf, "Connection attempt for %s denied from %s",
		       GET_NAME (d->character), d->host);
	      mudlog (buf, NRM, LVL_GOD, TRUE);
	      return;
	    }
      if (!check_multiplaying(d->host) && (GET_LEVEL(d->character) < LVL_IMMORT)
       && !PLR_FLAGGED(d->character, PLR_MULTIOK)) {
       SEND_TO_Q(
"Sorry, there is already more then one connection to the MUD from your host.\r\n"
"If you are playing from a shared connection please e-mail help@deltamud.net\r\n"
"for access.\r\n\r\n", d);

	STATE(d) = CON_CLOSE;
       sprintf(buf, "Connection attempt for %s denied from %s - multi-play",
	       GET_NAME(d->character), d->host);
       mudlog(buf, NRM, LVL_IMPL, TRUE);
       return;
      }
	  if (GET_LEVEL (d->character) < circle_restrict)
	    {
	      SEND_TO_Q ("The game is temporarily restricted.. try again later.\r\n", d);
	      STATE (d) = CON_CLOSE;
	      sprintf (buf, "Request for login denied for %s [%s] (wizlock)",
		       GET_NAME (d->character), d->host);
	      mudlog (buf, NRM, LVL_GOD, TRUE);
	      return;
	    }
	  /* check and make sure no other copies of this player are logged in */
	  if (perform_dupe_check (d))
	    return;

	  if (GET_LEVEL (d->character) >= LVL_IMMORT)
	    SEND_TO_Q (imotd, d);
	  else
	    SEND_TO_Q (motd, d);

	  user_cntr(d);
	  sprintf (buf, "%s [%s] has connected.", GET_NAME (d->character), d->host);
	  mudlog (buf, BRF, MAX (LVL_GRGOD, GET_INVIS_LEV (d->character)), TRUE);

	  if (load_result)
	    {
	      sprintf (buf, "\r\n\r\n\007\007\007"
		  "%s%d LOGIN FAILURE%s SINCE LAST SUCCESSFUL LOGIN.%s\r\n",
		       CCRED (d->character, C_SPR), load_result,
		 (load_result > 1) ? "S" : "", CCNRM (d->character, C_SPR));
	      SEND_TO_Q (buf, d);
	      GET_BAD_PWS (d->character) = 0;
	    }
	  SEND_TO_Q ("\r\n", d);
	  do_time (d->character, "", 0, 0);
	  SEND_TO_Q ("\r\n\n*** PRESS RETURN: ", d);
	  STATE (d) = CON_RMOTD;
	}
      break;

    case CON_NEWPASSWD:
    case CON_CHPWD_GETNEW:
      if (!*arg || strlen (arg) > MAX_PWD_LENGTH || strlen (arg) < 3 ||
	  !str_cmp (arg, GET_NAME (d->character)))
	{
	  SEND_TO_Q ("\r\nIllegal password.\r\n", d);
	  SEND_TO_Q ("Password: ", d);
	  return;
	}
      if (GET_PASSWD (d->character)) free(GET_PASSWD (d->character));
      GET_PASSWD (d->character)=strdup(CRYPT (arg, GET_NAME (d->character)));

      SEND_TO_Q ("\r\nPlease retype password: ", d);
      if (STATE (d) == CON_NEWPASSWD)
	STATE (d) = CON_CNFPASSWD;
      else
	STATE (d) = CON_CHPWD_VRFY;

      break;

    case CON_CNFPASSWD:
    case CON_CHPWD_VRFY:
      if (strncmp (CRYPT (arg, GET_PASSWD (d->character)), 
		   GET_PASSWD (d->character), MAX_PWD_LENGTH))
	{
	  SEND_TO_Q ("\r\nPasswords don't match... start over.\r\n", d);
	  SEND_TO_Q ("Password: ", d);
	  if (STATE (d) == CON_CNFPASSWD)
	    STATE (d) = CON_NEWPASSWD;
	  else
	    STATE (d) = CON_CHPWD_GETNEW;
	  return;
	}
      echo_on (d);

      if (STATE (d) == CON_CNFPASSWD)
	{
	  SEND_TO_Q ("Are you completely new to MUDing &c(&YY&c/&YN&c)&n? ",
		     d);
	  STATE (d) = CON_NEWBIE;
	}
      else
	{
	  save_char (d->character, NOWHERE);
	  echo_on (d);
	  SEND_TO_Q ("\r\nDone.\n\r", d);
	  SEND_TO_Q (MENU, d);
	  STATE (d) = CON_MENU;
	}

      break;

    case CON_NEWBIE:
      switch (*arg)
	{
	case 'y':
	case 'Y':
	  d->character->player_specials->saved.newbie = 1;
	  break;
	case 'n':
	case 'N':
	  d->character->player_specials->saved.newbie = 0;
	  break;
	default:
	 SEND_TO_Q ("Please type Yes or No: ", d);
	  return;
	  break;
	}

      SEND_TO_Q ("\r\nWhat is your sex &c(&YM&c/&YF&c)&n? ", d);
      STATE (d) = CON_QSEX;
      break;

    case CON_QSEX:              /* query sex of new user         */
      switch (*arg)
	{
	case 'm':
	case 'M':
	  d->character->player.sex = SEX_MALE;
	  break;
	case 'f':
	case 'F':
	  d->character->player.sex = SEX_FEMALE;
	  break;
	default:
	  SEND_TO_Q ("That is not a sex..\r\n"
		     "What IS your sex? ", d);
	  return;
	  break;

	}
      SEND_TO_Q (race_menu, d);
      SEND_TO_Q ("\r\nTo see a race's average statistics type help <race letter>.\r\nRace: ",
d);
      STATE (d) = CON_QRACE;
      break;

    case CON_QRACE:
      if (!strncasecmp(arg, "help", 4)) {
        load_result = parse_race (arg[5]);
        arg[5]='\0';
        if (load_result == RACE_UNDEFINED)
          {
            SEND_TO_Q ("\r\nThat's not a race.\r\nRace: ", d);
            return;
          }
       sprintf(buf2, "At 11 as the universal statistic average, your race averages the following
abilities:\r\n"
                      "Str: %2d Int: %2d Wis: %2d Dex: %2d Con: %2d Cha: %2d\r\nRace: ",
                      (GET_RACE_MIN(load_result, 1)+GET_RACE_MAX(load_result, 1))/2,
                      (GET_RACE_MIN(load_result, 2)+GET_RACE_MAX(load_result, 2))/2,
                      (GET_RACE_MIN(load_result, 3)+GET_RACE_MAX(load_result, 3))/2,
                      (GET_RACE_MIN(load_result, 4)+GET_RACE_MAX(load_result, 4))/2,
                      (GET_RACE_MIN(load_result, 5)+GET_RACE_MAX(load_result, 5))/2,
                      (GET_RACE_MIN(load_result, 6)+GET_RACE_MAX(load_result, 6))/2);
        SEND_TO_Q(buf2, d);
        return;
      }
      load_result = parse_race (*arg);
      if (load_result == RACE_UNDEFINED)
	{
	  SEND_TO_Q ("\r\nThat's not a race.\r\nRace: ", d);
	  return;
	}
      else
	GET_RACE (d->character) = load_result;

      SEND_TO_Q (deity_menu, d);
      SEND_TO_Q ("\r\nDeity: ", d);
      STATE (d) = CON_QDEITY;
      break;

    case CON_QDEITY:
      load_result = parse_deity (*arg);
      if (load_result == DEITY_UNDEFINED)
	{
	  SEND_TO_Q ("\r\nThat's not a deity.\r\nDeity: ", d);
	  return;
	}
      else
	GET_DEITY (d->character) = load_result;
	
      SEND_TO_Q (class_menu, d);
      SEND_TO_Q ("\r\nClass: ", d);
      STATE (d) = CON_QCLASS;
      break;

    case CON_QCLASS:
      load_result = parse_class (*arg);
      if (load_result == CLASS_UNDEFINED)
	{
	  SEND_TO_Q ("\r\nThat's not a class.\r\nClass: ", d);
	  return;
	}
      else
	GET_CLASS (d->character) = load_result;
      if (d->character->player_specials->saved.newbie == 0){
	SEND_TO_Q (town_menu, d);
	SEND_TO_Q ("\r\nTown: ", d);
	STATE (d) = CON_QHOMETOWN;
      } else {
	SEND_TO_Q ("\r\nYour hometown has been set to the "
		   "capital city of Anacreon.\r\n\r\n", d);
	GET_HOME (d->character) = 1;
	STATE (d) = CON_QROLLSTATS;
        SEND_TO_Q ("\r\nHere is a brief explanation of each ability:\r\n"
	   "[&YStr&n] - Strength determines how hard you hit your "
	   "opponents in a fight.\r\n\r\n"
	   "[&YInt&n] - Intelligence determines how well you hit your "
	   "opponents in a fight,\r\n"
	   "        and also the amount of magic points for spells "
	   "(clerics and mages).\r\n\r\n"
	   "[&YWis&n] - Wisdom determines how well you hit your "
	   "opponents in a fight,\r\n"
	   "        and also the amount of magic spells you can "
	   "learn (clerics and mages).\r\n\r\n"
	   "[&YDex&n] - Dexterity determines how well you fight in "
	   "a battle, and also\r\n"
	   "        how cunning and sneaky you are.\r\n\r\n"
	   "[&YCon&n] - Constitution determines how much health "
	   "you have.\r\n\r\n"
	   "[&YCha&n] - Charisma determines how good you are with "
	   "people :)\r\n\r\n", d);
           roll_real_abils (d->character);
           sprintf(buf, "\r\nStr: %d Int: %d Wis: %d Dex: %d Con: %d Cha: %d\r\n"
                   "Are these values acceptable? (Y/&YN&n): ", d->character->real_abils.str,
                   d->character->real_abils.intel, d->character->real_abils.wis, d->character->real_abils.dex,
                   d->character->real_abils.con, d->character->real_abils.cha);
           SEND_TO_Q(buf, d);
      }
      break;

    case CON_QHOMETOWN:
      load_result = parse_town (*arg);
      if (load_result == TOWN_UNDEFINED)
	{
	  SEND_TO_Q ("\r\nThat's not a town.\r\nTown: ", d);
	  return;
	}
      else
	GET_HOME (d->character) = load_result;
      STATE (d) = CON_QROLLSTATS;
      roll_real_abils (d->character);
      sprintf(buf, "\r\nStr: %d Int: %d Wis: %d Dex: %d Con: %d Cha: %d\r\n"
              "Are these values acceptable? (Y/&YN&n): ", d->character->real_abils.str,
              d->character->real_abils.intel, d->character->real_abils.wis, d->character->real_abils.dex,
              d->character->real_abils.con, d->character->real_abils.cha);
      SEND_TO_Q(buf, d);
      break;

    case CON_QROLLSTATS:
      switch (*arg) {
        case 'Y':
        case 'y':
          start_player(d);
          break;
        default:
          roll_real_abils (d->character);
          sprintf(buf, "\r\nStr: %d Int: %d Wis: %d Dex: %d Con: %d Cha: %d\r\n"
                       "Are these values acceptable? (Y/&YN&n): ", d->character->real_abils.str,
                       d->character->real_abils.intel, d->character->real_abils.wis, d->character->real_abils.dex,
                       d->character->real_abils.con, d->character->real_abils.cha);
          SEND_TO_Q(buf, d);
          break;
      }
      break;

    case CON_RMOTD:             /* read CR after printing motd   */
      SEND_TO_Q (MENU, d);
      STATE (d) = CON_MENU;
      break;

    case CON_MENU:              /* get selection from main menu  */
      switch (*arg)
	{
	case '0':
	  SEND_TO_Q ("\r\nYou awaken, and find yourself in a land called reality.\r\nWe hope you come back to Deltania soon!\r\n\r\n", d);
	  STATE (d) = CON_CLOSE;
	  break;

	case '1':
	  load_result = enter_player_game(d);
	  send_to_char (WELC_MESSG, d->character);
	  act("$n has entered the game.", TRUE, d->character, 0, 0, TO_ROOM);

	  STATE (d) = CON_PLAYING;
	  if (!GET_LEVEL (d->character))
	    {
	      do_start (d->character);
	      send_to_char (START_MESSG, d->character);
	      do_newbie (d->character);
	    }
	  look_at_room (d->character, 0);
	  if (has_mail (GET_IDNUM (d->character)))
	    send_to_char ("You have mail waiting.\r\n", d->character);
	  
	  GET_ARENASTAT(d->character) = ARENA_NOT;
	  OBSERVE_BY(d->character) = NULL;
	  OBSERVING(d->character) = NULL;
	  REMOVE_BIT(PLR_FLAGS(d->character), PLR_QUESTOR);
	  GET_QUESTGIVER(d->character) = NULL;
	  GET_COUNTDOWN(d->character) = 0;
	  GET_QUESTMOB(d->character) = 0;
	  
	    /* they don't need this if hero */
	 if (GET_LEVEL(d->character) >= LVL_HERO)
	 REMOVE_BIT(PRF_FLAGS(d->character), PRF_DISPEXP);
	      
	  if (load_result == 2)
	    {                   /* rented items lost */
	      send_to_char ("\r\n\007You could not afford your rent!\r\n"
			    "Your possesions have been donated to the kingdom of Anacreon!\r\n",
			    d->character);
	    }
	  d->has_prompt = 0;
 
      /* This is no longer needed, since everyone has trust now.
       *    if (GET_LEVEL(d->character) < LVL_IMMORT)
       *   GET_TRUST_LEVEL(d->character) = GET_LEVEL(d->character); */

	  read_aliases(d->character);
	  break;

	case '2':
	  if (d->character->player.description)
	    {
	      SEND_TO_Q ("Current description:\r\n", d);
	      SEND_TO_Q (d->character->player.description, d);
	      /*
	       * Don't free this now... so that the old description gets loaded
	       * as the current buffer in the editor.  Do setup the ABORT buffer
	       * here, however.
	       *
	       * free(d->character->player.description);
	       * d->character->player.description = NULL;
	       */
	      d->backstr = str_dup (d->character->player.description);
	    }
	  SEND_TO_Q ("Enter the new text you'd like others to see when they look at you.\r\n", d);
	  SEND_TO_Q ("(/s saves /h for help)\r\n", d);
	  d->str = &d->character->player.description;
	  d->max_str = EXDSCR_LENGTH;
	  STATE (d) = CON_EXDESC;
	  break;

	case '3':
	  page_string (d, background, 0);
	  STATE (d) = CON_RMOTD;
	  break;

	case '4':
	  page_string (d, news, 0);
	  STATE (d) = CON_RMOTD;
	  break;

	case '5':
	  page_string (d, policies, 0);
	  STATE (d) = CON_RMOTD;
	  break;

	case '6':
	  SEND_TO_Q ("\r\n", d);
	  do_who (d->character, "", 0, 0);
	  STATE (d) = CON_RMOTD;
	  SEND_TO_Q ("\r\n\n*** PRESS RETURN: ", d);
	  break;

	case '7':
	  SEND_TO_Q ("\r\nEnter your old password: ", d);
	  echo_off (d);
	  STATE (d) = CON_CHPWD_GETOLD;
	  break;

	case '8':
	  SEND_TO_Q ("\r\nEnter your password for verification: ", d);
	  echo_off (d);
	  STATE (d) = CON_DELCNF1;
	  break;

	default:
	  SEND_TO_Q ("\r\nThat's not a menu choice!\r\n", d);
	  SEND_TO_Q (MENU, d);
	  break;
	}

      break;

    case CON_CHPWD_GETOLD:
      if (strncmp (CRYPT (arg, GET_PASSWD (d->character)), GET_PASSWD (d->character), MAX_PWD_LENGTH))
	{
	  echo_on (d);
	  SEND_TO_Q ("\r\nIncorrect password.\r\n", d);
	  SEND_TO_Q (MENU, d);
	  STATE (d) = CON_MENU;
	  return;
	}
      else
	{
	  SEND_TO_Q ("\r\nEnter a new password: ", d);
	  STATE (d) = CON_CHPWD_GETNEW;
	  return;
	}
      break;

    case CON_DELCNF1:
      echo_on (d);
      if (strncmp (CRYPT (arg, GET_PASSWD (d->character)), GET_PASSWD (d->character), MAX_PWD_LENGTH))
	{
	  SEND_TO_Q ("\r\nIncorrect password.\r\n", d);
	  SEND_TO_Q (MENU, d);
	  STATE (d) = CON_MENU;
	}
      else
	{
	  SEND_TO_Q ("\r\nYOU ARE ABOUT TO DELETE THIS CHARACTER PERMANENTLY.\r\n"
		     "ARE YOU ABSOLUTELY SURE?\r\n\r\n"
		     "Please type \"yes\" to confirm: ", d);
	  STATE (d) = CON_DELCNF2;
	}
      break;

    case CON_DELCNF2:
      if (!strcmp (arg, "yes") || !strcmp (arg, "YES"))
	{
	  if (PLR_FLAGGED (d->character, PLR_FROZEN))
	    {
	      SEND_TO_Q ("You try to kill yourself, but the ice stops you.\r\n", d);
	      SEND_TO_Q ("Character not deleted.\r\n\r\n", d);
	      STATE (d) = CON_CLOSE;
	      return;
	    }
	  if (GET_LEVEL (d->character) < LVL_GRGOD)
	    SET_BIT (PLR_FLAGS (d->character), PLR_DELETED);
	  save_char (d->character, NOWHERE);
	  Crash_delete_file (GET_NAME (d->character));
	  sprintf (buf, "Character '%s' deleted!\r\n"
		   "Goodbye.\r\n", GET_NAME (d->character));
	  SEND_TO_Q (buf, d);
	  sprintf (buf, "%s (lev %d) has self-deleted.", GET_NAME (d->character),
		   GET_LEVEL (d->character));
	  mudlog (buf, NRM, LVL_GOD, TRUE);
	  STATE (d) = CON_CLOSE;
	  return;
	}
      else
	{
	  SEND_TO_Q ("\r\nCharacter not deleted.\r\n", d);
	  SEND_TO_Q (MENU, d);
	  STATE (d) = CON_MENU;
	}
      break;

    case CON_CLOSE:
      close_socket (d);
      break;

    default:
      log ("SYSERR: Nanny: illegal state of con'ness; closing connection");
      close_socket (d);
      break;
    }
}
