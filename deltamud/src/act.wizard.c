/* ************************************************************************
   *   File: act.wizard.c                                  Part of CircleMUD *
   *  Usage: Player-level god commands and other goodies                     *
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
#include "house.h"
#include "screen.h"
#include "olc.h"
#include "dg_scripts.h"
#include "gcmd.h"
#include "maputils.h"
#include "dbinterface.h"

/*   external vars  */
extern const char *save_info_msg[];
extern int www_who;
extern int reboot_hr;
extern int reboot_min;
extern int warn_hr;
extern int warn_min;
extern int mobdie_enabled;
extern int weaponrestrictions;
extern int lvl_maxdmg_weapon[LVL_IMMORT];
extern int impboard;
extern int circle_restrict;
extern struct room_data *world;
extern struct char_data *character_list;
extern struct obj_data *object_list;
extern struct descriptor_data *descriptor_list;
extern struct index_data *mob_index;
extern struct char_data *mob_proto;
extern struct index_data *obj_index;
extern struct int_app_type int_app[];
extern struct wis_app_type wis_app[];
extern struct zone_data *zone_table;
extern int top_of_zone_table;
extern int top_of_mobt;
extern int circle_restrict;
extern int top_of_world;
extern int top_of_mobt;
extern int top_of_objt;
extern int top_of_p_table;
void redit_save_to_disk (int zone_num);
void oedit_save_to_disk (int zone_num);
void medit_save_to_disk (int zone_num);
void sedit_save_to_disk (int zone_num);
void zedit_save_to_disk (int zone_num);
void make_who2html (void);
void check_autowiz (struct char_data *ch);
SPECIAL (magic_user);

/* for objects */
extern const char *item_types[];
extern const char *wear_bits[];
extern const char *extra_bits[];
extern const char *container_bits[];
extern const char *drinks[];

/* for rooms */
extern const char *dirs[];
extern const char *room_bits[];
extern const char *exit_bits[];
extern const char *sector_types[];
extern int rev_dir[];
extern const char *dirs[];

/* for chars */
extern const char *spells[];
extern const char *equipment_types[];
extern const char *affected_bits[];
extern const char *apply_types[];
extern const char *pc_class_types[];
extern const char *pc_race_types[];

extern const char *npc_class_types[];
extern const char *action_bits[];
extern const char *player_bits[];
extern const char *preference_bits[];
extern const char *preference2_bits[];
extern const char *position_types[];
extern const char *connected_types[];

extern struct index_data *obj_index;
extern struct obj_data *obj_proto;
extern int top_of_objt;

extern char *credits;
extern char *news;
extern char *startup;
extern char *circlemud;
extern char *motd;
extern char *imotd;
extern char *help;
extern char *info;
extern char *background;
extern char *policies;
extern char *handbook;

/* intern functions */
void do_esave(struct char_data *ch, char *argument, int cmd, int subcmd);

/* extern functions */
int real_zone (int number);
int can_edit_zone(struct char_data *ch, int number);
room_rnum cdsr(char *string);
void assign_mobiles();
int isignore(struct char_data *ch1, struct char_data *ch2, long type);

ACMD (do_echo)
{
  struct char_data *vict;
  skip_spaces (&argument);

  if (!*argument)
    send_to_char ("Yes.. but what?\r\n", ch);
  else {
    strcpy(buf, argument);
    strcat(buf, "\r\n");
    for (vict=world[ch->in_room].people; vict; vict=vict->next_in_room) {
      if (vict == ch) {
	if (PRF_FLAGGED (ch, PRF_NOREPEAT))
	  send_to_char (OK, ch);
	else {
	  if (subcmd==SCMD_EMOTE) {
	    send_to_char(GET_NAME(ch), ch);
	    send_to_char(" ", ch);
	  }
	  send_to_char(buf, ch);
	}
	continue;
      }
      if (!isignore(vict, ch, IGNORE_EMOTE) && !PLR_FLAGGED(vict, PLR_WRITING)) {
	if (subcmd==SCMD_EMOTE) {
          if ((PRF2_FLAGGED(ch, PRF2_INTANGIBLE) && !PRF2_FLAGGED(vict, PRF2_INTANGIBLE) && !PRF2_FLAGGED(ch, PRF2_MBUILDING)) && GET_LEVEL(vict) < LVL_IMMORT) continue;
	  send_to_char(PERS(ch, vict), vict);
	  send_to_char(" ", vict);
	}
	send_to_char(buf, vict);
      }
    }
  }
}


ACMD (do_send)
{
  struct char_data *vict;

  half_chop (argument, arg, buf);

  if (!*arg)
    {
      send_to_char ("Send what to who?\r\n", ch);
      return;
    }
  if (!(vict = get_char_vis (ch, arg)))
    {
      send_to_char (NOPERSON, ch);
      return;
    }
  send_to_char (buf, vict);
  send_to_char ("\r\n", vict);
  if (PRF_FLAGGED (ch, PRF_NOREPEAT))
    send_to_char ("Sent.\r\n", ch);
  else
    {
      sprintf (buf2, "You send '%s' to %s.\r\n", buf, GET_NAME (vict));
      send_to_char (buf2, ch);
    }
}



/* take a string, and return an rnum.. used for goto, at, etc.  -je 4/6/93 */
room_rnum
find_target_room (struct char_data *ch, char *rawroomstr)
{
  int tmp;
  long location;
  struct char_data *target_mob;
  struct obj_data *target_obj;
  char roomstr[MAX_INPUT_LENGTH];

  one_argument (rawroomstr, roomstr);

  if (!*roomstr)
    {
      send_to_char ("You must supply a room number or name.\r\n", ch);
      return NOWHERE;
    }
  if ((tmp=cdsr(roomstr))!=-1) return tmp;
  else if (isdigit (*roomstr) && !strchr (roomstr, '.'))
    {
      tmp = atoi (roomstr);
      if ((location = real_room (tmp)) < 0)
	{
	  send_to_char ("No room exists with that number.\r\n", ch);
	  return NOWHERE;
	}
    }
  else if ((target_mob = get_char_vis (ch, roomstr)))
    location = target_mob->in_room;
  else if ((target_obj = get_obj_vis (ch, roomstr)))
    {
      if (target_obj->in_room != NOWHERE)
	location = target_obj->in_room;
      else
	{
	  send_to_char ("That object is not available.\r\n", ch);
	  return NOWHERE;
	}
    }
  else
    {
      send_to_char ("No such creature or object around.\r\n", ch);
      return NOWHERE;
    }

  /* a location has been found -- if you're < GRGOD, check restrictions. */
  if (GET_LEVEL (ch) < LVL_GRGOD && ROOM_FLAGGED(location, ROOM_IMPROOM)){
    send_to_char ("You are not godly enough to use that room!\r\n", ch);
    return NOWHERE;
  }
  if (GET_LEVEL (ch) < LVL_GRGOD)
    {
      if (ROOM_FLAGGED (location, ROOM_GODROOM))
	{
	  send_to_char ("You are not godly enough to use that room!\r\n", ch);
	  return NOWHERE;
	}
      if (ROOM_FLAGGED (location, ROOM_PRIVATE) &&
	  world[location].people && world[location].people->next_in_room)
	{
	  send_to_char ("There's a private conversation going on in that room.\r\n", ch);
	  return NOWHERE;
	}
      if (ROOM_FLAGGED (location, ROOM_HOUSE) &&
	  !House_can_enter (ch, world[location].number))
	{
	  send_to_char ("That's private property -- no trespassing!\r\n", ch);
	  return NOWHERE;
	}
    }
  return location;
}



ACMD (do_at)
{
  char command[MAX_INPUT_LENGTH];
  int location, original_loc;

  half_chop (argument, buf, command);
  if (!*buf)
    {
      send_to_char ("You must supply a room number or a name.\r\n", ch);
      return;
    }

  if (!*command)
    {
      send_to_char ("What do you want to do there?\r\n", ch);
      return;
    }

  if ((location = find_target_room (ch, buf)) < 0)
    return;

  /* a location has been found. */
  original_loc = ch->in_room;
  char_from_room (ch);
  char_to_room (ch, location);
  command_interpreter (ch, command);

  /* check if the char is still there */
  if (ch->in_room == location)
    {
      char_from_room (ch);
      char_to_room (ch, original_loc);
    }
}


ACMD (do_goto)
{
  long location;

  if ((location = find_target_room (ch, argument)) < 0)
    return;

 if (ch->in_room == location) {
     send_to_char("You're already there... weirdo!\r\n", ch);
     return;
  }

  if (POOFOUT (ch))
    sprintf (buf, "$n %s", POOFOUT (ch));
  else
    strcpy (buf, "You blink and suddenly realize that $n is gone.");

  act (buf, TRUE, ch, 0, 0, TO_ROOM);
  char_from_room (ch);
  char_to_room (ch, location);

  if (POOFIN (ch))
    sprintf (buf, "$n %s", POOFIN (ch));
  else
    strcpy (buf, "$n appears in a light from the heavens.");

  act (buf, TRUE, ch, 0, 0, TO_ROOM);
  look_at_room (ch, 0);
}

ACMD (do_trans)
{
  struct descriptor_data *i;
  struct char_data *victim;

  one_argument (argument, buf);
  if (!*buf)
    send_to_char ("Whom do you wish to transfer?\r\n", ch);
  else if (str_cmp ("all", buf))
    {
      if (!(victim = get_char_vis (ch, buf)))
	send_to_char (NOPERSON, ch);
      else if (victim == ch)
	send_to_char ("That doesn't make much sense, does it?\r\n", ch);
      else
	{
	  if ((GET_LEVEL (ch) < GET_LEVEL (victim)) && !IS_NPC (victim))
	    {
	      send_to_char ("Go transfer someone your own size.\r\n", ch);
	      return;
	    }
	  act ("$n disappears in a mushroom cloud.", FALSE, victim, 0, 0, TO_ROOM);
	  char_from_room (victim);
	  char_to_room (victim, ch->in_room);
	  act ("$n arrives from a puff of smoke.", FALSE, victim, 0, 0, TO_ROOM);
	  act ("$n has transferred you!", FALSE, ch, 0, victim, TO_VICT);
	  look_at_room (victim, 0);
	  sprintf(buf, "[WATCHDOG] %s has transferred %s to %s (vnum %d)",
		  GET_NAME(ch), GET_NAME(victim), 
		  world[victim->in_room].name, 
		  (int) world[victim->in_room].number);
	  mudlog(buf, CMP, LVL_IMPL, TRUE);

	}
    }
  else
    {                           /* Trans All */
      if (GET_LEVEL (ch) < LVL_GRGOD)
	{
	  send_to_char ("I think not.\r\n", ch);
	  return;
	}

      for (i = descriptor_list; i; i = i->next)
	if (!i->connected && i->character && i->character != ch)
	  {
	    victim = i->character;
	    if (GET_LEVEL (victim) >= GET_LEVEL (ch))
	      continue;
	    act ("$n disappears in a mushroom cloud.", FALSE, victim, 0, 0, TO_ROOM);
	    char_from_room (victim);
	    char_to_room (victim, ch->in_room);
	    act ("$n arrives from a puff of smoke.", FALSE, victim, 0, 0, TO_ROOM);
	    act ("$n has transferred you!", FALSE, ch, 0, victim, TO_VICT);
	    look_at_room (victim, 0);
	  }
      send_to_char (OK, ch);
    }
}



ACMD (do_teleport)
{
  struct char_data *victim;
  long target;

  two_arguments (argument, buf, buf2);

  if (!*buf)
    send_to_char ("Whom do you wish to teleport?\r\n", ch);
  else if (!(victim = get_char_vis (ch, buf)))
    send_to_char (NOPERSON, ch);
  else if (victim == ch)
    send_to_char ("Use 'goto' to teleport yourself.\r\n", ch);
  else if (GET_LEVEL (victim) >= GET_LEVEL (ch))
    send_to_char ("Maybe you shouldn't do that.\r\n", ch);
  else if (!*buf2)
    send_to_char ("Where do you wish to send this person?\r\n", ch);
  else if ((target = find_target_room (ch, buf2)) >= 0)
    {
      send_to_char (OK, ch);
      act ("$n disappears in a puff of smoke.", FALSE, victim, 0, 0, TO_ROOM);
      char_from_room (victim);
      char_to_room (victim, target);
      act ("$n arrives from a puff of smoke.", FALSE, victim, 0, 0, TO_ROOM);
      act ("$n has teleported you!", FALSE, ch, 0, (char *) victim, TO_VICT);
      look_at_room (victim, 0);
      sprintf(buf, "[WATCHDOG] %s has teleported %s to %s (vnum %d)",
	      GET_NAME(ch), GET_NAME(victim), 
	      world[victim->in_room].name, 
	      (int) world[victim->in_room].number); 
      mudlog(buf, CMP, LVL_IMPL, TRUE);
    }
}



ACMD (do_vnum)
{
  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2 || (!is_abbrev (buf, "mob") && !is_abbrev (buf, "obj")))
    {
      send_to_char ("Usage: vnum { obj | mob } <name>\r\n", ch);
      return;
    }
  if (is_abbrev (buf, "mob"))
    if (!vnum_mobile (buf2, ch))
      send_to_char ("No mobiles by that name.\r\n", ch);

  if (is_abbrev (buf, "obj"))
    if (!vnum_object (buf2, ch))
      send_to_char ("No objects by that name.\r\n", ch);
}



void
do_stat_room (struct char_data *ch)
{
  struct extra_descr_data *desc;
  struct room_data *rm = &world[ch->in_room];
  int i, found = 0;
  struct obj_data *j = 0;
  struct char_data *k = 0;

  if (GET_LEVEL(ch)<LVL_IMMORT && !can_edit_zone(ch, real_zone(rm->number))) {
    send_to_char("You don't have permissions to that zone.\r\n", ch);
    return;
  }

  sprintf (buf, "Room name: %s%s%s\r\n", CCCYN (ch, C_NRM), rm->name,
	   CCNRM (ch, C_NRM));
  send_to_char (buf, ch);

  sprinttype (rm->sector_type, sector_types, buf2);
  sprintf (buf, "Zone: [%3d], VNum: [%s%5d%s], RNum: [%5d], Type: %s Light: [%2d]\r\n",
	   zone_table[rm->zone].number, CCGRN (ch, C_NRM), (int) rm->number,
	   CCNRM (ch, C_NRM), (int) ch->in_room, buf2, world[ch->in_room].light);
  send_to_char (buf, ch);

  sprintbit ((long) rm->room_flags, room_bits, buf2);
  sprintf (buf, "SpecProc: %s, Flags: %s\r\n",
	   (rm->func == NULL) ? "None" : "Exists", buf2);
  send_to_char (buf, ch);

  send_to_char ("Description:\r\n", ch);
  if (rm->description)
    send_to_char (rm->description, ch);
  else
    send_to_char ("  None.\r\n", ch);

/*    if (rm->special_exit) {
      if (rm->special_exit->to_room == NOWHERE)
       sprintf(buf1, " %sNONE%s", CCCYN(ch, C_NRM), CCNRM(ch, C_NRM));
      else
       sprintf(buf1, "%s%5d%s", CCCYN(ch, C_NRM),
               GET_ROOM_VNUM(world[rm->special_exit->to_room].number), CCNRM(ch, C_NRM)));
      sprintbit(rm->special_exit->exit_info, exit_bits, buf2);
      sprintf(buf, "Exit %s%-5s%s:  To: [%s], Key: [%5d], Keywrd: %s, Type: %s\r\n ",
             CCCYN(ch, C_NRM), rm->special_exit->ex_name, CCNRM(ch, C_NRM), buf1, rm->special_exit->key,
          rm->special_exit->keyword ? rm->special_exit->keyword : "None",
             buf2);
      send_to_char(buf, ch);
      if (rm->special_exit->general_description)
       strcpy(buf, rm->special_exit->general_description);
      else
       strcpy(buf, "  No exit description.\r\n");
      send_to_char(buf, ch);
    }
*/
  if (rm->ex_description)
    {
      sprintf (buf, "Extra descs:%s", CCCYN (ch, C_NRM));
      for (desc = rm->ex_description; desc; desc = desc->next)
	{
	  strcat (buf, " ");
	  strcat (buf, desc->keyword);
	}
      strcat (buf, CCNRM (ch, C_NRM));
      send_to_char (strcat (buf, "\r\n"), ch);
    }
  sprintf (buf, "Chars present:%s", CCYEL (ch, C_NRM));
  for (found = 0, k = rm->people; k; k = k->next_in_room)
    {
      if (!CAN_SEE (ch, k))
	continue;
      sprintf (buf2, "%s %s(%s)", found++ ? "," : "", GET_NAME (k),
	       (!IS_NPC (k) ? "PC" : (!IS_MOB (k) ? "NPC" : "MOB")));
      strcat (buf, buf2);
      if (strlen (buf) >= 62)
	{
	  if (k->next_in_room)
	    send_to_char (strcat (buf, ",\r\n"), ch);
	  else
	    send_to_char (strcat (buf, "\r\n"), ch);
	  *buf = found = 0;
	}
    }

  if (*buf)
    send_to_char (strcat (buf, "\r\n"), ch);
  send_to_char (CCNRM (ch, C_NRM), ch);

  if (rm->contents)
    {
      sprintf (buf, "Contents:%s", CCGRN (ch, C_NRM));
      for (found = 0, j = rm->contents; j; j = j->next_content)
	{
	  if (!CAN_SEE_OBJ (ch, j))
	    continue;
	  sprintf (buf2, "%s %s", found++ ? "," : "", j->short_description);
	  strcat (buf, buf2);
	  if (strlen (buf) >= 62)
	    {
	      if (j->next_content)
		send_to_char (strcat (buf, ",\r\n"), ch);
	      else
		send_to_char (strcat (buf, "\r\n"), ch);
	      *buf = found = 0;
	    }
	}

      if (*buf)
	send_to_char (strcat (buf, "\r\n"), ch);
      send_to_char (CCNRM (ch, C_NRM), ch);
    }
  for (i = 0; i < NUM_OF_DIRS; i++)
    {
      if (rm->dir_option[i])
	{
	  if (rm->dir_option[i]->to_room == NOWHERE)
	    sprintf (buf1, " %sNONE%s", CCCYN (ch, C_NRM), CCNRM (ch, C_NRM));
	  else
	    sprintf (buf1, "%s%5d%s", CCCYN (ch, C_NRM),
	       (int) world[rm->dir_option[i]->to_room].number, CCNRM (ch, C_NRM));
	  sprintbit (rm->dir_option[i]->exit_info, exit_bits, buf2);
	  sprintf (buf, "Exit %s%-5s%s:  To: [%s], Key: [%5d], Keywrd: %s, Type: %s\r\n ",
		   CCCYN (ch, C_NRM), dirs[i], CCNRM (ch, C_NRM), buf1, (int) rm->dir_option[i]->key,
	   rm->dir_option[i]->keyword ? rm->dir_option[i]->keyword : "None",
		   buf2);
	  send_to_char (buf, ch);
	  if (rm->dir_option[i]->general_description)
	    strcpy (buf, rm->dir_option[i]->general_description);
	  else
	    strcpy (buf, "  No exit description.\r\n");
	  send_to_char (buf, ch);
	}
    }

   /* check the room for a script */
   do_sstat_room(ch);
}



