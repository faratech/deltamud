/* ************************************************************************
   *   File: spells.c                                      Part of CircleMUD *
   *  Usage: Implementation of "manual spells".  Circle 2.2 spell compat.    *
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
#include "spells.h"
#include "handler.h"
#include "db.h"
#include "interpreter.h"
#include "house.h"

extern int arena_zone;
extern int arena_preproom;
extern int arena_observeroom;
extern int arena_combatant;
extern int arena_observer;
extern int arena_flee_timeout;
extern struct char_data *arenamaster;
extern struct room_data *world;
extern struct obj_data *object_list;
extern struct char_data *character_list;
/* extern struct cha_app_type cha_app[]; */
/* extern struct int_app_type int_app[]; */
extern struct index_data *obj_index;

extern struct descriptor_data *descriptor_list;
extern struct zone_data *zone_table;

extern struct house_control_rec house_control[MAX_HOUSES];
extern int num_of_houses;

extern int mini_mud;
extern int pk_allowed;

extern struct default_mobile_stats *mob_defaults;
extern char weapon_verbs[];
extern int *max_ac_applys;
extern struct apply_mod_defaults *apmd;

extern char *dirs[];
void clearMemory (struct char_data *ch);
void act (char *str, int i, struct char_data *c, struct obj_data *o,
	  void *vict_obj, int j);

void damage (struct char_data *ch, struct char_data *victim,
	     int damage, int weapontype);

void match_over(struct char_data *, struct char_data *, char *, int);
void weight_change_object (struct obj_data *obj, int weight);
void add_follower (struct char_data *ch, struct char_data *leader);
int mag_savingthrow (struct char_data *ch, struct char_data *victim);
char* numdisplay(int);
int find_house (room_vnum vnum);

/*
 * Special spells appear below.
 */

ASPELL (spell_create_water)
{
  int water;

  void name_to_drinkcon (struct obj_data *obj, int type);
  void name_from_drinkcon (struct obj_data *obj);

  if (ch == NULL || obj == NULL)
    return;
  level = MAX (MIN (level, LVL_IMPL), 1);

  if (GET_OBJ_TYPE (obj) == ITEM_DRINKCON)
    {
      if ((GET_OBJ_VAL (obj, 2) != LIQ_WATER) && (GET_OBJ_VAL (obj, 1) != 0))
	{
	  name_from_drinkcon (obj);
	  GET_OBJ_VAL (obj, 2) = LIQ_SLIME;
	  name_to_drinkcon (obj, LIQ_SLIME);
	}
      else
	{
	  water = MAX (GET_OBJ_VAL (obj, 0) - GET_OBJ_VAL (obj, 1), 0);
	  if (water > 0)
	    {
	      if (GET_OBJ_VAL (obj, 1) >= 0)
		name_from_drinkcon (obj);
	      GET_OBJ_VAL (obj, 2) = LIQ_WATER;
	      GET_OBJ_VAL (obj, 1) += water;
	      name_to_drinkcon (obj, LIQ_WATER);
	      weight_change_object (obj, water);
	      act ("$p is filled.", FALSE, ch, obj, 0, TO_CHAR);
	    }
	}
    }
}


ASPELL (spell_recall)
{
  extern long mortal_start_room[NUM_STARTROOMS + 1]; 
  /*  extern long r_mortal_start_room[NUM_STARTROOMS + 1]; */
  char mybuf[1024];
  struct char_data *victor;
  int home;

  home = GET_HOME(ch);
  if (!(home >= 1 && home <= NUM_STARTROOMS)){
    sprintf(mybuf, "DEBUG: Recall Trigger - %s GET_HOME is %d (invalid)",
	    GET_NAME(ch), home);
    mudlog(mybuf, NRM, LVL_GRGOD, TRUE);
    home = 0;
  }

  if (victim == NULL || IS_NPC (victim))
    return;

  if (RIDING(victim) || RIDDEN_BY(victim)) {
  send_to_char("The spell fails because your victim is atop a mount.\r\n", ch);
  return;  
  }

  act ("$n disappears.", TRUE, victim, 0, 0, TO_ROOM);

  if (IS_ARENACOMBATANT(victim)){
    victor = FIGHTING(victim);
    sprintf(mybuf, "(Recalled)");
    if (GET_ARENAFLEETIMER(victim) >= 1 
	&& GET_ARENAFLEETIMER(victim) <= 1+arena_flee_timeout){
      victor = LASTFIGHTING(victim);
      send_to_char("You recalled before the flee-recall timer expired.\r\n"
		   "You have conceded the match!\r\n", victim);
      sprintf(mybuf, "(Fled & Recalled)");
      GET_ARENAFLEETIMER(victim) = 0;
    }
    match_over(victor, victim, mybuf, FALSE);
    char_from_room (victim);
    char_to_room (victim, real_room (arena_preproom));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
    return;
  }

  char_from_room (victim);
  if (GET_ARENASTAT(victim) == ARENA_OBSERVER){
    char_to_room (victim, real_room (arena_observeroom));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
    return;
  }

  if (home == 0) {
    char_to_room (victim, real_room (mortal_start_room[1]));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
  } else {
    char_to_room (victim, real_room (mortal_start_room[home]));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
  }
}


