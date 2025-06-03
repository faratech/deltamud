/* ************************************************************************
   *   File: config.c                                      Part of CircleMUD *
   *  Usage: Configuration of various aspects of CircleMUD operation         *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#define __CONFIG_C__

#include "conf.h"
#include "sysdep.h"
#include "structs.h"

#define TRUE	1
#define YES	1
#define FALSE	0
#define NO	0

/*
 * Below are several constants which you can change to alter certain aspects
 * of the way CircleMUD acts.  Since this is a .c file, all you have to do
 * to change one of the constants (assuming you keep your object files around)
 * is change the constant in this file and type 'make'.  Make will recompile
 * this file and relink; you don't have to wait for the whole thing to
 * recompile as you do if you change a header file.
 *
 * I realize that it would be slightly more efficient to have lots of
 * #defines strewn about, so that, for example, the autowiz code isn't
 * compiled at all if you don't want to use autowiz.  However, the actual
 * code for the various options is quite small, as is the computational time
 * in checking the option you've selected at run-time, so I've decided the
 * convenience of having all your options in this one file outweighs the
 * efficency of doing it the other way.
 *
 */

/****************************************************************************/
/****************************************************************************/


/* GAME PLAY OPTIONS */

/*
 * pk_allowed sets the tone of the entire game.  If pk_allowed is set to
 * NO, then players will not be allowed to kill, summon, charm, or sleep
 * other players, as well as a variety of other "asshole player" protections.
 * However, if you decide you want to have an all-out knock-down drag-out
 * PK Mud, just set pk_allowed to YES - and anything goes.
 */
int pk_allowed = NO;

/* PK victim minimum level */
byte pk_victim_min = 10;

/* Jail room Number? */
int jail_num = 400;

/* Arena stuff */
int arena_zone = 48;              /* arena zone number? */
int arena_entrance = 4800;        /* arena entrance?    */
int arena_preproom = 4801;        /* arena prep room?   */
int arena_observeroom = 4899;     /* arena observatory? */
int arena_leave_penalty_mult = 100; /* Leave penalty multiplier */
int arena_flee_timeout = 3;       /* # tics for flee-recall timeout */
/* note: arena_combatant and arena_observer fees are now a multiplier 
 * based on player's level 
 */
int arena_combatant = 1000;      /* Arena combantant entrance fee? */
int arena_observer = 0;        /* Arena observer entrance fee?   */
struct char_data *defaultobserve; /* Default combatant to observe?  */
struct char_data *arenamaster;

/* Start room number for TOTAL newbies */
int newbie_room = 2200;

/* bail multiplier */
int bail_multiplier = 20000;

/* bail XP multiplier */
int xp_multiplier = 5;

/* is playerthieving allowed? */
int pt_allowed = YES;

/* If player thieving is allowed, then is the player markable as THIEF
   if caught? */
int pt_markable = NO;

/* weapon restrictions? */
int weaponrestrictions = YES;

/* www who system? (see code for further settings) */
int www_who = NO;

/* Auto System Reboot settings */
int autoreboot = 0;
int reboot_hr = 6;
int reboot_min = 30;
int warn_hr = 6;
int warn_min = 20;

/* mobdie password */
char *mobdie_pwd = "argh";
int mobdie_enabled = 0;

/* minimum level a player must be to shout/holler/gossip/auction */
int level_can_shout = 1;

/* number of movement points it costs to holler */
int holler_move_cost = 50;

/* exp change limits */
int max_exp_gain = 1000000000;	/* max gainable per kill */
int max_exp_loss = 15000000;	/* max losable per death */

/* number of tics (usually 75 seconds) before PC/NPC corpses decompose */
int max_npc_corpse_time = 5;
int max_pc_corpse_time = 10;

/* should items in death traps automatically be junked? */
int dts_are_dumps = YES;

int impboard=1200;

/* "okay" etc. */
char *OK = "&YOkay.&n\r\n";
char *NOPERSON = "&CNo-one by that name here.&n\r\n";
char *NOEFFECT = "&CNothing seems to happen.&n\r\n";

/****************************************************************************/
/* Address to bind do; NULL for all interfaces.*/

//const char *DFLT_IP="205.252.89.140";
const char *DFLT_IP="206.161.127.225";
//const char *DFLT_IP=NULL;