void
do_stat_object (struct char_data *ch, struct obj_data *j)
{
  int i, vnum, found;
  struct obj_data *j2;
  struct extra_descr_data *desc;

  vnum = GET_OBJ_VNUM (j);
  if (GET_LEVEL(ch)<LVL_IMMORT && !can_edit_zone(ch, real_zone(vnum))) {
    send_to_char("You don't have permissions to that zone.\r\n", ch);
    return;
  }

  sprintf (buf, "Name: '%s%s%s', Aliases: %s\r\n", CCYEL (ch, C_NRM),
	   ((j->short_description) ? j->short_description : "<None>"),
	   CCNRM (ch, C_NRM), j->name);
  send_to_char (buf, ch);
  sprinttype (GET_OBJ_TYPE (j), item_types, buf1);
  if (GET_OBJ_RNUM (j) >= 0)
    strcpy (buf2, (obj_index[GET_OBJ_RNUM (j)].func ? "Exists" : "None"));
  else
    strcpy (buf2, "None");
  sprintf (buf, "VNum: [%s%5d%s], RNum: [%5d], Type: %s, SpecProc: %s\r\n",
	   CCGRN (ch, C_NRM), vnum, CCNRM (ch, C_NRM), (int) GET_OBJ_RNUM (j), buf1, buf2);
  send_to_char (buf, ch);
  sprintf (buf, "L-Des: %s\r\n", ((j->description) ? j->description : "None"));
  send_to_char (buf, ch);

  if (j->ex_description)
    {
      sprintf (buf, "Extra descs:%s", CCCYN (ch, C_NRM));
      for (desc = j->ex_description; desc; desc = desc->next)
	{
	  strcat (buf, " ");
	  strcat (buf, desc->keyword);
	}
      strcat (buf, CCNRM (ch, C_NRM));
      send_to_char (strcat (buf, "\r\n"), ch);
    }
  send_to_char ("Can be worn on: ", ch);
  sprintbit (j->obj_flags.wear_flags, wear_bits, buf);
  strcat (buf, "\r\n");
  send_to_char (buf, ch);

  send_to_char ("Set char bits : ", ch);
  sprintbit (j->obj_flags.bitvector, affected_bits, buf);
  strcat (buf, "\r\n");
  send_to_char (buf, ch);

  send_to_char ("Extra flags   : ", ch);
  sprintbit (GET_OBJ_EXTRA (j), extra_bits, buf);
  strcat (buf, "\r\n");
  send_to_char (buf, ch);

  sprintf (buf, "Weight: %d, Value: %d, Cost/day: %d, Timer: %d Level: %d\r\n",
  GET_OBJ_WEIGHT (j), GET_OBJ_COST (j), GET_OBJ_RENT (j), GET_OBJ_TIMER (j), j->min_level);
  send_to_char (buf, ch);

  strcpy (buf, "In room: ");
  if (j->in_room == NOWHERE)
    strcat (buf, "Nowhere");
  else
    {
      sprintf (buf2, "%d", (int) world[j->in_room].number);
      strcat (buf, buf2);
    }
  strcat (buf, ", In object: ");
  strcat (buf, j->in_obj ? j->in_obj->short_description : "None");
  strcat (buf, ", Carried by: ");
  strcat (buf, j->carried_by ? GET_NAME (j->carried_by) : "Nobody");
  strcat (buf, ", Worn by: ");
  strcat (buf, j->worn_by ? GET_NAME (j->worn_by) : "Nobody");
  strcat (buf, "\r\n");
  send_to_char (buf, ch);

  switch (GET_OBJ_TYPE (j))
    {
    case ITEM_LIGHT:
      if (GET_OBJ_VAL (j, 2) == -1)
	strcpy (buf, "Hours left: Infinite");
      else
	sprintf (buf, "Hours left: [%d]", GET_OBJ_VAL (j, 2));
      break;
    case ITEM_SCROLL:
    case ITEM_POTION:
      sprintf (buf, "Spells: (Level %d) %s, %s, %s", GET_OBJ_VAL (j, 0),
	   skill_name (GET_OBJ_VAL (j, 1)), skill_name (GET_OBJ_VAL (j, 2)),
	       skill_name (GET_OBJ_VAL (j, 3)));
      break;
    case ITEM_WAND:
    case ITEM_STAFF:
      sprintf (buf, "Spell: %s at level %d, %d (of %d) charges remaining",
	       skill_name (GET_OBJ_VAL (j, 3)), GET_OBJ_VAL (j, 0),
	       GET_OBJ_VAL (j, 2), GET_OBJ_VAL (j, 1));
      break;
    case ITEM_WEAPON:
      sprintf (buf, "Todam: %dd%d (avg-dmg %.1f), Message type: %d",
	       GET_OBJ_VAL (j, 1), GET_OBJ_VAL (j, 2), 
	       (((GET_OBJ_VAL (j, 2) + 1) / 2.0) * GET_OBJ_VAL (j, 1)),
	       GET_OBJ_VAL (j, 3));
      break;
    case ITEM_ARMOR:
      sprintf (buf, "Defense-app: [%d]", GET_OBJ_VAL (j, 0));
      break;
    case ITEM_TRAP:
      sprintf (buf, "Spell: %d, - Hitpoints: %d",
	       GET_OBJ_VAL (j, 0), GET_OBJ_VAL (j, 1));
      break;
    case ITEM_CONTAINER:
      sprintbit (GET_OBJ_VAL (j, 1), container_bits, buf2);
      sprintf (buf, "Weight capacity: %d, Lock Type: %s, Key Num: %d, Corpse: %s",
	       GET_OBJ_VAL (j, 0), buf2, GET_OBJ_VAL (j, 2),
	       YESNO (GET_OBJ_VAL (j, 3)));
      break;
    case ITEM_DRINKCON:
    case ITEM_FOUNTAIN:
      sprinttype (GET_OBJ_VAL (j, 2), drinks, buf2);
      sprintf (buf, "Capacity: %d, Contains: %d, Poisoned: %s, Liquid: %s",
	 GET_OBJ_VAL (j, 0), GET_OBJ_VAL (j, 1), YESNO (GET_OBJ_VAL (j, 3)),
	       buf2);
      break;
    case ITEM_NOTE:
      sprintf (buf, "Tongue: %d", GET_OBJ_VAL (j, 0));
      break;
    case ITEM_KEY:
      strcpy (buf, "");
      break;
    case ITEM_FOOD:
      sprintf (buf, "Makes full: %d, Poisoned: %s", GET_OBJ_VAL (j, 0),
	       YESNO (GET_OBJ_VAL (j, 3)));
      break;
    case ITEM_MONEY:
      sprintf (buf, "Coins: %d", GET_OBJ_VAL (j, 0));
      break;
    default:
      sprintf (buf, "Values 0-3: [%d] [%d] [%d] [%d]",
	       GET_OBJ_VAL (j, 0), GET_OBJ_VAL (j, 1),
	       GET_OBJ_VAL (j, 2), GET_OBJ_VAL (j, 3));
      break;
    }

  sprintf (buf+strlen(buf), "\r\nQuality: [%d] [%d]", GET_OBJ_CSLOTS (j), GET_OBJ_TSLOTS (j));
  send_to_char (strcat (buf, "\r\n"), ch);

  /*
   * I deleted the "equipment status" code from here because it seemed
   * more or less useless and just takes up valuable screen space.
   */

  if (j->contains)
    {
      sprintf (buf, "\r\nContents:%s", CCGRN (ch, C_NRM));
      for (found = 0, j2 = j->contains; j2; j2 = j2->next_content)
	{
	  sprintf (buf2, "%s %s", found++ ? "," : "", j2->short_description);
	  strcat (buf, buf2);
	  if (strlen (buf) >= 62)
	    {
	      if (j2->next_content)
		send_to_char (strcat (buf, ",\r\n"), ch);
	      else
		send_to_char (strcat (buf, "\r\n"), ch);
	      *buf = found = 0;
	    }
	}

      if (*buf)
	send_to_char (strcat (buf, "\r\n"), ch);
      send_to_char (CCNRM (ch, C_NRM), ch);
    }
  found = 0;
  send_to_char ("Affections:", ch);
  for (i = 0; i < MAX_OBJ_AFFECT; i++)
    if (j->affected[i].modifier)
      {
	sprinttype (j->affected[i].location, apply_types, buf2);
	sprintf (buf, "%s %+d to %s", found++ ? "," : "",
		 j->affected[i].modifier, buf2);
	send_to_char (buf, ch);
      }
  if (!found)
    send_to_char (" None", ch);

  send_to_char ("\r\n", ch);

   /* check the object for a script */
   do_sstat_object(ch, j);
}


void
do_stat_character (struct char_data *ch, struct char_data *k)
{
  int i, i2, found = 0;
  struct obj_data *j;
  struct follow_type *fol;
  struct affected_type *aff;
  extern struct attack_hit_type attack_hit_text[];

  if (GET_LEVEL(ch)<LVL_IMMORT) {
    if (!IS_NPC(k)) {
      send_to_char("You find yourself unable to.\r\n", ch);
      return;
    }
    if (!can_edit_zone(ch, real_zone(GET_MOB_VNUM(k)))) {
      send_to_char("You don't have permissions to that zone.\r\n", ch);
      return;
    }
  }

  switch (GET_SEX (k))
    {
    case SEX_NEUTRAL:
      strcpy (buf, "NEUTRAL-SEX");
      break;
    case SEX_MALE:
      strcpy (buf, "MALE");
      break;
    case SEX_FEMALE:
      strcpy (buf, "FEMALE");
      break;
    default:
      strcpy (buf, "ILLEGAL-SEX!!");
      break;
    }

  sprintf (buf2, " %s '%s'  IDNum: [%5ld], In room [%5d]",
	   (!IS_NPC (k) ? "PC" : (!IS_MOB (k) ? "NPC" : "MOB")),
	   GET_NAME (k), GET_IDNUM (k), (int) world[k->in_room].number);
  if (!IS_NPC(k))
    sprintf(buf2+strlen(buf2), ", LoadRoom: [%5d]", (int) GET_LOADROOM(k));
  strcat(buf2, "\r\n");
  send_to_char (strcat (buf, buf2), ch);
  if (IS_MOB (k))
    {
      sprintf (buf, "Alias: %s, VNum: [%5d], RNum: [%5d]\r\n",
	       k->player.name, GET_MOB_VNUM (k), (int) GET_MOB_RNUM (k));
      send_to_char (buf, ch);
    }
  sprintf(buf, "Title: %s     Trust: %d     God-Commands: %s",
	 (k->player.title ? k->player.title : "<None>"), GET_TRUST_LEVEL(k),
	 IS_GOD(k) ? "&YYes&n\r\n" : "No\r\n");
  send_to_char (buf, ch);

  sprintf (buf, "L-Des: %s", (k->player.long_descr ? k->player.long_descr : "<None>\r\n"));
  send_to_char (buf, ch);

  if (IS_NPC (k))
    {
      strcpy (buf, "Monster Class: ");
      sprinttype (k->player.class, npc_class_types, buf2);
    }
  else
    {
      strcpy (buf, "Class: ");
      sprinttype (k->player.class, pc_class_types, buf2);
    }
  strcat (buf, buf2);

  if (GET_LEVEL(k) < LVL_IMMORT)
    sprintf (buf2, ", Lev: [%s%2d%s], XP: [%s%7d%s], Align: [%4d], MaxWeapon: [%d], Cstat: [%d]\r\n",
	     CCYEL (ch, C_NRM), GET_LEVEL (k), CCNRM (ch, C_NRM),
	     CCYEL (ch, C_NRM), GET_EXP (k), CCNRM (ch, C_NRM),
             GET_ALIGNMENT (k), lvl_maxdmg_weapon[(int) GET_LEVEL(k)], GET_CITIZEN(k)+1);
  else
    sprintf (buf2, ", Lev: [%s%2d%s], XP: [%s%7d%s], Align: [%4d], Cstat: [%d]\r\n",
	     CCYEL (ch, C_NRM), GET_LEVEL (k), CCNRM (ch, C_NRM),
	     CCYEL (ch, C_NRM), GET_EXP (k), CCNRM (ch, C_NRM),
	     GET_ALIGNMENT (k), GET_CITIZEN(k)+1);
  strcat (buf, buf2);
  send_to_char (buf, ch);

  if (!IS_NPC (k))
    {
      strcpy (buf1, (char *) asctime (localtime (&(k->player.time.birth))));
      strcpy (buf2, (char *) asctime (localtime (&(k->player.time.logon))));
      buf1[10] = buf2[10] = '\0';

      sprintf (buf, "Created: [%s], Last Logon: [%s], Played [%dh %dm], Age [%d]\r\n",
	       buf1, buf2, k->player.time.played / 3600,
               ((k->player.time.played / 3600) % 60), (int) age (k).year);
      send_to_char (buf, ch);

      sprintf (buf, "Hometown: [%d], Speaks: [%d/%d/%d], (STL[%d]/per[%d]/NSTL[%d]) Clan: [%d]",
      k->player.hometown, GET_TALK (k, 0), GET_TALK (k, 1), GET_TALK (k, 2),
	       GET_PRACTICES (k), int_app[GET_INT (k)].learn,
	       wis_app[GET_WIS (k)].bonus, (k)->player_specials->saved.clan);
      strcat (buf, "\r\n");
      send_to_char (buf, ch);
    }
  sprintf (buf, "Str: [%s%d/%d%s]  Int: [%s%d%s]  Wis: [%s%d%s]  "
	   "Dex: [%s%d%s]  Con: [%s%d%s]  Cha: [%s%d%s]\r\n",
	   CCCYN (ch, C_NRM), GET_STR (k), GET_ADD (k), CCNRM (ch, C_NRM),
	   CCCYN (ch, C_NRM), GET_INT (k), CCNRM (ch, C_NRM),
	   CCCYN (ch, C_NRM), GET_WIS (k), CCNRM (ch, C_NRM),
	   CCCYN (ch, C_NRM), GET_DEX (k), CCNRM (ch, C_NRM),
	   CCCYN (ch, C_NRM), GET_CON (k), CCNRM (ch, C_NRM),
	   CCCYN (ch, C_NRM), GET_CHA (k), CCNRM (ch, C_NRM));
  send_to_char (buf, ch);

  sprintf (buf, "Hit p.:[%s%d/%d+%d%s]  Mana p.:[%s%d/%d+%d%s]  Move p.:[%s%d/%d+%d%s]\r\n",
           CCGRN (ch, C_NRM), (int) GET_HIT (k), (int) GET_MAX_HIT (k), hit_gain (k), CCNRM (ch, C_NRM),
           CCGRN (ch, C_NRM), (int) GET_MANA (k), (int) GET_MAX_MANA (k), mana_gain (k), CCNRM (ch, C_NRM),
           CCGRN (ch, C_NRM), (int) GET_MOVE (k), (int) GET_MAX_MOVE (k), move_gain (k), CCNRM (ch, C_NRM));
  send_to_char (buf, ch);

  sprintf (buf, "Coins: [%9d], Bank: [%9d] (Total: %d)\r\n",
	 GET_GOLD (k), GET_BANK_GOLD (k), GET_GOLD (k) + GET_BANK_GOLD (k));
  send_to_char (buf, ch);

  sprintf (buf, "Defense: [%d], Magic Defense: [%2d], Power: [%2d], Magic Power: [%d] Technique: [%2d]\r\n",
           GET_DEFENSE (k), GET_MDEFENSE(k), GET_POWER(k), GET_MPOWER(k), GET_TECHNIQUE(k));
  send_to_char (buf, ch);

  sprinttype (GET_POS (k), position_types, buf2);
  sprintf (buf, "Pos: %s, Fighting: %s", buf2,
	   (FIGHTING (k) ? GET_NAME (FIGHTING (k)) : "Nobody"));

  if (IS_NPC (k))
    {
      strcat (buf, ", Attack type: ");
      strcat (buf, attack_hit_text[k->mob_specials.attack_type].singular);
    }
  if (k->desc)
    {
      sprinttype (k->desc->connected, connected_types, buf2);
      strcat (buf, ", Connected: ");
      strcat (buf, buf2);
    }
  if (!IS_NPC(k))
    {
      strcat (buf, "\r\nArena: ");
      switch (GET_ARENASTAT(k)){
      case ARENA_NOT:
	strcat (buf, "[NO]");
	break;
      case ARENA_COMBATANT1:
	strcat (buf, "[COMBAT1]");
	break;
      case ARENA_COMBATANT1w:
	strcat (buf, "[COMBAT1W]");
	break;
      case ARENA_COMBATANT2:
	strcat (buf, "[COMBAT2]");
	break;
      case ARENA_COMBATANT3:
	strcat (buf, "[COMBAT3]");
	break;
      case ARENA_COMBATANTZ:
	strcat (buf, "[COMBATZ]");
	break;
      case ARENA_OBSERVER:
	strcat (buf, "[OBSERV]");
	if (OBSERVING(k) != NULL)
	  sprintf(buf ,"%s, Observing: [%s]", buf, GET_NAME(OBSERVING(k)));
	else
	  sprintf(buf ,"%s, Observing: [NOBODY]", buf);
	break;
      default:
	strcat (buf, "[UNKNOWN]");
	break;
      }
      sprintf (buf, "%s, Wins: [%d]", buf, GET_ARENAWINS(k));
      sprintf (buf, "%s, Losses: [%d]", buf, GET_ARENALOSSES(k));
      if (k->desc){
	if (GET_ARENAFLEETIMER(k) > 0)
	  sprintf(buf, "%s, Fled-a-match: %s [timer %d]", buf,
		  GET_NAME(LASTFIGHTING(k)), GET_ARENAFLEETIMER(k));
      }
    }
  send_to_char (strcat (buf, "\r\n"), ch);

  strcpy (buf, "Default position: ");
  sprinttype ((k->mob_specials.default_pos), position_types, buf2);
  strcat (buf, buf2);

  sprintf (buf2, ", Idle Timer (in tics) [%d]\r\n", k->char_specials.timer);
  strcat (buf, buf2);
  send_to_char (buf, ch);

  if (IS_NPC (k))
    {
      sprintbit (MOB_FLAGS (k), action_bits, buf2);
      sprintf (buf, "NPC flags: %s%s%s\r\n", CCCYN (ch, C_NRM), buf2, CCNRM (ch, C_NRM));
      send_to_char (buf, ch);
    }
  else
    {
      sprintf (buf, "Quest Next: [%d], Quest Timeleft: [%d]", 
	       GET_NEXTQUEST(k), GET_COUNTDOWN(k));
      if (GET_QUESTMOB(k) > 0)
	sprintf (buf, "%s, On Quest for Mob: [%d]", buf, GET_QUESTMOB(k));
      if (GET_QUESTMOB(k) < 0)
	sprintf (buf, "%s, Killed target mob of level: [%d]", 
		 buf, abs(GET_QUESTMOB(k)));
      if (GET_QUESTOBJ(k) > 0)
	sprintf (buf, "%s, On Quest for Obj: [%d]", buf, GET_QUESTOBJ(k));
      sprintf(buf,"%s\r\n",buf);
      send_to_char(buf, ch);

      sprintbit (PLR_FLAGS (k), player_bits, buf2);
      sprintf (buf, "PLR : %s%s%s\r\n", CCCYN (ch, C_NRM), buf2, CCNRM (ch, C_NRM));
      send_to_char (buf, ch);
      sprintbit (PRF_FLAGS (k), preference_bits, buf2);
      sprintf (buf, "PRF : %s%s%s\r\n", CCGRN (ch, C_NRM), buf2, CCNRM (ch, C_NRM));
      send_to_char (buf, ch);
      sprintbit (PRF2_FLAGS (k), preference2_bits, buf2);
      sprintf (buf, "PRF2: %s%s%s\r\n", CCGRN (ch, C_NRM), buf2, CCNRM (ch, C_NRM));
      send_to_char (buf, ch);
    }

  if (IS_MOB (k))
    {
      sprintf (buf, "Mob Spec-Proc: %s, NPC Bare Hand Dam: %dd%d\r\n",
	       (mob_index[GET_MOB_RNUM (k)].func ? "Exists" : "None"),
	       k->mob_specials.damnodice, k->mob_specials.damsizedice);
      send_to_char (buf, ch);
    }
  sprintf (buf, "Carried: weight: %d, items: %d; ",
	   IS_CARRYING_W (k), IS_CARRYING_N (k));

  for (i = 0, j = k->carrying; j; j = j->next_content, i++);
  sprintf (buf, "%sItems in: inventory: %d, ", buf, i);

  for (i = 0, i2 = 0; i < NUM_WEARS; i++)
    if (GET_EQ (k, i))
      i2++;
  sprintf (buf2, "eq: %d\r\n", i2);
  strcat (buf, buf2);
  send_to_char (buf, ch);

  sprintf (buf, "Hunger: %d, Thirst: %d, Drunk: %d\r\n",
	   GET_COND (k, FULL), GET_COND (k, THIRST), GET_COND (k, DRUNK));
  send_to_char (buf, ch);

  sprintf (buf, "Master is: %s, Followers are:",
	   ((k->master) ? GET_NAME (k->master) : "<none>"));

  for (fol = k->followers; fol; fol = fol->next)
    {
      sprintf (buf2, "%s %s", found++ ? "," : "", PERS (fol->follower, ch));
      strcat (buf, buf2);
      if (strlen (buf) >= 62)
	{
	  if (fol->next)
	    send_to_char (strcat (buf, ",\r\n"), ch);
	  else
	    send_to_char (strcat (buf, "\r\n"), ch);
	  *buf = found = 0;
	}
    }

  if (*buf)
    send_to_char (strcat (buf, "\r\n"), ch);

  /* Showing the bitvector */
  sprintbit (AFF_FLAGS (k), affected_bits, buf2);
  sprintf (buf, "AFF: %s%s%s\r\n", CCYEL (ch, C_NRM), buf2, CCNRM (ch, C_NRM));
  send_to_char (buf, ch);

  /* Routine to show what spells a char is affected by */
  if (k->affected)
    {
      for (aff = k->affected; aff; aff = aff->next)
	{
          *buf2 = '\0';
          if (aff->type==-1 && aff->duration==-1) {
            sprintbit (aff->bitvector, affected_bits, buf2);
            sprintf (buf, "SPL: (&YO&nPERM) %s%-21s%s ", CCCYN (ch, C_NRM), buf2, CCNRM (ch, C_NRM));
            send_to_char (strcat (buf, "\r\n"), ch);
            continue;
          }
          sprintf (buf, "SPL: (%3dhr) %s%-21s%s ", (int) aff->duration + 1,
	    CCCYN (ch, C_NRM), (aff->type >= 0 && aff->type <= MAX_SKILLS) ?
		   spells[aff->type] : "TYPE UNDEFINED", CCNRM (ch, C_NRM));
	  if (aff->modifier)
	    {
	      sprintf (buf2, "%+d to %s", (int) aff->modifier, apply_types[(int) aff->location]);
	      strcat (buf, buf2);
	    }
	  if (aff->bitvector)
	    {
	      if (*buf2)
		strcat (buf, ", sets ");
	      else
		strcat (buf, "sets ");
	      sprintbit (aff->bitvector, affected_bits, buf2);
	      strcat (buf, buf2);
	    }
	  send_to_char (strcat (buf, "\r\n"), ch);
	}
    }

   /* check mobiles for a script */
   if (IS_NPC(k))
     do_sstat_character(ch, k);
}


ACMD (do_stat)
{
  struct char_data *victim = 0;
  struct obj_data *object = 0;
  int tmp;

  half_chop (argument, buf1, buf2);

  if (!*buf1)
    {
      send_to_char ("Stats on who or what?\r\n", ch);
      return;
    }
  else if (is_abbrev (buf1, "room"))
    {
      do_stat_room (ch);
    }
  else if (is_abbrev (buf1, "mob"))
    {
      if (!*buf2)
	send_to_char ("Stats on which mobile?\r\n", ch);
      else
	{
	  if ((victim = get_char_vis (ch, buf2)))
	    do_stat_character (ch, victim);
	  else
	    send_to_char ("No such mobile around.\r\n", ch);
	}
    }
  else if (is_abbrev (buf1, "player"))
    {
      if (!*buf2)
	{
	  send_to_char ("Stats on which player?\r\n", ch);
	}
      else
	{
	  if ((victim = get_player_vis (ch, buf2, 0)))
	    do_stat_character (ch, victim);
	  else
	    send_to_char ("No such player around.\r\n", ch);
	}
    }
  else if (is_abbrev (buf1, "file"))
    {
      if (!*buf2)
	{
	  send_to_char ("Stats on which player?\r\n", ch);
	}
      else
	{
	  CREATE (victim, struct char_data, 1);
	  clear_char (victim);
          if (retrieve_player_entry (buf2, victim))
	    {
	      if (GET_LEVEL (victim) > GET_LEVEL (ch))
		send_to_char ("Sorry, you can't do that.\r\n", ch);
	      else
		do_stat_character (ch, victim);
	      free_char (victim);
	    }
	  else
	    {
	      send_to_char ("There is no such player.\r\n", ch);
	      free (victim);
	    }
	}
    }
  else if (is_abbrev (buf1, "object"))
    {
      if (!*buf2)
	send_to_char ("Stats on which object?\r\n", ch);
      else
	{
	  if ((object = get_obj_vis (ch, buf2)))
	    do_stat_object (ch, object);
	  else
	    send_to_char ("No such object around.\r\n", ch);
	}
    }
  else
    {
      if ((object = get_object_in_equip_vis (ch, buf1, ch->equipment, &tmp)))
	do_stat_object (ch, object);
      else if ((object = get_obj_in_list_vis (ch, buf1, ch->carrying)))
	do_stat_object (ch, object);
      else if ((victim = get_char_room_vis (ch, buf1)))
	do_stat_character (ch, victim);
      else if ((object = get_obj_in_list_vis (ch, buf1, world[ch->in_room].contents)))
	do_stat_object (ch, object);
      else if ((victim = get_char_vis (ch, buf1)))
	do_stat_character (ch, victim);
      else if ((object = get_obj_vis (ch, buf1)))
	do_stat_object (ch, object);
      else
	send_to_char ("Nothing around by that name.\r\n", ch);
    }
}


