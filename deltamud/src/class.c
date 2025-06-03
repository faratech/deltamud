/*
************************************************************************
   *   File: class.c                                       Part of CircleMUD *
   *  Usage: Source file for class-specific code                             *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

/*
 * This file attempts to concentrate most of the code which must be changed
 * in order for new classes to be added.  If you're adding a new class,
 * you should go through this entire file from beginning to end and add
 * the appropriate new special cases for your new class.
 */


#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "buffer.h"
#include "db.h"
#include "utils.h"
#include "spells.h"
#include "interpreter.h"
#include "comm.h"
extern long r_mortal_start_room[NUM_STARTROOMS + 1];

/* Names first */

const char *class_abbrevs[] =
{
  "Mu",
  "Cl",
  "Th",
  "Wa",
  "Ar",
  "\n"
};


const char *pc_class_types[] =
{
  "Magic User",
  "Cleric",
  "Thief",
  "Warrior",
  "Artisan",
  "\n"
};


/* The menu for choosing a class in interpreter.c: */
const char *class_menu =
"\r\n"
"&YSelect a class&n&R:&n\r\n"
"  &c[&na&c]&n Cleric   - religious and holy people who use defensive magic.\r\n"
"  &c[&nb&c]&n Thief    - stealthy and dexterous rogues, considered cunning.\r\n"
"  &c[&nc&c]&n Warrior  - strong and powerful fighters, weapon masters.\r\n"
"  &c[&nd&c]&n Mage     - wise and intelligent, and use offensive spells.\r\n"
"  &c[&ne&c]&n Artisan  - well adjusted, excellent item makers and traders.\r\n";
/*
 * The code to interpret a class letter -- used in interpreter.c when a
 * new character is selecting a class and by 'set class' in act.wizard.c.
 */

int
parse_class (char arg)
{
  arg = LOWER (arg);

  switch (arg)
    {
    case 'd':
      return CLASS_MAGIC_USER;
      break;
    case 'a':
      return CLASS_CLERIC;
      break;
    case 'c':
      return CLASS_WARRIOR;
      break;
    case 'b':
      return CLASS_THIEF;
      break;
    case 'e':
      return CLASS_ARTISAN;
      break;
    default:
      return CLASS_UNDEFINED;
      break;
    }
}

const char *town_menu =
"\r\n"
"&YSelect your home&n&R:&n\r\n"
"  &c[&na&c]&n Bah      - capitol of the Kingdom of Anacreon\r\n"
"  &c[&nb&c]&n Bah      - an Elven town of magic &r(&Rexpert&r)&n\r\n"
"  &c[&nc&c]&n Bah      - a human town of mystery &r(&Rexpert&r)&n\r\n";

int
parse_town (char arg)
{
  arg = LOWER (arg);

  switch (arg)
    {
    case 'a':
      return 1;
      break;
    case 'b':
      return 2;
      break;
    case 'c':
      return 3;
      break;
    default:
      return TOWN_UNDEFINED;
      break;
    }
}
/*
 * bitvectors (i.e., powers of two) for each class, mainly for use in
 * do_who and do_users.  Add new classes at the end so that all classes
 * use sequential powers of two (1 << 0, 1 << 1, 1 << 2, 1 << 3, 1 << 4,
 * 1 << 5, etc.
 */

long
find_class_bitvector (char arg)
{
  arg = LOWER (arg);

  switch (arg)
    {
    case 'd':
      return (1 << 0);
      break;
    case 'a':
      return (1 << 1);
      break;
    case 'c':
      return (1 << 2);
      break;
    case 'b':
      return (1 << 3);
      break;
    case 'e':
      return (1 << 4);
      break;
    default:
      return 0;
      break;
    }
}