/* mySQL Host Database Information */
const char        *mySQL_host="localhost";
const unsigned int mySQL_port=4001;
const char        *mySQL_user="system-mud";
const char        *mySQL_pass="v5f9J8z0lm883jdks83jf45kj32l5hlh5k3j25k2jlj23h23";

/****************************************************************************/


/* RENT/CRASHSAVE OPTIONS */

/*
 * Should the MUD allow you to 'rent' for free?  (i.e. if you just quit,
 * your objects are saved at no cost, as in Merc-type MUDs.)
 */
int free_rent = NO;

/* maximum number of items players are allowed to rent */
int max_obj_save = 50;

/* receptionist's surcharge on top of item costs */
int min_rent_cost = 250;

/*
 * Should the game automatically save people?  (i.e., save player data
 * every 4 kills (on average), and Crash-save as defined below.
 */
int auto_save = YES;

/*
 * if auto_save (above) is yes, how often (in minutes) should the MUD
 * Crash-save people's objects?   Also, this number indicates how often
 * the MUD will Crash-save players' houses.
 */
int autosave_time = 5;

/* Lifetime of crashfiles and forced-rent (idlesave) files in days */
int crash_file_timeout = 10;

/* Lifetime of normal rent files in days */
int rent_file_timeout = 30;


/****************************************************************************/
/****************************************************************************/


/* ROOM NUMBERS */

/* virtual number of room that mortals should enter at */
/* long mortal_start_room = 3001; */

/* virtual number of room that immorts should enter at by default */
long immort_start_room = 1204;

/* virtual number of room that frozen players should enter at */
long frozen_start_room = 1202;

/*
 * virtual numbers of donation rooms.  note: you must change code in
 * do_drop of act.item.c if you change the number of non-NOWHERE
 * donation rooms.
 */
long donation_room_1 = 146;            /* Start Town 1 */
long donation_room_2 = NOWHERE;       /* unused - room for expansion */
long donation_room_3 = NOWHERE;	/* unused - room for expansion */


/****************************************************************************/
/****************************************************************************/


/* GAME OPERATION OPTIONS */

/*
 * This is the default port the game should run on if no port is given on
 * the command-line.  NOTE WELL: If you're using the 'autorun' script, the
 * port number there will override this setting.  Change the PORT= line in
 * instead of (or in addition to) changing this.
 */
int DFLT_PORT = 4000;

/* default directory to use as data directory */
char *DFLT_DIR = "lib";

/* What file to log messages to (ex: "log/syslog").  "" == stderr */
char *LOGFILE = "syslog";

/* maximum number of players allowed before game starts to turn people away */
int MAX_PLAYERS = 300;

/* maximum size of bug, typo and idea files in bytes (to prevent bombing) */
int max_filesize = 50000;

/* maximum number of password attempts before disconnection */
int max_bad_pws = 2;

/*
 * Some nameservers are very slow and cause the game to lag terribly every 
 * time someone logs in.  The lag is caused by the gethostbyaddr() function
 * which is responsible for resolving numeric IP addresses to alphabetic names.
 * Sometimes, nameservers can be so slow that the incredible lag caused by
 * gethostbyaddr() isn't worth the luxury of having names instead of numbers
 * for players' sitenames.
 *
 * If your nameserver is fast, set the variable below to NO.  If your
 * nameserver is slow, of it you would simply prefer to have numbers
 * instead of names for some other reason, set the variable to YES.
 *
 * You can experiment with the setting of nameserver_is_slow on-line using
 * the SLOWNS command from within the MUD.
 */

/* the nameserver isn't slow the MUD is :P */
int nameserver_is_slow = YES;

char *ANSI =
"[0;31;1mRED[31;0m [0;34;1mBLUE[34;0m [0;32;1mGREEN[32;0m\r\n"
"Is the above text shown in color? ";

char *MENU =
"\r\n"
"&GWelcome to the DeltaMUD Menu&n\r\n"
"&B------------------------------&n\r\n"
"&R[&n&C0&n&R]&n Exit from DeltaMUD.\r\n"
"&R[&n&C1&n&R]&n Enter the game.\r\n"
"&R[&n&C2&n&R]&n Enter description.\r\n"
"&R[&n&C3&n&R]&n Read the background story.\r\n"
"&R[&n&C4&n&R]&n Read the latest news.\r\n"
"&R[&n&C5&n&R]&n Read the game policy.\r\n"
"&R[&n&C6&n&R]&n See who is online.\r\n"
"&R[&n&C7&n&R]&n Change password.\r\n"
"&R[&n&C8&n&R]&n Delete this character.\r\n"
"&B------------------------------&n\r\n"
"\r\n"
"   Make your choice: ";