ASPELL (spell_teleport)
{
  int to_room;
  extern int top_of_world;

  if (victim != NULL)
    return;

  do
    {
      to_room = number (0, top_of_world);
    }
  while (ROOM_FLAGGED (to_room, ROOM_PRIVATE | ROOM_DEATH));

  act ("$n slowly fades out of existence and is gone.",
       FALSE, victim, 0, 0, TO_ROOM);
  char_from_room (victim);
  char_to_room (victim, to_room);
  act ("$n slowly fades into existence.", FALSE, victim, 0, 0, TO_ROOM);
  look_at_room (victim, 0);
}

#define SUMMON_FAIL "You failed.\r\n"

ASPELL (spell_summon)
{
  extern int real_zone(int number);
  extern struct index_data *mob_index;
  if (ch == NULL || victim == NULL)
    return;

  if (GET_LEVEL (victim) > MIN (LVL_IMMORT - 1, level + 3))
    {
      send_to_char (SUMMON_FAIL, ch);
      return;
    }
  if (IS_NPC(victim))
    if (real_zone(GET_MOB_VNUM(victim))!=-1)
      if (zone_table[real_zone(GET_MOB_VNUM(victim))].status_mode==0) {
        send_to_char(SUMMON_FAIL, ch);
        return;
      }
 if (GET_ARENASTAT(victim) != ARENA_NOT && GET_ARENASTAT(ch) == ARENA_NOT){
   send_to_char ("Your target is in the arena right now.\r\n"
		 "Eldrich magic obstructs thee!\r\n", ch);
   return;
 }
 if (GET_ARENASTAT(victim) == ARENA_NOT && GET_ARENASTAT(ch) != ARENA_NOT){
   send_to_char ("You're in the arena right now whereas your target is not.\r\n"
		 "Eldrich magic obstructs thee!\r\n", ch);
   return;
 }

/*  if (!pk_allowed)
 *   { */
      if (MOB_FLAGGED (victim, MOB_AGGRESSIVE))
	{
	  act ("As the words escape your lips and $N travels\r\n"
	    "through time and space towards you, you realize that $E is\r\n"
	       "aggressive and might harm you, so you wisely send $M back.",
	       FALSE, ch, 0, victim, TO_CHAR);
	  return;
	}
      if (!IS_NPC (victim) && !PRF_FLAGGED (victim, PRF_SUMMONABLE))
	/* && !PLR_FLAGGED (victim, PLR_KILLER)) */
	{
	  sprintf (buf, "%s just tried to summon you to: %s.\r\n"
		   "%s failed because you have summon protection on.\r\n"
		   "Type NOSUMMON to allow other players to summon you.\r\n",
		   GET_NAME (ch), world[ch->in_room].name,
		   (ch->player.sex == SEX_MALE) ? "He" : "She");
	  send_to_char (buf, victim);

	  sprintf (buf, "You failed because %s has summon protection on.\r\n",
		   GET_NAME (victim));
	  send_to_char (buf, ch);

	  sprintf (buf, "%s failed summoning %s to %s.",
		 GET_NAME (ch), GET_NAME (victim), world[ch->in_room].name);
	  mudlog (buf, BRF, LVL_IMMORT, TRUE);
	  return;
/*	} */
    }

  if (MOB_FLAGGED (victim, MOB_NOSUMMON) ||
      (IS_NPC (victim) && mag_savingthrow (ch, victim)))
    {
      send_to_char (SUMMON_FAIL, ch);
      return;
    }

  act ("$n disappears suddenly.", TRUE, victim, 0, 0, TO_ROOM);

  char_from_room (victim);
  char_to_room (victim, ch->in_room);

  act ("$n arrives suddenly.", TRUE, victim, 0, 0, TO_ROOM);
  act ("$n has summoned you!", FALSE, ch, 0, victim, TO_VICT);
  look_at_room (victim, 0);
}



ASPELL (spell_locate_object)
{
  struct obj_data *i;
  char name[MAX_INPUT_LENGTH];
  int j;

  strcpy (name, fname (obj->name));
  j = level >> 1;

  for (i = object_list; i && (j > 0); i = i->next)
    {
      if (!is_name (name, i->name))
	continue;

      if (i->carried_by)
	sprintf (buf, "%s is being carried by %s.\n\r",
		 i->short_description, PERS (i->carried_by, ch));
      else if (i->in_room != NOWHERE)
	sprintf (buf, "%s is in %s.\n\r", i->short_description,
		 world[i->in_room].name);
      else if (i->in_obj)
	sprintf (buf, "%s is in %s.\n\r", i->short_description,
		 i->in_obj->short_description);
      else if (i->worn_by)
	sprintf (buf, "%s is being worn by %s.\n\r",
		 i->short_description, PERS (i->worn_by, ch));
      else
	sprintf (buf, "%s's location is uncertain.\n\r",
		 i->short_description);

      CAP (buf);
      send_to_char (buf, ch);
      j--;
    }

  if (j == level >> 1)
    send_to_char ("You sense nothing.\n\r", ch);
}



