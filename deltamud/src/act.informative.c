/* ************************************************************************
   *   File: act.informative.c                             Part of CircleMUD *
   *  Usage: Player-level commands of an informative nature                  *
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
#include "screen.h"
#include "clan.h"
#include "maputils.h"
#include "dg_scripts.h"
#include "dbinterface.h"

/* extern variables */
extern int pk_allowed;
extern int jail_num;
extern int exp_to_level();
extern int top_of_world;
extern struct room_data *world;
extern struct descriptor_data *descriptor_list;
extern struct char_data *character_list;
extern struct obj_data *object_list;
extern struct title_type titles[NUM_CLASSES][LVL_IMPL + 1];
extern struct command_info cmd_info[];
extern struct player_index_element *player_table;
extern struct index_data *mob_index;
extern int top_of_p_table;
extern int top_of_helpt;
extern struct help_index_element *help_table;
extern char *help;
ACMD (do_restore);
extern char *credits;
extern char *news;
extern char *info;
extern char *motd;
extern char *imotd;
extern char *wizlist;
extern char *immlist;
extern char *policies;
extern char *handbook;
extern char *circlemud;
extern const char *dirs[];
extern char *where[];
extern const char *color_liquid[];
extern char *fullness[];
extern char *connected_types[];
extern char *class_abbrevs[];
extern char *race_abbrevs[];
extern const char *room_bits[];
extern char *spells[];
extern const char *pc_race_types[];
extern const char *pc_class_types[];
extern char *citizen_colors[];
extern struct citizen_type citizen_titles[7];
extern struct clan_info clan[MAX_CLANS];
/* global */
int boot_high = 0;

static char *ctypes[];

/* extern functions */
long find_class_bitvector (char arg);
char *title_male (int class, int level);
char *title_female (int class, int level);
char *town_name (struct char_data *ch);
char *deity_name (struct char_data *ch);
char *race_name (struct char_data *ch);
char *class_name (struct char_data *ch);
char *sex_name (struct char_data *ch);
void quicksort(void *, int, int, int (*comp)(int, int));
char* numdisplay(int);

void warn_blizzard (struct char_data *ch)
{
send_to_char("You are freezing and see snow everywhere, a blizzard is in progress!\r\n", ch);
}

void
show_obj_to_char (struct obj_data *object, struct char_data *ch,
		  int mode)
{
  bool found;

  *buf = '\0';
  if ((mode == 0) && object->description)
    strcpy (buf, object->description);
  else if (object->short_description && ((mode == 1) ||
				 (mode == 2) || (mode == 3) || (mode == 4)))
    strcpy (buf, object->short_description);
  else if (mode == 5)
    {
      if (GET_OBJ_TYPE (object) == ITEM_NOTE)
	{
	  if (object->action_description)
	    {
	      strcpy (buf, "There is something written upon it:\r\n\r\n");
	      strcat (buf, object->action_description);
	      page_string (ch->desc, buf, 1);
	    }
	  else
	    act ("It's blank.", FALSE, ch, 0, 0, TO_CHAR);
	  return;
	}
      else if (GET_OBJ_TYPE (object) != ITEM_DRINKCON)
	{
	  strcpy (buf, "You see nothing special..");
	}
      else                      /* ITEM_TYPE == ITEM_DRINKCON||FOUNTAIN */
	strcpy (buf, "It looks like a drink container.");
    }
  if (mode != 3)
    {
      found = FALSE;
      if (IS_OBJ_STAT (object, ITEM_INVISIBLE))
	{
	  strcat (buf, " (invisible)");
	  found = TRUE;
	}
      if (IS_OBJ_STAT (object, ITEM_BLESS) && IS_AFFECTED (ch, AFF_DETECT_ALIGN))
	{
	  strcat (buf, " ..It glows blue!");
	  found = TRUE;
	}
      if (IS_OBJ_STAT (object, ITEM_MAGIC) && IS_AFFECTED (ch, AFF_DETECT_MAGIC))
	{
	  strcat (buf, " ..It glows yellow!");
	  found = TRUE;
	}
      if (IS_OBJ_STAT (object, ITEM_GLOW))
	{
	  strcat (buf, " ..It has a soft glowing aura!");
	  found = TRUE;
	}
      if (IS_OBJ_STAT (object, ITEM_HUM))
	{
	  strcat (buf, " ..It emits a faint humming sound!");
	  found = TRUE;
	}
    }
  strcat (buf, "\r\n");
  page_string (ch->desc, buf, 1);
}


void
list_obj_to_char (struct obj_data *list, struct char_data *ch, int mode, bool show)
{
  struct obj_data *i, *j;
  char buf[10];
  bool found;
  int num;

  found = FALSE;
  for (i = list; i; i = i->next_content)
    {
      num = 0;
      for (j = list; j != i; j = j->next_content)
	if (j->item_number == NOTHING)
	  {
	    if (strcmp (j->short_description, i->short_description) == 0)
	      break;
	  }
	else if (j->item_number == i->item_number)
	  break;
      if (j != i)
	continue;
      for (j = i; j; j = j->next_content)
	if (j->item_number == NOTHING)
	  {
	    if (strcmp (j->short_description, i->short_description) == 0)
	      num++;
	  }
	else if (j->item_number == i->item_number)
	  num++;

      if (CAN_SEE_OBJ (ch, i))
	{
          send_to_char("&G", ch);
	  if (num != 1)
	    {
	      sprintf (buf, "(%i) ", num);
	      send_to_char (buf, ch);
	    }
	  show_obj_to_char (i, ch, mode);
	  found = TRUE;
	}
    }
  if (!found && show)
    send_to_char (" Nothing.\r\n", ch);
}


void
diag_char_to_char (struct char_data *i, struct char_data *ch)
{
  int percent;

  if (GET_MAX_HIT (i) > 0)
    percent = (100 * GET_HIT (i)) / GET_MAX_HIT (i);
  else
    percent = -1;               /* How could MAX_HIT be < 1?? */

  strcpy (buf, PERS (i, ch));
  CAP (buf);

  if (percent >= 100)
    strcat (buf, " is in excellent condition.\r\n");
  else if (percent >= 90)
    strcat (buf, " has a few scratches.\r\n");
  else if (percent >= 75)
    strcat (buf, " has some small wounds and bruises.\r\n");
  else if (percent >= 50)
    strcat (buf, " has quite a few wounds.\r\n");
  else if (percent >= 30)
    strcat (buf, " has some big nasty wounds and scratches.\r\n");
  else if (percent >= 15)
    strcat (buf, " looks pretty hurt.\r\n");
  else if (percent >= 0)
    strcat (buf, " is in awful condition.\r\n");
  else
    strcat (buf, " is bleeding awfully from big wounds.\r\n");

  send_to_char (buf, ch);
}

char *prompt_diag (struct char_data *i, struct char_data *ch)
{
  int percent;

  if (GET_MAX_HIT (i) > 0)
    percent = (100 * GET_HIT (i)) / GET_MAX_HIT (i);
  else
    percent = -1;               /* How could MAX_HIT be < 1?? */
//  strcpy (buf, PERS (i, ch));
//  CAP (buf);

  if (percent >= 100)   
      return "excellent";
  else if (percent >= 90)
      return "scratched";
  else if (percent >= 75)
      return "bruised";
  else if (percent >= 50)
      return "wounded";
  else if (percent >= 30)
      return "nasty";
  else if (percent >= 15)
      return "hurt";
  else if (percent >= 0)
      return "awful";
  else
      return "bleeding";
   
}

void
look_at_char (struct char_data *i, struct char_data *ch)
{
  int j, found;
  struct obj_data *tmp_obj;

  if (!ch->desc)
    return;

  if (i->player.description)
    send_to_char (i->player.description, ch);
  else
    act ("You see nothing special about $m.", FALSE, i, 0, ch, TO_VICT);

  diag_char_to_char (i, ch);

   if (RIDING(i) && RIDING(i)->in_room == i->in_room) {
     if (RIDING(i) == ch)
       act("$e is mounted on you.", FALSE, i, 0, ch, TO_VICT);
     else {
       sprintf(buf2, "$e is mounted upon %s.", PERS(RIDING(i), ch));
       act(buf2, FALSE, i, 0, ch, TO_VICT);
     }
   } else if (RIDDEN_BY(i) && RIDDEN_BY(i)->in_room == i->in_room) {
     if (RIDDEN_BY(i) == ch)
       act("You are mounted upon $m.", FALSE, i, 0, ch, TO_VICT);
     else {
       sprintf(buf2, "$e is mounted by %s.", PERS(RIDDEN_BY(i), ch));
       act(buf2, FALSE, i, 0, ch, TO_VICT);
     }
   }
  
if (!IS_NPC(i)) {
  *buf = '\0';
  if ((GET_LEVEL(i) == LVL_HERO)) {
    sprintf(buf, "%s%s is a %s %s and hero of %s.\r\n", buf,
       sex_name(i), race_name(i), class_name(i), town_name(i));
    send_to_char(buf, ch);
  } else {
  if ((GET_LEVEL(i) == LVL_IMMORT)) {
    sprintf(buf, "%s%s is a %s %s and immortal of %s.\r\n", buf,
       sex_name(i), race_name(i), class_name(i), town_name(i));
    send_to_char(buf, ch);
  } else {
  if ((GET_LEVEL(i) == LVL_DEMIGOD)) {
    sprintf(buf, "%s%s is a %s %s and Demi God of %s.\r\n", buf,
       sex_name(i), race_name(i), class_name(i), town_name(i));
    send_to_char(buf, ch);
  } else {
  if ((GET_LEVEL(i) >= LVL_GOD)) {
    sprintf(buf, "%s%s is a %s %s and God of %s.\r\n", buf,
	sex_name(i), race_name(i), class_name(i), town_name(i));
    send_to_char(buf, ch);
  } else {

  sprintf(buf, "%s%s is a %s %s and citizen of %s.\r\n", buf, sex_name(i), race_name(i), class_name(i), town_name(i));
  send_to_char(buf, ch);
    }
  }
 }
 }
}
  found = FALSE;
  for (j = 0; !found && j < NUM_WEARS; j++)
    if (GET_EQ (i, j) && CAN_SEE_OBJ (ch, GET_EQ (i, j)))
      found = TRUE;

  if (found)
    {
      act ("\r\n$n is using:", FALSE, i, 0, ch, TO_VICT);
      for (j = 0; j < NUM_WEARS; j++)
	if (GET_EQ (i, j) && CAN_SEE_OBJ (ch, GET_EQ (i, j)))
	  {
	    send_to_char (where[j], ch);
	    show_obj_to_char (GET_EQ (i, j), ch, 1);
	  }
    }
  if (ch != i && (GET_CLASS (ch) == CLASS_THIEF || GET_LEVEL (ch) >= LVL_IMMORT))
    {
      found = FALSE;
      act ("\r\nYou attempt to peek at $s inventory:", FALSE, i, 0, ch, TO_VICT);
      for (tmp_obj = i->carrying; tmp_obj; tmp_obj = tmp_obj->next_content)
	{
	  if (CAN_SEE_OBJ (ch, tmp_obj) && (number (0, 20) < GET_LEVEL (ch)))
	    {
	      show_obj_to_char (tmp_obj, ch, 1);
	      found = TRUE;
	    }
	}

      if (!found)
	send_to_char ("You can't see anything.\r\n", ch);
    }
}


char *list_all_char(struct char_data * i, struct char_data * ch, int num)
{
char *positions[] =
{
  " is lying here, dead.",
  " is lying here, mortally wounded.",
  " is lying here, incapacitated.",
  " is lying here, stunned.",
  " is sleeping here.",
  " is resting here.",
  " is meditating here.",
  " is sitting here.",
  "!FIGHTING!",
  " is standing here."
};

  if (IS_NPC (i) && i->player.long_descr && GET_POS (i) == GET_DEFAULT_POS (i))
    {
      send_to_char (CCYEL (ch, C_NRM), ch);
      if (IS_AFFECTED (i, AFF_INVISIBLE))
	strcpy (buf, "*");
      else
	*buf = '\0';

      if (IS_AFFECTED (ch, AFF_DETECT_ALIGN))
	{
	  if (IS_EVIL (i))
	    strcat (buf, "(Red Aura) ");
	  else if (IS_GOOD (i))
	    strcat (buf, "(Blue Aura) ");
	}

      if (IS_NPC (i) && GET_QUESTMOB (ch) == GET_MOB_VNUM (i))
	strcat (buf, "(TARGET) ");

      strcat (buf, i->player.long_descr);
	    if (num > 1)
	    {
		while ((buf[strlen(buf)-1]=='\r') ||
		       (buf[strlen(buf)-1]=='\n') ||
		       (buf[strlen(buf)-1]==' ')) {
		    buf[strlen(buf)-1] = '\0';
		}
		sprintf(buf2," [%d]\n\r", num);
		strcat(buf, buf2);
	    }
//      send_to_char (buf, ch);

      if (IS_AFFECTED (i, AFF_SANCTUARY))
	sprintf(buf+strlen(buf), "...%s glows with a bright light!\r\n", HSSH(i));
      if (IS_AFFECTED (i, AFF_CONVERGENCE))
	sprintf(buf+strlen(buf), "...%s glows in a red aura!\r\n", HSSH(i));
      if (IS_AFFECTED (i, AFF_AUTUS))
	sprintf(buf+strlen(buf), "...%s glows in a green aura!\r\n", HSSH(i));
      if (IS_AFFECTED (i, AFF_BLIND))
	sprintf(buf+strlen(buf), "...%s is groping around blindly!\r\n", HSSH(i));

      return strdup(buf);
    }
if (IS_NPC (i) && !(RIDING(i) && RIDING(i)->in_room == i->in_room))
    {
      strcpy (buf, i->player.short_descr);
      CAP (buf);
    }
  else
  if ((GET_LEVEL(i) < LVL_IMMORT) && (GET_CITIZEN(i) > 0))
    sprintf (buf, "&Y%s%s %s&Y", READ_CITIZEN(i), i->player.name, GET_TITLE (i));
  else
    sprintf (buf, "&Y%s %s&Y", i->player.name, GET_TITLE (i));

  if (IS_AFFECTED (i, AFF_INVISIBLE))
    strcat (buf, " (invisible)");
  if (IS_AFFECTED (i, AFF_HIDE))
    strcat (buf, " (hidden)");
  if (!IS_NPC (i) && !i->desc)
    strcat (buf, " (linkless)");
  if (PLR_FLAGGED (i, PLR_WRITING))
    strcat (buf, " (writing)");
  if (PRF_FLAGGED (i, PRF_AFK))
    strcat (buf, " (afk)");
  if (PRF2_FLAGGED (i, PRF2_LOCKOUT))
    strcat (buf, " (afk-locked)");
  if (PRF2_FLAGGED (i, PRF2_INTANGIBLE)) {
    if (PRF2_FLAGGED (i, PRF2_MBUILDING))
      strcat (buf, " (building)");
    else
      strcat (buf, " (dead)");
  }

   if (RIDING(i) && RIDING(i)->in_room == i->in_room) {
     strcat(buf, " is here, mounted upon ");
     if (RIDING(i) == ch)
       strcat(buf, "you");
     else
       strcat(buf, PERS(RIDING(i), ch));
     strcat(buf, ".");
   } else if (GET_POS(i) != POS_FIGHTING)
    strcat (buf, positions[(int) GET_POS (i)]);
  else
    {
      if (FIGHTING (i))
	{
	  strcat (buf, " is here, fighting ");
	  if (FIGHTING (i) == ch)
	    strcat (buf, "YOU!");
	  else
	    {
	      if (i->in_room == FIGHTING (i)->in_room)
		strcat (buf, PERS (FIGHTING (i), ch));
	      else
		strcat (buf, "someone who has already left");
	      strcat (buf, "!");
	    }
	}
      else                      /* NIL fighting pointer */
	strcat (buf, " is here struggling with thin air.");
    }

  if (IS_AFFECTED (ch, AFF_DETECT_ALIGN))
    {
      if (IS_EVIL (i))
	strcat (buf, " (Red Aura)");
      else if (IS_GOOD (i))
	strcat (buf, " (Blue Aura)");
    }
  strcat (buf, "\r\n");
//  send_to_char (buf, ch);

      if (IS_AFFECTED (i, AFF_SANCTUARY))
	sprintf(buf+strlen(buf), "...%s glows with a bright light!\r\n", HSSH(i));
      if (IS_AFFECTED (i, AFF_CONVERGENCE))
	sprintf(buf+strlen(buf), "...%s glows in a red aura!\r\n", HSSH(i));
      if (IS_AFFECTED (i, AFF_AUTUS))
	sprintf(buf+strlen(buf), "...%s glows in a green aura!\r\n", HSSH(i));
      if (IS_AFFECTED (i, AFF_BLIND))
	sprintf(buf+strlen(buf), "...%s is groping around blindly!\r\n", HSSH(i));
  return strdup(buf);
}