/*
 * These are definitions which control the guildmasters for each class.
 *
 * The first field (top line) controls the highest percentage skill level
 * a character of the class is allowed to attain in any skill.  (After
 * this level, attempts to practice will say "You are already learned in
 * this area."
 * 
 * The second line controls the maximum percent gain in learnedness a
 * character is allowed per practice -- in other words, if the random
 * die throw comes out higher than this number, the gain will only be
 * this number instead.
 *
 * The third line controls the minimu percent gain in learnedness a
 * character is allowed per practice -- in other words, if the random
 * die throw comes out below this number, the gain will be set up to
 * this number.
 * 
 * The fourth line simply sets whether the character knows 'spells'
 * or 'skills'.  This does not affect anything except the message given
 * to the character when trying to practice (i.e. "You know of the
 * following spells" vs. "You know of the following skills"
 */

#define SPELL	0
#define SKILL	1

/* #define LEARNED_LEVEL        0  % known which is considered "learned" */
/* #define MAX_PER_PRAC         1  max percent gain in skill per practice */
/* #define MIN_PER_PRAC         2  min percent gain in skill per practice */
/* #define PRAC_TYPE            3  should it say 'spell' or 'skill'?    */

int prac_params[4][NUM_CLASSES] =
{
  /* MAG        CLE     THE     WAR	ART */
  {95,		95,	85,	80, 	90},		/* learned level */
  {100,		100,	12,	12,	40},		/* max per prac */
  {25,		25,	0,	0,	10},		/* min per pac */
  {SPELL,	SPELL,	SKILL,	SKILL,	SKILL}		/* prac name */
};


/*
 * ...And the appropriate rooms for each guildmaster/guildguard; controls
 * which types of people the various guildguards let through.  i.e., the
 * first line shows that from room 3017, only MAGIC_USERS are allowed
 * to go south.
 */
int guild_info[][3] =
{

/* Midgaard */
  {CLASS_MAGIC_USER, 3017, SCMD_SOUTH},
  {CLASS_CLERIC, 3004, SCMD_NORTH},
  {CLASS_THIEF, 3027, SCMD_EAST},
  {CLASS_WARRIOR, 3021, SCMD_EAST},

/* Brass Dragon */
  {-999 /* all */ , 5065, SCMD_WEST},

/* this must go last -- add new guards above! */
  {-1, -1, -1}};



/* THAC0 for classes and levels.  (To Hit Armor Class 0) */

/* Quick and dirty I'm afraid, would have taken hours to work out a
   balanced Thac0 table for each class. Will write a formula later.
   --Raf 18/1/98 */

/*
 * Roll the 6 stats for a character... each stat is made of the sum of
 * the best 3 out of 4 rolls of a 6-sided die.  Each class then decides
 * which priority will be given for the best to worst stats.
 */

int stat_table[9][12] = {
/*		 Str	Int	Wis	Dex	Con	Cha */
/* Human    */	{6, 16,	6, 16,	6, 16,	6, 16,	6, 16,	6, 16},
/* Elf	    */	{4, 15,	8, 17,	3, 16,	7, 19,	6, 14,	8, 15},
/* Gnome    */	{6, 16,	7, 15,	5, 17,	5, 16,	8, 17,	5, 15},
/* Dwarf    */	{9, 18,	4, 14,	4, 16,	4, 15,  12, 18,	3, 15},
/* Troll    */	{12,19,	4, 15,	5, 15,	5, 16,	6, 18,	3, 15},
/* Goblin   */	{7, 17,	7, 16,	5, 15,	6, 18,	8, 17,	3, 13},
/* Drow	    */	{4, 15,	8, 17,	3, 16,	7, 19,	6, 14,	8, 15},
/* Orc	    */	{7, 18,	5, 17,	3, 15,	4, 15,  13, 19,	4, 12},
/* Minotaur */	{8, 18,	5, 15,	6, 15,	6, 17,	8, 19,	3, 12}
};