ASPELL (spell_charm)
{
  struct affected_type af;

  if (victim == NULL || ch == NULL)
    return;

  if (victim == ch)
    send_to_char ("You like yourself even better!\r\n", ch);
  else if (!IS_NPC (victim) && !PRF_FLAGGED (victim, PRF_SUMMONABLE))
    send_to_char ("You fail because SUMMON protection is on!\r\n", ch);
  else if (IS_AFFECTED (victim, AFF_SANCTUARY))
    send_to_char ("Your victim is protected by sanctuary!\r\n", ch);
  else if (MOB_FLAGGED (victim, MOB_NOCHARM))
    send_to_char ("Your victim resists!\r\n", ch);
  else if (IS_AFFECTED (ch, AFF_CHARM))
    send_to_char ("You can't have any followers of your own!\r\n", ch);
  else if (IS_AFFECTED (victim, AFF_CHARM) || level < GET_LEVEL (victim))
    send_to_char ("You fail.\r\n", ch);
  /* player charming another player - no legal reason for this */
  else if (!pk_allowed && !IS_NPC (victim))
    send_to_char ("You fail - shouldn't be doing it anyway.\r\n", ch);
  else if (circle_follow (victim, ch))
    send_to_char ("Sorry, following in circles can not be allowed.\r\n", ch);
  else if (mag_savingthrow (ch, victim))
    send_to_char ("Your victim resists!\r\n", ch);
  else
    {
      if (victim->master)
	stop_follower (victim);

      add_follower (victim, ch);

      af.type = SPELL_CHARM;

      if (GET_INT (victim))
	af.duration = 24 * MAX_PLAYER_STAT / GET_INT (victim);
      else
	af.duration = 24 * MAX_PLAYER_STAT;

      af.modifier = 0;
      af.location = 0;
      af.bitvector = AFF_CHARM;
      affect_to_char (victim, &af);

      act ("Isn't $n just such a nice fellow?", FALSE, ch, 0, victim, TO_VICT);
      if (IS_NPC (victim))
	{
	  REMOVE_BIT (MOB_FLAGS (victim), MOB_AGGRESSIVE);
	  REMOVE_BIT (MOB_FLAGS (victim), MOB_SPEC);
	}
    }
}