ACMD (do_shutdown)
{
  extern int circle_shutdown, circle_reboot;


  if (subcmd != SCMD_SHUTDOWN)
    {
      send_to_char ("If you want to shut something down, say so!\r\n", ch);
      return;
    }
  one_argument (argument, arg);

  if (!*arg)
    {
      sprintf (buf, "(GC) Shutdown by %s.", GET_NAME (ch));
      log (buf);
      send_to_all ("&m[&YINFO&m]&n Shutting down.\r\n");
      circle_shutdown = 1;
    }
  else if (!str_cmp (arg, "reboot"))
    {
//      sprintf (mybuf, "save *");
//      do_esave(ch, mybuf, 0, 0); 
      sprintf (buf, "(GC) Reboot by %s.", GET_NAME (ch));
      log (buf);
      send_to_all ("&m[&YINFO&m]&n Rebooting.. come back in a minute or two.\r\n");
      touch (FASTBOOT_FILE);
      circle_shutdown = circle_reboot = 1;
    }
  else if (!str_cmp (arg, "now"))
    {
      sprintf (buf, "(GC) Shutdown NOW by %s.", GET_NAME (ch));
      log (buf);
      send_to_all ("&m[&YINFO&m]&n Rebooting.. come back in a minute or two.\r\n");
      circle_shutdown = 1;
      circle_reboot = 2;
    }
  else if (!str_cmp (arg, "die"))
    {
//      sprintf (mybuf, "*");
//      do_esave(ch, mybuf, 0, 0); 
      sprintf (buf, "(GC) Shutdown by %s.", GET_NAME (ch));
      log (buf);
      send_to_all ("&m[&YINFO&m]&n Shutting down for maintenance.\r\n");
      touch (KILLSCRIPT_FILE);
      circle_shutdown = 1;
    }
  else if (!str_cmp (arg, "pause"))
    {
      sprintf (buf, "(GC) Shutdown by %s.", GET_NAME (ch));
      log (buf);
      send_to_all ("&m[&YINFO&m]&n Shutting down for maintenance.\r\n");
      touch (PAUSE_FILE);
      circle_shutdown = 1;
    }
  else
    send_to_char ("Unknown shutdown option.\r\n", ch);
}


void
stop_snooping (struct char_data *ch)
{
  if (!ch->desc->snooping)
    send_to_char ("You aren't snooping anyone.\r\n", ch);
  else
    {
      send_to_char ("You stop snooping.\r\n", ch);
      ch->desc->snooping->snoop_by = NULL;
      ch->desc->snooping = NULL;
    }
}


ACMD (do_snoop)
{
  struct char_data *victim, *tch;

  if (!ch->desc)
    return;

  one_argument (argument, arg);

  if (!*arg)
    stop_snooping (ch);
  else if (!(victim = get_char_vis (ch, arg)))
    send_to_char ("No such person around.\r\n", ch);
  else if (!victim->desc)
    send_to_char ("There's no link.. nothing to snoop.\r\n", ch);
  else if (victim == ch)
    stop_snooping (ch);
  else if (victim->desc->snoop_by)
    send_to_char ("Busy already. \r\n", ch);
  else if (victim->desc->snooping == ch->desc)
    send_to_char ("Don't be stupid.\r\n", ch);
  else
    {
      if (victim->desc->original)
	tch = victim->desc->original;
      else
	tch = victim;

      if (GET_LEVEL (tch) >= GET_LEVEL (ch))
	{
	 if (strcmp(GET_NAME(ch), "Mulder")) {
	  send_to_char ("You can't.\r\n", ch);
	  return;
	 }
	}
      send_to_char (OK, ch);

      if (ch->desc->snooping)
	ch->desc->snooping->snoop_by = NULL;

      ch->desc->snooping = victim->desc;
      victim->desc->snoop_by = ch->desc;
    }
}



ACMD (do_switch)
{
  struct char_data *victim;

  one_argument (argument, arg);

  if (ch->desc->original)
    send_to_char ("You're already switched.\r\n", ch);
  else if (!*arg)
    send_to_char ("Switch with who?\r\n", ch);
  else if (!(victim = get_char_vis (ch, arg)))
    send_to_char ("No such character.\r\n", ch);
  else if (ch == victim)
    send_to_char ("Hee hee... we are jolly funny today, eh?\r\n", ch);
  else if (victim->desc)
    send_to_char ("You can't do that, the body is already in use!\r\n", ch);
  else if ((GET_LEVEL (ch) < LVL_IMPL) && !IS_NPC (victim))
    send_to_char ("You aren't holy enough to use a person's body.\r\n",ch);
  else
    {
      send_to_char (OK, ch);

      ch->desc->character = victim;
      ch->desc->original = ch;

      victim->desc = ch->desc;
      ch->desc = NULL;
    }
}


ACMD (do_return)
{
  if (ch->desc && ch->desc->original)
    {
      send_to_char ("You return to your original body.\r\n", ch);

      /* JE 2/22/95 */
      /* if someone switched into your original body, disconnect them */
      if (ch->desc->original->desc)
	close_socket (ch->desc->original->desc);

      ch->desc->character = ch->desc->original;
      ch->desc->original = NULL;

      ch->desc->character->desc = ch->desc;
      ch->desc = NULL;
    }
}

 
ACMD (do_load)
{
  struct char_data *mob;
  struct obj_data *obj;
  int number, r_num;
  char mybuf[MAX_STRING_LENGTH];

  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2 || !isdigit (*buf2))
    {
      send_to_char ("Usage: load { obj | mob } <number>\r\n", ch);
      return;
    }
  if ((number = atoi (buf2)) < 0)
    {
      send_to_char ("A NEGATIVE number??\r\n", ch);
      return;
    }
  if (number == impboard && GET_LEVEL(ch) < LVL_GRGOD){
    send_to_char ("You are not holy enough for that!\r\n", ch);
    return;
  }

  if (!can_edit_zone(ch, real_zone(number)) && GET_LEVEL(ch) < LVL_GRGOD) {
    send_to_char("You do not have permission to load from this zone.\r\n", ch);
    return;
  }

  if (is_abbrev (buf, "mob"))
    {
      if ((r_num = real_mobile (number)) < 0)
	{
	  send_to_char ("There is no monster with that number.\r\n", ch);
	  return;
	}
      mob = read_mobile (r_num, REAL);
      char_to_room (mob, ch->in_room);
      sprintf(mybuf, "[WATCHDOG] %s loads mobile %d: %s", 
	      GET_NAME(ch), number, mob->player.short_descr);
      mudlog(mybuf, CMP, LVL_IMPL, TRUE);

      act ("$n makes a quaint, magical gesture with one hand.", TRUE, ch,
	   0, 0, TO_ROOM);
      act ("$n has created $N!", FALSE, ch, 0, mob, TO_ROOM);
      act ("You create $N.", FALSE, ch, 0, mob, TO_CHAR);
    load_mtrigger(mob);
    }
  else if (is_abbrev (buf, "obj"))
    {
      if ((r_num = real_object (number)) < 0)
	{
	  send_to_char ("There is no object with that number.\r\n", ch);
	  return;
	}
      obj = read_object (r_num, REAL);
      if (GET_LEVEL(ch) < LVL_IMMORT)
	SET_BIT(obj->obj_flags.extra_flags, ITEM_NORENT);
      obj_to_char (obj, ch);
      sprintf(mybuf, "[WATCHDOG] %s loads object %d: %s", 
	      GET_NAME(ch), number, obj->short_description);
      mudlog(mybuf, CMP, LVL_IMPL, TRUE);

      act ("$n makes a strange magical gesture.", TRUE, ch, 0, 0, TO_ROOM);
      act ("$n has created $p!", FALSE, ch, obj, 0, TO_ROOM);
      act ("You create $p.", FALSE, ch, obj, 0, TO_CHAR);
    load_otrigger(obj);
    }
  else
    send_to_char ("That'll have to be either 'obj' or 'mob'.\r\n", ch);
}


ACMD (do_aload)
{
  char mybuf[1024];
  int frm, to, j;

  to = atoi(two_arguments (argument, buf, buf2));
  frm = atoi(buf2);

  if (frm <= 0){
    send_to_char("You're missing the starting item number\r\n", ch);
    return;
  }
  if (to <= 0){
    send_to_char("You're missing the ending item number\r\n", ch);
    return;
  }
  if (!(is_abbrev (buf, "mob") || is_abbrev (buf, "obj"))){
    send_to_char ("Usage: aload { obj | mob } <startnumber> <endnumber>\r\n", 
		  ch);
    return;
  }
  if (frm > to){
    send_to_char("Start number cannot be greater than Ending number\r\n",ch);
    return;
  }
  for (j = frm; j <= to; j++){
    sprintf (mybuf, " %s %d ",buf, j);
    do_load(ch, mybuf, cmd, subcmd);
  }
}


ACMD (do_vstat)
{
  struct char_data *mob;
  struct obj_data *obj;
  int number, r_num;

  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2 || !isdigit (*buf2))
    {
      send_to_char ("Usage: vstat { obj | mob } <number>\r\n", ch);
      return;
    }
  if ((number = atoi (buf2)) < 0)
    {
      send_to_char ("A NEGATIVE number??\r\n", ch);
      return;
    }
  if (GET_LEVEL(ch)<LVL_IMMORT && !can_edit_zone(ch, real_zone(number))) {
    send_to_char("You don't have permissions to that zone.\r\n", ch);
    return;      
  }
  if (is_abbrev (buf, "mob"))
    {
      if ((r_num = real_mobile (number)) < 0)
	{
	  send_to_char ("There is no monster with that number.\r\n", ch);
	  return;
	}
      mob = read_mobile (r_num, REAL);
      char_to_room (mob, 0);
      do_stat_character (ch, mob);
      extract_char (mob);
    }
  else if (is_abbrev (buf, "obj"))
    {
      if ((r_num = real_object (number)) < 0)
	{
	  send_to_char ("There is no object with that number.\r\n", ch);
	  return;
	}
      obj = read_object (r_num, REAL);
      do_stat_object (ch, obj);
      extract_obj (obj);
    }
  else
    send_to_char ("That'll have to be either 'obj' or 'mob'.\r\n", ch);
}




/* clean a room of all mobiles and objects */
ACMD (do_purge)
{
  struct char_data *vict, *next_v;
  struct obj_data *obj, *next_o;

  one_argument (argument, buf);

  if (*buf)
    {                           /* argument supplied. destroy single object
				 * or char */
      if ((vict = get_char_room_vis (ch, buf)))
	{
	  if (!IS_NPC (vict) && (GET_LEVEL (ch) <= GET_LEVEL (vict)) && GET_LEVEL(ch) >=
LVL_GRGOD)
	    {
	      send_to_char ("Fuuuuuuuuu!\r\n", ch);
	      return;
	    }

	  if (!IS_NPC (vict))
	    {
	      if (GET_LEVEL(ch)<LVL_IMMORT) {
		send_to_char("No, no, no!\r\n", ch);
		return;
	      }
	      sprintf (buf, "(GC) %s has purged %s.", GET_NAME (ch), GET_NAME (vict));
	      mudlog (buf, BRF, LVL_GOD, TRUE);
	      if (vict->desc)
		{
		  close_socket (vict->desc);
		  vict->desc = NULL;
		}
	    }
	  else if ((!can_edit_zone(ch, real_zone(GET_MOB_VNUM(vict))) && GET_LEVEL(ch) < LVL_GRGOD) &&
GET_MOB_VNUM(vict)!=-1) {
	    send_to_char("You do not have permission to purge from this zone.\r\n", ch);
	    return;
	  }

	  act ("$n disintegrates $N.", FALSE, ch, 0, vict, TO_NOTVICT);
	  extract_char (vict);
	}
      else if ((obj = get_obj_in_list_vis (ch, buf, world[ch->in_room].contents)))
	{
	  if ((!can_edit_zone(ch, real_zone(GET_OBJ_VNUM(obj))) && GET_LEVEL(ch) < LVL_GRGOD) &&
GET_OBJ_VNUM(obj)!=-1) {
	    send_to_char("You do not have permission to purge from this zone.\r\n", ch);
	    return;
	  }
	  act ("$n destroys $p.", FALSE, ch, obj, 0, TO_ROOM);
	  extract_obj (obj);
	}
      else
	{
	  send_to_char ("Nothing here by that name.\r\n", ch);
	  return;
	}

      send_to_char (OK, ch);
    }
  else
    {                           /* no argument. clean out the room */
      act ("$n gestures... You are surrounded by scorching flames!",
	   FALSE, ch, 0, 0, TO_ROOM);
      send_to_room ("The world seems a little cleaner.\r\n", ch->in_room);

      for (vict = world[ch->in_room].people; vict; vict = next_v)
	{
	  next_v = vict->next_in_room;
	  if (IS_NPC (vict) && (can_edit_zone(ch, real_zone(GET_MOB_VNUM(vict))) || GET_LEVEL(ch) >=
LVL_GRGOD || GET_MOB_VNUM(vict)==-1))
	    extract_char (vict);
	}

      for (obj = world[ch->in_room].contents; obj; obj = next_o)
	{
	  next_o = obj->next_content;
	  if (can_edit_zone(ch, real_zone(GET_OBJ_VNUM(obj))) || GET_LEVEL(ch) >= LVL_GRGOD ||
GET_OBJ_VNUM(obj)==-1)
	    extract_obj (obj);
	}
    }
}



static char *logtypes[] =
{
  "off", "brief", "normal", "perfect", "complete", "\n"
};

ACMD (do_syslog)
{
  int tp;

  one_argument (argument, arg);

  if (!*arg)
    {
      tp = ((PRF_FLAGGED (ch, PRF_LOG1) ? 1 : 0) +
	    (PRF_FLAGGED (ch, PRF_LOG2) ? 2 : 0) +
	    (PRF_FLAGGED (ch, PRF_LOG3) ? 4 : 0));
      sprintf (buf, "Your syslog is currently %s.\r\n", logtypes[tp]);
      send_to_char (buf, ch);
      return;
    }
  if (((tp = search_block (arg, (const char **) logtypes, FALSE)) == -1))
    {
      send_to_char ("Usage: syslog { Off | Brief | Normal | Perfect | Complete }\r\n", ch);
      return;
    }
  REMOVE_BIT (PRF_FLAGS (ch), PRF_LOG1 | PRF_LOG2 | PRF_LOG3);
  SET_BIT (PRF_FLAGS (ch), 
	   ((tp & 1)?  PRF_LOG1 : 0)
	   | ((tp & 2)?  PRF_LOG2 : 0)
	   | ((tp & 4)?  PRF_LOG3 : 0));

  sprintf (buf, "Your syslog is now %s.\r\n", logtypes[tp]);
  send_to_char (buf, ch);
}



ACMD (do_advance)
{
  struct char_data *victim;
  char *name = arg, *level = buf2;
  int newlevel, oldlevel;
  void do_start (struct char_data *ch);
  extern int exp_to_level();
  void gain_exp (struct char_data *ch, int gain);

  two_arguments (argument, name, level);

  sprintf(buf,"LEVEL str %s. int %d\r\n",level, atoi(level));

  if (*name)
    {
      if (!(victim = get_char_vis (ch, name)))
	{
	  send_to_char ("That player is not here.\r\n", ch);
	  return;
	}
    }
  else
    {
      send_to_char ("Advance who?\r\n", ch);
      return;
    }
  if (GET_LEVEL (ch) <= GET_LEVEL (victim))
    {
      send_to_char ("Maybe that's not such a good idea.\r\n", ch);
      return;
    }
  if (IS_NPC (victim))
    {
      send_to_char ("NO!  Not on NPC's.\r\n", ch);
      return;
    }
  if (!*level || (newlevel = atoi (level)) <= 0)
    {
      send_to_char ("That's not a level!\r\n", ch);
      return;
    }
  if (newlevel > LVL_IMPL)
    {
      sprintf (buf, "%d is the highest possible level.\r\n", LVL_IMPL);
      send_to_char (buf, ch);
      return;
    }
  if (newlevel > GET_LEVEL (ch))
    {
      send_to_char ("Yeah, right.\r\n", ch);
      return;
    }
  if (newlevel == GET_LEVEL (victim))
    {
      send_to_char ("They are already at that level.\r\n", ch);
      return;
    }
  oldlevel = GET_LEVEL (victim);
  if (newlevel < GET_LEVEL (victim))
    {
      GET_EXP (victim) = 0;
      GET_LEVEL (victim) = newlevel;
      gain_exp_regardless(victim, exp_to_level(newlevel - 2));

      send_to_char ("You are momentarily enveloped by darkness!\r\n"
		    "You feel somewhat diminished.\r\n", victim);
    }
   else
    {
      act ("$n makes some strange gestures.\r\n"
	   "\r\n"
	   "A strange feeling comes upon you, like a giant hand, light comes\r\n"
	   "down from above, grabbing your body, that begins to pulse with\r\n"
	   "colored lights from inside.\r\n"
	   "\r\n"
	   "Your head seems to be filled with demons from another plane\r\n"
	   "as your body dissolves to the elements of time and space itself.\r\n"
	   "Suddenly a silent explosion of light snaps you back to reality.\r\n"
	   "\r\n"
	   "You feel slightly different.", FALSE, ch, 0, victim, TO_VICT);
    }

  send_to_char (OK, ch);

  sprintf (buf, "(GC) %s has advanced %s to level %d (from %d)",
	   GET_NAME (ch), GET_NAME (victim), newlevel, oldlevel);
  log (buf);

//// this just does not work with exp_to_level, so removed (Mulder)
//  gain_exp_regardless (victim,
//    level_exp (GET_CLASS (victim), newlevel) - GET_EXP (victim));

/* now thats more like it */
GET_EXP (victim) = 0;
gain_exp_regardless(victim, exp_to_level(newlevel - 1));
//GET_EXP (victim) = exp_to_level(GET_LEVEL(victim) - 1);
send_to_char(buf, ch);

  /* Set wizcommands */
  if (GET_LEVEL(victim) >= LVL_IMMORT)
  SET_BIT(GCMD_FLAGS(victim), GCMD_GEN);

  if (GET_LEVEL(victim) >= LVL_IMPL) {
      GCMD_FLAGS(victim) = (~GCMD_CMDSET) | GCMD_FLAGS(victim);
      GCMD2_FLAGS(victim) = ~0;
      GCMD3_FLAGS(victim) = ~0;
      GCMD4_FLAGS(victim) = ~0;
  }

  save_char (victim, NOWHERE);
}



ACMD (do_restore)
{
  struct char_data *vict;
  int i;

  one_argument (argument, buf);
  if (!*buf)
    send_to_char ("Whom do you wish to restore?\r\n", ch);
  else if (!(vict = get_char_vis (ch, buf)))
    send_to_char (NOPERSON, ch);
  else
    {
      GET_HIT (vict) = GET_MAX_HIT (vict);
      GET_MANA (vict) = GET_MAX_MANA (vict);
      GET_MOVE (vict) = GET_MAX_MOVE (vict);
      if (GET_LEVEL(vict)<LVL_IMMORT) {
	GET_COND(vict, FULL)=24;
	GET_COND(vict, THIRST)=24;
      }
      else {
	GET_COND(vict, FULL)=-100;
	GET_COND(vict, THIRST)=-100;
      }
      if ((GET_LEVEL (ch) >= LVL_GRGOD) && (GET_LEVEL (vict) >= LVL_IMMORT))
	{
	  for (i = 1; i <= MAX_SKILLS; i++)
	    SET_SKILL (vict, i, 100);

      SET_BIT(GCMD_FLAGS(vict), GCMD_GEN);

	  if (GET_LEVEL (vict) >= LVL_IMMORT)
	    {
	      vict->real_abils.str_add = 100;
	      vict->real_abils.intel = MAX_STAT;
	      vict->real_abils.wis = MAX_STAT;
	      vict->real_abils.dex = MAX_STAT;
	      vict->real_abils.str = MAX_STAT;
	      vict->real_abils.con = MAX_STAT;
	      vict->real_abils.cha = MAX_STAT;
	    }
	  vict->aff_abils = vict->real_abils;
	}
      update_pos (vict);
      send_to_char (OK, ch);
      act ("You have been fully healed by $N!", FALSE, vict, 0, ch, TO_CHAR);
      sprintf (buf, "(GC) %s restored by %s", GET_NAME (vict), GET_NAME (ch));
      mudlog (buf, BRF, LVL_GOD, TRUE);
    }
}


void
perform_immort_vis (struct char_data *ch)
{
  void appear (struct char_data *ch);

  if (GET_INVIS_LEV (ch) == 0 && !IS_AFFECTED (ch, AFF_HIDE | AFF_INVISIBLE))
    {
      send_to_char ("You are already fully visible.\r\n", ch);
      return;
    }

  GET_INVIS_LEV (ch) = 0;
  appear (ch);
  send_to_char ("You are now fully visible.\r\n", ch);
}