char *ASK_NAME =
"\r\nPlease enter a name&R:&n ";


char *WELC_MESSG =
"\r\n"
"Welcome to the ever changing world of Deltania..may your life here\r\n"
"be full of adventure and intrigue...\r\n"
"\r\n\r\n";

char *START_MESSG =
"\r\n"
"This is your new DeltaMUD character!  You can now earn &Ygold&n,\r\n"
"gain &Cexperience&n, find &Rweapons&n and &Mequipment&n, and much more.\r\n"
"\r\nThe first thing you should do is read the Newbie Guide. You do that\r\n"
"by typing 'read guide' (without the quotes, of course)\r\n"
"\r\n\r\n";
/****************************************************************************/
/****************************************************************************/


/* AUTOWIZ OPTIONS */

/* Should the game automatically create a new wizlist/immlist every time
   someone immorts, or is promoted to a higher (or lower) god level? */
int use_autowiz = YES;

/* If yes, what is the lowest level which should be on the wizlist?  (All
   immort levels below the level you specify will go on the immlist instead.) */
int min_wizlist_lev = LVL_IMMORT;
const long mortal_start_room[NUM_STARTROOMS + 1] =
{
  100,				/* Newbie loadroom element */
  100,				/* Itrius */
  100,			/*         Start Town 2 */
  100,                        /*   Start Town 3 */
};

const int training_pts[LVL_IMMORT] =
{
/* x0   x1   x2   x3   x4   x5   x6   x7   x8   x9  */
    0,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 0 - 9 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 10 - 19 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 20 - 29 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 30 - 39 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 40 - 49 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 50 - 59 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 60 - 69 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 70 - 79 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 80 - 89 */
    1,   0,   0,   0,   0,   0,   0,   0,   0,   0   /* 90 - 99 */
};

const int lvl_maxdmg_weapon[LVL_IMMORT] =
{
/* x0   x1   x2   x3   x4   x5   x6   x7   x8   x9  */
   15,  15,  15,  15,  15,  15,  15,  15,  15,  15,  /* 0 - 9 */
   20,  20,  20,  20,  20,  20,  20,  20,  20,  20,  /* 10 - 19 */
   25,  25,  25,  25,  25,  25,  25,  25,  25,  25,  /* 20 - 29 */
   30,  30,  30,  30,  30,  30,  30,  30,  30,  30,  /* 30 - 39 */
   35,  35,  35,  35,  35,  35,  35,  35,  35,  35,  /* 40 - 49 */
   45,  45,  45,  45,  45,  45,  45,  45,  45,  45,  /* 50 - 59 */
   50,  50,  50,  50,  50,  50,  50,  50,  50,  50,  /* 60 - 69 */
   60,  60,  60,  60,  60,  60,  60,  60,  60,  60,  /* 70 - 79 */
   75,  75,  75,  75,  75,  75,  75,  75,  75,  75,  /* 80 - 89 */
  100, 100, 100, 100, 100, 100, 100, 100, 100, 100,  /* 90 - 99 */
  100                                                /* 100 */
};

const int TEMPtraining_pts[LVL_IMPL] =
{
/* x0   x1   x2   x3   x4   x5   x6   x7   x8   x9  */
    0,   0,   0,   0,   0,   0,   0,   0,   0,   0,  /* 0 - 9 */
    1,   1,   1,   1,   1,   1,   1,   1,   1,   1,  /* 10 - 19 */
    2,   2,   2,   2,   2,   2,   2,   2,   2,   2,  /* 20 - 29 */
    3,   3,   3,   3,   3,   3,   3,   3,   3,   3,  /* 30 - 39 */
    4,   4,   4,   4,   4,   4,   4,   4,   4,   4,  /* 40 - 49 */
    5,   5,   5,   5,   5,   5,   5,   5,   5,   5,  /* 50 - 59 */
    6,   6,   6,   6,   6,   6,   6,   6,   6,   6,  /* 60 - 69 */
    7,   7,   7,   7,   7,   7,   7,   7,   7,   7,  /* 70 - 79 */
    8,   8,   8,   8,   8,   8,   8,   8,   8,   8,  /* 80 - 89 */
    9,   9,   9,   9,   9,   9,   9,   9,   9,   9,   /* 90 - 99 */
    100, 100, 100, 100, 100
};