ASPELL (spell_identify)
{
  int i;
  int found;
  int condition;

  struct time_info_data age (struct char_data *ch);

  extern char *spells[];

  extern char *item_types[];
  extern char *extra_bits[];
  extern char *apply_types[];
  extern char *affected_bits[];

  if (obj)
    {
      send_to_char ("You feel informed:\r\n", ch);
      sprintf (buf, "Object '%s', Item type: ", obj->short_description);
      sprinttype (GET_OBJ_TYPE (obj), (const char **) item_types, buf2);
      strcat (buf, buf2);
      strcat (buf, "\r\n");
      send_to_char (buf, ch);

      if (obj->obj_flags.bitvector)
	{
	  send_to_char ("Item will give you following abilities:  ", ch);
	  sprintbit (obj->obj_flags.bitvector, (const char **) affected_bits, buf);
	  strcat (buf, "\r\n");
	  send_to_char (buf, ch);
	}
      send_to_char ("Item is: ", ch);
      sprintbit (GET_OBJ_EXTRA (obj), (const char **) extra_bits, buf);
      strcat (buf, "\r\n");
      send_to_char (buf, ch);

      sprintf (buf, "Weight: %s, ",
	       numdisplay(GET_OBJ_WEIGHT (obj)));
      sprintf (buf, "%sValue: %s, ", buf,
	       numdisplay(GET_OBJ_COST (obj)));
      sprintf (buf, "%sRent: %s\r\n", buf,
	       numdisplay(GET_OBJ_RENT (obj)));

    send_to_char(buf, ch);

   if (GET_OBJ_TSLOTS(obj))
  condition = (GET_OBJ_CSLOTS(obj) * 100)/GET_OBJ_TSLOTS(obj);
    else
  condition = 0;

  if ((GET_OBJ_CSLOTS(obj) == 0) && (GET_OBJ_TSLOTS(obj) == 0)) {
        sprintf(buf, "Quality: INDESTRUCTABLE\r\n");
    send_to_char(buf, ch);
        }
       else
    if ((GET_LEVEL(ch) >= LVL_HERO)) {
        sprintf(buf, "Quality: %d/%d\r\nCondition: %d percent\r\n",
     GET_OBJ_CSLOTS(obj), GET_OBJ_TSLOTS(obj), condition);
    send_to_char(buf, ch);
        }
      else
    if ((condition <= 10)) {
        sprintf(buf, "Quality: Extremley Poor\r\n");
    send_to_char(buf, ch);
        }
      else
    if ((condition <= 20)) {
        sprintf(buf, "Quality: Poor\r\n");
    send_to_char(buf, ch);
        }
       else
    if ((condition <= 30)) {
        sprintf(buf, "Quality: Fair\r\n");
    send_to_char(buf, ch);
        }
     else
    if ((condition <= 40)) {
        sprintf(buf, "Quality: Moderate\r\n");
    send_to_char(buf, ch);
        }
     else
    if ((condition <= 50)) {
        sprintf(buf, "Quality: Good\r\n");
    send_to_char(buf, ch);
        }
     else
    if ((condition <= 60)) {
        sprintf(buf, "Quality: Very Good\r\n");
    send_to_char(buf, ch);
        }
     else
    if ((condition <= 70)) {
        sprintf(buf, "Quality: Excellent\r\n");
    send_to_char(buf, ch);
    }
    else
    if ((condition <= 80)) {
        sprintf(buf, "Quality: Superior\r\n");
    send_to_char(buf, ch);
    }
    else
  if ((condition <= 90)) {
        sprintf(buf, "Quality: Extremely Superior\r\n");
    send_to_char(buf, ch);
    }
   else
  if ((condition <= 100)) {
        sprintf(buf, "Quality: Brand New\r\n");
    send_to_char(buf, ch);
    }

      switch (GET_OBJ_TYPE (obj))
	{
	case ITEM_SCROLL:
	case ITEM_POTION:
	  sprintf (buf, "This %s casts: ", item_types[(int) GET_OBJ_TYPE (obj)]);

	  if (GET_OBJ_VAL (obj, 1) >= 1)
	    sprintf (buf, "%s %s", buf, spells[GET_OBJ_VAL (obj, 1)]);
	  if (GET_OBJ_VAL (obj, 2) >= 1)
	    sprintf (buf, "%s %s", buf, spells[GET_OBJ_VAL (obj, 2)]);
	  if (GET_OBJ_VAL (obj, 3) >= 1)
	    sprintf (buf, "%s %s", buf, spells[GET_OBJ_VAL (obj, 3)]);
	  sprintf (buf, "%s\r\n", buf);
	  send_to_char (buf, ch);
	  break;
	case ITEM_WAND:
	case ITEM_STAFF:
	  sprintf (buf, "This %s casts: ", item_types[(int) GET_OBJ_TYPE (obj)]);
	  sprintf (buf, "%s %s\r\n", buf, spells[GET_OBJ_VAL (obj, 3)]);
	  sprintf (buf, "%sIt has %d maximum charge%s and %d remaining.\r\n", buf,
		 GET_OBJ_VAL (obj, 1), GET_OBJ_VAL (obj, 1) == 1 ? "" : "s",
		   GET_OBJ_VAL (obj, 2));
	  send_to_char (buf, ch);
	  break;
	case ITEM_WEAPON:
	  sprintf (buf, "Damage Dice is '%dD%d'", GET_OBJ_VAL (obj, 1),
		   GET_OBJ_VAL (obj, 2));
	  sprintf (buf, "%s for an average per-round damage of %.1f.\r\n", buf,
	       (((GET_OBJ_VAL (obj, 2) + 1) / 2.0) * GET_OBJ_VAL (obj, 1)));
	  send_to_char (buf, ch);
	  break;
	case ITEM_ARMOR:
	  sprintf (buf, "Defense apply is %d\r\n", GET_OBJ_VAL (obj, 0));
	  send_to_char (buf, ch);
	  break;
	}
      found = FALSE;
      for (i = 0; i < MAX_OBJ_AFFECT; i++)
	{
	  if ((obj->affected[i].location != APPLY_NONE) &&
	      (obj->affected[i].modifier != 0))
	    {
	      if (!found)
		{
		  send_to_char ("Can affect you as :\r\n", ch);
		  found = TRUE;
		}
	      sprinttype (obj->affected[i].location, (const char **) apply_types, buf2);
	      sprintf (buf, "   Affects: %s By %d\r\n", buf2, obj->affected[i].modifier);
	      send_to_char (buf, ch);
	    }
	}
    }
  else if (victim)
    {				/* victim */
      sprintf (buf, "Name: %s\r\n", GET_NAME (victim));
      send_to_char (buf, ch);
      if (!IS_NPC (victim))
	{
	  sprintf (buf, "%s is %d years, %d months, %d days and %d hours old.\r\n",
                   GET_NAME (victim), (int) age (victim).year, (int) age (victim).month,
                   (int) age (victim).day, age (victim).hours);
	  send_to_char (buf, ch);
	}
      sprintf (buf, "Height %d cm, Weight %d pounds\r\n",
	       GET_HEIGHT (victim), GET_WEIGHT (victim));
      sprintf (buf, "%sLevel: %d, Hits: %d, Mana: %d\r\n", buf,
	       GET_LEVEL (victim), (int) GET_HIT (victim), (int) GET_MANA (victim));
      sprintf (buf, "%sPower: %d, Magic Power: %d, Defense: %d, Magic Defense: %d, Technique: %d\r\n", buf,
	       GET_POWER (victim), GET_MPOWER (victim), GET_DEFENSE (victim), GET_MDEFENSE(victim), GET_TECHNIQUE(victim));
      sprintf (buf, "%sStr: %d/%d, Int: %d, Wis: %d, Dex: %d, Con: %d, Cha: %d\r\n",
	       buf, GET_STR (victim), GET_ADD (victim), GET_INT (victim),
	       GET_WIS (victim), GET_DEX (victim), GET_CON (victim), GET_CHA (victim));
      send_to_char (buf, ch);

    }
}



ASPELL (spell_enchant_weapon)
{
  int i;

  if (ch == NULL || obj == NULL)
    return;

  if ((GET_OBJ_TYPE (obj) == ITEM_WEAPON) &&
      !IS_SET (GET_OBJ_EXTRA (obj), ITEM_MAGIC))
    {

      for (i = 0; i < MAX_OBJ_AFFECT; i++)
	if (obj->affected[i].location != APPLY_NONE)
	  return;

      SET_BIT (GET_OBJ_EXTRA (obj), ITEM_MAGIC);

      obj->affected[0].location = APPLY_POWER;
      obj->affected[0].modifier = 1 + (level >= MAX_PLAYER_STAT);

      obj->affected[1].location = APPLY_TECHNIQUE;
      obj->affected[1].modifier = 1 + (level >= MAX_PLAYER_STAT);

      if (IS_GOOD (ch))
	{
	  SET_BIT (GET_OBJ_EXTRA (obj), ITEM_ANTI_EVIL);
	  act ("$p glows blue.", FALSE, ch, obj, 0, TO_CHAR);
	}
      else if (IS_EVIL (ch))
	{
	  SET_BIT (GET_OBJ_EXTRA (obj), ITEM_ANTI_GOOD);
	  act ("$p glows red.", FALSE, ch, obj, 0, TO_CHAR);
	}
      else
	{
	  act ("$p glows yellow.", FALSE, ch, obj, 0, TO_CHAR);
	}
    }
}