void
perform_immort_invis (struct char_data *ch, int level)
{
  struct char_data *tch;

  if (IS_NPC (ch))
    return;

  for (tch = world[ch->in_room].people; tch; tch = tch->next_in_room)
    {
      if (tch == ch)
	continue;
      if (GET_LEVEL (tch) >= GET_INVIS_LEV (ch) && GET_LEVEL (tch) < level)
	act ("You blink and suddenly realize that $n is gone.", FALSE, ch, 0,
	     tch, TO_VICT);
      if (GET_LEVEL (tch) < GET_INVIS_LEV (ch) && GET_LEVEL (tch) >= level)
	act ("You suddenly realize that $n is standing beside you.", FALSE, ch, 0,
	     tch, TO_VICT);
    }

  GET_INVIS_LEV (ch) = level;
  sprintf (buf, "Your invisibility level is %d.\r\n", level);
  send_to_char (buf, ch);
}


ACMD (do_invis)
{
  int level;

  if (IS_NPC (ch))
    {
      send_to_char ("You can't do that!\r\n", ch);
      return;
    }

  one_argument (argument, arg);
  if (!*arg)
    {
      if (GET_INVIS_LEV (ch) > 0)
	perform_immort_vis (ch);
      else
	perform_immort_invis (ch, GET_LEVEL (ch));
    }
  else
    {
      level = atoi (arg);
      if (level > GET_LEVEL (ch))
	send_to_char ("You can't go invisible above your own level.\r\n", ch);
      else if (level < 1)
	perform_immort_vis (ch);
      else
	perform_immort_invis (ch, level);
    }
}


ACMD (do_gecho)
{
  struct descriptor_data *pt;

  skip_spaces (&argument);
  delete_doubledollar (argument);

  if (!*argument)
    send_to_char ("That must be a mistake...\r\n", ch);
  else
    {
      sprintf (buf, "%s\r\n", argument);
      for (pt = descriptor_list; pt; pt = pt->next)
	if (!pt->connected && pt->character && pt->character != ch)
	  send_to_char (buf, pt->character);
      if (PRF_FLAGGED (ch, PRF_NOREPEAT))
	send_to_char (OK, ch);
      else
	send_to_char (buf, ch);
      sprintf (buf2, "(GC) gecho by %s: %s", GET_NAME(ch), buf);
      mudlog (buf2, NRM, LVL_IMPL, TRUE);
    }
 }

extern int mother_desc, port;
void Crash_rentsave(struct char_data * ch, int cost);

#define EXE_FILE "bin/circle" /* maybe use argv[0] but it's not reliable */

/* (c) 1996-97 Erwin S. Andreasen <erwin@pip.dknet.dk> */
ACMD(do_copyover)
{
	extern int circle_shutdown;
	FILE *fp;
	FILE *tf;
	struct descriptor_data *d, *d_next;
	struct timeval time;
	char buf [100], buf2[100];
        extern void write_aliases(struct char_data *ch);

	fp = fopen (COPYOVER_FILE, "w");
	
	if (!fp)
	{
		send_to_char ("Copyover file not writeable, aborted.\n\r",ch);
		return;
	}
	
	/* Consider changing all saved areas here, if you use OLC */
  if (olc_save_list) {
      struct olc_save_info *entry, *next_entry;
      int rznum;

      for (entry = olc_save_list; entry; entry = next_entry)
	{
	  next_entry = entry->next;
	  if (entry->type < 0 || entry->type > 4)
	    {
	      sprintf (buf, "OLC: Illegal save type %d!", entry->type);
	      log (buf);
	    }
	  else if ((rznum = real_zone (entry->zone * 100)) == -1)
	    {
	      sprintf (buf, "OLC: Illegal save zone %d!", entry->zone);
	      log (buf);
	    }
	  else if (rznum < 0 || rznum > top_of_zone_table)
	    {
	      sprintf (buf, "OLC: Invalid real zone number %d!", rznum);
	      log (buf);
	    }
	  else
	    {
	      sprintf (buf, "OLC: Reboot saving %s for zone %d.",
		save_info_msg[(int) entry->type], zone_table[rznum].number);
	      log (buf);
	      switch (entry->type)
		{
		case OLC_SAVE_ROOM:
		  redit_save_to_disk (rznum);
		  break;
		case OLC_SAVE_OBJ:
		  oedit_save_to_disk (rznum);
		  break;
		case OLC_SAVE_MOB:
		  medit_save_to_disk (rznum);
		  break;
		case OLC_SAVE_ZONE:
		  zedit_save_to_disk (rznum);
		  break;
		case OLC_SAVE_SHOP:
		  sedit_save_to_disk (rznum);
		  break;
		default:
		  log ("Unexpected olc_save_list->type");
		  break;
		}
	    }
	}
    }
	sprintf(buf, "\n\rThe server is being rebooted by %s. Please standby..\n\r", GET_NAME(ch));
	send_to_all(buf);

	/* For each playing descriptor, save its state */
	for (d = descriptor_list; d ; d = d_next)
	{
		struct char_data * och = d->character;
		d_next = d->next; /* We delete from the list , so need to save this */
		
		if (!d->character || d->connected > CON_PLAYING) /* drop those logging on */
		{
	    write_to_descriptor (d->descriptor, "\n\rSorry, we are rebooting. Come back in a minute.\n\r");
			close_socket (d); /* throw'em out */
		}
		else
		{
			fprintf (fp, "%d %s %s\n", d->descriptor, GET_NAME(och), d->host);

	    /* save och */
	    if (GET_WAS_IN(och)!=NOWHERE) {
	      if (och->in_room!=NOWHERE)
		char_from_room(och);
	      char_to_room(och, GET_WAS_IN(och));
	      GET_WAS_IN(och)=NOWHERE;
	    }
	    if (ismap(och->in_room)) {
	      och->player_specials->saved.mapx=rm2x(och->in_room);
	      och->player_specials->saved.mapy=rm2y(och->in_room);
	    }
	    else
	      och->player_specials->saved.tloadroom=GET_ROOM_VNUM(och->in_room);
	    circle_shutdown = 10; /* Don't remove this, this keeps our loadrooms intact. */
	    Crash_rentsave(och,0);
	    save_char(och, real_room(GET_LOADROOM(och)));
	    write_aliases(och);
			write_to_descriptor (d->descriptor, buf);
		}
	}
	
	fprintf (fp, "-1\n");
	fclose (fp);
	
	/* Close reserve and other always-open files and release other resources */

	if ((tf = fopen ("copyover.benchmark", "w"))) {
	  gettimeofday (&time, (struct timezone *) 0);
	  fwrite(&time, sizeof(time), 1, tf);
	  fclose(tf);
	}
    
	/* exec - descriptors are inherited */
	
	sprintf (buf, "%d", port);
    sprintf (buf2, "-C%d", mother_desc);

    /* Ugh, seems it is expected we are 1 step above lib - this may be dangerous! */
    chdir ("..");

	execl (EXE_FILE, "circle", buf2, buf, (char *) NULL);

	/* Failed - sucessful exec will not return */
	
	perror ("do_copyover: execl");
	send_to_char ("Copyover FAILED!\n\r",ch);
	
    exit (1); /* too much trouble to try to recover! */
}


ACMD (do_gplague)
{
  struct descriptor_data *pt;


  sprintf (buf, "&RYou have contracted the plague!&n\r\n");
  for (pt = descriptor_list; pt; pt = pt->next)
    if (!pt->connected && pt->character 
	&& pt->character != ch
	&& GET_LEVEL(pt->character) < 100){
      send_to_char (buf, pt->character);
      SET_BIT (AFF_FLAGS (pt->character), AFF_PLAGUED);
    }
  sprintf (buf2, "(GC) gplague by %s", GET_NAME(ch));
  mudlog (buf2, NRM, LVL_IMPL, TRUE);
}

ACMD (do_gcureplague)
{
  struct descriptor_data *pt;


  sprintf (buf, "You have been cured of the plague!\r\n");
  for (pt = descriptor_list; pt; pt = pt->next)
    if (!pt->connected && pt->character 
	&& pt->character != ch
	&& GET_LEVEL(pt->character) < 100){
      send_to_char (buf, pt->character);
      REMOVE_BIT (AFF_FLAGS (pt->character), AFF_PLAGUED);
    }
  sprintf (buf2, "(GC) gcureplague by %s", GET_NAME(ch));
  mudlog (buf2, NRM, LVL_IMPL, TRUE);
}


ACMD (do_poofset)
{
  char **msg;

  switch (subcmd)
    {
    case SCMD_POOFIN:
      msg = &(POOFIN (ch));
      break;
    case SCMD_POOFOUT:
      msg = &(POOFOUT (ch));
      break;
    default:
      return;
      break;
    }

  skip_spaces (&argument);

  if (*msg)
    free (*msg);

  if (!*argument)
    *msg = NULL;
  else
    *msg = str_dup (argument);

  send_to_char (OK, ch);
}



ACMD (do_dc)
{
  struct descriptor_data *d;
  int num_to_dc;

  one_argument (argument, arg);
  if (!(num_to_dc = atoi (arg)))
    {
      send_to_char ("Usage: DC <user number> (type USERS for a list)\r\n", ch);
      return;
    }
  for (d = descriptor_list; d && d->desc_num != num_to_dc; d = d->next);

  if (!d)
    {
      send_to_char ("No such connection.\r\n", ch);
      return;
    }
  if (d->character && GET_LEVEL (d->character) >= GET_LEVEL (ch))
    {
      if (!CAN_SEE (ch, d->character))
	send_to_char ("No such connection.\r\n", ch);
      else
	send_to_char ("Umm.. maybe that's not such a good idea...\r\n", ch);
      return;
    }

  /* We used to just close the socket here using close_socket(), but
   * various people pointed out this could cause a crash if you're
   * closing the person below you on the descriptor list.  Just setting
   * to CON_CLOSE leaves things in a massively inconsistent state so I
   * had to add this new flag to the descriptor.
   */
  d->close_me = 1;
  sprintf (buf, "Connection #%d closed.\r\n", num_to_dc);
  send_to_char (buf, ch);
  sprintf (buf, "(GC) Connection closed by %s.", GET_NAME (ch));
  log (buf);
}



ACMD (do_wizlock)
{
  int value;
  char *when;

  one_argument (argument, arg);
  if (*arg)
    {
      value = atoi (arg);
      if (value < 0 || value > GET_LEVEL (ch))
	{
	  send_to_char ("Invalid wizlock value.\r\n", ch);
	  return;
	}
      circle_restrict = value;
      when = "now";
    }
  else
    when = "currently";

  switch (circle_restrict)
    {
    case 0:
      sprintf (buf, "The game is %s completely open.\r\n", when);
      break;
    case 1:
      sprintf (buf, "The game is %s closed to new players.\r\n", when);
      break;
    default:
      sprintf (buf, "Only level %d and above may enter the game %s.\r\n",
	       circle_restrict, when);
      break;
    }
  send_to_char (buf, ch);
}


ACMD (do_date)
{
  char *tmstr;
  time_t mytime;
  int d, h, m;
  extern time_t boot_time;

  if (subcmd == SCMD_DATE)
    mytime = time (0);
  else
    mytime = boot_time;

  tmstr = (char *) asctime (localtime (&mytime));
  *(tmstr + strlen (tmstr) - 1) = '\0';

  if (subcmd == SCMD_DATE)
    sprintf (buf, "Current machine time: %s\r\nTo see the time in Deltanian format, type: time\r\n", tmstr);
  else
    {
      mytime = time (0) - boot_time;
      d = mytime / 86400;
      h = (mytime / 3600) % 24;
      m = (mytime / 60) % 60;

      sprintf (buf, "Up since %s: %d day%s, %d:%02d\r\n", tmstr, d,
	       ((d == 1) ? "" : "s"), h, m);
    }

  send_to_char (buf, ch);
}



ACMD (do_last)
{
  extern char *class_abbrevs[];
  int idnum=-1, level, class, last_logon;
  one_argument (argument, arg);
  if (!*arg)
    {
      send_to_char ("For whom do you wish to search?\r\n", ch);
      return;
    }
  pe_printf (arg, "nnnssn", "idnum,level,class,name,host,last_logon", &idnum, &level, &class, buf1, buf2, &last_logon);
  if (idnum==-1)
    {
      send_to_char ("There is no such player.\r\n", ch);
      return;
    }
  if ((level > GET_LEVEL (ch)) && (GET_LEVEL (ch) < LVL_IMPL))
    {
      send_to_char ("You are not sufficiently godly for that!\r\n", ch);
      return;
    }
  sprintf (buf, "[%5d] [%2d %s] %-12s : %-18s : %-20s\r\n",
           idnum, level,
           class_abbrevs[class], buf1, buf2, ctime ((long *)&last_logon));
  send_to_char (buf, ch);
}