void roll_real_abils (struct char_data *ch) {
  int sa=0, ia=0, wa=0, da=0, ca=0;

  ch->real_abils.str_add = 0;

  switch (GET_CLASS (ch)) {
    case CLASS_MAGIC_USER:
      ia=number(2,3);
      wa=number(2,3);
      break;
    case CLASS_CLERIC:
      wa=number(2,3);
      ca=number(2,3);
      break;
    case CLASS_THIEF:
      da=number(2,3);
      ia=number(2,3);
      break;
    case CLASS_WARRIOR:
      sa=number(2,3);
      ca=number(2,3);
      break;
    case CLASS_ARTISAN:
      sa=number(2,3);
      ia=number(2,3);
      break;
  }
  ch->real_abils.str = MIN(number(GET_RACE_MIN(GET_RACE(ch), 1), GET_RACE_MAX(GET_RACE(ch), 1)) + sa, GET_RACE_MAX(GET_RACE(ch), 1));
  ch->real_abils.intel = MIN(number(GET_RACE_MIN(GET_RACE(ch), 2), GET_RACE_MAX(GET_RACE(ch), 2)) + ia, GET_RACE_MAX(GET_RACE(ch), 2));
  ch->real_abils.wis = MIN(number(GET_RACE_MIN(GET_RACE(ch), 3), GET_RACE_MAX(GET_RACE(ch), 3)) + wa, GET_RACE_MAX(GET_RACE(ch), 3));
  ch->real_abils.dex = MIN(number(GET_RACE_MIN(GET_RACE(ch), 4), GET_RACE_MAX(GET_RACE(ch), 4)) + da, GET_RACE_MAX(GET_RACE(ch), 4));
  ch->real_abils.con = MIN(number(GET_RACE_MIN(GET_RACE(ch), 5), GET_RACE_MAX(GET_RACE(ch), 5)) + ca, GET_RACE_MAX(GET_RACE(ch), 5));
  ch->real_abils.cha = MIN(number(GET_RACE_MIN(GET_RACE(ch), 6), GET_RACE_MAX(GET_RACE(ch), 6)), GET_RACE_MAX(GET_RACE(ch), 6));

  if (ch->real_abils.str == 18)
    ch->real_abils.str_add = number (0, 100);
  ch->aff_abils = ch->real_abils;
}

/* void roll_real_abils (struct char_data *ch)
{
  int i, j, k, temp;
  ubyte table[6];
  ubyte rolls[4];

  for (i = 0; i < 6; i++)
    table[i] = 0;

  for (i = 0; i < 6; i++)
    {

      for (j = 0; j < 4; j++)
	rolls[j] = number (1, 6);

      temp = rolls[0] + rolls[1] + rolls[2] + rolls[3] -
	MIN (rolls[0], MIN (rolls[1], MIN (rolls[2], rolls[3])));

      for (k = 0; k < 6; k++)
	if (table[k] < temp)
	  {
	    temp ^= table[k];
	    table[k] ^= temp;
	    temp ^= table[k];
	  }
    }

  ch->real_abils.str_add = 0;

  switch (GET_CLASS (ch))
    {
    case CLASS_MAGIC_USER:
      ch->real_abils.intel = table[0];
      ch->real_abils.wis = table[1];
      ch->real_abils.dex = table[2];
      ch->real_abils.str = table[3];
      ch->real_abils.con = table[4];
      ch->real_abils.cha = table[5];
      break;
    case CLASS_CLERIC:
      ch->real_abils.wis = table[0];
      ch->real_abils.intel = table[1];
      ch->real_abils.str = table[2];
      ch->real_abils.dex = table[3];
      ch->real_abils.con = table[4];
      ch->real_abils.cha = table[5];
      break;
    case CLASS_THIEF:
      ch->real_abils.dex = table[0];
      ch->real_abils.str = table[1];
      ch->real_abils.con = table[2];
      ch->real_abils.intel = table[3];
      ch->real_abils.wis = table[4];
      ch->real_abils.cha = table[5];
      break;
    case CLASS_WARRIOR:
      ch->real_abils.str = table[0];
      ch->real_abils.dex = table[1];
      ch->real_abils.con = table[2];
      ch->real_abils.wis = table[3];
      ch->real_abils.intel = table[4];
      ch->real_abils.cha = table[5];
      if (ch->real_abils.str == 18)
	ch->real_abils.str_add = number (0, 100);
      break;
    case CLASS_ARTISAN:
      ch->real_abils.con = table[0];
      ch->real_abils.wis = table[1];
      ch->real_abils.intel = table[2];
      ch->real_abils.cha = table[3];
      ch->real_abils.str = table[4];
      ch->real_abils.dex = table[5];
      break;
}
  ch->aff_abils = ch->real_abils;
  switch (GET_RACE (ch))
    {
    case RACE_HUMAN:
      break;
    case RACE_ELF:
      ch->real_abils.dex += 8;
      ch->real_abils.intel += 8;
      ch->real_abils.con -= 5;
      ch->real_abils.str -= 10;
      break;
    case RACE_GNOME:
      ch->real_abils.intel += 5;
      ch->real_abils.wis -= 5;
      break;
    case RACE_DWARF:
      ch->real_abils.con += 10;
      ch->real_abils.intel -= 4;
      ch->real_abils.cha -= 6;
      break;
    case RACE_TROLL:
      ch->real_abils.str += 8;
      ch->real_abils.intel -= 10;
      ch->real_abils.dex -= 5;
      break;
    case RACE_GOBLIN:
      ch->real_abils.str += 4;
      ch->real_abils.dex += 4;
      ch->real_abils.intel += 4;
      ch->real_abils.wis -= 12;
      break;
    case RACE_MINOTAUR:
      ch->real_abils.str += 5;
      ch->real_abils.con += 5;
      ch->real_abils.wis -= 8;
      ch->real_abils.intel -= 8;
      break;
    case RACE_ORC:
      ch->real_abils.str -= 2;
      ch->real_abils.con += 2;
      ch->real_abils.wis -= 5;
      ch->real_abils.intel += 2;
      break;
    case RACE_DROW:
      ch->real_abils.str += 1;
      ch->real_abils.wis += 1;
      ch->real_abils.intel += 1;
      ch->real_abils.con += 2;
      ch->real_abils.dex -= 8;      
      break;
    default:
      break;
    }
}
*/