ASPELL (spell_detect_poison)
{
  if (victim)
    {
      if (victim == ch)
	{
	  if (IS_AFFECTED (victim, AFF_POISON))
	    send_to_char ("You can sense poison in your blood.\r\n", ch);
	  else
	    send_to_char ("You feel healthy.\r\n", ch);
	}
      else
	{
	  if (IS_AFFECTED (victim, AFF_POISON))
	    act ("You sense that $E is poisoned.", FALSE, ch, 0, victim, TO_CHAR);
	  else
	    act ("You sense that $E is healthy.", FALSE, ch, 0, victim, TO_CHAR);
	}
    }

  if (obj)
    {
      switch (GET_OBJ_TYPE (obj))
	{
	case ITEM_DRINKCON:
	case ITEM_FOUNTAIN:
	case ITEM_FOOD:
	  if (GET_OBJ_VAL (obj, 3))
	    act ("You sense that $p has been contaminated.", FALSE, ch, obj, 0, TO_CHAR);
	  else
	    act ("You sense that $p is safe for consumption.", FALSE, ch, obj, 0,
		 TO_CHAR);
	  break;
	default:
	  send_to_char ("You sense that it should not be consumed.\r\n", ch);
	}
    }
}
ASPELL (spell_fear)
{
  struct char_data *target = (struct char_data *) victim;
  struct char_data *next_target;
  int rooms_to_flee = 0;

  ACMD (do_flee);

  if (ch == NULL)
    return;

  send_to_char ("You radiate an aura of fear into the room!\r\n", ch);
  act ("$n is surrounded by an aura of fear!", TRUE, ch, 0, 0, TO_ROOM);

  for (target = world[ch->in_room].people; target; target = next_target)
    {
      next_target = target->next_in_room;

      if (target == NULL)
	return;

      if (target == ch)
	continue;

      if (GET_LEVEL (target) >= LVL_IMMORT)
	continue;

      if (mag_savingthrow (ch, target))
	{
	  sprintf (buf, "%s is unaffected by the fear!\r\n", GET_NAME (target));
	  act (buf, TRUE, ch, 0, 0, TO_ROOM);
	  send_to_char ("Your victim is not afraid of the likes of you!\r\n", ch);
	  if (IS_NPC (target))
	    hit (target, ch, TYPE_UNDEFINED);
	}
      else
	{
	  for (rooms_to_flee = level / 10; rooms_to_flee > 0; rooms_to_flee--)
	    {
	      send_to_char ("You flee in terror!\r\n", target);
	      do_flee (target, "", 0, 0);
	    }
	}
    }
}
ASPELL (spell_recharge)
{
  int restored_charges = 0, explode = 0;
  if (ch == NULL || obj == NULL)
    return;
  /* This is on my mud, comment off on yours
   * if (GET_OBJ_EXTRA(obj) == ITEM_NO_RECHARGE) {
   * send_to_char("This item cannot be recharged.\r\n", ch);   return; } */
  if (GET_OBJ_TYPE (obj) == ITEM_WAND)
    {
      if (GET_OBJ_VAL (obj, 2) < GET_OBJ_VAL (obj, 1))
	{
	  send_to_char ("You attempt to recharge the wand.\r\n", ch);
	  restored_charges = number (1, 5);
	  GET_OBJ_VAL (obj, 2) += restored_charges;
	  if (GET_OBJ_VAL (obj, 2) > GET_OBJ_VAL (obj, 1))
	    {
	      send_to_char ("The wand is overcharged and explodes!\r\n", ch);
	      sprintf (buf, "%s overcharges %s and it explodes!\r\n",
		       GET_NAME (ch), obj->name);
	      act (buf, TRUE, 0, 0, 0,
		   TO_NOTVICT);
	      explode = dice (GET_OBJ_VAL (obj, 2), 2);
	      GET_HIT (ch) -= explode;
	      update_pos (ch);
	      extract_obj (obj);
	      return;
	    }
	  else
	    {
	      sprintf (buf, "You restore %d charges to the wand.\r\n",
		       restored_charges);
	      send_to_char (buf, ch);
	      return;
	    }
	}
      else
	{
	  send_to_char ("That item is already at full charges!\r\n", ch);
	  return;
	}
    }
  else if (GET_OBJ_TYPE (obj) == ITEM_STAFF)
    {
      if (GET_OBJ_VAL (obj, 2) < GET_OBJ_VAL (obj, 1))
	{
	  send_to_char ("You attempt to recharge the staff.\r\n", ch);
	  restored_charges = number (1, 3);
	  GET_OBJ_VAL (obj, 2) += restored_charges;
	  if (GET_OBJ_VAL (obj, 2) > GET_OBJ_VAL (obj, 1))
	    {
	      send_to_char ("The staff is overcharged and explodes!\r\n", ch);
	      sprintf (buf, "%s overcharges %s and it explodes!\r\n",
		       GET_NAME (ch), obj->name);
	      act (buf, TRUE, 0, 0, 0, TO_NOTVICT);
	      explode = dice (GET_OBJ_VAL (obj, 2), 3);
	      GET_HIT (ch) -=
		explode;
	      update_pos (ch);
	      extract_obj (obj);
	      return;
	    }
	  else
	    {
	      sprintf (buf, "You restore %d charges to the staff.\r\n",
		       restored_charges);
	      send_to_char (buf, ch);
	      return;
	    }
	}
      else
	{
	  send_to_char ("That item is already at full charges!\r\n", ch);
	  return;
	}
    }
}
#define PORTAL 20