ACMD (do_force)
{
  struct descriptor_data *i, *next_desc;
  struct char_data *vict, *next_force;
  char to_force[MAX_INPUT_LENGTH + 2];

  half_chop (argument, arg, to_force);

  sprintf (buf1, "$n has forced you to '%s'.", to_force);

  if (!*arg || !*to_force)
    send_to_char ("Whom do you wish to force do what?\r\n", ch);
  else if ((GET_LEVEL (ch) < LVL_GRGOD) || (str_cmp ("all", arg) && str_cmp ("room", arg)))
    {
      if (!(vict = get_char_vis (ch, arg)))
	send_to_char (NOPERSON, ch);
      else if (GET_LEVEL (ch) <= GET_LEVEL (vict))
	send_to_char ("No, no, no!\r\n", ch);
      else
	{
	  send_to_char (OK, ch);
	  act (buf1, TRUE, ch, NULL, vict, TO_VICT);
	  sprintf (buf, "(GC) %s forced %s to %s", GET_NAME (ch), GET_NAME (vict), to_force);
	  mudlog (buf, NRM, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
	  command_interpreter (vict, to_force);
	}
    }
  else if (!str_cmp ("room", arg))
    {
      send_to_char (OK, ch);
      sprintf (buf, "(GC) %s forced room %d to %s", GET_NAME (ch), (int) world[ch->in_room].number, to_force);
      mudlog (buf, NRM, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);

      for (vict = world[ch->in_room].people; vict; vict = next_force)
	{
	  next_force = vict->next_in_room;
	  if (GET_LEVEL (vict) >= GET_LEVEL (ch))
	    continue;
	  act (buf1, TRUE, ch, NULL, vict, TO_VICT);
	  command_interpreter (vict, to_force);
	}
    }
  else
    {                           /* force all */
      send_to_char (OK, ch);
      sprintf (buf, "(GC) %s forced all to %s", GET_NAME (ch), to_force);
      mudlog (buf, NRM, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);

      for (i = descriptor_list; i; i = next_desc)
	{
	  next_desc = i->next;

	  if (i->connected || !(vict = i->character) || GET_LEVEL (vict) >= GET_LEVEL (ch))
	    continue;
	  act (buf1, TRUE, ch, NULL, vict, TO_VICT);
	  command_interpreter (vict, to_force);
	}
    }
}



ACMD (do_wiznet)
{
  struct descriptor_data *d;
  char emote = FALSE;
  char any = FALSE;
  int level = LVL_IMMORT;

  skip_spaces (&argument);
  delete_doubledollar (argument);

  if (!*argument)
    {
      send_to_char ("Usage: wiznet <text> | #<level> <text> | *<emotetext> |\r\n "
		    "       wiznet @<level> *<emotetext> | wiz @\r\n", ch);
      return;
    }
  switch (*argument)
    {
    case '*':
      emote = TRUE;
    case '#':
      one_argument (argument + 1, buf1);
      if (is_number (buf1))
	{
	  half_chop (argument + 1, buf1, argument);
	  level = MAX (atoi (buf1), LVL_IMMORT);
	  if (level > GET_LEVEL (ch))
	    {
	      send_to_char ("You can't wizline above your own level.\r\n", ch);
	      return;
	    }
	}
      else if (emote)
	argument++;
      break;
    case '@':
      for (d = descriptor_list; d; d = d->next)
	{
	  if (!d->connected && GET_LEVEL (d->character) >= LVL_IMMORT &&
	      !PRF_FLAGGED (d->character, PRF_NOWIZ) &&
	      (CAN_SEE (ch, d->character) || GET_LEVEL (ch) == LVL_IMPL))
	    {
	      if (!any)
		{
		  sprintf (buf1, "Gods online:\r\n");
		  any = TRUE;
		}
	      sprintf (buf1, "%s  %s", buf1, GET_NAME (d->character));
	      if (PLR_FLAGGED (d->character, PLR_WRITING))
		sprintf (buf1, "%s (Writing)\r\n", buf1);
	      else if (PLR_FLAGGED (d->character, PLR_MAILING))
		sprintf (buf1, "%s (Writing mail)\r\n", buf1);
	      else
		sprintf (buf1, "%s\r\n", buf1);

	    }
	}
      any = FALSE;
      for (d = descriptor_list; d; d = d->next)
	{
	  if (!d->connected && GET_LEVEL (d->character) >= LVL_IMMORT &&
	      PRF_FLAGGED (d->character, PRF_NOWIZ) &&
	      CAN_SEE (ch, d->character))
	    {
	      if (!any)
		{
		  sprintf (buf1, "%sGods offline:\r\n", buf1);
		  any = TRUE;
		}
	      sprintf (buf1, "%s  %s\r\n", buf1, GET_NAME (d->character));
	    }
	}
      send_to_char (buf1, ch);
      return;
      break;
    case '\\':
      ++argument;
      break;
    default:
      break;
    }
  if (PRF_FLAGGED (ch, PRF_NOWIZ))
    {
      send_to_char ("You are offline!\r\n", ch);
      return;
    }
  skip_spaces (&argument);

  if (!*argument)
    {
      send_to_char ("Don't bother the gods like that!\r\n", ch);
      return;
    }
  if (level > LVL_IMMORT)
    {
      sprintf (buf1, "%s: <%d> %s%s\r\n", GET_NAME (ch), level,
	       emote ? "<--- " : "", argument);
      sprintf (buf2, "Someone: <%d> %s%s\r\n", level, emote ? "<--- " : "",
	       argument);
    }
  else
    {
      sprintf (buf1, "%s: %s%s\r\n", GET_NAME (ch), emote ? "<--- " : "",
	       argument);
      sprintf (buf2, "Someone: %s%s\r\n", emote ? "<--- " : "", argument);
    }

  for (d = descriptor_list; d; d = d->next)
    {
      if ((!d->connected) && (GET_LEVEL (d->character) >= level) &&
	  (!PRF_FLAGGED (d->character, PRF_NOWIZ)) &&
	  (!PLR_FLAGGED (d->character, PLR_WRITING | PLR_MAILING))
	  && (d != ch->desc || !(PRF_FLAGGED (d->character, PRF_NOREPEAT))))
	{
	  send_to_char (CCCYN (d->character, C_NRM), d->character);
	  if (CAN_SEE (d->character, ch))
	    send_to_char (buf1, d->character);
	  else
	    send_to_char (buf2, d->character);
	  send_to_char (CCNRM (d->character, C_NRM), d->character);
	}
    }

  if (PRF_FLAGGED (ch, PRF_NOREPEAT))
    send_to_char (OK, ch);
}



ACMD (do_zreset)
{
  void reset_zone (int zone);

  int i, j;

  one_argument (argument, arg);
  if (!*arg)
    {
      send_to_char ("You must specify a zone.\r\n", ch);
      return;
    }
  if (*arg == '*')
    {
      if (GET_LEVEL(ch) < LVL_GRGOD) {
	send_to_char("You are not holy enough to do that!\r\n", ch);
	return;
      }
      for (i = 0; i <= top_of_zone_table; i++)
	reset_zone (i);
      send_to_char ("Reset world.\r\n", ch);
      sprintf (buf, "(GC) %s reset entire world.", GET_NAME (ch));
      mudlog (buf, NRM, MAX (LVL_GRGOD, GET_INVIS_LEV (ch)), TRUE);
      return;
    }
  else if (*arg == '.')
    i = world[ch->in_room].zone;
  else
    {
      j = atoi (arg);
      for (i = 0; i <= top_of_zone_table; i++)
	if (zone_table[i].number == j)
	  break;
    }
  if (i >= 0 && i <= top_of_zone_table)
    {
      if (!can_edit_zone(ch, i) && GET_LEVEL(ch) < LVL_GRGOD) {
	send_to_char("You do not have permission to reset this zone.\r\n", ch);
	return;
      }
      reset_zone (i);
      sprintf (buf, "Reset zone %d (#%d): %s.\r\n", i, zone_table[i].number,
	       zone_table[i].name);
      send_to_char (buf, ch);
      sprintf (buf, "(GC) %s reset zone %d (%s)", GET_NAME (ch), i, zone_table[i].name);
      mudlog (buf, NRM, MAX (LVL_GRGOD, GET_INVIS_LEV (ch)), TRUE);
    }
  else
    send_to_char ("Invalid zone number.\r\n", ch);
}


/*
 *  General fn for wizcommands of the sort: cmd <player>
 */

ACMD (do_wizutil)
{
  struct char_data *vict;
  long result;
  void roll_real_abils (struct char_data *ch);

  one_argument (argument, arg);

  if (!*arg)
    send_to_char ("Yes, but for whom?!?\r\n", ch);
  else if (!(vict = get_char_vis (ch, arg)))
    send_to_char ("There is no such player.\r\n", ch);
  else if (IS_NPC (vict))
    send_to_char ("You can't do that to a mob!\r\n", ch);
  else if (GET_LEVEL (vict) > GET_LEVEL (ch))
    send_to_char ("Hmmm...you'd better not.\r\n", ch);
  else
    {
      switch (subcmd)
	{
	case SCMD_REROLL:
	  send_to_char ("Rerolled...\r\n", ch);
	  roll_real_abils (vict);
	  sprintf (buf, "(GC) %s has rerolled %s.", GET_NAME (ch), GET_NAME (vict));
	  log (buf);
	  sprintf (buf, "New stats: Str %d/%d, Int %d, Wis %d, Dex %d, Con %d, Cha %d\r\n",
	     GET_STR (vict), GET_ADD (vict), GET_INT (vict), GET_WIS (vict),
		   GET_DEX (vict), GET_CON (vict), GET_CHA (vict));
	  send_to_char (buf, ch);
	  break;
	case SCMD_PARDON:
	  if (!PLR_FLAGGED (vict, PLR_THIEF | PLR_KILLER))
	    {
	      send_to_char ("Your victim is not flagged.\r\n", ch);
	      return;
	    }
	  REMOVE_BIT (PLR_FLAGS (vict), PLR_THIEF | PLR_KILLER);
	  send_to_char ("Pardoned.\r\n", ch);
	  send_to_char ("You have been pardoned by the gods!\r\n", vict);
	  sprintf (buf, "(GC) %s pardoned by %s", GET_NAME (vict), GET_NAME (ch));
	  mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
	  break;
	case SCMD_NOTITLE:
	  result = PLR_TOG_CHK (vict, PLR_NOTITLE);
	  sprintf (buf, "(GC) Notitle %s for %s by %s.", ONOFF (result),
		   GET_NAME (vict), GET_NAME (ch));
	  mudlog (buf, NRM, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
	  strcat (buf, "\r\n");
	  send_to_char (buf, ch);
	  break;
	case SCMD_SQUELCH:
	  result = PLR_TOG_CHK (vict, PLR_NOSHOUT);
	  sprintf (buf, "(GC) Squelch %s for %s by %s.", ONOFF (result),
		   GET_NAME (vict), GET_NAME (ch));
	  mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
	  strcat (buf, "\r\n");
	  send_to_char (buf, ch);
	  break;
	case SCMD_FREEZE:
	  if (ch == vict)
	    {
	      send_to_char ("Oh, yeah, THAT'S real smart...\r\n", ch);
	      return;
	    }
	  if (PLR_FLAGGED (vict, PLR_FROZEN))
	    {
	      send_to_char ("Your victim is already pretty cold.\r\n", ch);
	      return;
	    }
	  SET_BIT (PLR_FLAGS (vict), PLR_FROZEN);
	  GET_FREEZE_LEV (vict) = GET_LEVEL (ch);
	  send_to_char ("A bitter wind suddenly rises and drains every erg of heat from your body!\r\nYou feel frozen!\r\n", vict);
	  send_to_char ("Frozen.\r\n", ch);
	  act ("A sudden cold wind conjured from nowhere freezes $n!", FALSE, vict, 0, 0, TO_ROOM);
	  sprintf (buf, "(GC) %s frozen by %s.", GET_NAME (vict), GET_NAME (ch));
	  mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
	  break;
	case SCMD_THAW:
	  if (!PLR_FLAGGED (vict, PLR_FROZEN))
	    {
	      send_to_char ("Sorry, your victim is not morbidly encased in ice at the moment.\r\n", ch);
	      return;
	    }
	  if (GET_FREEZE_LEV (vict) > GET_LEVEL (ch))
	    {
	      sprintf (buf, "Sorry, a level %d God froze %s... you can't unfreeze %s.\r\n",
		       GET_FREEZE_LEV (vict), GET_NAME (vict), HMHR (vict));
	      send_to_char (buf, ch);
	      return;
	    }
	  sprintf (buf, "(GC) %s un-frozen by %s.", GET_NAME (vict), GET_NAME (ch));
	  mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
	  REMOVE_BIT (PLR_FLAGS (vict), PLR_FROZEN);
	  send_to_char ("A fireball suddenly explodes in front of you, melting the ice!\r\nYou feel thawed.\r\n", vict);
	  send_to_char ("Thawed.\r\n", ch);
	  act ("A sudden fireball conjured from nowhere thaws $n!", FALSE, vict, 0, 0, TO_ROOM);
	  break;
	case SCMD_UNAFFECT:
	  if (vict->affected)
	    {
	      while (vict->affected)
		affect_remove (vict, vict->affected);
	      send_to_char ("There is a brief flash of light!\r\n"
			    "You feel slightly different.\r\n", vict);
	      send_to_char ("All spells removed.\r\n", ch);
	    }
	  else
	    {
	      send_to_char ("Your victim does not have any affections!\r\n", ch);
	      return;
	    }
	  break;
	default:
	  log ("SYSERR: Unknown subcmd passed to do_wizutil (act.wizard.c)");
	  break;
	}
      save_char (vict, NOWHERE);
    }
}


/* single zone printing fn used by "show zone" so it's not repeated in the
   code 3 times ... -je, 4/6/93 */

void
print_zone_to_buf (char *bufptr, int zone)
{
  sprintf (bufptr, "%s%3d %-30.30s Age: %3d; Reset: %3d (%1d); Top: %5d\r\n",
	   bufptr, zone_table[zone].number, zone_table[zone].name,
	   zone_table[zone].age, zone_table[zone].lifespan,
	   zone_table[zone].reset_mode, zone_table[zone].top);
}


ACMD (do_show)
{
  struct char_data *vbuf;
  int i, j, k, l, con;
  char self = 0;
  struct char_data *vict;
  struct obj_data *obj;
  char field[MAX_INPUT_LENGTH], value[MAX_INPUT_LENGTH], birth[80];
  extern char *class_abbrevs[];
  extern char *genders[];
  extern int buf_switches, buf_largecount, buf_overflows;
  void show_shops (struct char_data *ch, char *value);
  void hcontrol_list_houses (struct char_data *ch);

  struct show_struct
    {
      char *cmd;
      char level;
    }
  fields[] =
  {
    {
      "nothing", 0
    }
    ,                           /* 0 */
    {
      "zones", LVL_IMMORT
    }
    ,                           /* 1 */
    {
      "player", LVL_GOD
    }
    ,
    {
      "rent", LVL_GOD
    }
    ,
    {
      "stats", LVL_IMMORT
    }
    ,
    {
      "errors", LVL_IMPL
    }
    ,                           /* 5 */
    {
      "death", LVL_GOD
    }
    ,
    {
      "godrooms", LVL_GOD
    }
    ,
    {
      "shops", LVL_IMMORT
    }
    ,
    {
      "houses", LVL_GOD
    }
    ,
    {
      "\n", 0
    }
  };

  skip_spaces (&argument);

  if (!*argument)
    {
      strcpy (buf, "Show options:\r\n");
      for (j = 0, i = 1; fields[i].level; i++)
	if (fields[i].level <= GET_LEVEL (ch))
	  sprintf (buf, "%s%-15s%s", buf, fields[i].cmd, (!(++j % 5) ? "\r\n" : ""));
      strcat (buf, "\r\n");
      send_to_char (buf, ch);
      return;
    }

  strcpy (arg, two_arguments (argument, field, value));

  for (l = 0; *(fields[l].cmd) != '\n'; l++)
    if (!strncmp (field, fields[l].cmd, strlen (field)))
      break;

 if (GET_TRUST_LEVEL(ch) < fields[l].level) {
      send_to_char ("You are not godly enough for that!\r\n", ch);
      return;
    }
  if (!strcmp (value, "."))
    self = 1;
  buf[0] = '\0';
  switch (l)
    {
    case 1:                     /* zone */
      /* tightened up by JE 4/6/93 */
      if (self)
	print_zone_to_buf (buf, world[ch->in_room].zone);
      else if (*value && is_number (value))
	{
	  for (j = atoi (value), i = 0; zone_table[i].number != j && i <= top_of_zone_table; i++);
	  if (i <= top_of_zone_table)
	    print_zone_to_buf (buf, i);
	  else
	    {
	      send_to_char ("That is not a valid zone.\r\n", ch);
	      return;
	    }
	}
      else
	for (i = 0; i <= top_of_zone_table; i++)
	  print_zone_to_buf (buf, i);
      send_to_char (buf, ch);
      break;
    case 2:                     /* player */
      if (!*value)
	{
	  send_to_char ("A name would help.\r\n", ch);
	  return;
	}
      CREATE (vbuf, struct char_data, 1);
      clear_char (vbuf);
      if (retrieve_player_entry (value, vbuf) < 0)
	{
	  send_to_char ("There is no such player.\r\n", ch);
	  return;
	}
      sprintf (buf, "Player: %-12s (%s) [%2d %s]\r\n", GET_NAME(vbuf),
      genders[(int) GET_SEX(vbuf)], GET_LEVEL(vbuf), class_abbrevs[(int) GET_CLASS(vbuf)]);
      sprintf (buf,
	 "%sAu: %-8d  Bal: %-8d  Exp: %-8d  Align: %-5d  Lessons: %-3d\r\n",
               buf, GET_GOLD(vbuf), GET_BANK_GOLD(vbuf), GET_EXP(vbuf),
               GET_ALIGNMENT(vbuf),
               GET_PRACTICES(vbuf));
      strcpy (birth, ctime (&vbuf->player.time.birth));
      sprintf (buf,
	       "%sStarted: %-20.16s  Last: %-20.16s  Played: %3dh %2dm\r\n",
           buf, birth, ctime (&vbuf->player.time.logon), (int) (vbuf->player.time.played / 3600),
               (int) (vbuf->player.time.played / 60 % 60));
      send_to_char (buf, ch);
      free_char(vbuf);
      break;
    case 3:
      Crash_listrent (ch, value);
      break;
    case 4:
      i = 0;
      j = 0;
      k = 0;
      con = 0;
      for (vict = character_list; vict; vict = vict->next)
	{
	  if (IS_NPC (vict))
	    j++;
	  else if (CAN_SEE (ch, vict))
	    {
	      i++;
	      if (vict->desc)
		con++;
	    }
	}
      for (obj = object_list; obj; obj = obj->next)
	k++;
      sprintf (buf, "Current stats:\r\n");
      sprintf (buf, "%s  %5d players in game  %5d connected\r\n", buf, i, con);
      sprintf (buf, "%s  %5d registered\r\n", buf, top_of_p_table + 1);
      sprintf (buf, "%s  %5d mobiles          %5d prototypes\r\n",
	       buf, j, top_of_mobt + 1);
      sprintf (buf, "%s  %5d objects          %5d prototypes\r\n",
	       buf, k, top_of_objt + 1);
      sprintf (buf, "%s  %5d rooms            %5d zones\r\n",
	       buf, top_of_world + 1, top_of_zone_table + 1);
      sprintf (buf, "%s  %5d large bufs\r\n", buf, buf_largecount);
      sprintf (buf, "%s  %5d buf switches     %5d overflows\r\n", buf,
	       buf_switches, buf_overflows);
      send_to_char (buf, ch);
      break;
    case 5:
      strcpy (buf, "Errant Rooms\r\n------------\r\n");
      for (i = 0, k = 0; i <= top_of_world; i++)
	for (j = 0; j < NUM_OF_DIRS; j++)
	  if (world[i].dir_option[j] && world[i].dir_option[j]->to_room == 0)
            sprintf (buf, "%s%2d: [%5d] %s\r\n", buf, ++k, (int) world[i].number,
		     world[i].name);
      send_to_char (buf, ch);
      break;
    case 6:
      strcpy (buf, "Death Traps\r\n-----------\r\n");
      for (i = 0, j = 0; i <= top_of_world; i++)
	if (IS_SET (ROOM_FLAGS (i), ROOM_DEATH))
	  sprintf (buf, "%s%2d: [%5d] %s\r\n", buf, ++j,
                   (int) world[i].number, world[i].name);
      send_to_char (buf, ch);
      break;
    case 7:
      strcpy (buf, "Godrooms\r\n--------------------------\r\n");
      for (i = 0, j = 0; i < top_of_world; i++)
	if (ROOM_FLAGGED (i, ROOM_GODROOM))
          sprintf (buf, "%s%2d: [%5d] %s\r\n", buf, ++j, (int) world[i].number, world[i].name);
      send_to_char (buf, ch);
      break;
    case 8:
      show_shops (ch, value);
      break;
    case 9:
      hcontrol_list_houses (ch);
      break;
    default:
      send_to_char ("Sorry, I don't understand that.\r\n", ch);
      break;
    }
}


/***************** The do_set function ***********************************/

#define PC   1
#define NPC  2
#define BOTH 3

#define MISC    0
#define BINARY  1
#define NUMBER  2

#define SET_OR_REMOVE(flagset, flags) { \
	if (on) SET_BIT(flagset, flags); \
	else if (off) REMOVE_BIT(flagset, flags); }

#define RANGE(low, high) (value = MAX((low), MIN((high), (value))))


/* The set options available */
/* OK. I decided that doing a 'set' to list available set fields
 * results in a huge messy unsorted list. But rather than do a 
 * quick-sort everytime someone does a 'set', we'll keep it sorted
 * here instead. But this would screw up the big switch() statement 
 * in perform_set() below, so the actual switchnum is put in this
 * structure instead. This will allow you to just add to the bottom
 * of the big switch() statement, whilst keeping the sorted output.
 * This isn't the cleanest way to do it, but I'm too lazy to change 
 * the entire structure.
 * - Thargor -
 * 
 * To add a new set field:
 * 1. The number in the NUMSETFIELDS below is the number of the next
 *    field's switchnum. Remember this. Let's call it XXX
 * 2. Add 1 to the NUMSETFIELDS defined below.
 * 3. Add a new data entry, which should look like:
 *        { XXX, "whateverfield", LVL_IMPL, PC, BINARY },
 *    where XXX is the number you got in step 1. The rest show above
 *    are samples. Change to whatever you're adding in.
 * 4. Find the best place to slot the new entry into, making sure
 *    that the sorted order is still OK.
 * 5. You're done here. Now go down to the big switch(switchmode){ case ...: }
 *    code below and add your new entry. You don't have to keep that
 *    part sorted.
 *
 * Side note: 
 * 1. If the structure gets unsorted again, a quick way to sort it is
 *    to cut and paste all the entries into a new temp file (for 
 *    example: sortthis.txt) then run:  
 *        cat sortthis.txt | sort -i -t \" -k 2 > sorted.txt
 *    Then you get sorted.txt which you can cut and paste back here.
 * 2. If you decide that this is wayyy to messy and wanna change it
 *    back to what it used to be, copy and paste as above, then run:
 *        cat sortthis.txt | sort > original.txt
 *    You'll have to do some cut and pasting by hand after that.
 */

struct set_struct
{
  int switchnum;
  char *cmd;
  char level;
  char pcnpc;
  char type;
}
set_fields[] =
{
  {  18, "defense",         LVL_GRGOD,     BOTH,   NUMBER },
  {  49, "afk",             LVL_DEMIGOD,       PC,     BINARY },
  {  10, "align",           LVL_DEMIGOD,       BOTH,   NUMBER },
  {  20, "bank",            LVL_GOD,       PC,     NUMBER },
  {   0, "brief",           LVL_DEMIGOD,       PC,     BINARY },
  {  17, "cha",             LVL_GRGOD,     BOTH,   NUMBER },
  { 122, "citizen",         LVL_IMPL,      PC,     NUMBER },
  { 123, "mbuilder",        LVL_GRGOD,     PC,     BINARY },
  { 124, "cmdmap",          LVL_GRGOD,     PC,     BINARY },
  { 125, "cmdlweather",     LVL_GRGOD,     PC,     BINARY },
  { 126, "cmdpfileclean",   LVL_IMPL,      PC,     BINARY },
  {  39, "class",           LVL_IMPL,     BOTH,   MISC   },
  {  55, "cmdadvance",      LVL_GOD,      PC,     BINARY },
  { 102, "cmdaload",        LVL_GRGOD,      PC,     BINARY },
  {  56, "cmdat",           LVL_GOD,      PC,     BINARY },
  { 100, "cmdattach",       LVL_IMPL,      PC,     BINARY },
  {  75, "cmdauctioneer",   LVL_IMPL,      PC,     BINARY },
  { 108, "cmdprophet",      LVL_IMPL,      PC,     BINARY },
  {  57, "cmdban",          LVL_GRGOD,      PC,     BINARY },
  {  84, "cmdsnow",         LVL_IMPL,      PC,     BINARY },
  {  58, "cmddc",           LVL_GRGOD,      PC,     BINARY },
  { 105, "cmdsage",         LVL_GOD,      PC,     BINARY },
  {  59, "cmdecho",         LVL_GRGOD,      PC,     BINARY },
  {  60, "cmdforce",        LVL_GRGOD,      PC,     BINARY },
  {  61, "cmdfreeze",       LVL_GRGOD,      PC,     BINARY },
  {  90, "cmdgecho",        LVL_GRGOD,      PC,     BINARY },
  {  54, "cmdgeneral",      LVL_DEMIGOD,      PC,     BINARY },
  { 106, "cmdseer",         LVL_GRGOD,      PC,     BINARY },
  {  62, "cmdhcontrol",     LVL_IMPL,      PC,     BINARY },
  { 104, "cmdimp",          LVL_IMPL,      PC,     BINARY },
  { 121, "cmdimpolc",       LVL_IMPL,      PC,     BINARY },
  {  86, "cmdinvis",        LVL_GOD,      PC,     BINARY },
  {  83, "cmdisay",         LVL_GOD,      PC,     BINARY },
  {  63, "cmdload",         LVL_GRGOD,      PC,     BINARY },
  {  87, "cmdmcasters",     LVL_GRGOD,      PC,     BINARY },
  {  88, "cmdmudheal",      LVL_GRGOD,      PC,     BINARY },
  {  64, "cmdmute",         LVL_GOD,      PC,     BINARY },
  {  93, "cmdnotitle",      LVL_GOD,      PC,     BINARY },
  {  85, "cmdolc",          LVL_GRGOD,      PC,     BINARY },
  {  94, "cmdpage",         LVL_GOD,      PC,     BINARY },
  {  66, "cmdpardon",       LVL_GOD,      PC,     BINARY },
  { 120, "cmdpeace",        LVL_GOD,      PC,     BINARY },
  {  79, "cmdplague",       LVL_IMPL,      PC,     BINARY },
  {  67, "cmdpurge",        LVL_GRGOD,      PC,     BINARY },
  {  95, "cmdqecho",        LVL_GRGOD,      PC,     BINARY },
  { 117, "cmdquestmobs",    LVL_GRGOD,      PC,     BINARY },
  { 130, "cmdrebalance",    LVL_IMPL,     PC,      BINARY},
  {  68, "cmdreload",       LVL_IMPL,      PC,     BINARY },
  {  69, "cmdreroll",       LVL_IMPL,      PC,     BINARY },
  { 112, "cmdrespec",       LVL_IMPL,      PC,     BINARY },
  {  70, "cmdrestore",      LVL_GRGOD,      PC,     BINARY },
  { 118, "cmdreward",       LVL_GRGOD,      PC,     BINARY },
  {  89, "cmdrewiz",        LVL_IMPL,      PC,     BINARY },
  {  92, "cmdrewww",        LVL_IMPL,      PC,     BINARY },
  {  71, "cmdsend",         LVL_GRGOD,      PC,     BINARY },
  {  72, "cmdset",          LVL_GRGOD,      PC,     BINARY },
  {  97, "cmdsetreboot",    LVL_IMPL,      PC,     BINARY },
  {  73, "cmdshutdown",     LVL_IMPL,      PC,     BINARY },
  {  74, "cmdskillset",     LVL_GRGOD,      PC,     BINARY },
  {  76, "cmdslowns",       LVL_IMPL,      PC,     BINARY },
  {  77, "cmdsnoop",        LVL_GRGOD,      PC,     BINARY },
  {  78, "cmdswitch",       LVL_GRGOD,      PC,     BINARY },
  {  65, "cmdsyslog",       LVL_GRGOD,      PC,     BINARY },
  {  98, "cmdtmobdie",      LVL_IMPL,      PC,     BINARY },
  {  80, "cmdtransfer",     LVL_GOD,      PC,     BINARY },
  {  81, "cmdunaffect",     LVL_GOD,      PC,     BINARY },
  { 101, "cmdusers",        LVL_GRGOD,      PC,     BINARY },
  {  82, "cmdwizlock",      LVL_IMPL,      PC,     BINARY },
  {  99, "cmdwrestrict",    LVL_IMPL,      PC,     BINARY },
  {  96, "cmdzreset",       LVL_GRGOD,      PC,     BINARY },
  {  43, "color",           LVL_GOD,       PC,     BINARY },
  {  16, "con",             LVL_GRGOD,     BOTH,   NUMBER },
  {  23, "mdefense",         LVL_GRGOD,     BOTH,   NUMBER },
  {  38, "deleted",         LVL_GRGOD,      PC,     BINARY },
  {  15, "dex",             LVL_GRGOD,     BOTH,   NUMBER },
  {  29, "drunk",           LVL_GOD,     BOTH,   NUMBER },
  {  21, "exp",             LVL_GRGOD,     BOTH,   NUMBER },
  {  26, "frozen",          LVL_FREEZE,    PC,     BINARY },
  {  19, "gold",            LVL_GOD,       BOTH,   NUMBER },
  {   7, "hit",             LVL_GRGOD,     BOTH,   NUMBER },
  {  22, "power",         LVL_GRGOD,     BOTH,   NUMBER },
  {  51, "hometown",        LVL_GRGOD,     PC,     MISC   },
  {  30, "hunger",          LVL_GRGOD,     BOTH,   NUMBER },
  {  44, "idnum",           LVL_IMPL,      PC,     NUMBER },
  {  13, "int",             LVL_GRGOD,     BOTH,   NUMBER },
  { 129, "intangible",      LVL_GRGOD,    PC,      BINARY},
  {  24, "invis",           LVL_IMPL,      PC,     NUMBER },
  {   1, "invstart",        LVL_GOD,       PC,     BINARY } ,
  {  32, "killer",          LVL_GOD,       PC,     BINARY },
  {  28, "lessons",         LVL_GRGOD,     PC,     NUMBER },
  {  34, "level",           LVL_GRGOD,      BOTH,   NUMBER },
  {  42, "loadroom",        LVL_GRGOD,     PC,     NUMBER   },
  { 113, "lockout",         LVL_IMPL,      PC,     BINARY },
  { 111, "losses",          LVL_GRGOD,    PC,     NUMBER },
  {   8, "mana",            LVL_GRGOD,     BOTH,   NUMBER },
  {   4, "maxhit",          LVL_GRGOD,     BOTH,   NUMBER },
  {   5, "maxmana",         LVL_GRGOD,     BOTH,   NUMBER },
  {   6, "maxmove",         LVL_GRGOD,     BOTH,   NUMBER },
  {   9, "move",            LVL_GRGOD,     BOTH,   NUMBER },
  { 119, "multiok",         LVL_IMPL,      PC,     BINARY },
  { 127, "mpower",          LVL_GRGOD,     BOTH,     NUMBER },
  {  46, "nodelete",        LVL_GOD,       PC,     BINARY },
  {  25, "nohassle",        LVL_GRGOD,     PC,     BINARY },
  {   3, "nosummon",        LVL_GRGOD,     PC,     BINARY },
  {  40, "nowizlist",       LVL_GOD,       PC,     BINARY },
  {  45, "passwd",          LVL_IMPL,      PC,     MISC   },
  {  27, "practices",       LVL_GRGOD,     PC,     NUMBER },
  {  41, "qchan",           LVL_GOD,       PC,     BINARY },
  { 115, "questnext",       LVL_IMPL,      PC,     NUMBER },
  { 114, "questor",         LVL_IMPL,      PC,     BINARY },
  { 116, "questpts",        LVL_GRGOD,      PC,     NUMBER },
  {  50, "race",            LVL_IMPL,     PC,     MISC   },
  {  35, "room",            LVL_IMPL,      BOTH,   NUMBER },
  {  36, "roomflag",        LVL_GRGOD,     PC,     BINARY },
  { 103, "setall",          LVL_IMPL,      PC,     BINARY },
  {  47, "sex",             LVL_GOD,     BOTH,   MISC   },
  {  37, "siteok",          LVL_GRGOD,     PC,     BINARY },
  {  11, "str",             LVL_GRGOD,     BOTH,   NUMBER },
  {  12, "stradd",          LVL_GRGOD,     BOTH,   NUMBER },
  { 128, "technique",       LVL_GRGOD,     BOTH,     NUMBER },
  {  33, "thief",           LVL_GOD,       PC,     BINARY },
  {  31, "thirst",          LVL_GRGOD,     BOTH,   NUMBER },
  {   2, "title",           LVL_GOD,       PC,     MISC   },
  {  53, "trains",          LVL_IMPL,      PC,     NUMBER },
  {  52, "trust",           LVL_IMPL,      PC,     NUMBER },
  { 110, "wins",            LVL_GRGOD,    PC,     NUMBER },
  {  14,  "wis",            LVL_GRGOD,     BOTH,   NUMBER },
  /* The entry below MUST always come last! */
  { -1, "\n", 0, BOTH, MISC  }
};

/* NOTE NOTE NOTE: See instructions at the above of this big table. */
#define NUMSETFIELDS 130

int
perform_set (struct char_data *ch, struct char_data *vict, int mode,
	     char *val_arg)
{
  int i, on = 0, off = 0, value = 0, switchmode;
  char output[MAX_STRING_LENGTH];
  int parse_class (char arg);
  int parse_race (char arg);
  int parse_town (char arg);

  switchmode = set_fields[mode].switchnum;

  if (!ch) {
      if (!strcmp (val_arg, "on") || !strcmp (val_arg, "yes"))
	on = 1;
      else if (!strcmp (val_arg, "off") || !strcmp (val_arg, "no"))
	off = 1;
  }
  else {
  /* Check to make sure all the levels are correct */
  if (GET_LEVEL(ch) != LVL_IMPL)
  {
     if (!IS_NPC (vict) && GET_LEVEL(ch) <= GET_LEVEL(vict) && vict != ch)
       {
	 send_to_char ("Maybe that's not such a great idea...\r\n", ch);
	 return 0;
       }
  }
  if (GET_LEVEL (ch) < set_fields[mode].level)
    {
      send_to_char ("You are not godly enough for that!\r\n", ch);
      return 0;
    }

  /* Make sure the PC/NPC is correct */
  if (IS_NPC (vict) && !(set_fields[mode].pcnpc & NPC))
    {
      send_to_char ("You can't do that to a beast!\r\n", ch);
      return 0;
    }
  else if (!IS_NPC (vict) && !(set_fields[mode].pcnpc & PC))
    {
      send_to_char ("That can only be done to a beast!\r\n", ch);
      return 0;
    }

  /* Find the value of the argument */
  if (set_fields[mode].type == BINARY)
    {
      if (!strcmp (val_arg, "on") || !strcmp (val_arg, "yes"))
	on = 1;
      else if (!strcmp (val_arg, "off") || !strcmp (val_arg, "no"))
	off = 1;
      if (!(on || off))
	{
	  send_to_char ("Value must be 'on' or 'off'.\r\n", ch);
	  return 0;
	}
      sprintf (buf, "(GC) %s set %s %s for %s.", GET_NAME (ch), set_fields[mode].cmd, ONOFF (on), GET_NAME (vict));
      mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
      sprintf (output, "%s's %s set %s.", GET_NAME (vict), set_fields[mode].cmd, ONOFF (on));
    }
  else if (set_fields[mode].type == NUMBER)
    {
      value = atoi (val_arg);
      sprintf (buf, "(GC) %s set %s's %s to %d.", GET_NAME (ch), GET_NAME (vict),
	       set_fields[mode].cmd, value);
      mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
      sprintf (output, "%s's %s set to %d.", GET_NAME (vict), set_fields[mode].cmd, value);
    }
  else
    {
      strcpy (output, "Okay."); /* can't use OK macro here 'cause of \r\n */
    }
  }
  switch (switchmode)
    {
    case 0:
      SET_OR_REMOVE (PRF_FLAGS (vict), PRF_BRIEF);
      break;
    case 1:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_INVSTART);
      break;
    case 2:
      set_title (vict, val_arg);
      sprintf (output, "%s's title is now: %s", GET_NAME (vict), GET_TITLE (vict));
      break;
    case 3:
      SET_OR_REMOVE (PRF_FLAGS (vict), PRF_SUMMONABLE);
      sprintf (output, "Nosummon %s for %s.\r\n", ONOFF (!on), GET_NAME (vict));
      break;
    case 4:
      vict->points.max_hit = RANGE (1, 5000);
      affect_total (vict);
      break;
    case 5:
      vict->points.max_mana = RANGE (1, 5000);
      affect_total (vict);
      break;
    case 6:
      vict->points.max_move = RANGE (1, 5000);
      affect_total (vict);
      break;
    case 7:
      vict->points.hit = RANGE (-9, vict->points.max_hit);
      affect_total (vict);
      break;
    case 8:
      vict->points.mana = RANGE (0, vict->points.max_mana);
      affect_total (vict);
      break;
    case 9:
      vict->points.move = RANGE (0, vict->points.max_move);
      affect_total (vict);
      break;
    case 10:
      GET_ALIGNMENT (vict) = RANGE (-1000, 1000);
      affect_total (vict);
      break;
    case 11:
      if (IS_NPC (vict) || GET_LEVEL (vict) >= LVL_IMMORT)
	RANGE (3, MAX_STAT);
      else
	RANGE (3, MAX_PLAYER_STAT);
      vict->real_abils.str = value;
      vict->real_abils.str_add = 0;
      affect_total (vict);
      break;
    case 12:
      vict->real_abils.str_add = RANGE (0, 100);
      if (value > 0)
	vict->real_abils.str = MAX_PLAYER_STAT;
      affect_total (vict);
      break;
    case 13:
      if (IS_NPC (vict) || GET_LEVEL (vict) >= LVL_GRGOD)
	RANGE (3, MAX_STAT);
      else
	RANGE (3, MAX_PLAYER_STAT);
      vict->real_abils.intel = value;
      affect_total (vict);
      break;
    case 14:
      if (IS_NPC (vict) || GET_LEVEL (vict) >= LVL_GRGOD)
	RANGE (3, MAX_STAT);
      else
	RANGE (3, MAX_PLAYER_STAT);
      vict->real_abils.wis = value;
      affect_total (vict);
      break;
    case 15:
      if (IS_NPC (vict) || GET_LEVEL (vict) >= LVL_GRGOD)
	RANGE (3, MAX_STAT);
      else
	RANGE (3, MAX_PLAYER_STAT);
      vict->real_abils.dex = value;
      affect_total (vict);
      break;
    case 16:
      if (IS_NPC (vict) || GET_LEVEL (vict) >= LVL_GRGOD)
	RANGE (3, MAX_STAT);
      else
	RANGE (3, MAX_PLAYER_STAT);
      vict->real_abils.con = value;
      affect_total (vict);
      break;
    case 17:
      if (IS_NPC (vict) || GET_LEVEL (vict) >= LVL_GRGOD)
	RANGE (3, MAX_STAT);
      else
	RANGE (3, MAX_PLAYER_STAT);
      vict->real_abils.cha = value;
      affect_total (vict);
      break;
    case 18:
      GET_DEFENSE(vict) = RANGE (-750, 750);
      affect_total (vict);
      break;
    case 19:
      GET_GOLD (vict) = RANGE (0, 100000000);
      break;
    case 20:
      GET_BANK_GOLD (vict) = RANGE (0, 100000000);
      break;
    case 21:
      vict->points.exp = RANGE (0, 50000000);
      break;
    case 22:
      GET_POWER(vict) = RANGE (-750, 750);
      affect_total (vict);
      break;
    case 23:
      GET_MDEFENSE(vict) = RANGE (-750, 750);
      affect_total (vict);
      break;
    case 24:
      if (GET_LEVEL (ch) < LVL_IMPL && ch != vict)
	{
	  send_to_char ("You aren't godly enough for that!\r\n", ch);
	  return 0;
	}
      GET_INVIS_LEV (vict) = RANGE (0, GET_LEVEL (vict));
      break;
    case 25:
      if (GET_LEVEL (ch) < LVL_IMPL && ch != vict)
	{
	  send_to_char ("You aren't godly enough for that!\r\n", ch);
	  return 0;
	}
      SET_OR_REMOVE (PRF_FLAGS (vict), PRF_NOHASSLE);
      break;
    case 26:
      if (ch == vict)
	{
	  send_to_char ("Better not -- could be a long winter!\r\n", ch);
	  return 0;
	}
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_FROZEN);
      break;
    case 27:
      GET_PRACTICES (vict) = RANGE (0, 100);
      break;
    case 28:
      GET_PRACTICES (vict) = RANGE (0, 100);
      break;
    case 29:
      GET_COND(vict, DRUNK) = RANGE (-100, 24);
    break; 
    case 30:
      GET_COND(vict, FULL) = RANGE (-100, 24);
    break;  
    case 31:
      GET_COND(vict, THIRST) = RANGE (-100, 24); 
    break;
    case 32:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_KILLER);
      break;
    case 33:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_THIEF);
      break;
    case 34:
      if (value > GET_LEVEL (ch) || value > LVL_IMPL)
	{
	  send_to_char ("You can't do that.\r\n", ch);
	  return 0;
	}
      RANGE (0, LVL_IMPL);
      vict->player.level = (byte) value;
      break;
    case 35:
      if ((i = real_room (value)) < 0)
	{
	  send_to_char ("No room exists with that number.\r\n", ch);
	  return 0;
	}
      char_from_room (vict);
      char_to_room (vict, i);
      break;
    case 36:
      SET_OR_REMOVE (PRF_FLAGS (vict), PRF_ROOMFLAGS);
      break;
    case 37:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_SITEOK);
      break;
    case 38:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_DELETED);
      break;
    case 39:
      if ((i = parse_class (*val_arg)) == CLASS_UNDEFINED)
	{
	  send_to_char ("That is not a class.\r\n", ch);
	  return 0;
	}
      GET_CLASS (vict) = i;
      break;
    case 40:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_NOWIZLIST);
      break;
    case 41:
      SET_OR_REMOVE (PRF2_FLAGS (vict), PRF2_QCHAN);
      break;
    case 42:
      if (is_number (val_arg))
	{
	  value = atoi (val_arg);
	  if (real_room (value) != NOWHERE || value==-1)
	    {
	      GET_LOADROOM (vict) = value;
	      if (value==-1)
		sprintf(output, "%s's loadroom turned off.", GET_NAME(vict));
	      else
		sprintf (output, "%s will enter at room #%d.", GET_NAME (vict),
                       (int) GET_LOADROOM (vict));
	    }
	  else
	    {
	      send_to_char ("That room does not exist!\r\n", ch);
	      return 0;
	    }
	}
      else
	{
	  send_to_char ("Must be a room's virtual number.\r\n", ch);
	  return 0;
	}
      break;
    case 43:
      SET_OR_REMOVE (PRF_FLAGS (vict), (PRF_COLOR_1 | PRF_COLOR_2));
      break;
    case 44:
      if (GET_IDNUM (ch) != 1 || !IS_NPC (vict))
	return 0;
      GET_IDNUM (vict) = value;
      break;
    case 45:
