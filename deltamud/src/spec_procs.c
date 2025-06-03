/* ************************************************************************
   *   File: spec_procs.c                                  Part of CircleMUD *
   *  Usage: implementation of special procedures for mobiles/objects/rooms  *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#include "conf.h"
#include "sysdep.h"


#include "structs.h"
#include "utils.h"
#include "comm.h"
#include "interpreter.h"
#include "handler.h"
#include "db.h"
#include "spells.h"
#include "dg_scripts.h"
#include "maputils.h"

/*   external vars  */
extern int arena_zone;
extern int arena_preproom;
extern int arena_observeroom;
extern int arena_combatant;
extern int arena_observer;
extern struct char_data *arenamaster;
extern struct char_data *defaultobserve;
extern struct room_data *world;
extern struct char_data *character_list;
extern struct descriptor_data *descriptor_list;
extern struct index_data *mob_index;
extern struct index_data *obj_index;
extern struct time_info_data time_info;
extern struct command_info cmd_info[];

/* extern functions */
extern int has_boat(struct char_data *ch);
void write_aliases(struct char_data *ch);
void add_follower (struct char_data *ch, struct
char_data *leader);
void deobserve(struct char_data *);
void clearobservers(struct char_data*);
void linkobserve(struct char_data *who, struct char_data *to);
void bup_affects(struct char_data *ch);
void restore_bup_affects(struct char_data *ch);
char* numdisplay(int);
ACMD(do_tell);
ACMD(do_action);
ACMD (do_gen_comm);
void Crash_rentsave (struct char_data *ch, int cost);

struct social_type
  {
    char *cmd;
    int next_line;
  };


/* ********************************************************************
   *  Special procedures for mobiles                                     *
   ******************************************************************** */

int spell_sort_info[MAX_SKILLS + 1];

extern char *spells[];

void 
sort_spells (void)
{
  int a, b, tmp;

  /* initialize array */
  for (a = 1; a < MAX_SKILLS; a++)
    spell_sort_info[a] = a;

  /* Sort.  'a' starts at 1, not 0, to remove 'RESERVED' */
  for (a = 1; a < MAX_SKILLS - 1; a++)
    for (b = a + 1; b < MAX_SKILLS; b++)
      if (strcmp (spells[spell_sort_info[a]], spells[spell_sort_info[b]]) > 0)
	{
	  tmp = spell_sort_info[a];
	  spell_sort_info[a] = spell_sort_info[b];
	  spell_sort_info[b] = tmp;
	}
}


char *
how_good (int percent)
{
  static char buf[256];

  if (percent == 0)
    strcpy (buf, " (not learned)");
  else if (percent <= 10)
    strcpy (buf, " (awful)");
  else if (percent <= 20)
    strcpy (buf, " (bad)");
  else if (percent <= 40)
    strcpy (buf, " (poor)");
  else if (percent <= 55)
    strcpy (buf, " (average)");
  else if (percent <= 70)
    strcpy (buf, " (fair)");
  else if (percent <= 80)
    strcpy (buf, " (good)");
  else if (percent <= 85)
    strcpy (buf, " (very good)");
  else
    strcpy (buf, " (superb)");
  sprintf(buf+strlen(buf), " %d", percent);
  return (buf);
}

char *prac_types[] =
{
  "spell",
  "skill"
};

#define LEARNED_LEVEL	0	/* % known which is considered "learned" */
#define MAX_PER_PRAC	1	/* max percent gain in skill per practice */
#define MIN_PER_PRAC	2	/* min percent gain in skill per practice */
#define PRAC_TYPE	3	/* should it say 'spell' or 'skill'?       */

/* actual prac_params are in class.c */
extern int prac_params[4][NUM_CLASSES];

#define LEARNED(ch) (prac_params[LEARNED_LEVEL][(int)GET_CLASS(ch)])
#define MINGAIN(ch) (prac_params[MIN_PER_PRAC][(int)GET_CLASS(ch)])
#define MAXGAIN(ch) (prac_params[MAX_PER_PRAC][(int)GET_CLASS(ch)])
#define SPLSKL(ch) (prac_types[prac_params[PRAC_TYPE][(int)GET_CLASS(ch)]])

void 
list_skills (struct char_data *ch)
{
  extern char *spells[];
  extern struct spell_info_type spell_info[];
  int i, sortpos;

  if (!GET_PRACTICES (ch))
    strcpy (buf, "You have no practice sessions remaining.\r\n");
  else
    sprintf (buf, "You have %d practice session%s remaining.\r\n",
	     GET_PRACTICES (ch), (GET_PRACTICES (ch) == 1 ? "" : "s"));

  sprintf (buf, "%sYou know of the following %ss:\r\n", buf, SPLSKL (ch));

  strcpy (buf2, buf);

  for (sortpos = 1; sortpos < MAX_SKILLS; sortpos++)
    {
      i = spell_sort_info[sortpos];
      if (strlen (buf2) >= MAX_STRING_LENGTH - 32)
	{
	  strcat (buf2, "**OVERFLOW**\r\n");
	  break;
	}
      if (GET_LEVEL (ch) >= spell_info[i].min_level[(int) GET_CLASS (ch)])
	{
	  sprintf (buf, "%-20s %s\r\n", spells[i], how_good (GET_SKILL (ch, i)));
	  strcat (buf2, buf);
	}
    }

  page_string (ch->desc, buf2, 1);
}