ASPELL(spell_portal)
{
  /* create a magic portal */
  struct obj_data *tmp_obj, *tmp_obj2;
  struct extra_descr_data *ed;
  struct room_data *rp, *nrp;
  struct char_data *tmp_ch = (struct char_data *) victim;
  char buf[512];

  assert(ch);
  assert((level >= 0) && (level <= LVL_IMPL));


  /*
    check target room for legality.
   */
  rp = &world[ch->in_room];
  tmp_obj = read_object(PORTAL, VIRTUAL);
  if (!rp || !tmp_obj) {
    send_to_char("The magic fails.\n\r", ch);
    extract_obj(tmp_obj);
    return;
  }
  if (IS_SET(rp->room_flags, ROOM_NOMAGIC) || (PRF2_FLAGGED(victim, PRF2_INTANGIBLE) &&
(GET_LEVEL(ch)<LVL_IMMORT || IS_NPC(ch)))) {
    send_to_char("Eldritch wizardry obstructs thee.\n\r", ch);
    extract_obj(tmp_obj);
    return;
  }

  if (IS_SET(rp->room_flags, ROOM_TUNNEL)) {
    send_to_char("There is no room in here to summon!\n\r", ch);
    extract_obj(tmp_obj);
    return;
  }

  if (!(nrp = &world[tmp_ch->in_room])) {
    char str[180];
    sprintf(str, "%s tried to portal to %s but target not in any room!", GET_NAME(ch), GET_NAME(tmp_ch));
    log(str);
    send_to_char("The magic cannot locate the target.\n", ch);
    extract_obj(tmp_obj);
    return;
  }

 if (IS_NPC(tmp_ch) && GET_LEVEL(ch) < LVL_IMMORT) {
    send_to_char("The magic cannot locate the target.\n", ch);
    extract_obj(tmp_obj);
    return;
  }

 if (tmp_ch->in_room == ch->in_room) {
        send_to_char("You are already at the target area.\n", ch);
     extract_obj(tmp_obj);
     return;
}
 if ((GET_LEVEL(tmp_ch) >= LVL_IMMORT) && (GET_LEVEL(ch) < LVL_IMMORT)) {
     send_to_char("You cannot travel to the gods!\n", ch);
     extract_obj(tmp_obj);
     return;
}

 if (GET_ARENASTAT(tmp_ch) != ARENA_NOT && GET_ARENASTAT(ch) == ARENA_NOT){
   send_to_char ("Your target is in the arena right now.\r\n"
		 "Eldrich magic obstructs thee!\r\n", ch);
   extract_obj(tmp_obj);
   return;
 }
 if (GET_ARENASTAT(tmp_ch) == ARENA_NOT && GET_ARENASTAT(ch) != ARENA_NOT){
   send_to_char ("You're in the arena right now whereas your target is not.\r\n"
		 "Eldrich magic obstructs thee!\r\n", ch);
   extract_obj(tmp_obj);
   return;
 }
   
  if (ROOM_FLAGGED(tmp_ch->in_room, ROOM_NOMAGIC)) {
    send_to_char("Your target is protected against your magic.\n\r", ch);
    extract_obj(tmp_obj);
    return;
  }
  if (ROOM_FLAGGED(tmp_ch->in_room, ROOM_HOUSE) || ROOM_FLAGGED(tmp_ch->in_room, ROOM_HOUSE_CRASH)) {
    if (find_house(world[tmp_ch->in_room].number) != GET_IDNUM(ch)) {
      send_to_char("Your target is protected against your magic.\n\r", ch);
      extract_obj(tmp_obj);
      return;
    }
  }

  if (ROOM_FLAGGED(tmp_ch->in_room, ROOM_PRIVATE)) {
    send_to_char("Your target is protected against your magic.\n\r", ch);
    extract_obj(tmp_obj);
    return;
  }

  if (ROOM_FLAGGED(tmp_ch->in_room, ROOM_TUNNEL)) {
    send_to_char("There is not enough room there to portal!\n\r", ch);
    extract_obj(tmp_obj);
    return;
  }

 if (IS_AFFECTED (tmp_ch, AFF_NOPORTAL)) {
   send_to_char("Your target's magic resists your portal attempt.\r\n", ch);
   extract_obj(tmp_obj);
   return;
 }

sprintf(buf, "Through the mists of the portal, you can faintly see %s", nrp->name);
  CREATE(ed , struct extra_descr_data, 1);
  ed->next = tmp_obj->ex_description;
  tmp_obj->ex_description = ed;
  CREATE(ed->keyword, char, strlen(tmp_obj->name) + 1);
  strcpy(ed->keyword, tmp_obj->name);
  ed->description = str_dup(buf);

  tmp_obj->obj_flags.value[0] = 1;
  tmp_obj->obj_flags.value[1] = tmp_ch->in_room;

  if (GET_LEVEL(ch) < LVL_IMMORT)
    GET_OBJ_TIMER(tmp_obj)=1;
  else
    GET_OBJ_TIMER(tmp_obj)=-1; // Never dissapear.
  obj_to_room(tmp_obj,ch->in_room);

  act("$p suddenly appears.",TRUE,ch,tmp_obj,0,TO_ROOM);
  act("$p suddenly appears.",TRUE,ch,tmp_obj,0,TO_CHAR);

/* Portal at other side */
   rp = &world[ch->in_room];
   tmp_obj2 = read_object(PORTAL, VIRTUAL);
   if (!rp || !tmp_obj2) {
     send_to_char("The magic fails.\n\r", ch);
     extract_obj(tmp_obj2);

     return;
   }
  sprintf(buf, "Through the mists of the portal, you can faintly see %s", rp->name);

  CREATE(ed , struct extra_descr_data, 1);
  ed->next = tmp_obj2->ex_description;
  tmp_obj2->ex_description = ed;
  CREATE(ed->keyword, char, strlen(tmp_obj2->name) + 1);
  strcpy(ed->keyword, tmp_obj2->name);
  ed->description = str_dup(buf);

  tmp_obj2->obj_flags.value[0] = 1;
  tmp_obj2->obj_flags.value[1] = ch->in_room;

  if (GET_LEVEL(ch) < LVL_IMMORT)
    GET_OBJ_TIMER(tmp_obj2)=1;
  else
    GET_OBJ_TIMER(tmp_obj2)=-1; // Never dissapear.

  obj_to_room(tmp_obj2,tmp_ch->in_room);

  act("$p suddenly appears.",TRUE,tmp_ch,tmp_obj2,0,TO_ROOM);
  act("$p suddenly appears.",TRUE,tmp_ch,tmp_obj2,0,TO_CHAR);
}