/* Some initializations for characters, including initial skills */
void
do_start (struct char_data *ch)
{
  void advance_level (struct char_data *ch);

  GET_LEVEL (ch) = 1;
  GET_TRUST_LEVEL (ch) = 1;
  GET_EXP (ch) = 1;

  set_title (ch, NULL);
  /*  roll_real_abils(ch); *//* not needed anymore */
  ch->points.max_hit = 10;

  switch (GET_CLASS (ch))
    {

    case CLASS_MAGIC_USER:
      break;

    case CLASS_CLERIC:
      break;

    case CLASS_THIEF:
      SET_SKILL (ch, SKILL_SNEAK, 10);
      SET_SKILL (ch, SKILL_HIDE, 5);
      SET_SKILL (ch, SKILL_STEAL, 15);
      SET_SKILL (ch, SKILL_BACKSTAB, 10);
      SET_SKILL (ch, SKILL_PICK_LOCK, 10);
      SET_SKILL (ch, SKILL_TRACK, 10);
      SET_SKILL (ch, SKILL_SCAN, 1);
      break;

    case CLASS_WARRIOR:
      SET_SKILL (ch, SKILL_SCAN, 1);
      break;
 
    case CLASS_ARTISAN:
      break; 
    }

  advance_level (ch);

  GET_HIT (ch) = GET_MAX_HIT (ch);
  GET_MANA (ch) = GET_MAX_MANA (ch);
  GET_MOVE (ch) = GET_MAX_MOVE (ch);

  GET_COND (ch, THIRST) = 24;
  GET_COND (ch, FULL) = 24;
  GET_COND (ch, DRUNK) = 0;

  ch->player.time.played = 0;
  ch->player.time.logon = time (0);
}



/*
 * This function controls the change to maxmove, maxmana, and maxhp for
 * each class every time they gain a level.
 */