void send_list_to_char(char *string, struct char_data *ch) {
  if (string) {
    send_to_char(string, ch);
    free(string);
  }
}

void list_nostack_char_to_char (struct char_data *list, struct char_data *ch)
{
  struct char_data *i;

  for (i = list; i; i = i->next_in_room)
    if (ch != i)
      {
      if (RIDDEN_BY(i) && RIDDEN_BY(i)->in_room == i->in_room)
	 continue;
	if (CAN_SEE (ch, i))
	  send_list_to_char(list_all_char (i, ch, 0), ch);
	else if (IS_DARK (ch->in_room) && !CAN_SEE_IN_DARK (ch) &&
		 IS_AFFECTED (i, AFF_INFRAVISION))
	  send_to_char ("You see a pair of glowing red eyes looking your way.\r\n", ch);
      }
}
struct string_elem {
  char *string;
  int number;
};

void list_char_to_char (struct char_data *list, struct char_data *ch)
{
  struct char_data *i;
  char *line=NULL;
  struct string_elem lines[500];
  int lastline=0, j=0, fnd=0;
  for (j=0; j<=499; j++)
    lines[j].string=NULL;

  for (i = list; i; i = i->next_in_room)
    if (ch != i)
      {
	if (RIDDEN_BY(i) && RIDDEN_BY(i)->in_room == i->in_room)
	   continue;
	if (CAN_SEE (ch, i)) {
//        send_list_to_char(list_all_char (i, ch, 0), ch);
	  line=list_all_char (i, ch, 0);
	}
	else if (IS_DARK (ch->in_room) && !CAN_SEE_IN_DARK (ch) &&
		 IS_AFFECTED (i, AFF_INFRAVISION))
	  line=strdup ("You see a pair of glowing red eyes looking your way.\r\n");
	for (j=0; j<=lastline; j++) {
	  buf1[0]='\0';
	  if (!line || !lines[j].string) break;
	  if (!strncmp(line, lines[j].string, strlen(line))) {
	    lines[j].number++;
	    fnd=1;
	    break;
	  }
	}
	if (!fnd) {
	  lines[lastline].string=str_dup(line);
	  lines[lastline].number=1;
	  lastline++;
	}
	fnd=0;
	if (line) { free(line); line=NULL; }
      }
  for (j=0; j<lastline; j++) {
    if (lines[j].number>1) {
      sprintf(buf, "[%3d] ", lines[j].number);
      send_to_char(buf, ch);
    }
    send_to_char(lines[j].string, ch);
    free(lines[j].string);
  }
}

/*
void list_char_to_char(struct char_data *list, struct char_data *ch)
{
    struct char_data *i, *plr_list[100];
    int num, counter, locate_list[100], found=FALSE;

    num = 0;

    for (i=list; i; i = i->next_in_room) {
      if(i != ch)  {
      if (RIDDEN_BY(i) && RIDDEN_BY(i)->in_room == i->in_room)   
	 continue;
	if (CAN_SEE(ch, i))
	{
	    if (num< 100)
	    {
		found = FALSE;
		for (counter=0;(counter<num&& !found);counter++)
		{
		    if (i->nr == plr_list[counter]->nr &&
for odd reasons, you may want to seperate same nr mobs by factors 
other than position, the aff flags, and the fighters. (perhaps you want 
to list switched chars differently.. or polymorphed chars?)

			(GET_POS(i) == GET_POS(plr_list[counter])) &&
			(AFF_FLAGS(i)==AFF_FLAGS(plr_list[counter])) &&
			(FIGHTING(i) == FIGHTING(plr_list[counter])) &&
			!strcmp(GET_NAME(i), GET_NAME(plr_list[counter])))
		    {
			locate_list[counter] += 1;
			found=TRUE;
		    }
		}
		if (!found) {
		    plr_list[num] = i;
		    locate_list[num] = 1;
		    num++;
		}
	    } else {
	      send_list_to_char(list_all_char (i, ch, 0), ch);
	    }
	}
      }
    }
    if (num) {
	for (counter=0; counter<num; counter++) {
	    if (locate_list[counter] > 1) {
		send_list_to_char(list_all_char(plr_list[counter],ch,locate_list[counter]), ch);
	    } else {
		send_list_to_char(list_all_char(plr_list[counter],ch,0), ch);
	    }
	}
    }
}
*/

void
do_auto_exits (struct char_data *ch)
{
  int door;

  *buf = '\0';

  for (door = 0; door < NUM_OF_DIRS; door++)
    if (EXIT(ch, door) && EXIT(ch, door)->to_room != NOWHERE) {
      if (IS_SET(EXIT(ch, door)->exit_info, EX_HIDDEN) && GET_LEVEL(ch) < LVL_IMMORT) continue;
      if (IS_SET(EXIT(ch, door)->exit_info, EX_CLOSED))
	  sprintf(buf, "%s%c* ", buf, LOWER (*dirs[door]));
      else
	  sprintf(buf, "%s%c ", buf, LOWER (*dirs[door]));
    }
  if (SPEC_EXIT(ch->in_room) && SPEC_EXIT(ch->in_room)->ex_name)
    if (!IS_SET(SPEC_EXIT(ch->in_room)->exit_info, EX_HIDDEN) || GET_LEVEL(ch) >= LVL_IMMORT)
      sprintf(buf + strlen(buf), "%s ", SPEC_EXIT(ch->in_room)->keyword);

  sprintf (buf2, "%s[ Exits: %s]%s\r\n", CCCYN (ch, C_NRM),
	   *buf ? buf : "None! ", CCNRM (ch, C_NRM));

  send_to_char (buf2, ch);
}


ACMD (do_exits)
{
  int door;
  *buf = '\0';

  if (IS_AFFECTED (ch, AFF_BLIND))
    {
      send_to_char ("You can't see a damned thing, you're blind!\r\n", ch);
      return;
    }
  for (door = 0; door < NUM_OF_DIRS; door++)
    {
      if (EXIT (ch, door) && EXIT (ch, door)->to_room != NOWHERE &&
	  !IS_SET (EXIT (ch, door)->exit_info, EX_CLOSED))
	{
	  if (IS_SET(EXIT(ch, door)->exit_info, EX_HIDDEN) && GET_LEVEL(ch) < LVL_IMMORT)
continue;
	  if (GET_LEVEL (ch) >= LVL_IMMORT || PRF_FLAGGED(ch, PRF_ROOMFLAGS))
	    sprintf (buf2, "&Y%-5s&n &R-&n &C[&n%s&C]&n %s\r\n", dirs[door],
		     rcds(EXIT (ch, door)->to_room),
		     world[EXIT (ch, door)->to_room].name);             
	  else
	    {
	      sprintf (buf2, "&Y%-5s&n &R-&n ", dirs[door]);
	      if (IS_DARK (EXIT (ch, door)->to_room) && !CAN_SEE_IN_DARK (ch))
		strcat (buf2, "Too dark to tell\r\n");
	      else
		{
		  strcat (buf2, world[EXIT (ch, door)->to_room].name);
		  strcat (buf2, "\r\n");
		}
	    }
	  strcat (buf, CAP (buf2));
	}
      else if (EXIT (ch, door) && EXIT (ch, door)->to_room != NOWHERE &&
	       IS_SET (EXIT (ch, door)->exit_info, EX_CLOSED))
	{
	  if (GET_LEVEL (ch) >= LVL_IMMORT)
	      {
	      sprintf (buf2, "&Y%-5s&n &R-&n &C[&n%s&C]&n %s &R-&n &CCLOSED&n", dirs[door],
		       rcds(EXIT (ch, door)->to_room),
		       world[EXIT (ch, door)->to_room].name);
	      if (IS_SET (EXIT (ch, door)->exit_info, EX_LOCKED))
		strcat (buf2, " &R-&n &CLOCKED&n");
	      if (IS_SET (EXIT (ch, door)->exit_info, EX_PICKPROOF))
		strcat (buf2, " &R-&n &CPICKPROOF&n");
	      strcat (buf2, "\r\n");
	    }
	  else
	    sprintf (buf2, "&Y%-5s&n &R-&n The %s is closed\r\n", dirs[door],
		     fname (EXIT (ch, door)->keyword));
	  strcat (buf, CAP (buf2));
	}
    }
 
  send_to_char ("&CObvious exits:&n\r\n", ch);

  if (*buf)
    send_to_char (buf, ch);
  else
    send_to_char (" None.\r\n", ch);

    /* Special exits in this room? hoy hoy! - Mulder */
   if (SPEC_EXIT(ch->in_room) && SPEC_EXIT(ch->in_room)->keyword) {
     if (!IS_SET(SPEC_EXIT(ch->in_room)->exit_info, EX_HIDDEN) || GET_LEVEL(ch) >= LVL_IMMORT) {
       send_to_char ("&CSpecial exits:&n\r\n", ch);
       sprintf(buf, "&Y%-5s &R-&n Special Exit\r\n", SPEC_EXIT(ch->in_room)->keyword);
       send_to_char (buf, ch);
     }
   }
}

char *blood_messages[] = {
  "&RShould never see this (bug, please report).&n\r\n",
  "&RSome drops of blood can be seen here.&n",
  "&RMany drops of blood can be seen here.&n",
  "&RThere is a small pool of blood that has been spilt here.&n",
  "&RThere is a medium pool of blood that has been spilt here.&n",
  "&RThere is a large pool of blood that has been spilt here.&n",
  "&RSome blood and guts are all around you.&n",
  "&RSome blood and guts are all around you.&n",
  "&RAlot of blood and guts are here..&n",
  "&RThere are blood and guts all over the place..&n",
  "&RYou see nothing but blood and guts everywhere!&n",
  "\n"
};

char *snow_messages[] = {
  "&WShould never see this. (bug, please report).&n\r\n",
  "&WThere is less than an inch of snow on the ground.&n",
  "&WThere is an inch of white snow on the ground.&n",
  "&WThere are three inches of snow on the ground.&n",
  "&WThere are six inches of snow on the ground.&n",
  "&WThere is a foot of snow on the ground.&n",
  "&WThere are two feet of snow on the ground.&n",
  "&WThere are three feet of snow on the ground.&n",
  "&WThere are four feet of snow on the ground..&n",
  "&WThere are five feet of snow on the ground!&n",
  "&WTHERE IS SNOW EVERYWHERE.. . YOU CAN BARELY SEE@!*%###@%#@!&n",
  "\n"
};

void
look_at_room (struct char_data *ch, int ignore_brief)
{
  if (!ch->desc)
    return;
      
  if (IS_DARK (ch->in_room) && !CAN_SEE_IN_DARK (ch))
    {
      send_to_char ("It is pitch black...\r\n", ch);
      return;
    }
  else if (IS_AFFECTED (ch, AFF_BLIND))
    {
      send_to_char ("You see nothing but infinite darkness...\r\n", ch);
      return;
    }

  if (!PRF2_FLAGGED(ch, PRF2_NOMAP) && ismap(ch->in_room))
    printmap(ch->in_room, ch);
  send_to_char (CCCYN (ch, C_NRM), ch);
  if (PRF_FLAGGED (ch, PRF_ROOMFLAGS))
    {
      sprintbit ((long) ROOM_FLAGS (ch->in_room), room_bits, buf);
      sprintf (buf2, "[%s] %s [ %s]", rcds(ch->in_room), world[ch->in_room].name, buf);
      send_to_char (buf2, ch);
    }
  else
    send_to_char (world[ch->in_room].name, ch);
     
  send_to_char (CCNRM (ch, C_NRM), ch);
  send_to_char ("\r\n", ch);
  if ((!ismap(ch->in_room) && !PRF_FLAGGED(ch, PRF_BRIEF)) || ((ismap(ch->in_room) || PRF_FLAGGED(ch, PRF_BRIEF)) && ignore_brief) || ROOM_FLAGGED(ch->in_room, ROOM_DEATH))
    send_to_char (world[ch->in_room].description, ch);
    /* send the description IF the room is not a map AND the player isnt flagged brief
       if they are brief or the room is a map, check if it ignores brief, if it does send it
       send if either way if the room is a death room */

  /* autoexits */
  if (PRF_FLAGGED (ch, PRF_AUTOEXIT))
    do_auto_exits (ch);
  /* now list characters & objects */
    if (RM_SNOW(ch->in_room) > 0)
      act(snow_messages[RM_SNOW(ch->in_room)], FALSE, ch, 0, 0, TO_CHAR);
    if (RM_BLOOD(ch->in_room) > 0)
      act(blood_messages[RM_BLOOD(ch->in_room)], FALSE, ch, 0, 0, TO_CHAR);
  list_obj_to_char (world[ch->in_room].contents, ch, 0, FALSE);
  if (PRF_FLAGGED (ch, PRF_NOLOOKSTACK))
    list_nostack_char_to_char (world[ch->in_room].people, ch);
  else
    list_char_to_char (world[ch->in_room].people, ch);
  send_to_char (CCNRM (ch, C_NRM), ch);
}