ASPELL(spell_locate_target)
{
  struct char_data *i;
  char name[MAX_INPUT_LENGTH];
  int j;

  strcpy(name, fname(victim->player.name));
  j = level >> 1;

  for (i = character_list; i && (j > 0); i = i->next) {
    if (!is_name(name, i->player.name))
      continue;

    if (i->in_room != NOWHERE)
        sprintf(buf, "%s is in %s.\n\r", IS_NPC(i) ? i->player.short_descr : i->player.name,
                world[i->in_room].name);
    else
      sprintf(buf, "%s's location is uncertain.\r\n",
              IS_NPC(i) ? i->player.short_descr : i->player.name);

    CAP(buf);
    send_to_char(buf, ch);
    j--;
  }

  if (j == level >> 1)
    send_to_char("You sense nothing.\n\r", ch);
}
ACMD(do_speed)
{
if (GET_SKILL(ch, SKILL_SPEED) == 0)
{
send_to_char("You have no idea how to speed.\r\n", ch);
 return;
}
if (GET_MANA(ch) == GET_MAX_MANA(ch))
{
send_to_char("You retract and flex your muscles with strength.\r\n", ch);
GET_MANA(ch) = 0;
GET_MOVE(ch) = GET_MAX_MOVE(ch);
send_to_char("You feel revived and ready to move again.\r\n", ch);
}
 else {
send_to_char("You must have full mana in order to speed!\r\n", ch);
return;
}
}
#define CAN_LISTEN_BEHIND_DOOR(ch,dir)  \
                (GET_CLASS(ch) == CLASS_THIEF) && \
                (EXIT(ch, dir) && EXIT(ch, dir)->to_room != NOWHERE && \
                 IS_SET(EXIT(ch, dir)->exit_info, EX_CLOSED)))