SPECIAL (guild)
{
  int skill_num, percent;

  extern struct spell_info_type spell_info[];
  extern struct int_app_type int_app[];

  if (IS_NPC (ch) || !CMD_IS ("practice"))
    return 0;

  skip_spaces (&argument);

  if (!*argument)
    {
      list_skills (ch);
      return 1;
    }
  if (GET_PRACTICES (ch) <= 0)
    {
      send_to_char ("You do not seem to be able to practice now.\r\n", ch);
      return 1;
    }

  skill_num = find_skill_num (argument);

  if (skill_num < 1 ||
      GET_LEVEL (ch) < spell_info[skill_num].min_level[(int) GET_CLASS (ch)])
    {
      sprintf (buf, "You do not know of that %s.\r\n", SPLSKL (ch));
      send_to_char (buf, ch);
      return 1;
    }
  if (GET_SKILL (ch, skill_num) >= LEARNED (ch))
    {
      send_to_char ("You are already learned in that area.\r\n", ch);
      return 1;
    }
  send_to_char ("You practice for a while...\r\n", ch);
  GET_PRACTICES (ch)--;

  percent = GET_SKILL (ch, skill_num);
  percent += MIN (MAXGAIN (ch), MAX (MINGAIN (ch), int_app[GET_INT (ch)].learn));

  SET_SKILL (ch, skill_num, MIN (LEARNED (ch), percent));

  if (GET_SKILL (ch, skill_num) >= LEARNED (ch))
    send_to_char ("You are now learned in that area.\r\n", ch);

  return 1;
}



SPECIAL (dump)
{
  struct obj_data *k;
  int value = 0;

  ACMD (do_drop);
  char *fname (char *namelist);

  for (k = world[ch->in_room].contents; k; k = world[ch->in_room].contents)
    {
      act ("$p vanishes in a puff of smoke!", FALSE, 0, k, 0, TO_ROOM);
      extract_obj (k);
    }

  if (!CMD_IS ("drop"))
    return 0;

  do_drop (ch, argument, cmd, 0);

  for (k = world[ch->in_room].contents; k; k = world[ch->in_room].contents)
    {
      act ("$p vanishes in a puff of smoke!", FALSE, 0, k, 0, TO_ROOM);
      value += MAX (1, MIN (50, GET_OBJ_COST (k) / 10));
      extract_obj (k);
    }

  if (value)
    {
       if (GET_LEVEL(ch) < LVL_IMMORT) {
      act ("You are awarded for being a good citizen.", FALSE, ch, 0, 0, TO_CHAR);
      act ("$n has been awarded for being a good citizen.", TRUE, ch, 0, 0, TO_ROOM);

      if (GET_LEVEL (ch) < 3)
	gain_exp (ch, value);
      else
	GET_GOLD (ch) += value;
      }
    }
  return 1;
}