void
look_in_direction (struct char_data *ch, int dir)
{
  if (EXIT (ch, dir))
    {
      if (EXIT (ch, dir)->general_description)
	send_to_char (EXIT (ch, dir)->general_description, ch);
      else
	send_to_char ("You see nothing special.\r\n", ch);

      if (IS_SET (EXIT (ch, dir)->exit_info, EX_CLOSED) && EXIT (ch, dir)->keyword)
	{
	  sprintf (buf, "The %s is closed.\r\n", fname (EXIT (ch, dir)->keyword));
	  send_to_char (buf, ch);
	}
      else if (IS_SET (EXIT (ch, dir)->exit_info, EX_ISDOOR) && EXIT (ch, dir)->keyword)
	{
	  sprintf (buf, "The %s is open.\r\n", fname (EXIT (ch, dir)->keyword));
	  send_to_char (buf, ch);
	}
    }
  else
    send_to_char ("Nothing special there...\r\n", ch);
}



void
look_in_obj (struct char_data *ch, char *arg)
{
  struct obj_data *obj = NULL;
  struct char_data *dummy = NULL;
  int amt, bits;

  if (!*arg)
    send_to_char ("Look in what?\r\n", ch);
  else if (!(bits = generic_find (arg, FIND_OBJ_INV | FIND_OBJ_ROOM |
				  FIND_OBJ_EQUIP, ch, &dummy, &obj)))
    {
      sprintf (buf, "There doesn't seem to be %s %s here.\r\n", AN (arg), arg);
      send_to_char (buf, ch);
    }
  else if ((GET_OBJ_TYPE (obj) != ITEM_DRINKCON) &&
	   (GET_OBJ_TYPE (obj) != ITEM_FOUNTAIN) &&
	   (GET_OBJ_TYPE (obj) != ITEM_CONTAINER))
    send_to_char ("There's nothing inside that!\r\n", ch);
  else
    {
      if (GET_OBJ_TYPE (obj) == ITEM_CONTAINER)
	{
	  if (IS_SET (GET_OBJ_VAL (obj, 1), CONT_CLOSED))
	    send_to_char ("It is closed.\r\n", ch);
	  else
	    {
	      send_to_char (fname (obj->name), ch);
	      switch (bits)
		{
		case FIND_OBJ_INV:
		  send_to_char (" (carried): \r\n", ch);
		  break;
		case FIND_OBJ_ROOM:
		  send_to_char (" (here): \r\n", ch);
		  break;
		case FIND_OBJ_EQUIP:
		  send_to_char (" (used): \r\n", ch);
		  break;
		}

	      list_obj_to_char (obj->contains, ch, 2, TRUE);
	    }
	}
      else
	{                       /* item must be a fountain or drink container */
	  if (GET_OBJ_VAL (obj, 1) <= 0)
	    send_to_char ("It is empty.\r\n", ch);
	  else
	    {
	      if (GET_OBJ_VAL (obj, 0) <= 0 || GET_OBJ_VAL (obj, 1) > GET_OBJ_VAL (obj, 0))
		{
		  sprintf (buf, "Its contents seem somewhat murky.\r\n");       /* BUG */
		}
	      else
		{
		  amt = (GET_OBJ_VAL (obj, 1) * 3) / GET_OBJ_VAL (obj, 0);
		  sprinttype (GET_OBJ_VAL (obj, 2), color_liquid, buf2);
		  sprintf (buf, "It's %sfull of a %s liquid.\r\n", fullness[amt], buf2);
		}
	      send_to_char (buf, ch);
	    }
	}
    }
}



char *
find_exdesc (char *word, struct extra_descr_data *list)
{
  struct extra_descr_data *i;

  for (i = list; i; i = i->next)
    if (isname (word, i->keyword))
      return (i->description);

  return NULL;
}


/*
 * Given the argument "look at <target>", figure out what object or char
 * matches the target.  First, see if there is another char in the room
 * with the name.  Then check local objs for exdescs.
 */
void
look_at_target (struct char_data *ch, char *arg)
{
  int bits, found = 0, j;
  struct char_data *found_char = NULL;
  struct obj_data *obj = NULL, *found_obj = NULL;
  char *desc;

  if (!ch->desc)
    return;

  if (!*arg)
    {
      send_to_char ("Look at what?\r\n", ch);
      return;
    }
  bits = generic_find (arg, FIND_OBJ_INV | FIND_OBJ_ROOM | FIND_OBJ_EQUIP |
		       FIND_CHAR_ROOM, ch, &found_char, &found_obj);

  /* Is the target a character? */
  if (found_char != NULL)
    {
      look_at_char (found_char, ch);
      if (ch != found_char)
	{
	  if (CAN_SEE (found_char, ch))
	    act ("$n looks at you.", TRUE, ch, 0, found_char, TO_VICT);
	  act ("$n looks at $N.", TRUE, ch, 0, found_char, TO_NOTVICT);
	}
      return;
    }
  /* Does the argument match an extra desc in the room? */
  if ((desc = find_exdesc (arg, world[ch->in_room].ex_description)) != NULL)
    {
      page_string (ch->desc, desc, 0);
     return;
   }
   if (world[ch->in_room].special_exit && isname(arg, world[ch->in_room].special_exit->ex_name)) {
     if (world[ch->in_room].special_exit->general_description)
       page_string(ch->desc, world[ch->in_room].special_exit->general_description, FALSE);
     else
       send_to_char("You see nothing special.\r\n", ch);
 
     if (EXIT_FLAGGED(SPEC_EXIT(ch->in_room), EX_CLOSED) && SPEC_EXIT(ch->in_room)->keyword) {
       sprintf(buf, "The %s is closed.\r\n", SPEC_EXIT(ch->in_room)->keyword);
       send_to_char(buf, ch);
     } else if (EXIT_FLAGGED(SPEC_EXIT(ch->in_room), EX_ISDOOR) && SPEC_EXIT(ch->in_room)->keyword) {
       sprintf(buf, "The %s is open.\r\n", SPEC_EXIT(ch->in_room)->keyword);
       send_to_char(buf, ch);
     }
      return;
    }
  /* Does the argument match an extra desc in the char's equipment? */
  for (j = 0; j < NUM_WEARS && !found; j++)
    if (GET_EQ (ch, j) && CAN_SEE_OBJ (ch, GET_EQ (ch, j)))
      if ((desc = find_exdesc (arg, GET_EQ (ch, j)->ex_description)) != NULL)
	{
	  send_to_char (desc, ch);
	  found = 1;
	}
  /* Does the argument match an extra desc in the char's inventory? */

  for (obj = ch->carrying; obj && !found; obj = obj->next_content)
    {
      if (CAN_SEE_OBJ (ch, obj))
	if ((desc = find_exdesc (arg, obj->ex_description)) != NULL)
	  {
	    send_to_char (desc, ch);
	    found = 1;
	  }
    }

  /* Does the argument match an extra desc of an object in the room? */
  for (obj = world[ch->in_room].contents; obj && !found; obj = obj->next_content)
    if (CAN_SEE_OBJ (ch, obj))
      if ((desc = find_exdesc (arg, obj->ex_description)) != NULL)
	{
	  send_to_char (desc, ch);
	  found = 1;
	}
  if (bits)
    {                           /* If an object was found back in
				 * generic_find */
      if (!found)
	show_obj_to_char (found_obj, ch, 5);    /* Show no-description */
      else
	show_obj_to_char (found_obj, ch, 6);    /* Find hum, glow etc */
    }
  else if (!found)
    send_to_char ("You do not see that here.\r\n", ch);
}

ACMD (do_look)
{
  static char arg2[MAX_INPUT_LENGTH];
  int look_type;

  if (!ch->desc)
    return;

  if (GET_POS (ch) < POS_SLEEPING)
    send_to_char ("You can't see anything but stars!\r\n", ch);
  else if (IS_AFFECTED (ch, AFF_BLIND))
    send_to_char ("You can't see a damned thing, you're blind!\r\n", ch);
  else if (IS_DARK (ch->in_room) && !CAN_SEE_IN_DARK (ch))
    {
      send_to_char ("It is pitch black...\r\n", ch);
      /* glowing red eyes */
      if (PRF_FLAGGED (ch, PRF_NOLOOKSTACK))
	list_nostack_char_to_char (world[ch->in_room].people, ch);
	else
	list_char_to_char (world[ch->in_room].people, ch);
    }
  else
    {
      half_chop (argument, arg, arg2);

      if (subcmd == SCMD_READ)
	{
	  if (!*arg)
	    send_to_char ("Read what?\r\n", ch);
	  else
	    look_at_target (ch, arg);
	  return;
	}
      if (!*arg)        /* "look" alone, without an argument at all */
	look_at_room (ch, 1);
      else if (is_abbrev (arg, "in"))
	look_in_obj (ch, arg2);
      /* did the char type 'look <direction>?' */
      else if ((look_type = search_block (arg, dirs, FALSE)) >= 0)
	look_in_direction (ch, look_type);
      else if (is_abbrev (arg, "at"))
	look_at_target (ch, arg2);
      else
	look_at_target (ch, arg);
    }
}