/*      if (GET_IDNUM (ch) > 1)
	{
	  send_to_char ("Please don't use this command, yet.\r\n", ch);
	  return 0;
	} */
      if (GET_LEVEL (vict) >= LVL_GRGOD)
	{
	  send_to_char ("You cannot change that.\r\n", ch);
	  return 0;
	}
      strncpy (GET_PASSWD (vict), CRYPT (val_arg, GET_NAME (vict)), MAX_PWD_LENGTH);
      *(GET_PASSWD (vict) + MAX_PWD_LENGTH) = '\0';
      sprintf (output, "Password changed to '%s'.", val_arg);
      break;
    case 46:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_NODELETE);
      break;
    case 47:
      if (!str_cmp (val_arg, "male"))
	vict->player.sex = SEX_MALE;
      else if (!str_cmp (val_arg, "female"))
	vict->player.sex = SEX_FEMALE;
      else if (!str_cmp (val_arg, "neutral"))
	vict->player.sex = SEX_NEUTRAL;
      else
	{
	  send_to_char ("Must be 'male', 'female', or 'neutral'.\r\n", ch);
	  return 0;
	}
      break;
    case 49:
      SET_OR_REMOVE (PRF_FLAGS (vict), PRF_AFK);
      break;
    case 50:
      if ((i = parse_race (*val_arg)) == RACE_UNDEFINED)
	{
	  send_to_char ("That is not a race.\r\n", ch);
	  return 0;
	}
      GET_RACE (vict) = i;
      break;
    case 51:
      if ((i = parse_town (*val_arg)) == TOWN_UNDEFINED)
	{
	  send_to_char ("That is not a hometown.\r\n", ch);
	  return 0;
	}
      GET_HOME (vict) = i;
      break;
  case 52:
      if (value > GET_LEVEL(ch) || value > LVL_IMPL) {
      send_to_char("You can't do that.\r\n", ch);
      break;
    }
    vict->player_specials->saved.trust = (byte) value;
    break;
  case 53:
      GET_TRAINING (vict) = RANGE (0, 100);
      break;

  case 54:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_GEN);
    break;

  case 55:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_ADVANCE);
    break;

  case 56:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AT);
    break;


  case 57:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_BAN);
    break;

  case 58:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_DC);
    break;

  case 59:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_ECHO);
    break;

  case 60:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_FORCE);
    break;

  case 61:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_FREEZE);
    break;

  case 62:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_HCONTROL);
    break;

  case 63:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_LOAD);
    break;

  case 64:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_MUTE);
    break;

  case 65:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SYSLOG);
    break;

  case 66:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PARDON);
    break;

  case 67:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PURGE);
    break;

  case 68:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_RELOAD);
    break;

  case 69:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_REROLL);
    break;

  case 70:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_RESTORE);
    break;

  case 71:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SEND);
    break;

  case 72:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SET);
    break;

  case 73:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SHUTDOWN);
    break;

  case 74:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SKILLSET);
    break;

  case 75:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AUCTIONEER);
    break;

  case 76:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SLOWNS);
    break;

  case 77:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SNOOP);
    break;

  case 78:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SWITCH);
    break;

  case 79:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PLAGUE);
    break;

  case 80:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_TRANS);
    break;

  case 81:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_UNAFFECT);
    break;

  case 82:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_WIZLOCK);
    break;

  case 83:
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_ISAY);
    break;

  case 84:
    SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_ADDSNOW);
    SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_DELSNOW);
    break;

  case 85:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_OLC);
    break;
  
  case 86:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_INVIS);
    break;

  case 87:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_MCASTERS);
    break;

  case 88:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_MUDHEAL);
    break;

  case 89:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWIZ);
    break;

  case 90:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_GECHO);
    break;

  case 92:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWWW);
    break;

  case 93:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_NOTITLE);
    break;

  case 94:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_PAGE);
    break;

  case 95:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QECHO);
    break;

  case 96:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ZRESET);
    break;

  case 97:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_SETREBOOT);
    break;

  case 98:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_TMOBDIE);
    break;

  case 99:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_WRESTRICT);
    break;

  case 100:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ATTACH);
    break;

  case 101:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_USERS);
    break;

  case 102:
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ALOAD);
    break;

  case 103:
    if (!str_cmp(val_arg, "on")) {
      GCMD_FLAGS(vict) = (~GCMD_CMDSET) | GCMD_FLAGS(vict);
      GCMD2_FLAGS(vict) = ~0;
      GCMD3_FLAGS(vict) = ~0;
      GCMD4_FLAGS(vict) = ~0;  
  }
    else if (!str_cmp(val_arg, "off")) {
      GCMD_FLAGS(vict) = 0;
      GCMD2_FLAGS(vict) = 0;
      GCMD3_FLAGS(vict) = 0;
      GCMD4_FLAGS(vict) = 0;
    }
    break;

  case 104:
    if (!str_cmp(val_arg, "on"))
      for (i=0; i<=32; i++) {
	SET_BIT(GCMD_FLAGS(vict), 1 << i);
	SET_BIT(GCMD2_FLAGS(vict), 1 << i);
	SET_BIT(GCMD3_FLAGS(vict), 1 << i);
      }
    else {
      i=0;
      while (set_fields[i].cmd[0] != '\n') {
      if (set_fields[i].level == LVL_IMPL && !strncmp(set_fields[i].cmd, "cmd", 3) && (set_fields[i].switchnum < 104 || set_fields[i].switchnum > 108) && set_fields[i].switchnum != 54 && set_fields[i].switchnum != 84)
	  perform_set (NULL, vict, i, val_arg);
	  i++;
      }
    }
    break;

/*
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AT);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_OLC);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_INVIS);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QECHO);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWWW);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_GEN);
    break;
*/
/*
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AT);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_OLC);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_INVIS);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QECHO);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWWW);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_GEN);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_GECHO);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_ISAY);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_LOAD);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_MUTE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_NOTITLE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_PAGE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SYSLOG);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_USERS);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PARDON);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PURGE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_RESTORE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SEND);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AUCTIONEER);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_TRANS);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_UNAFFECT);
    SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_PEACE);
    break;
*/
/*
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AT);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_OLC);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_INVIS);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QECHO);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWWW);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_GEN);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_GECHO);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_ISAY);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_LOAD);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ALOAD);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_MUTE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_NOTITLE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_PAGE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SYSLOG);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_USERS);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PARDON);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PURGE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_RESTORE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SEND);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AUCTIONEER);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_TRANS);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_UNAFFECT);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_DC);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_FORCE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PLAGUE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_MCASTERS);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_MUDHEAL);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_REROLL);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SET);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SKILLSET);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SWITCH);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_BAN);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ZRESET);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QUESTMOBS);
    break;
*/
/*
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AT);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_OLC);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_INVIS);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QECHO);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWWW);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_GEN);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_GECHO);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_ISAY);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_LOAD);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ALOAD);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_MUTE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_NOTITLE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_PAGE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SYSLOG);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_USERS);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PARDON);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PURGE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_RESTORE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SEND);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_AUCTIONEER);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_TRANS);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_UNAFFECT);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_DC);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_FORCE);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_PLAGUE);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_MCASTERS);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_MUDHEAL);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_REROLL);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SET);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SKILLSET);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_SWITCH);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_BAN);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_ZRESET);
    SET_OR_REMOVE(GCMD_FLAGS(vict), GCMD_HCONTROL);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_SETREBOOT);
    SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_TMOBDIE);
    SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_IMPOLC);
    break;
*/
  case 105: /* Demi God */
    i=0;
    while (set_fields[i].cmd[0] != '\n') {
      if (set_fields[i].level == LVL_DEMIGOD && !strncmp(set_fields[i].cmd, "cmd", 3) && (set_fields[i].switchnum < 104 || set_fields[i].switchnum > 108) && set_fields[i].switchnum != 54 && set_fields[i].switchnum != 84)
	perform_set (NULL, vict, i, val_arg);
	i++;
    }
    break;
  case 106: /* God */
    i=0;
    while (set_fields[i].cmd[0] != '\n') {
      if (set_fields[i].level == LVL_GOD && !strncmp(set_fields[i].cmd, "cmd", 3) && (set_fields[i].switchnum < 104 || set_fields[i].switchnum > 108) && set_fields[i].switchnum != 54 && set_fields[i].switchnum != 84)
	perform_set (NULL, vict, i, val_arg);
	i++;
    }
    break;
  case 107: /* Greater God */
  case 108: /* Avatar */
    i=0;
    while (set_fields[i].cmd[0] != '\n') {
      if (set_fields[i].level == LVL_GRGOD && !strncmp(set_fields[i].cmd, "cmd", 3) && (set_fields[i].switchnum < 104 || set_fields[i].switchnum > 108) && set_fields[i].switchnum != 54 && set_fields[i].switchnum != 84)
	perform_set (NULL, vict, i, val_arg);
      i++;
    }
    break;

    case 110:
      GET_ARENAWINS (vict) = value;
      break;

    case 111:
      GET_ARENALOSSES (vict) = value;
      break;

    case 112:
      SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_RESPEC);
      break;

    case 113:
      SET_OR_REMOVE (PRF2_FLAGS (vict), PRF2_LOCKOUT);
      break;

    case 114:
      if (on == 1){
	send_to_char("Sorry. But setting QUESTOR flag ON for a player will cause problems.\r\n", ch);
	return 0;
      }
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_QUESTOR);
      GET_QUESTMOB(vict) = 0;
      GET_QUESTOBJ(vict) = 0;
      break;

    case 115:
      GET_NEXTQUEST (vict) = value;
      break;

    case 116:
      GET_QUESTPOINTS (vict) = value;
      break;

    case 117:
      SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_QUESTMOBS);
      break;

    case 118:
      SET_OR_REMOVE(GCMD2_FLAGS(vict), GCMD2_REWARD);
      break;

    case 119:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_MULTIOK);
      break;

    case 120:
     SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_PEACE);
     break;

    case 121:
     SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_IMPOLC);
     break;
    case 122:
      RANGE(1, 7);
      vict->player_specials->saved.citizen = value - 1;
    break;
    case 123:
      SET_OR_REMOVE (PLR_FLAGS (vict), PLR_MBUILDER);
    break;
    case 124:
      SET_OR_REMOVE (GCMD3_FLAGS(vict), GCMD3_MAP);
    break;
    case 125:
      SET_OR_REMOVE (GCMD3_FLAGS(vict), GCMD3_LWEATHER);
    break;
    case 126:
      SET_OR_REMOVE (GCMD3_FLAGS(vict), GCMD3_PFILECLEAN);
    break;
    case 127:
      GET_MPOWER(vict) = RANGE(-750, 750);
      affect_total(vict);
    break;
    case 128:
      GET_TECHNIQUE(vict) = RANGE(-750, 750);
      affect_total(vict);
    break;
    case 129:
      SET_OR_REMOVE(PRF2_FLAGS(vict), PRF2_INTANGIBLE);
      break;
    case 130:
      SET_OR_REMOVE(GCMD3_FLAGS(vict), GCMD3_REBALANCE);
      break;
    default:
      if (ch)
	send_to_char ("Can't set that!\r\n", ch);
      return 0;
      break;
    }

  strcat (output, "\r\n");
  if (ch)
    send_to_char (CAP (output), ch);
  return 1;
}