SPECIAL (mayor)
{
  ACMD (do_gen_door);

  static char open_path[] =
  "W3a3003b33000c111d0d111Oe333333Oe22c222112212111a1S.";

  static char close_path[] =
  "W3a3003b33000c111d0d111CE333333CE22c222112212111a1S.";

  static char *path;
  static int index;
  static bool move = FALSE;

  if (!move)
    {
      if (time_info.hours == 6)
	{
	  move = TRUE;
	  path = open_path;
	  index = 0;
	}
      else if (time_info.hours == 20)
	{
	  move = TRUE;
	  path = close_path;
	  index = 0;
	}
    }
  if (cmd || !move || (GET_POS (ch) < POS_SLEEPING) ||
      (GET_POS (ch) == POS_FIGHTING))
    return FALSE;

  switch (path[index])
    {
    case '0':
    case '1':
    case '2':
    case '3':
      perform_move (ch, path[index] - '0', 1);
      break;

    case 'W':
      GET_POS (ch) = POS_STANDING;
      act ("$n awakens and groans loudly.", FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'S':
      GET_POS (ch) = POS_SLEEPING;
      act ("$n lies down and instantly falls asleep.", FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'a':
      act ("$n says 'Hello, Honey!'", FALSE, ch, 0, 0, TO_ROOM);
      act ("$n smirks.", FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'b':
      act ("$n says 'What a view!  I must get something done about that dump!'",
	   FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'c':
      act ("$n says 'Vandals!  Youngsters nowadays have no respect for anything!'",
	   FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'd':
      act ("$n says 'Good day, citizens!'", FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'e':
      act ("$n says 'I hereby declare the markets open!'", FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'E':
      act ("$n says 'I hereby declare Anacreon closed!'", FALSE, ch, 0, 0, TO_ROOM);
      break;

    case 'O':
      do_gen_door (ch, "gate", 0, SCMD_UNLOCK);
      do_gen_door (ch, "gate", 0, SCMD_OPEN);
      break;

    case 'C':
      do_gen_door (ch, "gate", 0, SCMD_CLOSE);
      do_gen_door (ch, "gate", 0, SCMD_LOCK);
      break;

    case '.':
      move = FALSE;
      break;

    }

  index++;
  return FALSE;
}


/* ********************************************************************
   *  General special procedures for mobiles                             *
   ******************************************************************** */


void 
npc_steal (struct char_data *ch, struct char_data *victim)
{
  int gold;

  if (IS_NPC (victim))
    return;
  if (GET_LEVEL (victim) >= LVL_IMMORT)
    return;

  if (AWAKE (victim) && (number (0, GET_LEVEL (ch)) == 0))
    {
      act ("You discover that $n has $s hands in your wallet.", FALSE, ch, 0, victim, TO_VICT);
      act ("$n tries to steal gold from $N.", TRUE, ch, 0, victim, TO_NOTVICT);
    }
  else
    {
      /* Steal some gold coins */
      gold = (int) ((GET_GOLD (victim) * number (1, 10)) / 100);
      if (gold > 0)
	{
	  GET_GOLD (ch) += gold;
	  GET_GOLD (victim) -= gold;
	}
    }
}


SPECIAL (snake)
{
  if (cmd)
    return FALSE;

  if (GET_POS (ch) != POS_FIGHTING)
    return FALSE;

  if (FIGHTING (ch) && (FIGHTING (ch)->in_room == ch->in_room) &&
      (number (0, 42 - GET_LEVEL (ch)) == 0))
    {
      act ("$n bites $N!", 1, ch, 0, FIGHTING (ch), TO_NOTVICT);
      act ("$n bites you!", 1, ch, 0, FIGHTING (ch), TO_VICT);
      call_magic (ch, FIGHTING (ch), 0, SPELL_POISON, GET_LEVEL (ch));
      return TRUE;
    }
  return FALSE;
}


SPECIAL (thief)
{
  struct char_data *cons;

  if (cmd)
    return FALSE;

  if (GET_POS (ch) != POS_STANDING)
    return FALSE;

  for (cons = world[ch->in_room].people; cons; cons = cons->next_in_room)
    if (!IS_NPC (cons) && (GET_LEVEL (cons) < LVL_IMMORT) && (!number (0, 4)))
      {
	npc_steal (ch, cons);
	return TRUE;
      }
  return FALSE;
}


SPECIAL (magic_user)
{
  struct char_data *vict;

  if (cmd || GET_POS (ch) != POS_FIGHTING)
    return FALSE;

  /* pseudo-randomly choose someone in the room who is fighting me */
  for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
    if (FIGHTING (vict) == ch && !number (0, 4))
      break;

  /* if I didn't pick any of those, then just slam the guy I'm fighting */
  if (vict == NULL)
    vict = FIGHTING (ch);

  if ((GET_LEVEL (ch) > 13) && (number (0, 10) == 0))
    cast_spell (ch, vict, NULL, SPELL_SLEEP);

  if ((GET_LEVEL (ch) > 7) && (number (0, 8) == 0))
    cast_spell (ch, vict, NULL, SPELL_BLINDNESS);

  if (number (0, 4))
    return TRUE;

  if ( (GET_HIT(ch)/GET_MAX_HIT(ch) > 0.05)
       && (GET_HIT(ch)/GET_MAX_HIT(ch) < 0.5)){
    if (number(1,50) > (number(1,150)-GET_LEVEL(ch))){
      cast_spell(ch, ch, NULL, SPELL_HEAL);
      return TRUE;
    }
  }

  switch (GET_LEVEL (ch))
    {
    case 1:
    case 2:
    case 3:
    case 4:
    case 5:
    case 6:
    case 7:
    case 8:
    case 9:
    case 10:
    case 11:
    case 12:
    case 13:
    case 14:
    case 15:
    case 16:
    case 17:
    case 18:
    case 19:
    case 20:
    case 21:
    case 22:
    case 23:
    case 24:
    case 25:
    case 26:
    case 27:
    case 28:
    case 29:
    case 30:
    case 31:
    case 32:
    case 33:
    case 34:
    case 35:
    case 36:
    case 37:
    case 38:
    case 39:
    case 40:
    case 41:
    case 42:
    case 43:
    case 44:
    case 45:
    case 46:
    case 47:
    case 48:
    case 49:
    case 50:
    case 51:
    case 52:
    case 53:
    case 54:
    case 55:
    case 56:
    case 57:
    case 58:
    case 59:
    case 60:
    case 61:
    case 62:
    case 63:
    case 64:
    case 65:
    case 66:
    case 67:
    case 68:
    case 69:
    case 70:
    case 71:
    case 72:
    case 73:
    case 74:
    case 75:
    case 76:
    case 77:
    case 78:
    case 79:
    case 80:
    case 81:
    case 82:
    case 83:
    case 84:
    case 85:
    case 86:
    case 87:
    case 88:
    case 89:
    case 90:
    case 91:
    case 92:
    case 93:
    case 94:
    case 95:
    case 96:
    case 97:
    case 98:
    case 99:
    case 100:
    default:
    }
  return TRUE;

}


/* ********************************************************************
   *  Special procedures for mobiles                                      *
   ******************************************************************** */

SPECIAL (guild_guard)
{
  int i;
  extern int guild_info[][3];
  struct char_data *guard = (struct char_data *) me;
  char *buf = "The guard humiliates you, and blocks your way.\r\n";
  char *buf2 = "The guard humiliates $n, and blocks $s way.";

  if (!IS_MOVE (cmd) || IS_AFFECTED (guard, AFF_BLIND))
    return FALSE;

  if (GET_LEVEL (ch) >= LVL_IMMORT)
    return FALSE;

  for (i = 0; guild_info[i][0] != -1; i++)
    {
      if ((IS_NPC (ch) || GET_CLASS (ch) != guild_info[i][0]) &&
	  world[ch->in_room].number == guild_info[i][1] &&
	  cmd == guild_info[i][2])
	{
	  send_to_char (buf, ch);
	  act (buf2, FALSE, ch, 0, 0, TO_ROOM);
	  return TRUE;
	}
    }

  return FALSE;
}

SPECIAL (librarian)
{
  struct char_data *vict;

  if (cmd)
    return (0);
for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
{
  switch (number (0, 72))
    {
    case 0:
     act("$n says, 'I sell books from all over the land, why not buy one?'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 1:
      act("$n turns a page in the book she's reading.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 2:
      act("$n drinks a glass of wine.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 3:
     act("$n says, 'I'm reading a book about ancient Midgaard.'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 4:
      act("$n says, 'Thanks for being quiet in the library.'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 5:
      act("$n winks at $N suggestively.", TRUE, ch, 0, vict, TO_ROOM);
      return (1);
    case 6:
      act("$n starts sorting new books.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 7:
      act ("$n points at the sign on the wall.", FALSE, ch, 0, 0, TO_ROOM);
      act ("$n says, 'If you're looking for books just type: list'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 8:
      act ("$n says, 'I need a vacation. I'd love to see Jhaden'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 9:
      act("$n puts several books on a shelf.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 10:
act("$n snickers softly.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 11:
      act ("$n says, 'I wish people like you would write books.'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 12:
      act ("$n says, 'I once met AJ Trfiante here long ago.'", FALSE, ch, 0, 0, TO_ROOM);
      act ("$n says, 'It was at the debute of his restraunt'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 13:
act("$n seems to be getting tired.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 14:
act("$n leaves to a back room.", FALSE, ch, 0, 0, TO_ROOM);
act("$n returns with a new pile of books.", FALSE, ch, 0, 0, TO_ROOM);
      return (1);
    case 15:
      act ("$n says, 'I love to read. It makes you smart, you know.'", FALSE, ch, 0, 0, TO_ROOM);
      return (1);

    default:
      return (0);
    }
}
return (0);
}
SPECIAL (puff)
{
  ACMD (do_say);

  if (cmd)
    return (0);

  switch (number (0, 66))
    {
    case 0:
      do_say (ch, "The force is strong with this one!", 0, 0);
      return (1);
    case 1:
      do_say (ch, "If you only knew the POWER of the dark side", 0, 0);
      return (1);
    case 2:
  
      do_say (ch, "Read my lips.. no new taxes", 0, 0);
      return (1);
    case 3:
      do_say (ch, "Whenever I climb I am followed by a dog called Ego", 0, 0);
      return (1);
    case 4:
      do_say (ch, "I'll sleep when I'm dead", 0, 0);
      return (1);
    case 5:
      do_say (ch, "My advice to you is get married: if you find a good wife you'll be happy; if not,
you'll become a philosopher", 0, 0);
      return (1);
    case 6:
      do_say (ch, "We all agree that your theory is crazy, but is it crazy enough?", 0, 0);
      return (1);
    case 7:
      do_say (ch, "I loved Welmar more than any woman I had ever known", 0, 0);
      return (1);
    case 8:
      do_say (ch, "The graveyards are full of indispensable men", 0, 0);
      return (1);
    case 9:
      do_say (ch, "I have an existential map; it has 'you are here' written all over it", 0, 0);
      return (1);
    default:
      return (0);
    }
}



SPECIAL (fido)
{

  struct obj_data *i, *temp, *next_obj;


  if (cmd || !AWAKE (ch))
    return (FALSE);

  for (i = world[ch->in_room].contents; i; i = i->next_content)
    {
      if (GET_OBJ_TYPE (i) == ITEM_CONTAINER && GET_OBJ_VAL (i, 3))
	{
	  act ("$n savagely devours a corpse.", FALSE, ch, 0, 0, TO_ROOM);
	  for (temp = i->contains; temp; temp = next_obj)
	    {
	      next_obj = temp->next_content;
	      obj_from_obj (temp);
	      obj_to_room (temp, ch->in_room);
	    }
	  extract_obj (i);
	  return (TRUE);
	}
    }
  return (FALSE);
}



SPECIAL (janitor)
{
  struct obj_data *i;

  if (cmd || !AWAKE (ch))
    return (FALSE);

  for (i = world[ch->in_room].contents; i; i = i->next_content)
    {
      if (!CAN_WEAR (i, ITEM_WEAR_TAKE))
	continue;
      if (GET_OBJ_TYPE (i) != ITEM_DRINKCON && GET_OBJ_COST (i) >= 15)
	continue;
      act ("$n picks up some trash.", FALSE, ch, 0, 0, TO_ROOM);
      obj_from_room (i);
      obj_to_char (i, ch);
      return TRUE;
    }

  return FALSE;
}


SPECIAL (cityguard)
{
  struct char_data *tch, *evil;
  int max_evil;

  if (cmd || !AWAKE (ch) || FIGHTING (ch))
    return FALSE;


  max_evil = 1000;
  evil = 0;

  for (tch = world[ch->in_room].people; tch; tch = tch->next_in_room)
    {
      if (!IS_NPC (tch) && CAN_SEE (ch, tch) && IS_SET (PLR_FLAGS (tch), PLR_KILLER))
	{
	  act ("$n screams 'HEY!!!  You're one of those PLAYER KILLERS!!!!!!'", FALSE, ch, 0, 0, TO_ROOM);
	  hit (ch, tch, TYPE_UNDEFINED);
	  return (TRUE);
	}
    }

  for (tch = world[ch->in_room].people; tch; tch = tch->next_in_room)
    {
      if (!IS_NPC (tch) && CAN_SEE (ch, tch) && IS_SET (PLR_FLAGS (tch), PLR_THIEF))
	{
	  act ("$n screams 'HEY!!!  You're one of those PLAYER THIEVES!!!!!!'", FALSE, ch, 0, 0, TO_ROOM);
	  hit (ch, tch, TYPE_UNDEFINED);
	  return (TRUE);
	}
    }

  for (tch = world[ch->in_room].people; tch; tch = tch->next_in_room)
    {
      if (CAN_SEE (ch, tch) && FIGHTING (tch))
	{
	  if ((GET_ALIGNMENT (tch) < max_evil) &&
	      (IS_NPC (tch) || IS_NPC (FIGHTING (tch))))
	    {
	      max_evil = GET_ALIGNMENT (tch);
	      evil = tch;
	    }
	}
    }

  if (evil && (GET_ALIGNMENT (FIGHTING (evil)) >= 0))
    {
      act ("$n screams 'PROTECT THE INNOCENT!  BANZAI!  CHARGE!  ARARARAGGGHH!'", FALSE, ch, 0, 0, TO_ROOM);
      hit (ch, evil, TYPE_UNDEFINED);
      return (TRUE);
    }
  return (FALSE);
}


#define PET_PRICE(pet) (GET_LEVEL(pet) * 300)

SPECIAL (pet_shops)
{
  char buf[MAX_STRING_LENGTH], pet_name[256];
  int pet_room;
  struct char_data *pet;

  pet_room = ch->in_room + 1;

  if (CMD_IS ("list"))
    {
      send_to_char ("Available pets are:\r\n", ch);
      for (pet = world[pet_room].people; pet; pet = pet->next_in_room)
	{
	  sprintf (buf, "%8d - %s\r\n", PET_PRICE (pet), GET_NAME (pet));
	  send_to_char (buf, ch);
	}
      return (TRUE);
    }
  else if (CMD_IS ("buy"))
    {

      argument = one_argument (argument, buf);
      argument = one_argument (argument, pet_name);

      if (!(pet = get_char_room (buf, pet_room)))
	{
	  send_to_char ("There is no such pet!\r\n", ch);
	  return (TRUE);
	}
      if (GET_GOLD (ch) < PET_PRICE (pet))
	{
	  send_to_char ("You don't have enough gold!\r\n", ch);
	  return (TRUE);
	}
      GET_GOLD (ch) -= PET_PRICE (pet);

      pet = read_mobile (GET_MOB_RNUM (pet), REAL);
      GET_EXP (pet) = 0;
      SET_BIT (AFF_FLAGS (pet), AFF_CHARM);

      if (*pet_name)
	{
	  sprintf (buf, "%s %s", pet->player.name, pet_name);
	  /* free(pet->player.name); don't free the prototype! */
	  pet->player.name = str_dup (buf);

	  sprintf (buf, "%sA small sign on a chain around the neck says 'My name is %s'\r\n",
		   pet->player.description, pet_name);
	  /* free(pet->player.description); don't free the prototype! */
	  pet->player.description = str_dup (buf);
	}
      char_to_room (pet, ch->in_room);
      add_follower (pet, ch);
      load_mtrigger(pet);

      /* Be certain that pets can't get/carry/use/wield/wear items */
      IS_CARRYING_W (pet) = 1000;
      IS_CARRYING_N (pet) = 100;

      send_to_char ("May you enjoy your pet.\r\n", ch);
      act ("$n buys $N as a pet.", FALSE, ch, 0, pet, TO_ROOM);

      return 1;
    }
  /* All commands except list and buy */
  return 0;
}



/* ********************************************************************
   *  Special procedures for objects                                     *
   ******************************************************************** */


SPECIAL (temple_cleric)
{
  struct char_data *vict;
  struct char_data *hitme = NULL;
  static int this_hour;
  float temp1 = 1;
  float temp2 = 1;

  if (cmd)
    return FALSE;

  if (time_info.hours != 0)
    {

      this_hour = time_info.hours;

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  if (IS_AFFECTED (vict, AFF_POISON))
	    hitme = vict;
	}
      if (hitme != NULL)
	{
	  cast_spell (ch, hitme, NULL, SPELL_REMOVE_POISON);
	  return TRUE;
	}

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  if (IS_AFFECTED (vict, AFF_BLIND))
	    hitme = vict;
	}
      if (hitme != NULL)
	{
	  cast_spell (ch, hitme, NULL, SPELL_CURE_BLIND);
	  return TRUE;
	}

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  temp1 = GET_HIT (vict) / GET_MAX_HIT (vict);
	  if (temp1 < temp2)
	    {
	      temp2 = temp1;
	      hitme = vict;
	    }
	}
    if (hitme != NULL) {
          cast_spell(ch, hitme, NULL, SPELL_CURE_LIGHT);
          return TRUE;
          }
    }
  return 0;
}

SPECIAL (temple_healer)
{
  struct char_data *vict;
  struct char_data *hitme = NULL;
  static int this_hour;
  float temp1 = 1;
  float temp2 = 1;

  if (cmd)
    return FALSE;

  if (time_info.hours != 0)
    {

      this_hour = time_info.hours;

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  if (IS_AFFECTED (vict, AFF_POISON))
	    hitme = vict;
	}
      if (hitme != NULL)
	{
	  cast_spell (ch, hitme, NULL, SPELL_REMOVE_POISON);
	  return TRUE;
	}

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  if (IS_AFFECTED (vict, AFF_CURSE))
	    hitme = vict;
	}
      if (hitme != NULL)
	{
	  cast_spell (ch, hitme, NULL, SPELL_REMOVE_CURSE);
	  return TRUE;
	}

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  if (IS_AFFECTED (vict, AFF_BLIND))
	    hitme = vict;
	}
      if (hitme != NULL)
	{
	  cast_spell (ch, hitme, NULL, SPELL_CURE_BLIND);
	  return TRUE;
	}

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  temp1 = GET_HIT (vict) / GET_MAX_HIT (vict);
	  if (temp1 < temp2)
	    {
	      temp2 = temp1;
	      hitme = vict;
	    }
	}
    if (hitme != NULL) {
          cast_spell(ch, hitme, NULL, SPELL_HEAL);
          return TRUE;
          }
    }
  return 0;
}

SPECIAL (temple_mana_regenerator)
{
  struct char_data *vict;
  struct char_data *hitme = NULL;
  static int this_hour;
  float temp1 = 1;
  float temp2 = 1;

  if (cmd)
    return FALSE;

  if (time_info.hours != 0)
    {

      this_hour = time_info.hours;

      for (vict = world[ch->in_room].people; vict; vict = vict->next_in_room)
	{
	  temp1 = GET_MANA (vict) / GET_MAX_MANA (vict);
	  if (temp1 < temp2)
	    {
	      temp2 = temp1;
	      hitme = vict;
	    }
	}
    if (hitme != NULL) {
          cast_spell(ch, hitme, NULL, SPELL_REGEN_MANA);
          return TRUE;
          }
    }
  return 0;
}

SPECIAL (portal)
{
  struct obj_data *obj = (struct obj_data *) me;
  struct obj_data *port;
  char obj_name[MAX_STRING_LENGTH];

    if (!CMD_IS("enter")) return FALSE;

    argument = one_argument(argument,obj_name);
    if (!(port = get_obj_in_list_vis(ch, obj_name, world[ch->in_room].contents)))     {
      return(FALSE);
    }

    if (port != obj)
      return(FALSE);

    if (port->obj_flags.value[1] <= 0 ||
        port->obj_flags.value[1] > 32000) {
      send_to_char("The portal leads nowhere.\n\r", ch);
      return TRUE;
    }

    if (SECT(ch->in_room) == SECT_WATER_NOSWIM || SECT(port->obj_flags.value[1]) ==
SECT_WATER_NOSWIM) {
      if (RIDING(ch)) {
        if (!has_boat(RIDING(ch))) {
          send_to_char("Your mount needs a boat to go there.\r\n", ch);
          return TRUE;
        }
      }
      else
        if (!has_boat(ch)) {
          send_to_char("You need a boat to go there.\r\n", ch);
          return TRUE;
        }
    }
    act("$n enters $p, and vanishes!", FALSE, ch, port, 0, TO_ROOM);
    act("You enter $p, and you are transported elsewhere.", FALSE, ch, port, 0, TO_CHAR);
    char_from_room(ch);
    char_to_room(ch, port->obj_flags.value[1]);
    look_at_room(ch,0);
    act("$n appears from the glowing portal!", FALSE, ch, 0, 0, TO_ROOM);
  return TRUE;
}
extern int train_params[6][NUM_CLASSES];

SPECIAL(trainer)
{
  char trainitem[1024];

  if (IS_NPC(ch) || !CMD_IS("train"))
    return 0;

  skip_spaces(&argument); 

  if (!*argument) 
  {
    sprintf(buf,"Hit:%d Mana:%d Str:%d Con:%d Wis:%d Int:%d Dex:%d Cha:%d\r\n",
      (int) GET_MAX_HIT(ch), (int) GET_MAX_MANA(ch), GET_STR(ch), GET_CON(ch),
GET_WIS(ch),
      GET_INT(ch), GET_DEX(ch), GET_CHA(ch));
    sprintf(buf,"%sYou have %d training session",buf, GET_TRAINING(ch));
    if (GET_TRAINING(ch) == 1)
       sprintf(buf,"%s.\r\n",buf);
    else
       sprintf(buf,"%ss.\r\n",buf);
    send_to_char(buf,ch);
    return 1;
  }

  if (GET_TRAINING(ch) <= 0) {
    send_to_char("You do not seem to be able to train now.\r\n", ch);
    return 1;
  }

  /*  if (strcmp(argument,"hit")==0) */
  if (is_abbrev(argument,"hitpoints"))
    {
      GET_TRAINING(ch) -=1;
      ch->points.max_hit += 10;
      sprintf(trainitem, "maxhits goes up by 5!");
    } else
  if (is_abbrev(argument,"move"))
    {
      GET_TRAINING(ch) -=1;
      ch->points.max_move += 10;
      sprintf(trainitem, "maxmoves goes up by 5!");
    } else
  if (is_abbrev(argument,"mana"))
    {
      GET_TRAINING(ch) -=1;
      ch->points.max_mana += 10;
      sprintf(trainitem, "maxmana goes up by 5!");
    } else
  if (is_abbrev(argument,"strength"))
    {
      if (ch->real_abils.str >= train_params[0][(int)GET_CLASS(ch)])
       { 
	 send_to_char("Your are already fully trained there!\r\n",ch);
	 return 1; 
       }
      GET_TRAINING(ch) -=1;
      ch->real_abils.str+=1; 
      sprintf(trainitem, "strength goes up by 1!");
    } else
  if (is_abbrev(argument,"constitution"))
    {
      if (ch->real_abils.con >= train_params[1][(int)GET_CLASS(ch)])
       { send_to_char("Your are already fully trained there!\r\n",ch);
return 1; }
      GET_TRAINING(ch) -=1;
      /* ch->real_abils.con+=1; */
      ch->real_abils.con +=1; 
      sprintf(trainitem, "constitution goes up by 1!");
    } else
  if (is_abbrev(argument,"wisdom"))
    {
      if (ch->real_abils.wis >= train_params[2][(int)GET_CLASS(ch)])
       { send_to_char("Your are already fully trained there!\r\n",ch);
return 1; }
      GET_TRAINING(ch) -=1;
      ch->real_abils.wis +=1; 
      sprintf(trainitem, "wisdom goes up by 1!");
    } else
  if (is_abbrev(argument,"intelligence"))
    {
      if (ch->real_abils.intel >= train_params[3][(int)GET_CLASS(ch)])
       { send_to_char("Your are already fully trained there!\r\n",ch);
return 1; }
      GET_TRAINING(ch) -=1;
      ch->real_abils.intel+=1; 
      sprintf(trainitem, "intelligence goes up by 1!");
    } else
  if (is_abbrev(argument,"dexterity"))
    {
      if (ch->real_abils.dex >= train_params[4][(int)GET_CLASS(ch)])
       { send_to_char("Your are already fully trained there!\r\n",ch);
return 1; }
      GET_TRAINING(ch) -=1;
      ch->real_abils.dex+=1; 
      sprintf(trainitem, "dexterity goes up by 1!");
    } else
  if (is_abbrev(argument,"charisma"))
    {
      if (ch->real_abils.cha >= train_params[5][(int)GET_CLASS(ch)])
       { send_to_char("Your are already fully trained there!\r\n",ch);
return 1; }
      GET_TRAINING(ch) -=1;
      ch->real_abils.cha+=1; 
      sprintf(trainitem, "charisma goes up by 1!");
    } else
    {
      send_to_char("Train what?\r\n",ch);
      return 1;
    }
  affect_total (ch);
  save_char (ch, NOWHERE);

  sprintf(buf, "You train for a while...\r\nYour %s\r\n",trainitem);
  send_to_char(buf, ch);
  return 1;
}


SPECIAL(arenaentrancemaster)
{
//  struct char_data *who, *tmp;
  int cmd_puke = find_command("puke");
  int cmd_tell = find_command("tell");
  int fee = 0;
  char mybuf[1024];

  arenamaster = (struct char_data *) me;

  if (IS_NPC(ch) || !CMD_IS("arena"))
    return 0;
  
  skip_spaces(&argument); 

  if (!*argument) {
    fee = GET_LEVEL(ch) * arena_combatant;
    sprintf (mybuf, "%s Welcome to my Arena! Your fee as a "
	     "combatant will be %s coins,", GET_NAME(ch), numdisplay(fee));
    fee = GET_LEVEL(ch) * arena_observer;
    if (fee == 0)
      sprintf (mybuf, "%s and as an observer it's FREE!", mybuf);
    else
      sprintf (mybuf, "%s and as an observer it's %s coins.", mybuf, 
	       numdisplay(fee));

    do_tell(arenamaster, mybuf, cmd_tell, 0);
    deobserve(ch);
    clearobservers(ch);
    GET_ARENASTAT(ch) = ARENA_NOT;
    return 1;
  }
  if (is_abbrev(argument,"combatant")){
    fee = GET_LEVEL(ch) * arena_combatant;
    if (GET_GOLD(ch) < fee){
      sprintf(mybuf, "%s The fee for you is %s coins. "
	      "You don't have enough gold!", GET_NAME(ch),
	      numdisplay(fee));
      do_tell(arenamaster, mybuf, cmd_tell, 0);
      do_action(arenamaster, GET_NAME(ch), cmd_puke, 0);
      return 1;
    }
    sprintf(mybuf, "%s That'll be %s coins, thanks!", 
	    GET_NAME(ch), numdisplay(fee));
    do_tell(arenamaster, mybuf, cmd_tell, 0); 
    GET_GOLD(ch) -= fee;
    GET_ARENASTAT(ch) = ARENA_COMBATANT1;
    act ("$n admits $N as a combatant into the arena.", FALSE,
	 arenamaster, 0, ch, TO_NOTVICT);
    act ("$n admits you as a combatant into the arena.", FALSE, 
	 arenamaster, 0, ch, TO_VICT);
    char_from_room (ch);
    char_to_room (ch, real_room(arena_preproom));
    act ("$n has arrived.", FALSE, ch, 0, 0, TO_NOTVICT);
    look_at_room (ch, 0);
    if (defaultobserve != NULL){
      if (!IS_ARENACOMBATANT(defaultobserve))
	defaultobserve = ch;      
    }else{
      defaultobserve = ch;
    }
    if (GET_LEVEL(ch) < LVL_IMMORT){
      sprintf(mybuf, "%s has entered the arena as a combatant.", 
	      GET_NAME(ch));
      log (mybuf);
      do_gen_comm (arenamaster, mybuf, 1, SCMD_ARENA);
    }

    bup_affects(ch);
  } else if (is_abbrev(argument,"observer")){
    fee = GET_LEVEL(ch) * arena_observer;
    if (GET_GOLD(ch) < fee){
      sprintf(mybuf, "%s The fee for you is %s coins. "
	      "You don't have enough gold!", GET_NAME(ch),
	      numdisplay(fee));
      do_tell(arenamaster, mybuf, cmd_tell, 0); 
      do_action(arenamaster, GET_NAME(ch), cmd_puke, 0);
      return 1;
    }
    bzero(mybuf, 1024);
    if (defaultobserve == NULL)
      sprintf(mybuf, "%s Looks like there's currently no combatants there.",
	      GET_NAME(ch));
    else  if (!IS_ARENACOMBATANT(defaultobserve))
      sprintf(mybuf, "%s Looks like there's currently no combatants there.",
	      GET_NAME(ch));
    if (strlen(mybuf) > 0){
      do_tell(arenamaster, mybuf, cmd_tell, 0); 
      return 1;
    }

    if (fee == 0){
      sprintf(mybuf,"%s It's free to observe now!", GET_NAME(ch));
    }else{
      sprintf(mybuf, "%s That'll be %d coins, thanks!", 
	      GET_NAME(ch), fee);
    }
    do_tell(arenamaster, mybuf, cmd_tell, 0); 
    sprintf(mybuf, "%s You're currently observing the actions of %s.",
	    GET_NAME(ch), GET_NAME(defaultobserve));
    do_tell(arenamaster, mybuf, cmd_tell, 0); 
    /* Now let's link */
    linkobserve(ch, defaultobserve);

    GET_GOLD(ch) -= fee;
    GET_ARENASTAT(ch) = ARENA_OBSERVER;
    act ("$n admits $N into the arena observatory.", FALSE, 
	 arenamaster, 0, ch, TO_NOTVICT);
    act ("$n admits you into the arena observatory.", FALSE, 
	 arenamaster, 0, ch, TO_VICT);
    char_from_room (ch);
    char_to_room (ch, real_room(arena_observeroom));
    act ("$n has arrived.", FALSE, ch, 0, 0, TO_NOTVICT);
    look_at_room (ch, 0);

    if (GET_LEVEL(ch) < LVL_IMMORT){
      sprintf(mybuf, "%s has entered the arena as an observer.", 
	      GET_NAME(ch));
      do_gen_comm (arenamaster, mybuf, 1, SCMD_ARENA);
    }
    
  } else{
    sprintf (mybuf, "%s Welcome to my Arena! Combatant or Observer?\r\n",
	     GET_NAME(ch));
    do_tell(arenamaster, mybuf, cmd_tell, 0);
    deobserve(ch);
    clearobservers(ch);
    GET_ARENASTAT(ch) = ARENA_NOT;
    return 1;
  }

  return 1;
}

SPECIAL(tent) {
  struct obj_data *obj;
  struct descriptor_data *d, *next_d;

  if (CMD_IS("camp")) {
    if (!ismap(ch->in_room)) {
      send_to_char("You may only setup camp on the surface map.\r\n", ch);
      return 1;
    }
    for (obj=ch->carrying; obj; obj=obj->next_content) {
      if (obj==me) {
        ch->player_specials->saved.mapx=rm2x(ch->in_room);
        ch->player_specials->saved.mapy=rm2y(ch->in_room);
          send_to_char("\r\nYou setup camp and safely fall asleep...\r\n", ch);
          act("$n sets up camp and leaves the game.", FALSE, ch, 0, 0, TO_ROOM);
          write_aliases(ch);
          Crash_rentsave(ch, 0);
          sprintf (buf, "%s has camped out of the game. (%s)", GET_NAME (ch),
rcds(ch->in_room));
          mudlog (buf, NRM, MAX (LVL_IMMORT, GET_INVIS_LEV (ch)), TRUE);
          for (d = descriptor_list; d; d = next_d)
            {
              next_d = d->next;
              if (d == ch->desc)
                continue;
              if (d->character && (GET_IDNUM (d->character) == GET_IDNUM (ch)))
                close_socket (d);
            }
          extract_char(ch);
          return 1;
      }
    }
    send_to_char("You carry nothing to setup camp with.\r\n", ch);
    return 1;
  }
  return 0;
}