ACMD (do_examine)
{
  int bits;
  int condition;
  struct char_data *tmp_char;
  struct obj_data *tmp_object;

  one_argument (argument, arg);

  if (!*arg)
    {
      send_to_char ("Examine what?\r\n", ch);
      return;
    }
  look_at_target (ch, arg);

  bits = generic_find (arg, FIND_OBJ_INV | FIND_OBJ_ROOM | FIND_CHAR_ROOM |
		       FIND_OBJ_EQUIP, ch, &tmp_char, &tmp_object);

  if (tmp_object)
    {
      if ((GET_OBJ_TYPE (tmp_object) == ITEM_DRINKCON) ||
	  (GET_OBJ_TYPE (tmp_object) == ITEM_FOUNTAIN) ||
	  (GET_OBJ_TYPE (tmp_object) == ITEM_CONTAINER))
	{
	  send_to_char ("When you look inside, you see:\r\n", ch);
	  look_in_obj (ch, arg);
 // don't report condition if npc/pc
 if ((tmp_char) && (!tmp_object)) {
  return;
 }
	}
 // condition reports as a percentage now, so items can have different
 // starting tslots and cslots, eg. for wood and steel

 if (GET_OBJ_TSLOTS(tmp_object))
 condition = (GET_OBJ_CSLOTS(tmp_object) * 100)/GET_OBJ_TSLOTS(tmp_object);
  else
 condition = 0;

 if (condition == 0) {
   sprintf(buf, "It appears to be indestructable!\r\n");
   send_to_char(buf, ch);
   return;
   }
 else
 if (condition <= 10) {
   sprintf(buf, "It appears to be in extremley poor condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
  else
 if (condition <= 20) {
   sprintf(buf, "It appears to be in poor condition.\r\n");
   send_to_char(buf, ch);

   return;
   }
 else
 if (condition <= 30) {
   sprintf(buf, "It appears to be in fair condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 if (condition <= 40) {
   sprintf(buf, "It appears to be in moderate condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 else
 if (condition <= 50) {
   sprintf(buf, "It appears to be in good condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 else
 if (condition <= 60) {
   sprintf(buf, "It appears to be in very good condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 else
 if (condition <= 70) {
   sprintf(buf, "It appears to bein excellent condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 else
 if (condition <= 80) {
   sprintf(buf, "It appears to be in superior condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 else
 if (condition <= 90) {
   sprintf(buf, "It appears to be in extremely superior condition.\r\n");
   send_to_char(buf, ch);
   return;
   }
 else {
   sprintf(buf, "It appears to be as good as new!\r\n");
   send_to_char(buf, ch);
   return;
   }
 }
}



ACMD (do_gold)
{
  if (GET_GOLD (ch) == 0)
    send_to_char ("You're broke!\r\n", ch);
  else if (GET_GOLD (ch) == 1)
    send_to_char ("You have one miserable little gold coin.\r\n", ch);
  else
    {
      sprintf (buf, "You have %s gold coins.\r\n", numdisplay(GET_GOLD (ch)));
      send_to_char (buf, ch);
    }
}

ACMD (do_checkbail)
{
  if (!((!pk_allowed) && (PLR_FLAGGED (ch, PLR_KILLER)) 
      && (ch->in_room == real_room(jail_num)))){
    send_to_char ("You're (happily) not serving a jailterm right now.\r\n", ch);
    return;
  }

  sprintf (buf, "Bail for this offence has been set at %d gold coins.\r\n",
	   GET_BAIL_AMT (ch));
  send_to_char (buf, ch);

}

ACMD (do_mcheck)
{
  if (IS_JURISDICTED(ch->in_room))
    send_to_char ("This area is protected under jurisdiction.\r\n", ch);
  else 
    send_to_char ("This area is not under jurisdiction.\r\n", ch);
}

ACMD (do_worth)
{
  int gold, inven_val, eq_val, j;
  struct obj_data *i;

  gold = GET_GOLD (ch);
  inven_val = 0;
  eq_val = 0;

  for (j = 0; j < NUM_WEARS; j++)
    if (GET_EQ (ch, j))
      eq_val += GET_OBJ_COST( GET_EQ(ch, j) );

  for (i = ch->carrying; i; i = i->next_content)
    inven_val += GET_OBJ_COST(i);

  sprintf (buf, "You have %s gold coins on hand.\r\n", numdisplay(gold));
  send_to_char (buf, ch);
  sprintf (buf, "You have %s gold coins in the bank.\r\n", 
	   numdisplay(GET_BANK_GOLD(ch)));
  send_to_char (buf, ch);
  sprintf (buf, "You have inventory worth %s gold coins.\r\n", 
	   numdisplay(inven_val));
  send_to_char (buf, ch);
  sprintf (buf, "You have equipment worth %s gold coins.\r\n", 
	   numdisplay(eq_val));
  send_to_char (buf, ch);
  if (gold+inven_val+eq_val == 0)
    sprintf (buf, "Ouch! Looks like you're broke!\r\n");
  else
    sprintf (buf, "For a net worth of %s gold coins.\r\n", 
	     numdisplay(gold+GET_BANK_GOLD(ch)+inven_val+eq_val));
  send_to_char (buf, ch);

}

ACMD (do_status) /* 'status' aka new 'score' -- Thargor */
{
  struct time_info_data playing_time;
  struct time_info_data real_time_passed (time_t t2, time_t t1);
  int inven_val, eq_val, j, tnl;
  struct obj_data *i;
  struct char_data *vict = 0;
  char person[20];
  extern int exp_to_level();

  half_chop (argument, buf1, buf2);
  bzero (person, 20);
  if (!*buf1){
    vict = ch;
    sprintf(person, "You are");
  }else{
    if (GET_LEVEL(ch) < LVL_IMMORT){
      vict = ch;
      sprintf(person, "You are");
    }else{
      if (!(vict = get_char_vis(ch, buf1)) || IS_NPC(vict)) {
	send_to_char("Who is that?\r\n", ch);
	return;
      }
      if (GET_LEVEL(ch) < GET_LEVEL(vict)) {
	send_to_char("You can't see the score of people above your level.\r\n", ch);
	return;
      }
      sprintf(buf, "&W***** Score for %s ***** &n\r\n", GET_NAME(vict));
      send_to_char(buf, ch);
      sprintf(person, "%s is", GET_NAME(vict));
    }
  }

  inven_val = 0;
  eq_val = 0;
  for (j = 0; j < NUM_WEARS; j++)
    if (GET_EQ (vict, j))
      eq_val += GET_OBJ_COST( GET_EQ(vict, j) );

  for (i = vict->carrying; i; i = i->next_content)
    inven_val += GET_OBJ_COST(i);

  playing_time = real_time_passed ((time (0) - vict->player.time.logon) +
				   vict->player.time.played, 0);

  sprintf(buf, "&G%-10s:&n %s %s &G(&nlevel %d&G)&n.\r\n", person,
	       GET_NAME (vict), GET_TITLE (vict), GET_LEVEL (vict));

  sprintf(buf, "%s&GClass     :&n ", buf);

  if ((GET_LEVEL(vict) == LVL_HERO)) {
    sprintf(buf, "%s%s %s, hero of %s.\r\n", buf,
       race_name(vict), class_name(vict), town_name(vict));
  } else {
  if ((GET_LEVEL(vict) == LVL_IMMORT)) {
    sprintf(buf, "%s%s %s, immortal of %s.\r\n", buf,
       race_name(vict), class_name(vict), town_name(vict));
  } else {
  if ((GET_LEVEL(vict) == LVL_DEMIGOD)) {
    sprintf(buf, "%s%s %s, Demi God of %s.\r\n", buf,
       race_name(vict), class_name(vict), town_name(vict));
  } else {
  if ((GET_LEVEL(vict) >= LVL_GOD)) {
    sprintf(buf, "%s%s %s, God of %s.\r\n", buf,
	race_name(vict), class_name(vict), town_name(vict));
  } else {
  sprintf(buf, "%s%s %s, citizen of %s.\r\n", buf,
 race_name(vict), class_name(vict), town_name(vict));
     }
    }
   }
 }
  sprintf(buf, "%s&GAge       :&n %d years, %d months, %d days, "
	  "%d hours old.\r\n", buf,
	   (int) age (vict).year, age (vict).month, age (vict).day, age (vict).hours);

  if ((age (vict).month == 0) && (age (vict).day == 0))
   strcat (buf, "&GBirthday  :&w It's your birthday today. Happy Birthday!&n\r\n");

  sprintf (buf, "%s&GPlaytime  :&n %-2d days and %-2d hours    "
	   "&GQuest Pts    :&n %s\r\n",buf,
	   playing_time.day, playing_time.hours,
	   numdisplay(GET_QUESTPOINTS (vict)));

  sprintf (buf, "%s&GHeight    :&n %-3d centimeters         "
	   "&GWeight       :&n %-3d pounds\r\n",buf,
	   GET_HEIGHT (vict), GET_WEIGHT (vict));

  
  sprintf(buf, "%s&b---------------------------------------------------------------------&n\r\n",buf);
  
  sprintf (buf, "%s&CHit Pts   :&n %-5s ", buf,
	   numdisplay(GET_HIT (vict)));
  sprintf (buf, "%s(max: %-5s)      ", buf,
	   numdisplay(GET_MAX_HIT (vict)));
  sprintf (buf, "%s&CMana Pts     :&n %-5s ", buf,
	   numdisplay(GET_MANA (vict)));
  sprintf (buf, "%s(max: %-5s)\r\n", buf,
	   numdisplay(GET_MAX_MANA (vict)));

  sprintf (buf, "%s&CMove Pts  :&n %-5s ", buf,
	   numdisplay(GET_MOVE (vict)));
  sprintf (buf, "%s(max: %-5s)      ", buf,
	   numdisplay(GET_MAX_MOVE (vict)));
  sprintf (buf, "%s&CAlignment    :&n %s \r\n",buf,
	   numdisplay(GET_ALIGNMENT (vict)));

   tnl = exp_to_level(GET_LEVEL(vict)) - GET_EXP(vict);
  sprintf (buf, "%s&CExp Pts   :&n %-12s            ",buf, 
	   numdisplay(GET_EXP(vict)));
  sprintf (buf, "%s&CExp to Level :&n %s \r\n", buf,
	   (GET_LEVEL(vict)<100)? numdisplay(tnl):"N/A");

  sprintf (buf, "%s&CDefense   :&n %-4d                    "
	   "&CGold on Hand :&n %s \r\n",buf, 
	   (int) GET_DEFENSE(vict), numdisplay(GET_GOLD(vict)));

  sprintf (buf, "%s&CEq Worth  :&n %-15s         ", buf,
	   numdisplay(eq_val+inven_val));
  sprintf (buf, "%s&CGold in Bank :&n %s \r\n",buf, 
	   numdisplay(GET_BANK_GOLD(vict)));

  sprintf (buf, "%s&CArena Wins:&n %-3d                     ", buf,
	   GET_ARENAWINS(vict));
  sprintf (buf, "%s&CArena Losses :&n %d \r\n",buf, 
	   GET_ARENALOSSES(vict));

  sprintf (buf, "%s&yHometown  :&n %-3s                ", buf,
	   town_name(vict));
  sprintf (buf, "%s&yDeity        :&n %s \r\n",buf, 
	   deity_name(vict));
  sprintf(buf, "%s&b---------------------------------------------------------------------&n\r\n",buf);

  sprintf (buf, "%s&YStrength  :&n %-2d/%-4d    "
	   "&YIntelligence  :&n %-2d         "
	   "&YWisdom   :&n %-2d\r\n",
	   buf,
	   GET_STR (vict), GET_ADD(vict), GET_INT (vict), GET_WIS (vict));

  sprintf (buf, "%s&YDexterity :&n %-2d         "
	   "&YConstitution  :&n %-2d         "
	   "&YCharisma :&n %-2d\r\n",
	   buf,
	   GET_DEX (vict), GET_CON (vict), GET_CHA (vict));

  sprintf(buf, "%s&b---------------------------------------------------------------------&n\r\n",buf);

  switch (GET_POS (vict))
    {
    case POS_DEAD:
      sprintf (buf, "%s%s DEAD!\r\n", buf, person);
      break;
    case POS_MORTALLYW:
      sprintf (buf, "%s%s mortally wounded!  You should seek help!\r\n", 
	      buf, person);
      break;
    case POS_INCAP:
      sprintf (buf, "%s%s incapacitated, slowly fading away...\r\n", 
	       buf, person);
      break;
    case POS_STUNNED:
      sprintf (buf, "%s%s stunned! Unable to move!\r\n", buf, person);
      break;
    case POS_SLEEPING:
      sprintf (buf, "%s%s sleeping.\r\n", buf, person);
      break;
    case POS_RESTING:
      sprintf (buf, "%s%s resting.\r\n", buf, person);
      break;
    case POS_SITTING:
      sprintf (buf, "%s%s sitting.\r\n", buf, person);
      break;
    case POS_FIGHTING:
      if (FIGHTING (vict))
	sprintf (buf, "%s%s fighting %s.\r\n", buf, person, 
		 PERS (FIGHTING (vict), vict));
      else
	sprintf (buf, "%s%s fighting thin air.\r\n", buf, person);
      break;
    case POS_STANDING:
      sprintf (buf, "%s%s standing.\r\n", buf, person);
      break;
    case POS_MEDITATING:
      sprintf (buf, "%s%s meditating.\r\n", buf, person);
      break;
    default:
      sprintf (buf, "%s%s floating.\r\n", buf, person);
      break;
    }

  if (GET_COND (vict, DRUNK) > 4)
    sprintf (buf, "%s%s intoxicated.\r\n", buf, person);

  if (GET_COND (vict, FULL) <= 0 && GET_COND(vict, FULL) > -24)
    sprintf (buf, "%s%s hungry.\r\n", buf, person);
  else
  if (GET_COND (vict, FULL) < -23 && GET_COND (vict, FULL)!=-100)
    sprintf(buf, "%s%s extremely hungry.\r\n", buf, person);

  if (GET_COND (vict, THIRST) <= 0 && GET_COND(vict, THIRST) > -12)
    sprintf (buf, "%s%s thirsty.\r\n", buf, person);
  else
  if (GET_COND (vict, THIRST) < -12 && GET_COND (vict, THIRST)!=-100)
    sprintf(buf, "%s%s extremely thirsty.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_BLIND))
    sprintf (buf, "%s%s blinded!\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_INVISIBLE))
    sprintf (buf, "%s%s invisible.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_DETECT_ALIGN))
    sprintf (buf, "%s%s able to see other people's auras.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_DETECT_INVIS))
    sprintf (buf, "%s%s sensitive to the presence of invisible things.\r\n", 
	     buf, person);

  if (IS_AFFECTED (vict, AFF_SENSE_LIFE))
    sprintf (buf, "%s%s sensitive to the presence of life.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_CURSE))
    sprintf (buf, "%s%s under someone's curse!\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_SLEEP))
    sprintf (buf, "%s%s sleeping and are unable to wake up.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_SANCTUARY))
    sprintf (buf, "%s%s protected by Sanctuary.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_CONVERGENCE))
    sprintf (buf, "%s%s able to converge %s magic power.\r\n", buf, person,
	    (ch == vict)? "your":HSHR(vict));

  if (IS_AFFECTED (vict, AFF_AUTUS))
    sprintf (buf, "%s%s able to conserve %s mana use.\r\n", buf, person,
	    (ch == vict)? "your":HSHR(vict));

  if (IS_AFFECTED (vict, AFF_NOPORTAL))
    sprintf (buf, "%s%s protected by the heavens from mortal portals.\r\n", 
	     buf,person);

  if (IS_AFFECTED (vict, AFF_POISON))
    sprintf (buf, "%s%s poisoned!\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_PLAGUED))
    sprintf (buf, "%s%s sick with the plague!\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_CHARM))
    sprintf (buf, "%s%s%s been charmed!\r\n", buf,
	    (ch == vict)? "You":CHSSH(vict), (ch == vict)? " have" : " has");

  if (affected_by_spell (vict, SPELL_ARMOR))
    sprintf (buf, "%s%s%s protected.\r\n", buf,
	    (ch == vict)? "You":CHSSH(vict), (ch == vict)? " feel" : " feels");

  if (IS_AFFECTED (vict, AFF_INFRAVISION))
    sprintf (buf, "%s%s eyes are glowing red.\r\n", buf,
	    (ch == vict)? "Your":CHSHR(vict));

  if (IS_AFFECTED (vict, AFF_CHAINED))
    sprintf (buf, "%s%s feet are chained together!\r\n", buf, (ch == vict)? "Your":CHSHR(vict));

  if (PRF_FLAGGED (vict, PRF_SUMMONABLE))
    sprintf (buf, "%s%s summonable by other players.\r\n", buf, person);

  if (IS_AFFECTED (vict, AFF_SNEAK))
    sprintf (buf, "%s%s currently sneaking about.\r\n", buf, person);
  
  if (IS_AFFECTED (vict, AFF_HIDE))
    sprintf (buf, "%s%s currently hidden from plain sight.\r\n", buf, person);
  
  send_to_char (buf, ch);
}

ACMD (do_inventory)
{
  send_to_char ("You are carrying:\r\n", ch);
  list_obj_to_char (ch->carrying, ch, 1, TRUE);
}


ACMD (do_equipment)
{
  int i, found = 0;

  send_to_char ("You are using:\r\n", ch);
  for (i = 0; i < NUM_WEARS; i++)
    {
      if (GET_EQ (ch, i))
	{
	  if (CAN_SEE_OBJ (ch, GET_EQ (ch, i)))
	    {
	      send_to_char (where[i], ch);
	      show_obj_to_char (GET_EQ (ch, i), ch, 1);
	      found = TRUE;
	    }
	  else
	    {
	      send_to_char (where[i], ch);
	      send_to_char ("Something.\r\n", ch);
	      found = TRUE;
	    }
	}
    }
  if (!found)
    {
      send_to_char (" Nothing.\r\n", ch);
    }
}


ACMD (do_time)
{
  char *suf;
  int weekday, day;
  extern struct time_info_data time_info;
  extern const char *weekdays[];
  extern const char *month_name[];

  sprintf (buf, "It is %d o'clock %s, on ",
	   ((time_info.hours % 12 == 0) ? 12 : ((time_info.hours) % 12)),
	   ((time_info.hours >= 12) ? "pm" : "am"));

  /* 35 days in a month */
  weekday = ((35 * time_info.month) + time_info.day + 1) % 7;

  strcat (buf, weekdays[weekday >= 0 && weekday < 7 ? weekday : 0]);
  strcat (buf, "\r\n");
  send_to_char (buf, ch);

  day = time_info.day + 1;      /* day in [1..35] */

  if (day == 1)
    suf = "st";
  else if (day == 2)
    suf = "nd";
  else if (day == 3)
    suf = "rd";
  else if (day < 20)
    suf = "th";
  else if ((day % 10) == 1)
    suf = "st";
  else if ((day % 10) == 2)
    suf = "nd";
  else if ((day % 10) == 3)
    suf = "rd";
  else
    suf = "th";

  sprintf (buf, "The %d%s Day of the %s, Year %d.\r\n",
	   day, suf, month_name[time_info.month < 17 && time_info.month >= 0 ? (int) time_info.month : 0],
           (int) time_info.year);

  send_to_char (buf, ch);
}

struct help_index_element *find_help(char *keyword)
{
  int i;

  for (i = 0; i < top_of_helpt; i++)
    if (isname(keyword, help_table[i].keywords))
      return (help_table + i);   

  return NULL;  
}

ACMD(do_help) 
{
  struct help_index_element *this_help;
  char entry[MAX_STRING_LENGTH];

  if (!ch->desc)
    return;

  skip_spaces(&argument);

  if (!*argument) {
    page_string(ch->desc, help, 0);
    return;
  }
 if (!help_table) {
    send_to_char("No help available.\r\n", ch);
    return;
  }

  if (!(this_help = find_help(argument))) {
      send_to_char("There is no help on that word.\r\n", ch);
    sprintf(buf, "HELP: %s tried to get help on %s", GET_NAME(ch), argument);
    log(buf);
      return;
    }
    
  if (this_help->min_level > GET_LEVEL(ch)) {
    send_to_char("There is no help on that word.\r\n", ch);
    return;
  }
    
  sprintf(entry, "%s\r\n%s", this_help->keywords, this_help->entry);

  page_string(ch->desc, entry, 0);
      
}


/*                                                          *
 * This hardly deserves it's own function, but I see future *
 * expansion capabilities...                                *
 *                                               TR 8-18-98 *
 *                                          Fixed:  8-25-98 */
int show_on_who_list(struct descriptor_data *d)
{  
  if ((STATE(d) != CON_PLAYING) && (STATE(d) != CON_MEDIT) &&
       (STATE(d) != CON_OEDIT) && (STATE(d) != CON_REDIT) &&
       (STATE(d) != CON_SEDIT) && (STATE(d) != CON_ZEDIT) &&
       (STATE(d) != CON_HEDIT) && (STATE(d) != CON_AEDIT) &&
       (STATE(d) != CON_TEXTED) && (STATE(d) != CON_TRIGEDIT))
    return 0;
  else
    return 1;
}


/* compare function used by do_who via quicksort */
int whocmp(int w1, int w2)
{

  if (w1 < w2)
    return -1;
  else if (w1 > w2)
    return 1;
  else
    return 0;
}

/*********************************************************************
* New 'do_who' by Daniel Koepke [aka., "Argyle Macleod"] of The Keep *
******************************************************************* */

char *WHO_USAGE =
"Usage: who [minlev[-maxlev]] [-n name] [-c classes] [-rzqimo] [-a]\r\n"
"\r\n"
"Classes: (M)age, (C)leric, (T)hief, (W)arrior\r\n"
"\r\n"
" Switches: \r\n"
"_.,-'^'-,._\r\n"
"\r\n"
"  -r = who is in the current room\r\n"
"  -z = who is in the current zone\r\n"
"  -a = who is in the arena\r\n"
"\r\n"
"  -q = only show autoquesters\r\n"
"  -i = only show immortals\r\n"
"  -m = only show mortals\r\n"
"  -o = only show outlaws\r\n"
"\r\n";

ACMD (do_who)
     /* NOTE NOTE NOTE NOTE NOTE */
     /* This is now the new who! */
     /* ======================== */
     /* If the new do_who causes core dumps tomorrow, rename this 
	function to do_NEWwho, AND rename the function 
	way above (like ~100 lines back) from do_OLDwho to do_who.
	-Thargor-
     */
{
  struct descriptor_data *d;
  struct char_data *wch;
  char Imm_buf[MAX_STRING_LENGTH];
  char Mort_buf[MAX_STRING_LENGTH];
  char name_search[MAX_NAME_LENGTH + 1];
  struct who_list immlist[100];
  struct who_list mortlist[100];  
  char mode;
  int low = 0, high = LVL_IMPL, showclass = 0;
  bool who_room = FALSE, who_zone = FALSE, who_quest = 0, who_arena = FALSE;
  bool outlaws = FALSE, noimm = FALSE, nomort = FALSE, who_fight = FALSE;
//  int clan_num;
  int j;
  int Wizards = 0, Mortals = 0, ismortal = 1;
  size_t i;

  const char *WizLevels[LVL_IMPL - (LVL_IMMORT - 1)] =
  {
    "    Immortal   ",
    "     Sage      ",
    "     Seer      ",
    "    Prophet    ",
    "* Implementor *"
  };

  skip_spaces (&argument);
  strcpy (buf, argument);
  name_search[0] = '\0';

  /* the below is from stock CircleMUD -- found no reason to rewrite it */
  while (*buf)
    {
      half_chop (buf, arg, buf1);
      if (isdigit (*arg))
	{
	  sscanf (arg, "%d-%d", &low, &high);
	  strcpy (buf, buf1);
	}
      else if (*arg == '-')
	{
	  mode = *(arg + 1);    /* just in case; we destroy arg in the switch */
	  switch (mode)
	    {
	    case 'f':
	      who_fight = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'o':
	      outlaws = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'z':
	      who_zone = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'q':
	      who_quest = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'l':
	      half_chop (buf1, arg, buf);
	      sscanf (arg, "%d-%d", &low, &high);
	      break;
	    case 'n':
	      half_chop (buf1, name_search, buf);
	      break;
	    case 'a':
	      who_arena = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'r':
	      who_room = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'c':
	      half_chop (buf1, arg, buf);
	      for (i = 0; i < strlen (arg); i++)
		showclass |= find_class_bitvector (arg[i]);
	      break;
	    case 'i':
	      nomort = TRUE;
	      strcpy (buf, buf1);
	      break;
	    case 'm':
	      noimm = TRUE;
	      strcpy (buf, buf1);
	      break;
	    default:
	      send_to_char (WHO_USAGE, ch);
	      return;
	      break;
	    }                   /* end of switch */

	}
      else
	{                       /* endif */
	  send_to_char (WHO_USAGE, ch);
	  return;
	}
    }                           /* end while (parser) */

  strcpy (Imm_buf,  "Immortals Currently Online\r\n&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&n\r\n");
  strcpy (Mort_buf, "Mortals Currently Online\r\n&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&g-&y-&n\r\n");

  for (d = descriptor_list; d; d = d->next)
    {
//    if (!show_on_who_list(d))
//      continue;
      if (d->connected)
	continue;
      if (d->original)
	wch = d->original;
      else if (!(wch = d->character))
	continue;

      if (!CAN_SEE (ch, wch))
	continue;
      if (GET_LEVEL (wch) < low || GET_LEVEL (wch) > high)
	continue;
      if ((noimm && GET_LEVEL (wch) >= LVL_IMMORT) || (nomort && GET_LEVEL (wch) < LVL_IMMORT))
	continue;
      if (*name_search && str_cmp (GET_NAME (wch), name_search) && !strstr (GET_TITLE (wch), name_search))
	continue;
      if (outlaws && !PLR_FLAGGED (wch, PLR_KILLER) && !PLR_FLAGGED (wch, PLR_THIEF))
	continue;
      if (who_quest && !PLR_FLAGGED (wch, PLR_QUESTOR))
	continue;
      if (who_zone && world[ch->in_room].zone != world[wch->in_room].zone)
	continue;
      if (who_room && (wch->in_room != ch->in_room))
	continue;
      if (who_arena && (!(IS_ARENACOMBATANT(wch))))
	continue;
      if (showclass && !(showclass & (1 << GET_CLASS (wch))))
	continue;

      if (GET_LEVEL (wch) >= LVL_IMMORT)
	{
	  ismortal = 0;
	  sprintf (buf2, "%s[%s] %s %s&n", CCYEL (ch, C_SPR),
		   WizLevels[GET_LEVEL (wch) - LVL_IMMORT], GET_NAME (wch),
		   GET_TITLE (wch));      
	}
      else
	{
      if (GET_LEVEL (wch) == LVL_HERO)
	{
	  ismortal = 1;
       if ((GET_LEVEL(wch) < LVL_IMMORT) && (GET_CITIZEN(wch) > 0))
	  sprintf (buf2, "&B[&n100 &YHero &B]%s %s%s%s %s&n",
(_clrlevel(ch) >= C_NRM ? citizen_colors[GET_CITIZEN(wch)] : CCNRM(ch, C_NRM)), READ_CITIZEN(wch),
		   CCNRM(ch,C_NRM), GET_NAME (wch), GET_TITLE (wch));
	 else
	  sprintf (buf2, "&B[&n100 &YHero &B]%s %s %s&n",
		   CCNRM(ch,C_NRM), GET_NAME (wch), GET_TITLE (wch));
	}
       
      else
	{
	  ismortal = 1;
       if ((GET_LEVEL(wch) < LVL_IMMORT) && (GET_CITIZEN(wch) > 0))
	  sprintf (buf2, "&B[&n%2d %s %s&B]%s %s%s%s %s&n", GET_LEVEL (wch),
		   RACE_ABBR (wch), CLASS_ABBR (wch),
(_clrlevel(ch) >= C_NRM ? citizen_colors[GET_CITIZEN(wch)] : CCNRM(ch, C_NRM)), READ_CITIZEN(wch),
		   CCNRM(ch,C_NRM), GET_NAME (wch), GET_TITLE (wch));
	else
	  sprintf (buf2, "&B[&n%2d %s %s&B]%s %s %s&n", GET_LEVEL (wch),
		   RACE_ABBR (wch), CLASS_ABBR (wch),
		   CCNRM(ch,C_NRM), GET_NAME (wch), GET_TITLE (wch));
	}
      }
      *buf = '\0';              /* **BUG FIX: Revision 2** */


      if (GET_INVIS_LEV (wch))
	sprintf (buf2, "%s (i%d)", buf2, (int) GET_INVIS_LEV (wch));
      else if (IS_AFFECTED (wch, AFF_INVISIBLE))
	strcat (buf2, " (invis)");

      if (PLR_FLAGGED (wch, PLR_MAILING))
	strcat (buf2, " (mailing)");
      else if ((STATE(d) >= CON_OEDIT) && (STATE(d) <= CON_TEXTED))
	strcat(buf, " (OLC)");
      else if (PLR_FLAGGED (wch, PLR_WRITING))
	strcat (buf2, " (writing)");
      if (PRF_FLAGGED (wch, PRF_AFK))
	strcat (buf2, " (away)");
      if (PRF_FLAGGED (wch, PRF_DEAF))
	strcat (buf2, " (deaf)");
      if (PRF_FLAGGED (wch, PRF_NOTELL))
	strcat (buf2, " (notell)");
      if (PRF2_FLAGGED (wch, PRF2_QCHAN))
	strcat (buf2, " (qchan)");
      if (PLR_FLAGGED (wch, PLR_QUESTOR))
	strcat (buf2, " (autoquest)");
      if (PLR_FLAGGED (wch, PLR_THIEF))
	strcat (buf2, " (&RTHIEF&n)");
      if (PLR_FLAGGED (wch, PLR_KILLER))
	strcat (buf2, " (&RKILLER&n)");
      if (PRF2_FLAGGED (wch, PRF2_MBUILDING))
	strcat (buf2, " (&Ybuilding&n)");
      if (PRF2_FLAGGED (wch, PRF2_INTANGIBLE) && !PRF2_FLAGGED(wch, PRF2_MBUILDING))
	strcat (buf2, " (&Kdead&n)");
      if (GET_LEVEL (wch) >= LVL_IMMORT)
	strcat (buf2, CCNRM (ch, C_SPR));
      if (who_arena)
	sprintf (buf2, "%s (Arena Wins/Losses: %d/%d)", buf2,
		 GET_ARENAWINS(wch), GET_ARENALOSSES(wch));

      if (who_fight && FIGHTING(wch) != NULL && GET_LEVEL(ch) >= LVL_IMMORT
	  && GET_LEVEL(wch) < LVL_IMMORT){
	sprintf (buf2,"%s (Fighting %s)", buf2, GET_NAME(FIGHTING(wch)));
      }
      /* hardcoded - which isn't good. but what the heck :) */
      if ((wch->in_room == real_room(2200)
	  || wch->in_room == real_room(2201)
	  || wch->in_room == real_room(2202)
	  || wch->in_room == real_room(2203)
	  || wch->in_room == real_room(2204)
	  || wch->in_room == real_room(2205)
	  || wch->in_room == real_room(2206)
	  || wch->in_room == real_room(2207)
	  || wch->in_room == real_room(2208)
	  || wch->in_room == real_room(2209)
	  || wch->in_room == real_room(2210)
	  || wch->in_room == real_room(2211)
	  || wch->in_room == real_room(2212)
	  || wch->in_room == real_room(2213)
	  || wch->in_room == real_room(2214)
	  || wch->in_room == real_room(2215)
	  || wch->in_room == real_room(2216))
	  && GET_LEVEL(wch) < LVL_IMMORT)
	strcat (buf2, " (In Newbie School)");
      
      if (GET_CLAN(wch) > -1 && GET_CLAN_RANK(wch) > 0)
	sprintf(buf2+strlen(buf2), " &B[&n%s of %s&B]", clan[GET_CLAN(wch)].rank_name[GET_CLAN_RANK(wch)-1], clan[GET_CLAN(wch)].name);
      strcat (buf2, "&n\r\n\0");

      /* Thargor - alright let's update the respective who_lists */
      if (!ismortal){
	/*
	immlist[Wizards] = malloc(sizeof(struct who_list));
	if (immlist[Wizards] == NULL){
	  mudlog("DEBUG:Malloc failed for immlist at do_who().", PFT,
		 LVL_IMPL, TRUE);
	}else{
	  immlist[Wizards]->level = GET_LEVEL(wch);
	  immlist[Wizards]->desc = strdup(buf2);
	  Wizards++;
	} 
	*/
	immlist[Wizards].level = GET_LEVEL(wch);
	if (strlen(buf2) > SMALL_BUFSIZE){
	  mudlog ("DEBUG: buf2 is too long at do_who().", PFT, LVL_IMPL, TRUE);
	}else{
	  bzero(immlist[Wizards].desc, SMALL_BUFSIZE);
	  strcpy(immlist[Wizards].desc, buf2);
	  Wizards++;
	}
      }else{
	mortlist[Mortals].level = GET_LEVEL(wch);
	if (strlen(buf2) > SMALL_BUFSIZE){
	  mudlog ("DEBUG: buf2 is too long at do_who().", PFT, LVL_IMPL, TRUE);
	}else{
	  bzero(mortlist[Mortals].desc, SMALL_BUFSIZE);
	  strcpy(mortlist[Mortals].desc, buf2);
	  Mortals++;
	}

      }   

    }                           /* end of for */

  /* Thargor - Alright, now let's sort the whole dang shawamp thingy */ 
  quicksort((void *) immlist, 0, Wizards-1, whocmp);
  quicksort((void *) mortlist, 0, Mortals-1, whocmp);

  /* finally update the actual buffers used for printout */
  /*
  for (j = (Wizards-1); j >= 0; j--){
    if (immlist[j]->desc != NULL){
      sprintf(Imm_buf, "%s%s", Imm_buf, immlist[j]->desc);
      free(immlist[j]->desc);
    }
    if (immlist[j] != NULL)
      free(immlist[j]);
  }
  */
  for (j = (Wizards-1); j >= 0; j--){
    if (strlen(immlist[j].desc) > 0)
      sprintf(Imm_buf, "%s%s", Imm_buf, immlist[j].desc);
  }

  /*
  for (j = (Mortals-1); j >= 0; j--){
    if (mortlist[j]->desc != NULL){
      sprintf(Mort_buf, "%s%s", Mort_buf, mortlist[j]->desc);
      free(mortlist[j]->desc);
    }
    if (mortlist[j] != NULL)
      free(mortlist[j]);
  }
  */
  for (j = (Mortals-1); j >= 0; j--){
    if (strlen(mortlist[j].desc) > 0)
      sprintf(Mort_buf, "%s%s", Mort_buf, mortlist[j].desc);
  }

//  send_to_char ("       &c-&G=&c-&G=&c- &mD &YE &mL &YT &mA &YN &mI &YA &c-&G=&c-&G=&c-&n"
//              "\r\n\r\n",ch);

  if (Wizards)
    {
      page_string (ch->desc, Imm_buf, 0);
      send_to_char ("\r\n", ch);
    }

  if (Mortals)
    {
      page_string (ch->desc, Mort_buf, 0);
      send_to_char ("\r\n", ch);
    }

  if ((Wizards + Mortals) == 0)
    strcpy (buf, "No wizards or mortals are currently visible to you.\r\n");
  if (Wizards)
    sprintf (buf, "There %s %d visible immortal%s%s", (Wizards == 1 ? "is" : "are"), Wizards, (Wizards == 1 ? "" : "s"), (Mortals ? " and there" : "."));
  if (Mortals)
    sprintf (buf, "%s %s %d visible mortal%s.", (Wizards ? buf : "There"), (Mortals == 1 ? "is" : "are"), Mortals, (Mortals == 1 ? "" : "s"));
  strcat (buf, "\r\n");

  if ((Wizards + Mortals) > boot_high)
    boot_high = Wizards + Mortals;
  sprintf (buf, "%sThere is a boot time high of %d player%s.\r\n", buf, boot_high, (boot_high == 1 ? "" : "s"));
  send_to_char (buf, ch);
} /* end do_who */


#define USERS_FORMAT \
"format: users [-l minlevel[-maxlevel]] [-n name] [-h host] [-c classlist] [-o] [-p]\r\n"

ACMD (do_users)
{
  extern char *connected_types[];
  char line[200], line2[220], idletime[10], classname[20];
  char state[30], *timeptr, *format, mode;
  char name_search[MAX_INPUT_LENGTH], host_search[MAX_INPUT_LENGTH];
  struct char_data *tch;
  struct descriptor_data *d;
  size_t i;
  int low = 0, high = LVL_IMPL, num_can_see = 0;
  int showclass = 0, outlaws = 0, playing = 0, deadweight = 0;

  host_search[0] = name_search[0] = '\0';

  strcpy (buf, argument);
  while (*buf)
    {
      half_chop (buf, arg, buf1);
      if (*arg == '-')
	{
	  mode = *(arg + 1);    /* just in case; we destroy arg in the switch */
	  switch (mode)
	    {
	    case 'o':
	    case 'k':
	      outlaws = 1;
	      playing = 1;
	      strcpy (buf, buf1);
	      break;
	    case 'p':
	      playing = 1;
	      strcpy (buf, buf1);
	      break;
	    case 'd':
	      deadweight = 1;
	      strcpy (buf, buf1);
	      break;
	    case 'l':
	      playing = 1;
	      half_chop (buf1, arg, buf);
	      sscanf (arg, "%d-%d", &low, &high);
	      break;
	    case 'n':
	      playing = 1;
	      half_chop (buf1, name_search, buf);
	      break;
	    case 'h':
	      playing = 1;
	      half_chop (buf1, host_search, buf);
	      break;
	    case 'c':
	      playing = 1;
	      half_chop (buf1, arg, buf);
	      for (i = 0; i < strlen (arg); i++)
		showclass |= find_class_bitvector (arg[i]);
	      break;
	    default:
	      send_to_char (USERS_FORMAT, ch);
	      return;
	      break;
	    }                   /* end of switch */

	}
      else
	{                       /* endif */
	  send_to_char (USERS_FORMAT, ch);
	  return;
	}
    }                           /* end while (parser) */
  strcpy (line,
	  "Num Class   Name         State          Idl Login@   Site\r\n");
  strcat (line,
	  "--- ------- ------------ -------------- --- -------- ------------------------\r\n");
  send_to_char (line, ch);

  one_argument (argument, arg);

  for (d = descriptor_list; d; d = d->next)
    {
      if (d->connected && playing)
	continue;
      if (!d->connected && deadweight)
	continue;
      if (!d->connected)
	{
	  if (d->original)
	    tch = d->original;
	  else if (!(tch = d->character))
	    continue;

	  if (*host_search && !strstr (d->host, host_search))
	    continue;
	  if (*name_search && str_cmp (GET_NAME (tch), name_search))
	    continue;
	  if (!CAN_SEE (ch, tch) || GET_LEVEL (tch) < low || GET_LEVEL (tch) > high)
	    continue;
	  if (outlaws && !PLR_FLAGGED (tch, PLR_KILLER) &&
	      !PLR_FLAGGED (tch, PLR_THIEF))
	    continue;
	  if (showclass && !(showclass & (1 << GET_CLASS (tch))))
	    continue;
	  if (GET_INVIS_LEV (ch) > GET_LEVEL (ch))
	    continue;

	  if (d->original)
	    sprintf (classname, "[%2d %s]", GET_LEVEL (d->original),
		     CLASS_ABBR (d->original));
	  else
	    sprintf (classname, "[%2d %s]", GET_LEVEL (d->character),
		     CLASS_ABBR (d->character));
	}
      else
	strcpy (classname, "   -   ");

      timeptr = asctime (localtime (&d->login_time));
      timeptr += 11;
      *(timeptr + 8) = '\0';

      if (!d->connected && d->original)
	strcpy (state, "Switched");
      else
	strcpy (state, connected_types[d->connected]);

      if (d->character && !d->connected && GET_LEVEL (d->character) < LVL_GOD)
	sprintf (idletime, "%3d", d->character->char_specials.timer *
		 SECS_PER_MUD_HOUR / SECS_PER_REAL_MIN);
      else
	strcpy (idletime, "");

      format = "%3d %-7s %-12s %-14s %-3s %-8s ";

      if (d->character && d->character->player.name)
	{
	  if (d->original)
	    sprintf (line, format, d->desc_num, classname,
		     d->original->player.name, state, idletime, timeptr);
	  else
	    sprintf (line, format, d->desc_num, classname,
		     d->character->player.name, state, idletime, timeptr);
	}
      else
	sprintf (line, format, d->desc_num, "   -   ", "UNDEFINED",
		 state, idletime, timeptr);

      if (d->host && *d->host)
	sprintf (line + strlen (line), "[%s]\r\n", d->host);
      else
	strcat (line, "[Hostname unknown]\r\n");

      if (d->connected)
	{
	  sprintf (line2, "%s%s%s", CCGRN (ch, C_SPR), line, CCNRM (ch, C_SPR));
	  strcpy (line, line2);
	}
      if (d->connected || (!d->connected && CAN_SEE (ch, d->character)))
	{
	  send_to_char (line, ch);
	  num_can_see++;
	}
    }

  sprintf (line, "\r\n%d visible sockets connected.\r\n", num_can_see);
  send_to_char (line, ch);
}


/* Generic page_string function for displaying text */
ACMD (do_gen_ps)
{
  extern char circlemud_version[];

  switch (subcmd)
    {
    case SCMD_CREDITS:
      page_string (ch->desc, credits, 0);
      break;
    case SCMD_NEWS:
      page_string (ch->desc, news, 0);
      break;
    case SCMD_INFO:
      page_string (ch->desc, info, 0);
      break;
    case SCMD_WIZLIST:
//      page_string (ch->desc, wizlist, 0);
      send_to_char(wizlist, ch);
      break;
    case SCMD_IMMLIST:
//      page_string (ch->desc, immlist, 0);
      send_to_char(immlist, ch);
      break;
    case SCMD_HANDBOOK:
      page_string (ch->desc, handbook, 0);
      break;
    case SCMD_POLICIES:
      page_string (ch->desc, policies, 0);
      break;
    case SCMD_MOTD:
      page_string (ch->desc, motd, 0);
      break;
    case SCMD_IMOTD:
      page_string (ch->desc, imotd, 0);
      break;
    case SCMD_CLEAR:
      send_to_char ("\033[H\033[J", ch);
      break;
    case SCMD_VERSION:
      send_to_char (circlemud_version, ch);
      send_to_char(strcat(strcpy(buf, DG_SCRIPT_VERSION), "\r\n"), ch);
      break;
    case SCMD_WHOAMI:
      send_to_char (strcat (strcpy (buf, GET_NAME (ch)), "\r\n"), ch);
      break;
    case SCMD_CIRCLEMUD:
      page_string (ch->desc, circlemud, 0);
      break;
    default:
      return;
      break;
    }
}


void
perform_mortal_where (struct char_data *ch, char *arg)
{
  register struct char_data *i;
  register struct descriptor_data *d;

  if (IS_DARK (ch->in_room) && !CAN_SEE_IN_DARK (ch))
    {
      send_to_char ("It is pitch black...\r\n", ch);
      return;
    }
  else
    {
      if (IS_AFFECTED (ch, AFF_BLIND))
	{
	  send_to_char ("You can't see a damned thing, you're blind!\r\n", ch);
	  return;
	}
    }
  if (!*arg)
    {
      send_to_char ("Players in your Zone\r\n--------------------\r\n", ch);
      for (d = descriptor_list; d; d = d->next)
	if (!d->connected)
	  {
	    i = (d->original ? d->original : d->character);
	    if (i && CAN_SEE (ch, i) && (i->in_room != NOWHERE) &&
		(world[ch->in_room].zone == world[i->in_room].zone))
	      {
		sprintf (buf, "&Y%-20s &R-&n %s\r\n", GET_NAME (i), world[i->in_room].name);
		send_to_char (buf, ch);
	      }
	  }
    }
  else
    {                           /* print only FIRST char, not all. */
      for (i = character_list; i; i = i->next)
	if (world[i->in_room].zone == world[ch->in_room].zone && CAN_SEE (ch, i) &&
	    (i->in_room != NOWHERE) && isname (arg, i->player.name))
	  {
	    sprintf (buf, "&Y%-25s &R-&n %s\r\n", GET_NAME (i), world[i->in_room].name);
	    send_to_char (buf, ch);
	    return;
	  }
      send_to_char ("No-one around by that name.\r\n", ch);
    }
}


void
print_object_location (int num, struct obj_data *obj, struct char_data *ch,
		       int recur)
{
  if (num > 0)
    sprintf (buf, "O&c%3d&n. %-25s &R-&n ", num, obj->short_description);
  else
    sprintf (buf, "&c%33s&n", " &R-&n ");

  if (obj->in_room > NOWHERE)
    {
      sprintf (buf + strlen (buf), "&C[&n%s&C]&n %s\n\r",
	       rcds(obj->in_room), world[obj->in_room].name);
      send_to_char (buf, ch);
    }
  else if (obj->carried_by)
    {
      sprintf (buf + strlen (buf), "carried by %s\n\r",
	       PERS (obj->carried_by, ch));
      send_to_char (buf, ch);
    }
  else if (obj->worn_by)
    {
      if (GET_LEVEL (obj->worn_by) <= GET_LEVEL (ch)){
	sprintf (buf + strlen (buf), "worn by %s\n\r",
		 PERS (obj->worn_by, ch));
	send_to_char (buf, ch);
      }
    }
  else if (obj->in_obj)
    {
      sprintf (buf + strlen (buf), "inside %s%s\n\r",
	       obj->in_obj->short_description, (recur ? ", which is" : " "));
      send_to_char (buf, ch);
      if (recur)
	print_object_location (0, obj->in_obj, ch, recur);
    }
  else
    {
      sprintf (buf + strlen (buf), "in an unknown location\n\r");
      send_to_char (buf, ch);
    }
}



void
perform_immort_where (struct char_data *ch, char *arg)
{
  register struct char_data *i;
  register struct obj_data *k;
  struct descriptor_data *d;
  int num = 0, found = 0;

  if (!*arg)
    {
      send_to_char ("Players\r\n-------\r\n", ch);
      for (d = descriptor_list; d; d = d->next)
	if (!d->connected)
	  {
	    i = (d->original ? d->original : d->character);
	    if (i && CAN_SEE (ch, i) && (i->in_room != NOWHERE) &&
		!(GET_LEVEL(ch) < GET_LEVEL(i)))
	      {
		if (d->original)
		  sprintf (buf, "&Y%-20s &R- &C[&n%s&C]&n %s (in %s)\r\n",
			   GET_NAME (i), rcds(d->character->in_room),
			   world[d->character->in_room].name, GET_NAME (d->character));
		else
		  sprintf (buf, "&Y%-20s &R- &C[&n%s&C]&n %s\r\n", GET_NAME (i),
			   rcds(i->in_room), world[i->in_room].name);
		send_to_char (buf, ch);
	      }
	  }
    }
  else
    {
      for (i = character_list; i; i = i->next)
	if (CAN_SEE (ch, i) && i->in_room != NOWHERE 
	    && isname (arg, i->player.name) 
	    && !(GET_LEVEL(ch) < GET_LEVEL(i)))
	  {
	    found = 1;
	    sprintf (buf, "M&c%3d&n. %-25s &R-&n &C[&n%s&C]&n %s\r\n", ++num, GET_NAME (i),
		     rcds(i->in_room), world[i->in_room].name);
	    send_to_char (buf, ch);
	  }
      for (num = 0, k = object_list; k; k = k->next)
	if (CAN_SEE_OBJ (ch, k) && isname (arg, k->name) &&
	    (!k->carried_by || CAN_SEE (ch, k->carried_by)))
	  {
	    found = 1;
	    print_object_location (++num, k, ch, TRUE);
	  }
      if (!found)
	send_to_char ("Couldn't find any such thing.\r\n", ch);
    }
}



ACMD (do_where)
{
  one_argument (argument, arg);

  if (GET_LEVEL (ch) >= LVL_IMMORT)
    perform_immort_where (ch, arg);
  else
    perform_mortal_where (ch, arg);
}



int estimate_difficulty(struct char_data *ch, struct char_data *victim)
{
  int diff;

  diff = (GET_LEVEL (victim) - GET_LEVEL (ch))
         + GET_DEFENSE(victim)-GET_DEFENSE(ch)
         + GET_MDEFENSE(victim)-GET_MDEFENSE(ch)
         + GET_POWER(victim)-GET_POWER(ch)
         + GET_MPOWER(victim)-GET_MPOWER(ch)
         + GET_TECHNIQUE(victim)-GET_TECHNIQUE(ch);
  return diff;
}

ACMD (do_consider)
{
  struct char_data *victim;
  int diff;

  one_argument (argument, buf);

  if (!(victim = get_char_room_vis (ch, buf)))
    {
      send_to_char ("Consider killing who?\r\n", ch);
      return;
    }
  if (victim == ch)
    {
      send_to_char ("Easy!  Very easy indeed!\r\n", ch);
      return;
    }
  if (GET_LEVEL(victim) >= 101)
    {
      send_to_char ("Now that's not a smart thing to even think about!\r\n",
		    ch);
      return;
    }
  if (!IS_NPC (victim))
    {
      send_to_char ("Would you like to borrow a cross and a shovel?\r\n", ch);
      return;
    }

  diff = estimate_difficulty(ch, victim);

  if (diff <= -10)
    send_to_char ("Now where did that chicken go?\r\n", ch);
  else if (diff <= -5)
    send_to_char ("You could do it with a needle!\r\n", ch);
  else if (diff <= -2)
    send_to_char ("Easy.\r\n", ch);
  else if (diff <= -1)
    send_to_char ("Fairly easy.\r\n", ch);
  else if (diff == 0)
    send_to_char ("The perfect match!\r\n", ch);
  else if (diff <= 1)
    send_to_char ("You would need some luck!\r\n", ch);
  else if (diff <= 2)
    send_to_char ("You would need a lot of luck!\r\n", ch);
  else if (diff <= 3)
    send_to_char ("You would need a lot of luck and great equipment!\r\n", ch);
  else if (diff <= 5)
    send_to_char ("Do you feel lucky, punk?\r\n", ch);
  else if (diff <= 10)
    send_to_char ("Just stay there while we get you an ambulance.\r\n", ch);
  else if (diff <= 20)
    send_to_char ("Are you mad!?\r\n", ch);
  else if (diff <= 40)
    send_to_char ("The undertaker is on his way with your coffin.\r\n", ch);
  else if (diff <= 60)
    send_to_char ("Got a deathwish?!?\r\n", ch);
  else if (diff <= 80)
    send_to_char ("You ARE mad!\r\n", ch);
  else if (diff <= 100)
    send_to_char ("You ARE completely stark raving MAD!!\r\n", ch);
  else
    send_to_char ("What are you, immortal?!?\r\n", ch);

}



ACMD (do_diagnose)
{
  struct char_data *vict;

  one_argument (argument, buf);

  if (*buf)
    {
      if (!(vict = get_char_room_vis (ch, buf)))
	{
	  send_to_char (NOPERSON, ch);
	  return;
	}
      else
	diag_char_to_char (vict, ch);
    }
  else
    {
      if (FIGHTING (ch))
	diag_char_to_char (FIGHTING (ch), ch);
      else
	send_to_char ("Diagnose who?\r\n", ch);
    }
}


static char *ctypes[] =
{
  "off", "sparse", "normal", "complete", "\n"};

ACMD (do_color)
{
  int tp;

  if (IS_NPC (ch))
    return;

  one_argument (argument, arg);

  if (!*arg)
    {
      sprintf (buf, "Your current color level is %s.\r\n", ctypes[COLOR_LEV (ch)]);
      send_to_char (buf, ch);
      return;
    }
  if (((tp = search_block (arg, (const char **) ctypes, FALSE)) == -1))
    {
      send_to_char ("Usage: color { Off | Sparse | Normal | Complete }\r\n", ch);
      return;
    }
  REMOVE_BIT (PRF_FLAGS (ch), PRF_COLOR_1 | PRF_COLOR_2);
  SET_BIT (PRF_FLAGS (ch), (PRF_COLOR_1 * (tp & 1)) | (PRF_COLOR_2 * (tp & 2) >> 1));

  sprintf (buf, "Your %scolor%s is now %s.\r\n", CCRED (ch, C_SPR),
	   CCNRM (ch, C_OFF), ctypes[tp]);
  send_to_char (buf, ch);
}


ACMD (do_toggle)
{
  if (IS_NPC (ch))
    return;
  if (GET_WIMP_LEV (ch) == 0)
    strcpy (buf2, "OFF");
  else
    sprintf (buf2, "%-3d", GET_WIMP_LEV (ch));

  sprintf (buf,
	   "Hit Pnt Display: %-3s    "
	   "     Brief Mode: %-3s    "
	   " Summon Protect: %-3s\r\n"

	   "   Move Display: %-3s    "
	   "   Compact Mode: %-3s    "
	   "       On Quest: %-3s\r\n"

	   "   Mana Display: %-3s    "
	   "         NoTell: %-3s    "
	   "   Repeat Comm.: %-3s\r\n"

	   " Auto Show Exit: %-3s    "
	   "           Deaf: %-3s    "
	   "     Wimp Level: %-3s\r\n"

	   " Gossip Channel: %-3s    "
	   "Auction Channel: %-3s    "
	   "  Grats Channel: %-3s\r\n"

	   "   Auto Looting: %-3s    "
	   " Auto Splitting: %-3s    "
	   "      Auto Gold: %-3s\r\n"

	   "    Exp Display: %-3s    "
	   "       AFK Mode: %-3s    "
	   "          NoTic: %-3s\r\n"

	   "   Mob Stacking: %-3s    "
	   "      World Map: %-3s    "
	   "    Color Level: %-8s\r\n"
           "          Mercy: %-3s    "
           "   Advanced Map: %-3s\r\n",

	   ONOFF (PRF_FLAGGED (ch, PRF_DISPHP)),
	   ONOFF (PRF_FLAGGED (ch, PRF_BRIEF)),
	   ONOFF (!PRF_FLAGGED (ch, PRF_SUMMONABLE)),

	   ONOFF (PRF_FLAGGED (ch, PRF_DISPMOVE)),
	   ONOFF (PRF_FLAGGED (ch, PRF_COMPACT)),
	   YESNO (PRF2_FLAGGED (ch, PRF2_QCHAN)),

	   ONOFF (PRF_FLAGGED (ch, PRF_DISPMANA)),
	   ONOFF (PRF_FLAGGED (ch, PRF_NOTELL)),
	   YESNO (!PRF_FLAGGED (ch, PRF_NOREPEAT)),

	   ONOFF (PRF_FLAGGED (ch, PRF_AUTOEXIT)),
	   YESNO (PRF_FLAGGED (ch, PRF_DEAF)),
	   buf2,

	   ONOFF (!PRF_FLAGGED (ch, PRF_NOGOSS)),
	   ONOFF (!PRF_FLAGGED (ch, PRF_NOAUCT)),
	   ONOFF (!PRF_FLAGGED (ch, PRF_NOGRATZ)),

	   ONOFF (PRF_FLAGGED (ch, PRF_AUTOLOOT)),
	   ONOFF (PRF_FLAGGED (ch, PRF_AUTOSPLIT)),
	   ONOFF (PRF_FLAGGED (ch, PRF_AUTOGOLD)),

	   ONOFF (PRF_FLAGGED (ch, PRF_DISPEXP)),
	   ONOFF (PRF_FLAGGED (ch, PRF_AFK)),
	   ONOFF (!PRF_FLAGGED (ch, PRF_NOTIC)),

	   ONOFF (!PRF_FLAGGED (ch, PRF_NOLOOKSTACK)),
	   ONOFF (!PRF2_FLAGGED (ch, PRF2_NOMAP)),
	   ctypes[COLOR_LEV (ch)],
           ONOFF (PRF2_FLAGGED (ch, PRF2_MERCY)),
           ONOFF (PRF2_FLAGGED (ch, PRF2_ADVANCEDMAP)));

  send_to_char (buf, ch);
}

struct sort_struct {
  int sort_pos;
  byte type;
//  byte is_social;
} *cmd_sort_info = NULL;

int num_of_cmds;

#define TYPE_CMD        (1 << 0)
#define TYPE_SOCIAL     (1 << 1)
#define TYPE_WIZCMD     (1 << 2)

void sort_commands(void) {
  int a, b, tmp;
  ACMD(do_action);  
  num_of_cmds = 0;
   
  /*
   * first, count commands (num_of_commands is actually one greater than the
   * number of commands; it inclues the '\n'.
   */
  while (*complete_cmd_info[num_of_cmds].command != '\n')
    num_of_cmds++;

   /* check if there was an old sort info.. then free it -- aedit -- M. Scott*/
   if (cmd_sort_info) free(cmd_sort_info);
  /* create data array */
  CREATE(cmd_sort_info, struct sort_struct, num_of_cmds);
  
  /* initialize it */
  for (a = 1; a < num_of_cmds; a++) {
    cmd_sort_info[a].sort_pos = a;
//    cmd_sort_info[a].is_social = (complete_cmd_info[a].command_pointer == do_action);

    if (complete_cmd_info[a].command_pointer == do_action)
      cmd_sort_info[a].type = TYPE_SOCIAL;

    if (IS_GODCMD(a) || IS_GODCMD2(a) || IS_GODCMD3(a) || IS_GODCMD4(a))
      cmd_sort_info[a].type |= TYPE_WIZCMD;

    if (!cmd_sort_info[a].type)
      cmd_sort_info[a].type = TYPE_CMD;

  }
     
  /* the infernal special case */
//  cmd_sort_info[find_command("insult")].is_social = TRUE;
  cmd_sort_info[find_command("insult")].type = TYPE_SOCIAL;

  /* Sort.  'a' starts at 1, not 0, to remove 'RESERVED' */
  for (a = 1; a < num_of_cmds - 1; a++)   
    for (b = a + 1; b < num_of_cmds; b++)
     if (strcmp(complete_cmd_info[cmd_sort_info[a].sort_pos].command,
		complete_cmd_info[cmd_sort_info[b].sort_pos].command) > 0) {
	tmp = cmd_sort_info[a].sort_pos;
	cmd_sort_info[a].sort_pos = cmd_sort_info[b].sort_pos;
	cmd_sort_info[b].sort_pos = tmp;
      }
}  

ACMD(do_commands)
{
  int no, i, cmd_num;
  int type;
  struct char_data *vict;
  
  one_argument(argument, arg);
    
  if (*arg) {
    if (!(vict = get_char_vis(ch, arg)) || IS_NPC(vict)) {
      send_to_char("Who is that?\r\n", ch);
      return;
    }
    if (GET_LEVEL(ch) < GET_LEVEL(vict)) {
      send_to_char("You can't see the commands of people above your level.\r\n", ch);
      return;
    }
  } else
    vict = ch;
  
  if (subcmd == SCMD_SOCIALS)
    type = TYPE_SOCIAL;
  else if (subcmd == SCMD_WIZHELP)
    type = TYPE_WIZCMD;
  else
    type = TYPE_CMD;
      
  sprintf(buf, "The following %s%s are available to %s:\r\n",
	  (subcmd == SCMD_WIZHELP) ? "privileged " : "",
	  (subcmd == SCMD_SOCIALS) ? "socials" : "commands",
	  vict == ch ? "you" : GET_NAME(vict));
      
  /* cmd_num starts at 1, not 0, to remove 'RESERVED' */
  for (no = 1, cmd_num = 1; cmd_num < num_of_cmds; cmd_num++) {
    i = cmd_sort_info[cmd_num].sort_pos;
    
    /* check if its the right type of command */
    if (!(type & cmd_sort_info[i].type))
      continue;
    
    /* check if vict is high enough level to use cmd */
    if ((GET_LEVEL(vict) < complete_cmd_info[i].minimum_level) && !IS_GOD(vict))
      continue;
	  
    /* check if its a mob command */
    if (IS_NPCCMD(i) && !IS_NPC(vict))
      continue;
  
    /* check if vict has proper privelages */
    if ((subcmd == SCMD_WIZHELP) &&
	!((IS_GODCMD(i) && IS_SET(GCMD_FLAGS(vict), complete_cmd_info[i].godcmd)) ||
	  (IS_GODCMD2(i) && IS_SET(GCMD2_FLAGS(vict), complete_cmd_info[i].godcmd)) ||
	  (IS_GODCMD3(i) && IS_SET(GCMD3_FLAGS(vict), complete_cmd_info[i].godcmd)) ||
	  (IS_GODCMD4(i) && IS_SET(GCMD4_FLAGS(vict), complete_cmd_info[i].godcmd))))
      continue;
    
      if (subcmd == SCMD_WIZHELP) {
      sprintf(buf + strlen(buf), "&Y%-11s&n", complete_cmd_info[i].command);
      if (!(no % 7))
	strcat(buf, "\r\n");
      no++;
} 
      else  {
      sprintf(buf + strlen(buf), "&g%-11s&n", complete_cmd_info[i].command);
      if (!(no % 7))
	strcat(buf, "\r\n");
      no++;
     }
    }
      
  strcat(buf, "\r\n");
  send_to_char(buf, ch);
}


ACMD (do_players)
{
  int count = 0;
  MYSQL_RES *result;
  MYSQL_ROW row;

  buf[0]='\0';

  sprintf (buf, "\r\nKey:\r\n"
		"&RDeleted &YImmortal &BBuilder\r\n"
		"&GHero    &WMortal\r\n\r\n");

  sprintf(buf2, "SELECT name,act,level from player_main");
  QUERY_DATABASE(SQLdb, buf2, strlen(buf2));

  if (!(result=STORE_RESULT(SQLdb))) return;

  while ((row=FETCH_ROW(result))) {
    sprintf (buf, "%s%s%-20s ", buf, IS_SET(ATOIROW(1), PLR_DELETED) ? "&R" :
      (ATOIROW(2) >= LVL_IMMORT ? "&Y" : (ATOIROW(2) == LVL_HERO ? "&G" : 
      (IS_SET(ATOIROW(1), PLR_MBUILDER) ? "&B" : "&W"))),
      row[0]);
    count++;
    if (count == 3) {
      count = 0;
      strcat (buf, "\r\n");
    }
  }
  mysql_free_result(result);
  page_string (ch->desc, buf, 1);
}

ACMD (do_mudheal)
{
  struct descriptor_data *d;
  struct char_data *wch;

  for (d = descriptor_list; d; d = d->next)
    {
      if (d->connected)
	continue;

      if (d->original)
	wch = d->original;
      else if (!(wch = d->character))
	continue;
      GET_HIT(d->character) = GET_MAX_HIT(d->character);
      GET_MANA(d->character) = GET_MAX_MANA(d->character);
      GET_MOVE(d->character) = GET_MAX_MOVE(d->character);
      send_to_char("You have been fully healed by the gods!\r\n", d->character);
      update_pos(d->character);
    }
  sprintf (buf, "(GC) mudheal by %s.", GET_NAME (ch));
  mudlog (buf, NRM, MAX (LVL_GRGOD, GET_INVIS_LEV (ch)), TRUE);
  
return;
}
ACMD (do_forage)
{
  struct obj_data *item_found = '\0';
  int item_no = 3015;           /* Initialize with first item poss. */
  *buf =
    '\0';
  if (GET_MOVE (ch) < 120)
    {
      send_to_char ("You do not have enough energy right now.\r\n", ch);
      return;
    }
  if (SECT (ch->in_room) != SECT_FIELD && SECT (ch->in_room) != SECT_FOREST
  && SECT (ch->in_room) != SECT_HILLS && SECT (ch->in_room) != SECT_MOUNTAIN)
    {
      send_to_char ("You cannot forage on this type of terrain!\r\n",
		    ch);
      return;
    }
  if (GET_SKILL (ch, SKILL_FORAGE) <= 0)
    {
      send_to_char ("You have no idea how to forage!\r\n", ch);
      return;
    }
  send_to_char ("You start searching the area for signs of food.\r\n",
		ch);
  act ("$n starts foraging the area for food.\r\n", FALSE, ch, 0, 0,
       TO_ROOM);
  if (number (1, 101) > GET_SKILL (ch, SKILL_FORAGE))
    {
      WAIT_STATE (ch, PULSE_VIOLENCE * 2);
      GET_MOVE (ch) -= (120 - GET_LEVEL (ch));
      send_to_char ("\r\nYou have no luck finding anything to eat.\r\n",
		    ch);
      return;
    }
  else
    {
      switch (number (1, 7))
	{
	case 1:
	  item_no = 3015;
	  break;                /*<--- Here are the objects you need to
				   code */
	case 2:         /* Add more or remove some, just change the
				 */
	  item_no = 3009;
	  break;                /* switch(number(1, X) */
	case 3:
	  item_no = 6023;
	  break;
	case 4:
	  item_no = 1020;
	  break;
	case 5:
	  item_no = 3010;
	  break;
	case 6:
	  item_no = 2505;
	  break;
	case 7:
	  item_no = 10015;
	  break;
	}
      WAIT_STATE (ch, PULSE_VIOLENCE * 2);      /* Not really necessary */
      GET_MOVE (ch) -= (120 - GET_LEVEL (ch));
      item_found = read_object (item_no, VIRTUAL);
      obj_to_char (item_found,
		   ch);
      sprintf (buf, "%sYou have found %s!\r\n", buf,
	       item_found->short_description);
      send_to_char (buf, ch);
      act ("$n has found something in his forage attempt.\r\n", FALSE, ch, 0,
	   0, TO_ROOM);
      return;
    }
}
void user_cntr(struct descriptor_data *d) {
FILE *uc_fp;
long u_cnt = 0;
uc_fp = fopen("USRCNT", "r+b");

 if (uc_fp != NULL) {
  fread(&u_cnt, sizeof (long), 1, uc_fp);
  u_cnt ++;  rewind(uc_fp);
  fwrite(&u_cnt, sizeof (long), 1, uc_fp);
 fclose(uc_fp);
  sprintf(buf, "\r\n  You are player #%ld to logon since April 13, 1998\r\n",
u_cnt);
  SEND_TO_Q(buf, d);
}
}
char * town_name (struct char_data *ch)
{
if ((GET_HOME(ch) == 0))
return 0;
 else
if ((GET_HOME(ch) == 1))
return "Anacreon";
 else
if ((GET_HOME(ch) == 2))
return "Jhaden";
 else
if ((GET_HOME(ch) == 3))
return "Strundhaven";
 else
if ((GET_HOME(ch) == 4))
return "Locris"; 

log("Warning: player town not found. Returning null");
return 0;
}

char * class_name (struct char_data *ch)
{
if ((GET_CLASS(ch) == CLASS_MAGIC_USER))
return "Mage";
 else
if ((GET_CLASS(ch) == CLASS_CLERIC))
return "Cleric";
 else
if ((GET_CLASS(ch) == CLASS_THIEF))
return "Thief";
 else
if ((GET_CLASS(ch) == CLASS_WARRIOR))
return "Warrior";
 else
if ((GET_CLASS(ch) == CLASS_ARTISAN))
return "Artisan";
 else
log("Warning: player class not found. Returning null");
return 0;
}
char * race_name (struct char_data *ch)
{
if ((GET_RACE(ch) == RACE_HUMAN))
return "Human";
 else
if ((GET_RACE(ch) == RACE_ELF))
return "Elf";
 else
if ((GET_RACE(ch) == RACE_GNOME))
return "Gnome";
 else
if ((GET_RACE(ch) == RACE_DWARF))
return "Dwarf";
 else
if ((GET_RACE(ch) == RACE_TROLL))
return "Troll";
 else
if ((GET_RACE(ch) == RACE_GOBLIN))
return "Goblin";
 else
if ((GET_RACE(ch) == RACE_DROW))
return "Drow";
 else
if ((GET_RACE(ch) == RACE_ORC))
return "Orc";
 else
if ((GET_RACE(ch) == RACE_MINOTAUR))
return "Minotaur";
 else
log("Warning: player race not found. Returning null");
return 0;
}
char * deity_name (struct char_data *ch)
{
if ((GET_DEITY(ch) == DEITY_AETOS))
return "Aetos";
 else
if ((GET_DEITY(ch) == DEITY_CORGUS))
return "Corgus";
 else
if ((GET_DEITY(ch) == DEITY_LYTHERN))
return "Lythern";
 else
if ((GET_DEITY(ch) == DEITY_PALLAS))
return "Pallas";
 else
if ((GET_DEITY(ch) == DEITY_EROS))
return "Eros";
 else
if ((GET_DEITY(ch) == DEITY_CAILIA))
return "Cailia";
 else
if ((GET_DEITY(ch) == DEITY_MARBIN))
return "Marbin";
 else
if ((GET_DEITY(ch) == DEITY_POSEIDON))
return "Poseidon";
 else
if ((GET_DEITY(ch) == DEITY_LELU))
return "Lelu";
 else
if ((GET_DEITY(ch) == DEITY_CHAOS))
return "Chaos";
 else
if ((GET_DEITY(ch) == DEITY_ELESTRA))
return "Elestra";
 else
if ((GET_DEITY(ch) == DEITY_NEMONICA))
return "Nemonica";
 else
if ((GET_DEITY(ch) == DEITY_DURN))
return "Durn";
 else
if ((GET_DEITY(ch) == DEITY_RANUS))
return "Ranus";
 else
if ((GET_DEITY(ch) == DEITY_INCUBUS))
return "Incubus";
 else
log("Warning: player deity not found. Returning null");
return 0;
}
char * sex_name (struct char_data *ch)
{
if ((GET_SEX(ch) == SEX_MALE))
return "He";
 else
if ((GET_SEX(ch) == SEX_FEMALE))
return "She";
 else
if ((GET_SEX(ch) == SEX_NEUTRAL))
return "It";
 else
log("Warning: player sex not found. Returning null");
return 0;
}

ACMD(do_whois)
{
  struct char_data *victim = 0;

  skip_spaces(&argument);

  if (!*argument) {
      send_to_char("Do a whois on which player?\r\n", ch);
    } 
else {
      CREATE(victim, struct char_data, 1);
      clear_char(victim);

      if (retrieve_player_entry(argument, victim)) {

if ((GET_LEVEL(ch) < LVL_IMMORT) && (GET_LEVEL(victim) >= LVL_IMMORT)) {
 send_to_char("Information about immortals is unavailable.\r\n", ch);
return;
}
        strcpy (buf2, (char *) ctime (&(victim->player.time.logon)));
	sprintf(buf, "%s is a level %d, %s %s.\r\n%s was last on at %s\r\n", GET_NAME(victim), GET_LEVEL(victim), race_name(victim), class_name(victim), GET_NAME(victim), buf2);
	send_to_char(buf, ch);
      } else {
	send_to_char("There is no such player.\r\n", ch);
      }
      free_char(victim);
    }
}
ACMD(do_scan)
{
  struct char_data *i;
  int is_in, dir, dis, maxdis, found = 0;

  const char *distance[] = {
    "right here",
    "immediately ",
    "nearby ",
    "a ways ",
    "far ",
    "very far ",
    "extremely far ",
    "impossibly far ",
  };

  if (IS_AFFECTED(ch, AFF_BLIND)) {
    act("You can't see anything, you're blind!", TRUE, ch, 0, 0, TO_CHAR);
    return;
  }
  if ((GET_MOVE(ch) < 3) && (GET_LEVEL(ch) < LVL_IMMORT)) {
    act("You are too exhausted.", TRUE, ch, 0, 0, TO_CHAR);
    return;
  }

  maxdis = (1 + ((GET_SKILL(ch, SKILL_SCAN) * 5) / 100));
  if (GET_LEVEL(ch) >= LVL_IMMORT)
    maxdis = 7;

  act("You quickly scan the area and see:", TRUE, ch, 0, 0, TO_CHAR);
  act("$n quickly scans the area.", FALSE, ch, 0, 0, TO_ROOM);
  if (GET_LEVEL(ch) < LVL_IMMORT)
    GET_MOVE(ch) -= 3;

  is_in = ch->in_room;
  for (dir = 0; dir < NUM_OF_DIRS; dir++) {
    ch->in_room = is_in;
    for (dis = 0; dis <= maxdis; dis++) {
      if (((dis == 0) && (dir == 0)) || (dis > 0)) {
	for (i = world[ch->in_room].people; i; i = i->next_in_room) {
	  if ((!((ch == i) && (dis == 0))) && CAN_SEE(ch, i)) {
	    sprintf(buf, "&Y%33s&R:&W %s%s%s%s&n", GET_NAME(i), distance[dis],
		    ((dis > 0) && (dir < (NUM_OF_DIRS - 2))) ? "to the " : "",
		    (dis > 0) ? dirs[dir] : "",
		    ((dis > 0) && (dir > (NUM_OF_DIRS - 3))) ? "wards" : "");
	    act(buf, TRUE, ch, 0, 0, TO_CHAR);
	    found++;
	  }
	}
      }
      if (!CAN_GO(ch, dir) || (world[ch->in_room].dir_option[dir]->to_room == is_in))
	break;
      else
	ch->in_room = world[ch->in_room].dir_option[dir]->to_room;
    }
  }
  if (found == 0)
    act("Nobody anywhere near you.", TRUE, ch, 0, 0, TO_CHAR);
  ch->in_room = is_in;
}
ACMD (do_rnum) {
  sprintf(buf,"You are in room %d (real room: %d).\r\n", 
	  (int) world[ch->in_room].number, real_room(world[ch->in_room].number));
  send_to_char(buf, ch); 
  if (IS_SET (ROOM_FLAGS (ch->in_room), ROOM_HOUSE_CRASH) 
      && GET_LEVEL(ch) > LVL_IMMORT)
    send_to_char("Crashsave flag on house.\r\n",ch);
}

ACMD (do_levels)
{
  int i, xp=0, lastxp=0;
  char* numdisplay(int);

  if (IS_NPC (ch))
    {
      send_to_char ("You ain't nothin' but a hound-dog.\r\n", ch);
      return;
    }
  *buf = '\0';

    strcat(buf, "Total/per-level experience points needed to complete each level:\r\n\r\n");

  for (i = 1; i < LVL_IMMORT; i++) {
    xp = exp_to_level(i);
    sprintf (buf + strlen (buf), "&b[&c%-4d&b]&n %s", i, numdisplay(xp));
    sprintf (buf + strlen (buf), " - %s\r\n", numdisplay(xp - lastxp));
    lastxp=xp;
  }
  xp=exp_to_level(LVL_IMMORT);
    sprintf (buf + strlen (buf), "&b[&Y%-4d&b]&n %s", LVL_IMMORT, numdisplay(xp));
    sprintf (buf + strlen (buf), " - %s\r\n", numdisplay(xp - lastxp));
  page_string (ch->desc, buf, 1);
}