ACMD(do_listen)
{
   struct char_data *tch, *tch_next;
   int dir, percent, found = 0;
   char *heard_nothing = "You don't hear anything unusual.\r\n";
   char *room_spiel    = "$n seems to listen intently for something.";

   percent = number(1,101);

   if(GET_SKILL(ch, SKILL_LISTEN) < percent) {
      send_to_char(heard_nothing, ch);
      return;
   }

   one_argument(argument, buf);

   if(!*buf) {  
      /* no argument means that the character is listening for
       * hidden or invisible beings in the room he/she is in
       */
      for(tch = world[ch->in_room].people; tch; tch = tch_next) {
         tch_next = tch->next_in_room;
         if((tch != ch) && !CAN_SEE(ch, tch) && (GET_LEVEL(tch) < LVL_IMMORT)) 
            found++;
      }
      if(found) {
         if(GET_LEVEL(ch) >= 15) {  
            /* being a higher level is better */
            sprintf(buf, "You hear what might be %d creatures invisible, or hiding.\r\n", \
                        MAX(1,(found+number(0,1)-number(0,1))));
         }
         else sprintf(buf, "You hear an odd rustling in the immediate area.\r\n");
         send_to_char(buf, ch);
      }
      else send_to_char(heard_nothing, ch);
      act(room_spiel, TRUE, ch, 0, 0, TO_ROOM);
      return;
   }
   else {
      /* the argument must be one of the cardinal directions: north, 
       * south, etc.
       */
      for(dir = 0; dir < NUM_OF_DIRS; dir++) {
         if(!strncmp(buf, dirs[dir], strlen(buf)))
            break;
      }
      if (dir == NUM_OF_DIRS) {
         send_to_char("Listen where?\r\n", ch);
         return;
      }
      if((CAN_GO(ch, dir)) || (CAN_LISTEN_BEHIND_DOOR(ch, dir)) {
         for(tch = world[EXIT(ch, dir)->to_room].people; tch; tch=tch_next) {
            tch_next = tch->next_in_room;
            found++;
         }
         if(found) {
            if(GET_LEVEL(ch) >= 15) {
               sprintf(buf, "You hear what might be %d creatures %s%s.\r\n", \
                        MAX(1,(found+number(0,1)-number(0,1))),
                        ((dir==5)?"below":(dir==4)?"above": "to the "), 
                        ((dir==5)?"":(dir==4)?"":dirs[dir]));
            }
            else sprintf(buf, "You hear sounds from %s%s.\r\n", \
                        ((dir==5)?"below":(dir==4)?"above": "the "), 
                        ((dir==5)?"":(dir==4)?"":dirs[dir]));
            send_to_char(buf, ch);
         }
         else send_to_char(heard_nothing, ch);
         act(room_spiel, TRUE, ch, 0, 0, TO_ROOM);
         return;
      }
      else send_to_char("You can't listen in that direction.\r\n", ch);
      return;
   }
   return;
}
ASPELL(spell_home)
{
  extern long mortal_start_room[NUM_STARTROOMS + 1];
  int i, homenum = 0, loadrm = -1;
  char mybuf[1024];
  struct char_data *victor;

  for (i = 0; i < num_of_houses; i++)
    if (house_control[i].owner == GET_IDNUM(ch))
      homenum = house_control[i].vnum;

  if (homenum == 0){
    send_to_char("The spell fails because you don't own a house!\r\n", ch);
    return;
  }

  if (RIDING(victim) || RIDDEN_BY(victim)) {
  send_to_char("The spell fails because your victim is atop a mount.\r\n", ch);
  return;  
  }

  loadrm = GET_HOME(ch);
  if (!(loadrm >= 1 && loadrm <= NUM_STARTROOMS)){
    sprintf(buf, "DEBUG: Home Recall Trigger - %s GET_HOME is %d (invalid)",
            GET_NAME(ch), loadrm);
    mudlog(buf, NRM, LVL_GRGOD, TRUE);
    loadrm = 1;
  }
  loadrm = real_room(mortal_start_room[loadrm]);

  if (ch->in_room == real_room(homenum)){
    send_to_char("The spell fails because you're already at home!\r\n", ch);
    return;
  }

  if (IS_ARENACOMBATANT(victim)){
    victor = FIGHTING(victim);
    sprintf(mybuf, "(Recalled)");
    if (GET_ARENAFLEETIMER(victim) >= 1
        && GET_ARENAFLEETIMER(victim) <= 1+arena_flee_timeout){
      victor = LASTFIGHTING(victim);
      send_to_char("You recalled before the flee-recall timer expired.\r\n"
                   "You have conceded the match!\r\n", victim);
      sprintf(mybuf, "(Fled & Recalled)");
      GET_ARENAFLEETIMER(victim) = 0;
    }
    match_over(victor, victim, mybuf, FALSE);
    char_from_room (victim);
    char_to_room (victim, real_room (arena_preproom));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
    return;
  }

  act ("$n magically teleports out.", FALSE, ch, 0, 0, TO_ROOM);
  char_from_room(ch);

  if (GET_ARENASTAT(victim) == ARENA_OBSERVER){
    char_to_room (victim, real_room (arena_observeroom));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
    return;
  }

  char_to_room(ch, real_room(homenum));
  act("$n suddenly appears in the room.", FALSE, ch, 0, 0, TO_ROOM);

  look_at_room(ch, 0);
  send_to_char("\r\nAhhhh... Home Sweet Home!\r\n", ch);

}
ASPELL (spell_retreat)
{
  int load_room = 0;
  char mybuf[1024];
  struct char_data *victor;

  if (victim == NULL || IS_NPC (victim))
    return;

    if ((load_room == NOWHERE || load_room == 0 || !load_room)) {
    send_to_char("You must rent somewhere before you can retreat!\r\n", ch);
    return;
  }

  if (RIDING(victim) || RIDDEN_BY(victim)) {
  send_to_char("The spell fails because your victim is atop a mount.\r\n", ch);
  return;  
  }

  act ("$n disappears.", TRUE, victim, 0, 0, TO_ROOM);
  if (IS_ARENACOMBATANT(victim)){
    victor = FIGHTING(victim);
    sprintf(mybuf, "(Recalled)");
    if (GET_ARENAFLEETIMER(victim) >= 1
        && GET_ARENAFLEETIMER(victim) <= 1+arena_flee_timeout){
      victor = LASTFIGHTING(victim);
      send_to_char("You recalled before the flee-recall timer expired.\r\n"
                   "You have conceded the match!\r\n", victim);
      sprintf(mybuf, "(Fled & Recalled)");
      GET_ARENAFLEETIMER(victim) = 0;
    }
    match_over(victor, victim, mybuf, FALSE);
    char_from_room (victim);
    char_to_room (victim, real_room (arena_preproom));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
    return;
  }
    
  char_from_room (victim);
  if (GET_ARENASTAT(victim) == ARENA_OBSERVER){
    char_to_room (victim, real_room (arena_observeroom));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
    return;
  }
     
    char_to_room (victim, real_room(GET_LOADROOM(victim)));
    act ("$n appears in the middle of the room.", TRUE, victim, 0, 0, TO_ROOM);
    look_at_room (victim, 0);
}