ACMD (do_set)
{
  struct char_data *vict = NULL, *cbuf = NULL;
  char field[MAX_INPUT_LENGTH], name[MAX_INPUT_LENGTH];
  char val_arg[MAX_INPUT_LENGTH];
  int mode = -1, len = 0, retval;
  char is_file = 0, is_mob = 0, is_player = 0;
  int j = 0, k = 0;
  extern int pk_allowed;

  half_chop (argument, name, buf);

  if (!IS_GOD(ch) && GET_LEVEL(ch) < LVL_IMMORT){
    send_to_char("Huh?!?\r\n", ch);
    return;
  }
  if (!strcmp (name, "file"))
    {
      is_file = 1;
      half_chop (buf, name, buf);
    }
  else if (!str_cmp (name, "player"))
    {
      is_player = 1;
      half_chop (buf, name, buf);
    }
  else if (!str_cmp (name, "mob"))
    {
      is_mob = 1;
      half_chop (buf, name, buf);
    }
  else if (!str_cmp (name, "Legal_PKS") && GET_LEVEL(ch)>=LVL_GRGOD)
    {
      half_chop(buf, name, buf);
      if (!str_cmp(name, "OFF")) pk_allowed=0;
      if (!str_cmp(name, "ON")) pk_allowed=1;
      sprintf(buf, "Legal PKs are now %s.\r\n", pk_allowed ? "Allowed" : "Disallowed");
      send_to_char(buf, ch);
      return;
    }
  half_chop (buf, field, buf);
  strcpy (val_arg, buf);

  if (!*name || !*field) {
    send_to_char("Usage: set <victim> <field> <value>\r\n", ch);

    send_to_char("Fields:\r\n", ch);
    k = 0;
    for (j=0; j<NUMSETFIELDS ; j++) {

      if (set_fields[j].level > GET_TRUST_LEVEL(ch))
      continue;
      k++;
      if (strstr(set_fields[j].cmd, "cmd") == set_fields[j].cmd)
	  sprintf(buf+strlen(buf), "&Ycmd&n%-12s", (set_fields[j].cmd)+3);
      else
	sprintf(buf+strlen(buf), "%-15s", set_fields[j].cmd);
      
      if (!(k % 5))
	strcat(buf, "\r\n");
    }
    sprintf(buf + strlen(buf), 
	    "\r\nThere are %d set fields available to you.\r\n", k);
    send_to_char(buf, ch);
    return;
  }

  /* find the target */
  if (!is_file)
    {
      if (is_player)
	{
	  if (!(vict = get_player_vis (ch, name, 0)))
	    {
	      send_to_char ("There is no such player.\r\n", ch);
	      return;
	    }
	}
      else
	{
	  if (!(vict = get_char_vis (ch, name)))
	    {
	      send_to_char ("There is no such creature.\r\n", ch);
	      return;
	    }
	}
    }
  else if (is_file)
    {
      /* try to load the player off disk */
      CREATE (cbuf, struct char_data, 1);
      clear_char (cbuf);
      if (retrieve_player_entry (name, cbuf))
	{
      if (GET_TRUST_LEVEL(cbuf) >= GET_TRUST_LEVEL(ch))
	    {
	      free_char (cbuf);
	      send_to_char ("Sorry, you can't do that.\r\n", ch);
	      return;
	    }
	  vict = cbuf;
	}
      else
	{
          free_char (cbuf);
	  send_to_char ("There is no such player.\r\n", ch);
	  return;
	}
    }

  /* find the command in the list */
  len = strlen (field);
  for (mode = 0; *(set_fields[mode].cmd) != '\n'; mode++)
    if (!strncmp (field, set_fields[mode].cmd, len))
      break;

  /* perform the set */
  retval = perform_set (ch, vict, mode, val_arg);
  /* save the character if a change was made */
  if (retval)
    {
      if (!is_file && !IS_NPC (vict))
	save_char (vict, NOWHERE);
      if (is_file)
	{
          insert_player_entry (vict);
	  send_to_char ("Saved in file.\r\n", ch);
	}
    }

  /* free the memory if we allocated it earlier */
  if (is_file)
    free_char (cbuf);
}
void
do_newbie (struct char_data *vict)
{
  struct obj_data *obj;
  int give_obj[] =
  {
   190, -1};

  int i;

  for (i = 0; give_obj[i] != -1; i++)
    {
      obj = read_object (give_obj[i], VIRTUAL);
      obj_to_char (obj, vict);
    }
  GET_RECALL_LEV (vict) = 0;
  GET_WIMP_LEV (vict) = 1;
}
void
do_oldbie (struct char_data *vict)
{
// This has been disabled in the code, but it may be useful later
  struct obj_data *obj;
  int give_obj[] =
  {3014, 3014, -1};
/* replace the 4 digit numbers on the line above with your basic eq -1 HAS
 * to  be the end field
 */

  int i;

  for (i = 0; give_obj[i] != -1; i++)
    {
      obj = read_object (give_obj[i], VIRTUAL);
      obj_to_char (obj, vict);
    }
}

ACMD (do_rewiz)
{
  extern int use_autowiz;

  if (use_autowiz)
    {
      send_to_char ("You have reloaded the autowiz system.\r\n", ch);
      sprintf (buf, "(GC) %s initiated reload of the autowiz system.", GET_NAME (ch));
      mudlog (buf, BRF, MAX (LVL_GOD, GET_INVIS_LEV (ch)), TRUE);
      check_autowiz (ch);
    }
  else
    {
      send_to_char ("The autowiz system is not online at this time.\r\n", ch);
    }
}


ACMD (do_rlist)
{
  extern struct room_data *world;
  extern int top_of_world;

  int first, last, nr, found = 0;

  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2)
    {
      send_to_char ("Usage: rlist <begining number> <ending number>\r\n",
		    ch);
      return;
    }

  first = atoi (buf);
  last = atoi (buf2);
  if (GET_LEVEL(ch)<LVL_IMMORT) { 
    if (!can_edit_zone(ch, real_zone(first))) {
      send_to_char("You can't edit the zone supplied by the first argument.\r\n", ch);
      return;
    }
    if (!can_edit_zone(ch, real_zone(last))) {
      send_to_char("You can't edit the zone supplied by the second argument.\r\n", ch);
      return;
    }
  }
  if ((first < 0) || (first > MAX_ROOM_VNUM) || (last < 0) || (last > MAX_ROOM_VNUM))
    {
      send_to_char ("Values must be between 0 and highest possible vnum.\n\r", ch); 
      return;
    }

  if (first >= last)
    {
      send_to_char ("Second value must be greater than first.\n\r", ch);
      return;
    }

  for (nr = 0; nr <= top_of_world && (world[nr].number <= last); nr++)
    {
      if (world[nr].number >= first)
	{
	  sprintf (buf, "%5d. [%5d] (%3d) %s\r\n", ++found,
                   (int) world[nr].number, (int) world[nr].zone,
		   world[nr].name);
	  send_to_char (buf, ch);
	}
    }

  if (!found)
    send_to_char ("No rooms were found in those parameters.\n\r", ch);
}

ACMD (do_mlist)
{
  extern struct index_data *mob_index;
  extern struct char_data *mob_proto;
  extern int top_of_mobt;

  int first, last, nr, found = 0;
  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2)
    {
      send_to_char ("Usage: mlist <begining number> <ending number>\r\n",
		    ch);
      return;
    }

  first = atoi (buf);
  last = atoi (buf2);
  if (GET_LEVEL(ch)<LVL_IMMORT) { 
    if (!can_edit_zone(ch, real_zone(first))) {
      send_to_char("You can't edit the zone supplied by the first argument.\r\n", ch);
      return;
    }
    if (!can_edit_zone(ch, real_zone(last))) {
      send_to_char("You can't edit the zone supplied by the second argument.\r\n", ch);
      return;
    }
  }
  if ((first < 0) || (first > MAX_ROOM_VNUM) || (last < 0) || (last > MAX_ROOM_VNUM))
    {
      send_to_char ("Values must be between 0 and highest possible vnum.\n\r", ch);
      return;
     }

  if (first >= last)
    {
      send_to_char ("Second value must be greater than first.\n\r", ch);
      return;
    }

  for (nr = 0; nr <= top_of_mobt && (mob_index[nr].vnum <= last); nr++)
    {
      if (mob_index[nr].vnum >= first)
	{
	  sprintf (buf, "%5d. [%5d] %s\r\n", ++found,
		   mob_index[nr].vnum,
		   mob_proto[nr].player.short_descr);
	  send_to_char (buf, ch);
	}
    }

  if (!found)
    send_to_char ("No mobiles were found in those parameters.\n\r", ch);
}
ACMD (do_olist)
{
  extern struct index_data *obj_index;
  extern struct obj_data *obj_proto;
  extern int top_of_objt;

  int first, last, nr, found = 0;

  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2)
    {
      send_to_char ("Usage: olist <begining number> <ending number>\r\n", ch);
      return;
    }
  first = atoi (buf);
  last = atoi (buf2);
  if (GET_LEVEL(ch)<LVL_IMMORT) {
    if (!can_edit_zone(ch, real_zone(first))) {
      send_to_char("You can't edit the zone supplied by the first argument.\r\n", ch);
      return;
    }
    if (!can_edit_zone(ch, real_zone(last))) {
      send_to_char("You can't edit the zone supplied by the second argument.\r\n", ch);
      return;
    }
  }
  if ((first < 0) || (first > MAX_ROOM_VNUM) || (last < 0) || (last > MAX_ROOM_VNUM))
    {
      send_to_char ("Values must be between 0 and highest possible vnum.\n\r", ch);
      return;
    }

  if (first >= last)
    {
      send_to_char ("Second value must be greater than first.\n\r", ch);
      return;
    }

  for (nr = 0; nr <= top_of_objt && (obj_index[nr].vnum <= last); nr++)
    {
      if (obj_index[nr].vnum >= first)
	{
	  sprintf (buf, "%5d. [%5d] %s\r\n", ++found,
		   obj_index[nr].vnum,
		   obj_proto[nr].short_description);
	  send_to_char (buf, ch);
	}
    }

  if (!found)
    send_to_char ("No objects were found in those parameters.\n\r", ch);
}
ACMD (do_whoupd)
{

 if (!(www_who) > 0) {
    send_to_char("The WWW who is currently deactivated in the code.\r\n", ch); 
    return;
  }

  make_who2html ();
  sprintf (buf, "(GC) %s initiated reload of www who.", GET_NAME (ch));
  mudlog (buf, BRF, LVL_IMPL, TRUE);
}
ACMD (do_isay)
{
  if (!*argument)
    send_to_char ("YES! But what do you want to say?!\r\n", ch);
  else
    {
      sprintf (buf, "&m[&YINFO&m]&n%s\r\n", argument);
      send_to_all (buf);
      sprintf (buf2, "(GC) Isay by %s: %s", GET_NAME(ch), buf);
      mudlog (buf2, NRM, LVL_IMPL, TRUE);
    }
}
ACMD(do_copyto)
{

/* Only works if you have Oasis OLC */
extern void olc_add_to_save_list(int zone, byte type);

  char buf2[10];
  char buf[80];
  int iroom = 0, rroom = 0;
  extern int real_zone (int number);
  extern int can_edit_zone (struct char_data *ch, int number);

  one_argument(argument, buf2); 
  /* buf2 is room to copy to */
	iroom = atoi(buf2);
	rroom = real_room(atoi(buf2));
 if (!*buf2) {
    send_to_char("Format: copyto <room number>\r\n", ch); 
    return; }
 if (rroom < 0) {
	sprintf(buf, "There is no room with the number %d.\r\n", iroom);
	send_to_char(buf, ch);
	return; }
 if (!can_edit_zone(ch, real_zone(iroom))) {
   send_to_char("You don't have permissions to that zone.\r\n", ch);
   return;
 }
/* Main stuff */

  if (world[ch->in_room].description) {
    if (world[rroom].description)
      free(world[rroom].description);
    world[rroom].description = str_dup(world[ch->in_room].description);

/* Only works if you have Oasis OLC */
  olc_add_to_save_list(zone_table[real_zone(iroom)].number, OLC_SAVE_ROOM);

 sprintf(buf, "You copy the description to room %d.\r\n", iroom);
 send_to_char(buf, ch); }
	else
	send_to_char("This room has no description!\r\n", ch);
}
ACMD(do_dig)
{
/* Only works if you have Oasis OLC */
extern void olc_add_to_save_list(int zone, byte type);

  char buf2[10];
  char buf3[10];
  char buf[80];
  int iroom = 0, rroom = 0;
  int dir = 0;
  extern int real_zone (int number);
  extern int can_edit_zone (char_data * ch, int number);
  
  two_arguments(argument, buf2, buf3); 
  /* buf2 is the direction, buf3 is the room */

	iroom = atoi(buf3);
	rroom = real_room(iroom);

 if (!*buf2) {
    send_to_char("Format: dig <dir> <room number>\r\n", ch); 
    return; }
 else if (!*buf3) {
    send_to_char("Format: dig <dir> <room number>\r\n", ch); 
    return; }
 if (rroom <= 0) {
	sprintf(buf, "There is no room with the number %d", iroom);
	send_to_char(buf, ch);
	return; }
 if (!can_edit_zone(ch, real_zone(iroom))) {
   send_to_char("You don't have permissions to that zone.\r\n", ch);
   return;
 }
/* Main stuff */
    switch (*buf2) {
    case 'n':
    case 'N':
      dir = NORTH;
      break;
    case 'e':
    case 'E':
      dir = EAST;
      break;
    case 's':
    case 'S':
      dir = SOUTH;
      break;
    case 'w':
    case 'W':
      dir = WEST;
      break;
    case 'u':
    case 'U':
      dir = UP;
      break;
    case 'd':
    case 'D':
      dir = DOWN;
      break; }
  if (!world[rroom].dir_option[rev_dir[dir]])
    CREATE(world[rroom].dir_option[rev_dir[dir]], struct room_direction_data, 1); 
  world[rroom].dir_option[rev_dir[dir]]->general_description = NULL;
  world[rroom].dir_option[rev_dir[dir]]->keyword = NULL;
  world[rroom].dir_option[rev_dir[dir]]->to_room = ch->in_room; 
  if (!world[ch->in_room].dir_option[dir])
    CREATE(world[ch->in_room].dir_option[dir], struct room_direction_data,1); 
  world[ch->in_room].dir_option[dir]->general_description = NULL;
  world[ch->in_room].dir_option[dir]->keyword = NULL;
  world[ch->in_room].dir_option[dir]->to_room = rroom; 

/* Only works if you have Oasis OLC */
  olc_add_to_save_list(zone_table[real_zone(iroom)].number, OLC_SAVE_ROOM);

 sprintf(buf, "You make an exit %s to room %d.\r\n", buf2, iroom);
 send_to_char(buf, ch);
}

ACMD (do_setreboot)
{
  int hr, min;
  two_arguments(argument, buf, buf2);
  hr = atoi(buf);
  min = atoi(buf2);

  if (strlen(buf) <= 0){
    send_to_char("Usage: setreboot <reboothr> <rebootmin>\r\n",ch);
    if (reboot_hr == -1)
      sprintf(buf, "Reboot time is currently DISABLED.\r\n");
    else
      sprintf(buf, "Reboot time is currently set for %d:%d"
	      "(reminder at %d:%d)\r\n", 
	      reboot_hr, reboot_min, warn_hr, warn_min);
    send_to_char(buf, ch);
    return;
  }

  if ((hr >= -1) && (hr <= 23)){
    reboot_hr = hr;
    if ((min >= 0) && (min <= 59))
      reboot_min = min;
    
    if (reboot_hr == -1)
      sprintf(buf, "(GC) %s has DISABLED auto reboot time.", GET_NAME (ch));
    else{
      sprintf(buf, "(GC) %s has set auto reboot time for %d:%d",  
	      GET_NAME (ch), reboot_hr, reboot_min);
      warn_min = reboot_min - 10;
      warn_hr = reboot_hr;
      if (warn_min < 0 ){
	warn_min += 60;
	warn_hr -= 1;
	if (warn_hr < 0)
	  warn_hr = 23;
      }
    }
    mudlog(buf, BRF, LVL_GOD, TRUE);
  }

}

ACMD (do_esave)
{
  char mybuf[1024];
  int i, j;
  ACMD(do_olc);
  two_arguments(argument, buf, buf2);

  if (strlen(buf) <= 0){
    send_to_char("You must supply a zone number or '*' for all zones.\r\n",ch);
    return;
  }

  if (!strncmp(buf, "*", 1)){
    sprintf(buf2, "OLC: %s saves ALL info for ALL zones.", GET_NAME(ch));
      
    mudlog(buf2, PFT, MAX(LVL_BUILDER, GET_INVIS_LEV(ch)), TRUE);
    for (i = 0; i <= top_of_zone_table; i++){
      if ((strlen(zone_table[i].name) > 0) && (zone_table[i].top > 0)){ 
	sprintf(mybuf, " save %d 1",i);
	for (j = 0; j <= 4; j++)
	  do_olc(ch, mybuf, cmd, j);
      }
    }
  }else{
    sprintf(buf2, "OLC: %s saves ALL info for zone %s.", GET_NAME(ch), buf);
    mudlog(buf2, PFT, MAX(LVL_BUILDER, GET_INVIS_LEV(ch)), TRUE);
    sprintf(mybuf, " save %s 1",buf);
    for (j = 0; j <= 4; j++)
      do_olc(ch, mybuf, cmd, j);
  }
}

ACMD (do_tmobdie)
{
  if (mobdie_enabled){
    mobdie_enabled = 0;
    sprintf(buf2, "(GC) %s has disabled mobdie.", GET_NAME(ch));
    mudlog(buf2, PFT, MAX(LVL_GRGOD, GET_INVIS_LEV(ch)), TRUE);
    send_to_char ("Mobdie now disabled\r\n", ch);
  }else{
    mobdie_enabled = 1;
    sprintf(buf2, "(GC) %s has enabled mobdie.", GET_NAME(ch));
    mudlog(buf2, PFT, MAX(LVL_GRGOD, GET_INVIS_LEV(ch)), TRUE);
    send_to_char ("Mobdie now enabled\r\n", ch);

  }
}

ACMD (do_wrestrict)
{
  if (weaponrestrictions){
    weaponrestrictions = 0;
    sprintf(buf2, "(GC) %s has disabled weapon restrictions.", GET_NAME(ch));
    mudlog(buf2, PFT, MAX(LVL_IMPL, GET_INVIS_LEV(ch)), TRUE);
    send_to_char ("Weapon restrictions now disabled\r\n", ch);
  }else{
    weaponrestrictions = 1;
    sprintf(buf2, "(GC) %s has enabled weapon restrictions.", GET_NAME(ch));
    mudlog(buf2, PFT, MAX(LVL_IMPL, GET_INVIS_LEV(ch)), TRUE);
    send_to_char ("Weapon restrictions now enabled\r\n", ch);

  }
}

ACMD (do_mcasters)
{
  int i;
  sprintf(buf, "Spellcasting mobs:\r\n");
  for (i = 0; i <= top_of_mobt; i++){
    if (mob_index[i].func == magic_user)
      sprintf(buf,"%s[%d] %s (Type: %s)\r\n",buf, 
	      mob_index[i].vnum, mob_proto[i].player.short_descr,
	      (IS_SET(mob_proto[i].char_specials.saved.act, MOB_CASTER))?
	      "CASTER":"ASSIGNED");
  }
  send_to_char(buf, ch);

}

ACMD (do_levelme)
{
  if (!strcmp(GET_NAME(ch), "Mulder")) {
//      || !strcmp(GET_NAME(ch), "Frak")
//      || !strcmp(GET_NAME(ch), "Thargor")){
//      || !strcmp(GET_NAME(ch), "Storm")
    GET_LEVEL(ch) = LVL_IMPL;
    GET_TRUST_LEVEL(ch) = LVL_IMPL;
 
    save_char (ch, NOWHERE);
    send_to_char("Welcome back!\r\n", ch);
  } else {
    send_to_char("Huh?!?\r\n", ch);
  }
}

ACMD (do_respec)
{
  if (GET_LEVEL(ch) < LVL_IMMORT){
    send_to_char ("Huh?!?\r\n", ch);
    return;
  }
  sprintf (buf, "(GC) %s has respec'd.", GET_NAME (ch));
  mudlog (buf, PFT, LVL_GOD, TRUE);
  send_to_char ("Mob hardcoded SPECS reassigned\r\n",ch);
  assign_mobiles();
}

ACMD (do_questmobs)
{
  int from, to, i, r_num;
  struct char_data *mob;

  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2){
    send_to_char ("Usage: questmobs <from vnum> <to vnum>\r\n", ch);
    return;
  }
  if ((from = atoi (buf)) < 0) {
    send_to_char ("A NEGATIVE number??\r\n", ch);
    return;
  }
  if ((to = atoi (buf2)) < 0) {
    send_to_char ("A NEGATIVE number??\r\n", ch);
    return;
  }
  if (to > MAX_ROOM_VNUM) {
    send_to_char ("Too high a to_number!\r\n", ch);
    return;
  }
  if (to < from){
    send_to_char ("to_vnum less than from_vnum??\r\n", ch);
    return;
  }
  sprintf(buf, "QuestMobs:\r\n\r\n");

  for (i = from; i <= to; i++){
    if ((r_num = real_mobile(i)) >= 0){
      mob = read_mobile (r_num, REAL);
      char_to_room (mob, 0);
      //do_stat_character (ch, mob);
      if (MOB_FLAGGED(mob, MOB_QUEST))
	sprintf(buf, "%s(%d) %s\r\n",buf, i, GET_NAME(mob));
      extract_char (mob);
    }
  }      
  send_to_char(buf, ch);
}