void
advance_level (struct char_data *ch)
{
  int add_hp = 0, add_mana = 0, add_move = 0, add_practices = 0, i;

  extern struct wis_app_type wis_app[];
  extern struct con_app_type con_app[];
  extern int training_pts[LVL_IMMORT];

  add_hp = con_app[GET_CON (ch)].hitp;

  switch (GET_CLASS (ch))
    {

    case CLASS_MAGIC_USER:
      add_hp += number (3, 8);
      add_mana = number (GET_LEVEL (ch), (int) (1.5 * GET_LEVEL (ch)));
      add_mana = MIN (add_mana, 10);
      add_move = number (0, 2);
      break;

    case CLASS_CLERIC:
      add_hp += number (5, 10);
      add_mana = number (GET_LEVEL (ch), (int) (1.5 * GET_LEVEL (ch)));
      add_mana = MIN (add_mana, 10);
      add_move = number (0, 2);
      break;

    case CLASS_THIEF:
      add_hp += number (7, 13);
      add_mana = 0;
      add_move = number (1, 3);
      break;

    case CLASS_ARTISAN:
      add_hp += number (8, 14);
      add_mana = number (GET_LEVEL (ch), (int) (1.5 * GET_LEVEL (ch)));
      add_mana = MIN (add_mana, 10);
      add_move = number (3, 6);
      break;

    case CLASS_WARRIOR:
      add_hp += number (10, 15);
      add_mana = 0;
      add_move = number (1, 3);
      break;
    }

  ch->points.max_hit += MAX (1, add_hp);
  ch->points.max_move += MAX (1, add_move);

  if (GET_LEVEL (ch) > 1)
    ch->points.max_mana += add_mana;

  if (GET_CLASS (ch) == CLASS_MAGIC_USER || GET_CLASS (ch) == CLASS_CLERIC)
    add_practices = MAX (2, wis_app[GET_WIS (ch)].bonus);
  else
    add_practices = MIN (2, MAX (1, wis_app[GET_WIS (ch)].bonus));

  GET_PRACTICES (ch) += add_practices;

  if (GET_LEVEL (ch) >= LVL_IMMORT)
    {
      for (i = 0; i < 3; i++)
	GET_COND (ch, i) = (char) -100;
      SET_BIT (PRF_FLAGS (ch), PRF_HOLYLIGHT);
    }

  if (GET_LEVEL (ch) > 1)
    {
      ch->player_specials->saved.newbie = 0;
    }

  GET_TRUST_LEVEL(ch) = GET_LEVEL(ch);
  GET_TRAINING(ch) += training_pts[(int)GET_LEVEL(ch)];


  sprintf(buf, "You've gained %d hp, %d mp, %d mv, %d practices, "
	  "and %d training sessions.\r\n", 
	  MAX (1, add_hp), add_mana, MAX (1, add_move),
	  add_practices, training_pts[(int)GET_LEVEL(ch)]);
  send_to_char(buf, ch);

  save_char (ch, NOWHERE);

  sprintf (buf, "%s advanced to level %d", GET_NAME (ch), GET_LEVEL (ch));
  mudlog (buf, BRF, MAX (LVL_IMMORT, GET_INVIS_LEV (ch)), TRUE);
}


/*
 * This simply calculates the backstab multiplier based on a character's
 * level.  This used to be an array, but was changed to be a function so
 * that it would be easier to add more levels to your MUD.  This doesn't
 * really create a big performance hit because it's not used very often.
 */
int
backstab_mult (int level)
{
  if (level <= 0)
    return 1;			/* level 0 */
  else if (level <= 7)
    return 2;			/* level 1 - 7 */
  else if (level <= 13)
    return 3;			/* level 8 - 13 */
  else if (level <= 20)
    return 4;			/* level 14 - 20 */
  else if (level <= 28)
    return 5;			/* level 21 - 28 */
  else if (level <= 36)
    return 6;
  else if (level <= 44)
    return 7;
  else if (level <= 52)
    return 8;
  else if (level <= 60)
    return 9;
  else if (level <= 68)
    return 10;
  else if (level <= 76)
    return 11;
  else if (level <= 84)
    return 12;
  else if (level < LVL_IMMORT)
    return 13;			/* all remaining mortal levels */
  else
    return 20;			/* immortals */
}

/*
 * invalid_class is used by handler.c to determine if a piece of equipment is
 * usable by a particular class, based on the ITEM_ANTI_{class} bitvectors.
 */

int
invalid_class (struct char_data *ch, struct obj_data *obj)
{
  if ((IS_OBJ_STAT (obj, ITEM_ANTI_MAGIC_USER) && IS_MAGIC_USER (ch)) ||
      (IS_OBJ_STAT (obj, ITEM_ANTI_CLERIC) && IS_CLERIC (ch)) ||
      (IS_OBJ_STAT (obj, ITEM_ANTI_WARRIOR) && IS_WARRIOR (ch)) ||
      (IS_OBJ_STAT (obj, ITEM_ANTI_THIEF) && IS_THIEF (ch)) ||
      (IS_OBJ_STAT (obj, ITEM_ANTI_ARTISAN) && IS_ARTISAN (ch))
    return 1;
  else
    return 0;
}

/*
 * SPELLS AND SKILLS.  This area defines which spells are assigned to
 * which classes, and the minimum level the character must be to use
 * the spell or skill.
 */
void
init_spell_levels (void)
{
  /* MAGES */
  spell_level (SKILL_MOUNT, CLASS_MAGIC_USER, 2);
  spell_level (SKILL_RIDING, CLASS_MAGIC_USER, 3);
  spell_level (SPELL_DETECT_INVIS, CLASS_MAGIC_USER, 4);
  spell_level (SPELL_DETECT_MAGIC, CLASS_MAGIC_USER, 5);
  spell_level (SPELL_INFRAVISION, CLASS_MAGIC_USER, 8);
  spell_level (SPELL_INVISIBLE, CLASS_MAGIC_USER, 9);
  spell_level (SPELL_ARMOR, CLASS_MAGIC_USER, 10);
  spell_level (SPELL_LOCATE_OBJECT, CLASS_MAGIC_USER, 13);
  spell_level (SPELL_STRENGTH, CLASS_MAGIC_USER, 15);
  spell_level (SKILL_MEDITATE, CLASS_MAGIC_USER, 17);
  spell_level (SPELL_SLEEP, CLASS_MAGIC_USER, 19);
  spell_level (SKILL_TAME, CLASS_MAGIC_USER, 21);
  spell_level (SPELL_BLINDNESS, CLASS_MAGIC_USER, 23);
  spell_level (SPELL_DETECT_POISON, CLASS_MAGIC_USER, 24);
  spell_level (SPELL_CURSE, CLASS_MAGIC_USER, 27);
  spell_level (SPELL_POISON, CLASS_MAGIC_USER, 29);
  spell_level (SPELL_CHARM, CLASS_MAGIC_USER, 33);
  spell_level (SPELL_SENSE_LIFE, CLASS_MAGIC_USER, 34);
  spell_level (SPELL_ENCHANT_WEAPON, CLASS_MAGIC_USER, 35);
  spell_level (SPELL_STONE_SKIN, CLASS_MAGIC_USER, 37);
  spell_level (SPELL_FEAR, CLASS_MAGIC_USER, 29);
  spell_level (SPELL_CLONE, CLASS_MAGIC_USER, 31);
  spell_level (SPELL_RECHARGE, CLASS_MAGIC_USER, 33);
  spell_level (SPELL_PORTAL, CLASS_MAGIC_USER, 35);
  spell_level (SPELL_GROUP_STONE_SKIN, CLASS_MAGIC_USER, 40);
  spell_level (SKILL_DODGE, CLASS_MAGIC_USER, 42);
  spell_level (SPELL_AUTUS, CLASS_MAGIC_USER, 75);
  spell_level (SPELL_CONVERGENCE, CLASS_MAGIC_USER, 80);
  spell_level (SPELL_RESIST_PORTAL, CLASS_MAGIC_USER, 85);
  spell_level (SPELL_HOME, CLASS_MAGIC_USER, 90);
  spell_level (SPELL_WATERWALK, CLASS_MAGIC_USER, 95);
  spell_level (SPELL_REDIRECT_CHARGE, CLASS_MAGIC_USER, 60);

  /* CLERICS */
  spell_level (SPELL_CURE_LIGHT, CLASS_CLERIC, 1);
  spell_level (SPELL_ARMOR, CLASS_CLERIC, 3);
  spell_level (SKILL_MOUNT, CLASS_CLERIC, 5);
  spell_level (SKILL_RIDING, CLASS_CLERIC, 7);
  spell_level (SPELL_CREATE_FOOD, CLASS_CLERIC, 9);
  spell_level (SPELL_CREATE_WATER, CLASS_CLERIC, 11);
  spell_level (SPELL_DETECT_POISON, CLASS_CLERIC, 13);
  spell_level (SPELL_DETECT_ALIGN, CLASS_CLERIC, 15);
  spell_level (SPELL_CURE_BLIND, CLASS_CLERIC, 17);
  spell_level (SPELL_BLESS, CLASS_CLERIC, 19);
  spell_level (SPELL_SENSE_LIFE, CLASS_CLERIC, 21);
  spell_level (SPELL_DETECT_INVIS, CLASS_CLERIC, 23);
  spell_level (SKILL_MEDITATE, CLASS_CLERIC, 25);
  spell_level (SPELL_BLINDNESS, CLASS_CLERIC, 27);
  spell_level (SPELL_INFRAVISION, CLASS_CLERIC, 29);
  spell_level (SKILL_TAME, CLASS_CLERIC, 33);
  spell_level (SPELL_POISON, CLASS_CLERIC, 35);
  spell_level (SPELL_GROUP_ARMOR, CLASS_CLERIC, 37);
  spell_level (SPELL_CURE_CRITIC, CLASS_CLERIC, 39);
  spell_level (SPELL_SUMMON, CLASS_CLERIC, 41);
  spell_level (SPELL_REMOVE_POISON, CLASS_CLERIC, 43);
  spell_level (SPELL_WORD_OF_RECALL, CLASS_CLERIC, 45);
  spell_level (SPELL_EARTHQUAKE, CLASS_CLERIC, 47);
  spell_level (SPELL_SANCTUARY, CLASS_CLERIC, 53);
  spell_level (SPELL_HEAL, CLASS_CLERIC, 57);
  spell_level (SPELL_CONTROL_WEATHER, CLASS_CLERIC, 59);
  spell_level (SPELL_GROUP_HEAL, CLASS_CLERIC, 63);
  spell_level (SKILL_DODGE, CLASS_CLERIC, 65);
  spell_level (SPELL_REMOVE_CURSE, CLASS_CLERIC, 67);
  spell_level (SPELL_ANIMATE_DEAD, CLASS_CLERIC, 69);
  spell_level (SPELL_STONE_SKIN, CLASS_CLERIC, 71);
  spell_level (SPELL_GROUP_STONE_SKIN, CLASS_CLERIC, 73);
  spell_level (SPELL_PORTAL, CLASS_CLERIC, 77);
  spell_level (SPELL_RECHARGE, CLASS_CLERIC, 79);
  spell_level (SPELL_LOCATE_TARGET, CLASS_CLERIC, 85);
  spell_level (SPELL_REGEN_MANA, CLASS_CLERIC, 91);
  spell_level (SPELL_WATERWALK, CLASS_CLERIC, 93);
  spell_level (SPELL_WORD_OF_RETREAT, CLASS_CLERIC, 95);

  /* THIEVES */
  spell_level (SKILL_SNEAK, CLASS_THIEF, 1);
  spell_level (SKILL_MOUNT, CLASS_THIEF, 3);
  spell_level (SKILL_RIDING, CLASS_THIEF, 5);
  spell_level (SKILL_PICK_LOCK, CLASS_THIEF, 7);
  spell_level (SKILL_BACKSTAB, CLASS_THIEF, 9);
  spell_level (SKILL_STEAL, CLASS_THIEF, 11);
  spell_level (SKILL_HIDE, CLASS_THIEF, 13);
  spell_level (SKILL_TRACK, CLASS_THIEF, 15);
  spell_level (SKILL_TAME, CLASS_THIEF, 17);
  spell_level (SKILL_SECOND_ATTACK, CLASS_THIEF, 19);
  spell_level (SKILL_LISTEN, CLASS_THIEF, 21);
  spell_level (SKILL_FORAGE, CLASS_THIEF, 23);
  spell_level (SKILL_SCAN, CLASS_THIEF, 25);
  spell_level (SKILL_CAMOUFLAGE, CLASS_THIEF, 27);
  spell_level (SKILL_BLANKET, CLASS_THIEF, 29);
  spell_level (SKILL_CIRCLE, CLASS_THIEF, 31);
  spell_level (SKILL_TRIP, CLASS_THIEF, 33);
  spell_level (SKILL_DISARM, CLASS_THIEF, 35);
  spell_level (SKILL_TARGET, CLASS_THIEF, 37);
  spell_level (SKILL_AVOID, CLASS_THIEF, 39);

  /* WARRIORS */
  spell_level (SKILL_KICK, CLASS_WARRIOR, 1);
  spell_level (SKILL_MOUNT, CLASS_WARRIOR, 3);
  spell_level (SKILL_RIDING, CLASS_WARRIOR, 5);
  spell_level (SKILL_RESCUE, CLASS_WARRIOR, 7);
  spell_level (SKILL_TAME, CLASS_WARRIOR, 9);
  spell_level (SKILL_TRACK, CLASS_WARRIOR, 11);
  spell_level (SKILL_SECOND_ATTACK, CLASS_WARRIOR, 13);
  spell_level (SKILL_BASH, CLASS_WARRIOR, 15);
  spell_level (SKILL_FORAGE, CLASS_WARRIOR, 17);
  spell_level (SKILL_SCAN, CLASS_WARRIOR, 19);
  spell_level (SKILL_ADRENALINE, CLASS_WARRIOR, 20);
  spell_level (SKILL_THIRD_ATTACK, CLASS_WARRIOR, 21);
  spell_level (SKILL_RIPOSTE, CLASS_WARRIOR, 23);
  spell_level (SKILL_SPEED, CLASS_WARRIOR, 25);
  spell_level (SKILL_BERSERK, CLASS_WARRIOR, 27);
  spell_level (SKILL_RAM_DOOR, CLASS_WARRIOR, 29);
  spell_level (SKILL_PARRY, CLASS_WARRIOR, 31);
  spell_level (SKILL_CHAIN_FOOTING, CLASS_WARRIOR, 50);
  spell_level (SKILL_BLOODLUST, CLASS_WARRIOR, 60);
  spell_level (SKILL_CARNALRAGE, CLASS_WARRIOR, 75);

 /* ARTISAN */

  spell_level(SKILL_MOUNT, CLASS_ARTISAN, 1);
  spell_level(SKILL_RIDING, CLASS_ARTISAN, 3);
  spell_level(SKILL_SCRIBE, CLASS_ARTISAN, 5);
  spell_level(SKILL_TAME, CLASS_ARTISAN, 7);
  spell_level(SKILL_BREW, CLASS_ARTISAN, 11); 
  spell_level(SKILL_REPAIR, CLASS_ARTISAN, 21);
  spell_level(SKILL_TAN, CLASS_ARTISAN, 25);
  spell_level(SKILL_FILLET, CLASS_ARTISAN, 31);
  spell_level(SKILL_CARVE, CLASS_ARTISAN, 33);
  spell_level(SKILL_FORGE, CLASS_ARTISAN, 35);
  spell_level(SKILL_DODGE, CLASS_ARTISAN, 37);
}

int train_params[6][NUM_CLASSES] = {
  /* MAG    CLE   THE   WAR  ART */
  {  14,    15,   16,   MAX_PLAYER_STAT,   16  },   /* Strength */
  {  14,    15,   16,   MAX_PLAYER_STAT,   17  },   /* Constitution */
  {  MAX_PLAYER_STAT,    MAX_PLAYER_STAT,   14,   15,   16  },   /* Wisdom */
  {  MAX_PLAYER_STAT,    17,   17,   15,   16  },   /* Intelligence */
  {  16,    14,   MAX_PLAYER_STAT,   17,   15  },   /* Dexterity */
  {  16,    17,   MAX_PLAYER_STAT,   15,   MAX_PLAYER_STAT  }    /* Charisma */
};