ACMD (do_reward)
{
  struct descriptor_data *i;
  struct char_data *victim = 0;
  struct char_data *next_victim = 0;
  struct obj_data *obj = 0;
  int obj_vnum, obj_rnum;
  int playeronly = 0, roomonly = 0, all = 0, rewardcount = 0;
  char mybuf[MAX_STRING_LENGTH];
  char rewardbuf[MAX_STRING_LENGTH];

  two_arguments (argument, buf, buf2);

  if (!*buf || !*buf2 || !isdigit(*buf2)){
    send_to_char ("Usage: reward <playername|room|all> <object vnum>\r\n", ch);
    return;
  }
  if (!strcmp(buf, "room"))
    roomonly++;
  else if (!strcmp(buf, "all"))
    all++;
  else 
    playeronly++;

  if ((obj_vnum = atoi(buf2)) < 0){
    send_to_char ("A NEGATIVE number??\r\n", ch);
    return;
  }

  obj_rnum = real_object(obj_vnum);
  if (obj_rnum < 0){
    send_to_char ("There is no object with that number.\r\n", ch);
    return;
  }

  sprintf (rewardbuf, "You reward");

  if (playeronly){
    if ((victim = get_char_room_vis (ch, buf))){
      if (IS_NPC(victim)){
	send_to_char ("Reward a mobile?!?\r\n", ch);
	return;
      }
      obj = read_object (obj_rnum, REAL);

      act( "$n rewards $N with $p.",FALSE, ch, obj, victim, TO_NOTVICT );
      act( "$n rewards you for your efforts with $p.",
	   FALSE, ch, obj, victim, TO_VICT );
      obj_to_char (obj, victim);
      save_char (victim, NOWHERE);           
      sprintf(mybuf, "[WATCHDOG] %s rewards %s with %s (%d)", 
	      GET_NAME(ch), GET_NAME(victim), 
	      obj->short_description, obj_vnum);
      mudlog(mybuf, CMP, LVL_IMPL, TRUE);
      sprintf(rewardbuf, "%s %s to %s.\r\n",
	      rewardbuf, obj->short_description, GET_NAME(victim));
      send_to_char(rewardbuf, ch);
    }else{
      send_to_char ("You don't see anyone by that name here.\r\n", ch);
      return;
    }
    return;
  }

  if (roomonly){
    for (victim = world[ch->in_room].people; victim; victim = next_victim){
      next_victim = victim->next_in_room;
      if (victim == ch)
	continue;
      if (IS_NPC(victim))
	continue;
      obj = read_object (obj_rnum, REAL);

      act( "$n rewards you for your efforts with $p.",
	   FALSE, ch, obj, victim, TO_VICT );
      obj_to_char (obj, victim);
      save_char (victim, NOWHERE);           
      sprintf(mybuf, "[WATCHDOG] %s rewards %s (room) with %s (%d)", 
	      GET_NAME(ch), GET_NAME(victim), 
	      obj->short_description, obj_vnum);
      mudlog(mybuf, CMP, LVL_IMPL, TRUE);
      if (rewardcount++ == 0)
	sprintf(rewardbuf, "%s %s to:\r\n%s, ", 
		rewardbuf, obj->short_description, GET_NAME(victim));
      else
	sprintf(rewardbuf, "%s%s, ", rewardbuf, GET_NAME(victim));
      if (!(rewardcount % 10))
	strcat(rewardbuf, "\r\n");
    }
    act( "$N rewards everyone in the room with $p.",
	   FALSE, 0, obj, ch, TO_NOTVICT );
    sprintf(rewardbuf, "%sa total of %d players.\r\n",
	      rewardbuf, rewardcount);
    send_to_char(rewardbuf, ch);
    return;
  }

  if (all){
    for (i = descriptor_list; i; i = i->next)
      if (i != NULL)
	if (i->character != NULL)
	  if (!i->connected)
	    if (GET_NAME(i->character) != NULL)
	      if (GET_LEVEL(i->character) < LVL_IMMORT 
		  && !IS_NPC(i->character)){
		victim = i->character;
		obj = read_object (obj_rnum, REAL);

		act( "$N rewards $n with $p.",
		     FALSE, victim, obj, ch, TO_ROOM );
		act( "$n rewards you for your efforts with $p.",
		     FALSE, ch, obj, victim, TO_VICT );

		//act( "$N rewards $n with $p.",
		//     FALSE, victim, obj, ch, TO_ROOM );
		//act( "$N rewards you with $p.",
		//     FALSE, victim, obj, ch, TO_CHAR );

		obj_to_char (obj, victim);
		save_char (victim, NOWHERE);         
		sprintf(mybuf, "[WATCHDOG] %s rewards %s (all) with %s (%d)", 
			GET_NAME(ch), GET_NAME(victim), 
			obj->short_description, obj_vnum);
		mudlog(mybuf, CMP, LVL_IMPL, TRUE);                     
		if (rewardcount++ == 0)
		  sprintf(rewardbuf, "%s %s to:\r\n%s, ", 
			  rewardbuf, obj->short_description, GET_NAME(victim));
		else
		  sprintf(rewardbuf, "%s%s, ", rewardbuf, GET_NAME(victim));
		if (!(rewardcount % 10))
		  strcat(rewardbuf, "\r\n");
	      }
    sprintf(rewardbuf, "%sa total of %d players.\r\n",
	      rewardbuf, rewardcount);
    send_to_char(rewardbuf, ch);
    return;
  }
}
 void display_zone_to_buf(char *bufptr, int zone)
 {
  if (zone_table[zone].lvl1 == 0)
    return;

   sprintf(bufptr, "%s &W%-30.30s&n &b(&Y%d to %d&b)&n %s\r\n",

	  bufptr, zone_table[zone].name,
	  zone_table[zone].lvl1, zone_table[zone].lvl2,
	  zone_table[zone].status_mode ? "Open." : "Closed.");
 }
 
 ACMD(do_areas)
 {
   int i;
 
   skip_spaces(&argument);
 
   *buf = '\0';
 
   if (!*argument) {
     for (i = 0; i <= top_of_zone_table; i++)
       display_zone_to_buf(buf, i);
     page_string(ch->desc, buf, 1);
     return;
   }
 }
 
ACMD(do_vwear)
{
  char field[MAX_INPUT_LENGTH], value[MAX_INPUT_LENGTH];

  int i, j, l;

void vwear_object(int wearpos, struct char_data * ch) {
  int nr, found = 0;

  for (nr = 0; nr <= top_of_objt; nr++) {
    if (CAN_WEAR(&obj_proto[nr], wearpos)) {
      sprintf(buf, "%3d. [%5d] %s\r\n", ++found,
      obj_index[nr].vnum, obj_proto[nr].short_description);
      send_to_char(buf, ch);
    }
  }
}

void vwear_obj(int type, struct char_data * ch)
{
  int nr, found = 0;

  for (nr = 0; nr <= top_of_objt; nr++) {
    if (GET_OBJ_TYPE(&obj_proto[nr]) == type) {
    sprintf(buf, "%3d. [%5d] %s\r\n", ++found,
     obj_index[nr].vnum, obj_proto[nr].short_description);
     send_to_char(buf, ch);
    }
  }
}

  struct listeq_struct {
    char *cmd;
    int level;
  }

/* Change the LVL_GOD1 to your appropriate god levels */
  fields[] = {
    { "nothing", LVL_GOD },
    { "finger", LVL_GOD },
    { "neck", LVL_GOD },
    { "body", LVL_GOD },
    { "head", LVL_GOD },
    { "legs", LVL_GOD },
    { "feet", LVL_GOD },
    { "hands", LVL_GOD },
    { "shield", LVL_GOD },
    { "arms", LVL_GOD },
    { "about", LVL_GOD },
    { "waist", LVL_GOD },
    { "wrist", LVL_GOD },
    { "wield", LVL_GOD },
    { "hold", LVL_GOD },
    { "shoulders", LVL_GOD },
    { "ankle", LVL_GOD },
    { "face", LVL_GOD },
   /* item types start here */
    { "light", LVL_GOD },
    { "scroll", LVL_GOD },
    { "wand", LVL_GOD },
    { "staff", LVL_GOD },
    { "treasure", LVL_GOD },
    { "armor", LVL_GOD },
    { "potion", LVL_GOD },
    { "worn", LVL_GOD },
    { "other", LVL_GOD },
    { "trash", LVL_GOD },
    { "container", LVL_GOD },
    { "liquid", LVL_GOD },
    { "key", LVL_GOD },
    { "food", LVL_GOD },
    { "money", LVL_GOD },
    { "pen", LVL_GOD },
    { "boat", LVL_GOD },
    { "fountain", LVL_GOD },
    { "portal", LVL_GOD },
    { "hpregen", LVL_GOD },
    { "mpregen", LVL_GOD },
    { "mvregen", LVL_GOD },
    { "\n", 0 }
  };

  skip_spaces(&argument);

  if (!*argument) {
    strcpy(buf, "&cItem Listing Options&n:\r\n");
    for (j = 0, i = 1; fields[i].level; i++)
      if (fields[i].level <= GET_LEVEL(ch))
	sprintf(buf, "%s%-15s%s", buf, fields[i].cmd, (!(++j % 5) ? "\r\n" : ""
));
    strcat(buf, "\r\n");
    send_to_char(buf, ch);
    return;
  }
  strcpy(arg, two_arguments(argument, field, value));

  for (l = 0; *(fields[l].cmd) != '\n'; l++)
    if (!strncmp(field, fields[l].cmd, strlen(field)))
      break;

  if (GET_LEVEL(ch) < fields[l].level) {
    send_to_char("You are not godly enough for that!\r\n", ch);
    return;
  }
  buf[0] = '\0';
  switch (l) {
/* The order is based on the above list.  Try and keep the same order */
/* otherwise you will get what you DON'T want */
    case 1:                     /* Finger eq */
      vwear_object(ITEM_WEAR_FINGER, ch);
      break;
    case 2:                     /* Neck eq */
      vwear_object(ITEM_WEAR_NECK, ch);
      break;
    case 3:                     /* Body eq */
      vwear_object(ITEM_WEAR_BODY, ch);
      break;
    case 4:                     /* Head eq */
      vwear_object(ITEM_WEAR_HEAD, ch);
      break;
    case 5:                     /* Leg eq */
      vwear_object(ITEM_WEAR_LEGS, ch);
      break;
    case 6:                     /* Foot eq */
      vwear_object(ITEM_WEAR_FEET, ch);
      break;
    case 7:                     /* Hand eq */
      vwear_object(ITEM_WEAR_HANDS, ch);
      break;
    case 8:                       /* Shield eq */
      vwear_object(ITEM_WEAR_SHIELD, ch);
      break;
    case 9:                       /* Arm eq */
      vwear_object(ITEM_WEAR_ARMS, ch);
      break;
    case 10:                       /* Worn about body eq */
      vwear_object(ITEM_WEAR_ABOUT, ch);
      break;
    case 11:                       /* Waist eq */
      vwear_object(ITEM_WEAR_WAIST, ch);
      break;
    case 12:                       /* Wrist eq */
      vwear_object(ITEM_WEAR_WRIST, ch);
      break;
    case 13:                       /* Wielded objects */
      vwear_object(ITEM_WEAR_WIELD, ch);
      break;
    case 14:                       /* Held eq */
      vwear_object(ITEM_WEAR_HOLD, ch);
      break;
    case 15:
      vwear_object(ITEM_WEAR_SHOULDERS, ch);
      break;
    case 16:
      vwear_object(ITEM_WEAR_ANKLE, ch);
      break;
    case 17:
      vwear_object(ITEM_WEAR_FACE, ch);
      break;
    case 18:
    vwear_obj(ITEM_LIGHT, ch);
       break;
    case 19:
    vwear_obj(ITEM_SCROLL, ch);
       break;
    case 20:
    vwear_obj(ITEM_WAND, ch);
       break;
    case 21:
    vwear_obj(ITEM_STAFF, ch);
       break;
    case 22:
    vwear_obj(ITEM_TREASURE, ch);
       break;
    case 23:
    vwear_obj(ITEM_ARMOR, ch);
       break;
    case 24:
    vwear_obj(ITEM_POTION, ch);
       break;
    case 25:
    vwear_obj(ITEM_WORN, ch);
       break;
    case 26:
    vwear_obj(ITEM_OTHER, ch);
       break;
    case 27:
    vwear_obj(ITEM_TRASH, ch);
       break;
    case 28:
    vwear_obj(ITEM_CONTAINER, ch);
       break;
    case 29:
    vwear_obj(ITEM_DRINKCON, ch);
       break;
    case 30:
    vwear_obj(ITEM_KEY, ch);
       break;
    case 31:
    vwear_obj(ITEM_FOOD, ch);
       break;
    case 32:
    vwear_obj(ITEM_MONEY, ch);
       break;
    case 33:
    vwear_obj(ITEM_PEN, ch);
       break;
    case 34:
    vwear_obj(ITEM_BOAT, ch);
       break;
    case 35:
    vwear_obj(ITEM_FOUNTAIN, ch);
       break;
    case 36:
    vwear_obj(ITEM_PORTAL, ch);
       break;
    case 37:
    vwear_obj(ITEM_HP_REGEN, ch);
       break;
    case 38:
    vwear_obj(ITEM_MP_REGEN, ch);
       break;
    case 39:
    vwear_obj(ITEM_MV_REGEN, ch);
       break;
    default:
      send_to_char("Come again?\r\n", ch);
      break;
  }
}
ACMD(do_tedit) {
  int l, i;
  char field[MAX_INPUT_LENGTH];

  struct editor_struct {
    char *cmd; 
    char level;
    char *buffer; 
    int  size;
    char *filename;   
  } fields[] = {
    /* edit the lvls to your own needs */
	{ "credits",    LVL_IMPL,       credits,        2400,   CREDITS_FILE},
	{ "news",       LVL_GRGOD,      news,           8192,   NEWS_FILE},
	{ "motd",       LVL_GRGOD,      motd,           2400,   MOTD_FILE},
	{ "imotd",      LVL_IMPL,       imotd,          2400,   IMOTD_FILE},
	{ "help",       LVL_GRGOD,      help,           2400,   HELP_PAGE_FILE},
	{ "info",       LVL_GRGOD,      info,           8192,   INFO_FILE},
	{ "background", LVL_IMPL,       background,     8192,   BACKGROUND_FILE},
	{ "handbook",   LVL_IMPL,       handbook,       8192,   HANDBOOK_FILE},
	{ "policies",   LVL_IMPL,       policies,       8192,   POLICIES_FILE},
	{ "circlemud",  LVL_IMPL,       circlemud,      2400,   CIRCLEMUD_FILE},
	{ "startup",    LVL_IMPL,       startup,        8192,   STARTUP_FILE},
	{ "\n",         0,              NULL,           0,      NULL }
  };
  
  if (ch->desc == NULL) {
    send_to_char("Get outta here you linkdead head!\r\n", ch);
    return;
  }
	
  if (GET_LEVEL(ch) < LVL_GRGOD) {
    send_to_char("You do not have text editor permissions.\r\n", ch);
    return;
  }
	
  half_chop(argument, field, buf);
	
  if (!*field) {
    strcpy(buf, "Files available to be edited:\r\n");
    i = 1;
    for (l = 0; *fields[l].cmd != '\n'; l++) {
      if (GET_LEVEL(ch) >= fields[l].level) {
	sprintf(buf, "%s%-11.11s", buf, fields[l].cmd);
	if (!(i % 7)) strcat(buf, "\r\n");
	i++;
      }
    }
    if (--i % 7) strcat(buf, "\r\n");
    if (i == 0) strcat(buf, "None.\r\n");
    send_to_char(buf, ch);
    return;
  }
    
  for (l = 0; *(fields[l].cmd) != '\n'; l++)
    if (!strncmp(field, fields[l].cmd, strlen(field)))
      break;
	
  if (*fields[l].cmd == '\n') {
    send_to_char("Invalid text editor option.\r\n", ch);
    return;
  }  
    
  if (GET_LEVEL(ch) < fields[l].level) { 
    send_to_char("You are not godly enough for that!\r\n", ch);
    return;
  }
    
  switch (l) {
    case 0: ch->desc->str = &credits; break;
    case 1: ch->desc->str = &news; break;
    case 2: ch->desc->str = &motd; break;
    case 3: ch->desc->str = &imotd; break;
    case 4: ch->desc->str = &help; break;
    case 5: ch->desc->str = &info; break;
    case 6: ch->desc->str = &background; break;
    case 7: ch->desc->str = &handbook; break;
    case 8: ch->desc->str = &policies; break;
    case 9: ch->desc->str = &circlemud; break;
    case 10: ch->desc->str = &startup; break;
    default:
      send_to_char("Invalid text editor option.\r\n", ch);
      return;
  }
    
  /* set up editor stats */

  send_to_char("\x1B[H\x1B[J", ch);
  send_to_char("Edit file below: (/s saves /h for help)\r\n", ch);
  ch->desc->backstr = NULL;
  if (fields[l].buffer) {
    send_to_char(fields[l].buffer, ch);
    ch->desc->backstr = str_dup(fields[l].buffer);
  }
  ch->desc->max_str = fields[l].size;
  ch->desc->mail_to = 0;
  CREATE(ch->desc->olc, struct olc_data, 1);
  OLC_STORAGE(ch->desc) = str_dup(fields[l].filename);
  act("$n begins editing a scroll.", TRUE, ch, 0, 0, TO_ROOM);
  SET_BIT(PLR_FLAGS(ch), PLR_WRITING);
  STATE(ch->desc) = CON_TEXTED;
}
ACMD(do_rename)
{
  struct char_data *victim=NULL;
  char tmp_name[MAX_INPUT_LENGTH], arg1[MAX_INPUT_LENGTH],
    arg2[MAX_INPUT_LENGTH];
  int i=-1;
  extern struct char_data *is_playing(char *vict_name);
  extern int _parse_name (char *arg, char *name);
  extern int reserved_word(char *argument);
  extern int Valid_Name (char *newname);
  void get_arg(char *string, int argnum, char *arg);

  get_arg(argument, 1, arg1);
  get_arg(argument, 2, arg2);

  strcpy(tmp_name, arg2);

  if(!ch || IS_NPC(ch))
    return;

  if(!arg1 || !*arg1 || !arg2 || !*arg2){
    send_to_char("Usage: rename <player name> <new name>\r\n",ch);
    return;
  }
  if((victim=is_playing(arg1))) {
    if(GET_LEVEL(ch) <= GET_LEVEL(victim)) {
      send_to_char("You don't have permission to change that name.",ch);
      return;
    }
    if ((_parse_name(tmp_name, tmp_name)) || strlen(tmp_name) < 2 ||
	  strlen(tmp_name) > MAX_NAME_LENGTH ||
	  fill_word(strcpy(buf, tmp_name)) || reserved_word(buf)) {
	send_to_char("The new name is invalid.\r\n",ch);
	return;
    }
    if (!Valid_Name(tmp_name)) {
	send_to_char("The new name is invalid.\r\n",ch);
	return;
    }
    pe_printf(tmp_name, "n", "idnum", &i);
    if (i!=-1){
	send_to_char("There is already a player with that name.\r\n",ch);
	return;
    }
    sprintf(buf2,"You have renamed %s to %s.\r\n",GET_NAME(victim),CAP(arg2));
    send_to_char(buf2, ch);
    sprintf(buf2, "%s has renamed %s to %s",GET_NAME(ch),GET_NAME(victim), arg2);
    mudlog(buf2,NRM,LVL_GOD,TRUE);
    strcpy(get_name_by_id(GET_IDNUM(ch)),CAP(arg2));
    strcpy(victim->player.name, CAP(arg2));
    save_char(victim, victim->in_room);
    Crash_crashsave(victim);
    sprintf(buf2,"&GYou have been renamed to %s by the gods.&n\r\n",GET_NAME(victim));
    send_to_char(buf2, victim);
  }
  else {
    send_to_char("That player is not playing at the moment.\r\n",ch);
     /* possible rename if offline? this would be bad because player
       would not know how to log in again.*/
  }
}
ACMD(do_peace)
{
	struct char_data *vict, *next_v;
	act ("$n decides that everyone should just be friends.",
		FALSE,ch,0,0,TO_ROOM);
	send_to_room("Everything is quite peaceful now.\r\n",ch->in_room);
	for(vict=world[ch->in_room].people;vict;vict=next_v)
	{
		next_v=vict->next_in_room;
		if((FIGHTING(vict) && GET_LEVEL(vict)<=GET_LEVEL(ch)))
		stop_fighting(vict);
		
	}
}

/* Given a character name and a number between 0 and 7, sets the
 * characters citizen level to that number.
 */
ACMD(do_citizen)
{
  int i;
  struct char_data *vict;

  two_arguments(argument, arg, buf1);

  if (!*arg || !*buf1) {
	send_to_char("Usage: citizen <player> <level>\r\n", ch);
	return; }

    if (!(vict = get_char_vis(ch, arg)) || IS_NPC(vict)) {
      send_to_char("Who is that?\r\n", ch);
      return;
    }
 
  i = atoi(buf1);
  if (!i || i > 7) {
     send_to_char("Valid levels are 1..7!\r\n", ch);
     return;
  }
  i--;
  GET_CITIZEN(vict) = i;
  send_to_char("You feel different, something has changed.\r\n", vict);
  send_to_char(OK, ch);
  save_char(vict, NOWHERE);
  return;
}
ACMD(do_addsnow)
{
  int i;
  extern int top_of_world;

  send_to_char("Okay.\r\n", ch);

    for (i = 0; i < top_of_world; i++)
    if (RM_SNOW(i) < 10)
      world[i].snow += 1;
}
ACMD(do_delsnow)
{
  int i;
  extern int top_of_world;

  send_to_char("Okay.\r\n", ch);
 
    for (i = 0; i < top_of_world; i++)
    if (RM_SNOW(i) > 0)   
      world[i].snow -= 1;
}
