/* ************************************************************************
   *   File: limits.c                                      Part of CircleMUD *
   *  Usage: limits & gain funcs for HMV, exp, hunger/thirst, idle time      *
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
#include "spells.h"
#include "comm.h"
#include "db.h"
#include "handler.h"
#include "dg_scripts.h"

extern struct char_data *character_list;
extern struct obj_data *object_list;
extern struct index_data *obj_index;
extern struct room_data *world;
extern int max_exp_gain;
extern int max_exp_loss;
extern int exp_to_level();
extern int arena_flee_timeout;
extern long r_immort_start_room;
void do_oldbie (struct char_data *vict);
void die(struct char_data * ch, struct char_data * killer);
char *title_male (int class, int level);
char *title_female (int class, int level);

extern int has_boat(struct char_data *ch);

/* When age < 15 return the value p0 */
/* When age in 15..29 calculate the line between p1 & p2 */
/* When age in 30..44 calculate the line between p2 & p3 */
/* When age in 45..59 calculate the line between p3 & p4 */
/* When age in 60..79 calculate the line between p4 & p5 */
/* When age >= 80 return the value p6 */
int 
graf (int age, int p0, int p1, int p2, int p3, int p4, int p5, int p6)
{

  if (age < 15)
    return (p0);		/* < 15   */
  else if (age <= 29)
    return (int) (p1 + (((age - 15) * (p2 - p1)) / 15));	/* 15..29 */
  else if (age <= 44)
    return (int) (p2 + (((age - 30) * (p3 - p2)) / 15));	/* 30..44 */
  else if (age <= 59)
    return (int) (p3 + (((age - 45) * (p4 - p3)) / 15));	/* 45..59 */
  else if (age <= 79)
    return (int) (p4 + (((age - 60) * (p5 - p4)) / 20));	/* 60..79 */
  else
    return (p6);		/* >= 80 */
}


/*
 * The hit_limit, mana_limit, and move_limit functions are gone.  They
 * added an unnecessary level of complexity to the internal structure,
 * weren't particularly useful, and led to some annoying bugs.  From the
 * players' point of view, the only difference the removal of these
 * functions will make is that a character's age will now only affect
 * the HMV gain per tick, and _not_ the HMV maximums.
 */

/* manapoint gain pr. game hour */
int 
mana_gain (struct char_data *ch)
{
  int gain;
  int i;

  if (IS_NPC (ch))
    {
      /* Neat and fast */
      gain = GET_LEVEL (ch);
    }
  else
    {
      gain = graf (age (ch).year, 4, 8, 12, 16, 12, 10, 8);

            for (i=0; i< NUM_WEARS; i++) {
       if(GET_EQ(ch, i)) {
        if(GET_OBJ_TYPE(GET_EQ(ch, i)) == ITEM_MP_REGEN) {
         gain += 10; /* this can be played around with to your liking */
        }
       }
      }

      /* Class calculations */

      /* Skill/Spell calculations */

      /* Position calculations    */
      switch (GET_POS (ch))
	{
	case POS_SLEEPING:
	  gain <<= 1;       /* mult by 2 */
	  break;
	case POS_MEDITATING:
	  gain <<= 3;       /* mult by 8 */
	  break;
	case POS_RESTING:
	  gain += (gain >> 1);	/* Divide by 2 */
	  break;
	case POS_SITTING:
	  gain += (gain >> 2);	/* Divide by 4 */
	  break;
	}

      if ((GET_CLASS (ch) == CLASS_MAGIC_USER) || (GET_CLASS (ch) == CLASS_CLERIC))
	gain <<= 1;
    }

  if (IS_AFFECTED (ch, AFF_POISON))
    gain >>= 2;

  if ((GET_COND (ch, FULL) == 0) || (GET_COND (ch, THIRST) == 0))
    gain >>= 2;

  if (IS_SET(ROOM_FLAGS(ch->in_room), ROOM_GOOD_REGEN))
    gain += (gain * 2);
  if (IS_SET(ROOM_FLAGS(ch->in_room), ROOM_BAD_REGEN)) gain=0;
  return (gain);
}


int 
hit_gain (struct char_data *ch)
/* Hitpoint gain pr. game hour */
{
  int gain;
  int i;

  if (IS_NPC (ch))
    {
      gain = GET_LEVEL (ch);
      /* Neat and fast */
    }
  else
    {

      gain = graf (age (ch).year, 8, 12, 20, 32, 16, 10, 4);

            for (i=0; i< NUM_WEARS; i++) {
       if(GET_EQ(ch, i)) {
        if(GET_OBJ_TYPE(GET_EQ(ch, i)) == ITEM_HP_REGEN) {  
         gain += 10; /* this can be played around with to your liking */
        }
       }
      }

      /* Class/Level calculations */

      /* Skill/Spell calculations */

      /* Position calculations    */

      switch (GET_POS (ch))
	{
	case POS_SLEEPING:
	  gain += (gain >> 1);	/* Divide by 2 */
	  break;
	case POS_MEDITATING:
	  gain += (gain >> 1);	/* Divide by 2 */
	  break;
	case POS_RESTING:
	  gain += (gain >> 2);	/* Divide by 4 */
	  break;
	case POS_SITTING:
	  gain += (gain >> 3);	/* Divide by 8 */
	  break;
	}

      if ((GET_CLASS (ch) == CLASS_MAGIC_USER) || (GET_CLASS (ch) == CLASS_CLERIC))
	gain >>= 1;
    }

  if (IS_AFFECTED (ch, AFF_POISON) || IS_AFFECTED (ch, AFF_PLAGUED))
    gain >>= 2;

  if ((GET_COND (ch, FULL) == 0) || (GET_COND (ch, THIRST) == 0))
    gain >>= 2;

  if (IS_SET(ROOM_FLAGS(ch->in_room), ROOM_GOOD_REGEN))
    gain += (gain * 2);
  if (IS_SET(ROOM_FLAGS(ch->in_room), ROOM_BAD_REGEN)) gain=0;
  return (gain);
}



int 
move_gain (struct char_data *ch)
/* move gain pr. game hour */
{
  int gain;
  int i;  

  if (IS_NPC (ch))
    {
      return (GET_LEVEL (ch));
      /* Neat and fast */
    }
  else
    {
      gain = graf (age (ch).year, 16, 20, 24, 20, 16, 12, 10);

            for (i=0; i< NUM_WEARS; i++) {
       if(GET_EQ(ch, i)) {
        if(GET_OBJ_TYPE(GET_EQ(ch, i)) == ITEM_MV_REGEN) {  
         gain += 10; /* this can be played around with to your liking */
        }
       }
      }

      /* Class/Level calculations */

      /* Skill/Spell calculations */


      /* Position calculations    */
      switch (GET_POS (ch))
	{
	case POS_SLEEPING:
	  gain += (gain >> 1);	/* Divide by 2 */
	  break;
	case POS_MEDITATING:
	  gain += (gain >> 1);	/* Divide by 2 */
	  break;
	case POS_RESTING:
	  gain += (gain >> 2);	/* Divide by 4 */
	  break;
	case POS_SITTING:
	  gain += (gain >> 3);	/* Divide by 8 */
	  break;
	}

      if (IS_AFFECTED (ch, AFF_POISON))
	gain >>= 2;

      if ((GET_COND (ch, FULL) == 0) || (GET_COND (ch, THIRST) == 0))
	gain >>= 2;

  if (IS_SET(ROOM_FLAGS(ch->in_room), ROOM_GOOD_REGEN))
    gain += (gain * 2);
      if (IS_SET(ROOM_FLAGS(ch->in_room), ROOM_BAD_REGEN)) gain=0;
      return gain;
    }
}



void 
set_title (struct char_data *ch, char *title)
{
  if (title == NULL)
    {
      if (GET_SEX (ch) == SEX_FEMALE)
	title = "the Woman";
      else
	title = "the Man";
    }

  if (strlen (title) > MAX_TITLE_LENGTH)
    title[MAX_TITLE_LENGTH] = '\0';

  if (GET_TITLE (ch) != NULL)
    free (GET_TITLE (ch));

  GET_TITLE (ch) = str_dup (title);
}

void 
check_autowiz (struct char_data *ch)
{
#ifndef CIRCLE_UNIX
  return;
#else
  char buf[100];
  extern int use_autowiz;
  extern int min_wizlist_lev;

  if (use_autowiz && GET_LEVEL (ch) >= LVL_HERO)
    {
      sprintf (buf, "nice ../bin/autowiz %d %s %d %s %d &", min_wizlist_lev,
	       WIZLIST_FILE, LVL_HERO, IMMLIST_FILE, (int) getpid ());
      mudlog ("Initiating autowiz.", PFT, LVL_IMMORT, FALSE);
      system (buf);
    }
#endif /* CIRCLE_UNIX */
}



void 
gain_exp (struct char_data *ch, int gain)
{
  int is_altered = FALSE;
//  int i;
//  struct obj_data *obj;
  int num_levels = 0;
  char buf[128];

  if (IS_NPC(ch))
    return;

  if ((!IS_NPC (ch) && ((GET_LEVEL (ch) < 1 || GET_LEVEL (ch) >= LVL_HERO))) ||
PRF2_FLAGGED(ch, PRF2_INTANGIBLE))
    return;

  if (IS_NPC (ch))
    {
      GET_EXP (ch) += gain;
      return;
    }

  if (IS_ARENACOMBATANT(ch)){
    return;
  }

  if (gain > 0)
    {
      gain = MIN (max_exp_gain, gain);	/* put a cap on the max gain per kill */
      GET_EXP (ch) += gain;
      while (GET_LEVEL (ch) < LVL_HERO &&
             GET_EXP (ch) >= exp_to_level (GET_LEVEL(ch)))
	{
	  GET_LEVEL (ch) += 1;
	  num_levels++;
	  advance_level (ch);
	  is_altered = TRUE;
	}

      if (is_altered)
	{

	  if (num_levels == 1)
	    send_to_char ("You rise a level!\r\n", ch);
	  else
	    {
	      sprintf (buf, "You rise %d levels!\r\n", num_levels);
	      send_to_char (buf, ch);
	    }
	  check_autowiz (ch);
	  sprintf (buf, "&m[&YINFO&m]&n %s has advanced to level %d!\r\n", GET_NAME (ch), GET_LEVEL (ch));
	  send_to_all (buf);

//	  if (GET_LEVEL (ch) == LVL_GETSTUFF)
//	    {
//	      do_oldbie (ch);
//	      send_to_char ("&YThe gods have rewarded you for getting to level 3!&n\r\nTwo gold bricks fall from the sky into your hands.&n\r\n", ch);
//	    }

         if (GET_LEVEL (ch) == 10)
          send_to_char("Congratulations on achieving level 10! Be warned, that you are no longer\r\nconsidered a newbie and can now be attacked by other players. You will\r\nnow lose experience when fleeing from combat.\r\n", ch);

	  if (GET_LEVEL (ch) == LVL_HERO)
	    {
	      sprintf (buf, "&m[&YINFO&m]&n %s has become a &YHERO&n!\r\n", 
		       GET_NAME (ch));
	      send_to_all (buf);
              set_title(ch, "the Hero");
	      send_to_char ("You shall forever be known as a hero throughout the land of Deltania!\r\nYou have reached level 100, for more information please type 'help hero'.\r\n", ch);
	      
              /* a hero sees all that is seen and unseen */
              SET_BIT (PRF_FLAGS (ch), PRF_HOLYLIGHT);

             
	      save_char (ch, NOWHERE);	     
	    }
	}
    }
  else if (gain < 0)
    {
      gain = MAX (-max_exp_loss, gain);		/* Cap max exp lost per death */
      GET_EXP (ch) += gain;
      if (GET_EXP (ch) < 0)
	GET_EXP (ch) = 0;
    }
}


void 
gain_exp_regardless (struct char_data *ch, int gain)
{
  int is_altered = FALSE;
  int num_levels = 0;

  if (IS_NPC(ch))
    return;

  GET_EXP (ch) += gain;
  if (GET_EXP (ch) < 0)
    GET_EXP (ch) = 0;

  if (!IS_NPC (ch))
    {
      while (GET_LEVEL (ch) < LVL_IMPL &&
          GET_EXP (ch) >= exp_to_level (GET_LEVEL(ch)))
	{
	  GET_LEVEL (ch) += 1;
	  num_levels++;
	  advance_level (ch);
	  is_altered = TRUE;
	}

      if (is_altered)
	{
	  if (num_levels == 1)
	    send_to_char ("You rise a level!\r\n", ch);
	  else
	    {
	      sprintf (buf, "You rise %d levels!\r\n", num_levels);
	      send_to_char (buf, ch);
	    }
	  check_autowiz (ch);
	  sprintf (buf, "&m[&YINFO&m]&n %s has advanced to level %d!\r\n", GET_NAME (ch), GET_LEVEL (ch));
	  send_to_all (buf);
	  if (GET_LEVEL (ch) == LVL_GETSTUFF)
	    {
	      do_oldbie (ch);
	      send_to_char ("&YThe gods have rewarded you for getting to level 3!&n\r\nTwo gold bricks fall from the sky into your hands.&n\r\n", ch);
	    }
	}
    }
}


void 
gain_condition (struct char_data *ch, int condition, int value)
{
  bool intoxicated;

  if (GET_COND (ch, condition) == -100)	/* No change */
    return;

  intoxicated = (GET_COND (ch, DRUNK) > 4);

  GET_COND (ch, condition) += value;

  if ((condition == DRUNK))
  GET_COND (ch, condition) = MAX (0, GET_COND (ch, condition));
  else
  GET_COND (ch, condition) = MAX (-72, GET_COND (ch, condition));

  GET_COND (ch, condition) = MIN (24, GET_COND (ch, condition));

  if (GET_COND (ch, condition) || PLR_FLAGGED (ch, PLR_WRITING))
    return;

  switch (condition)
    {
    case FULL:
      send_to_char ("You are hungry.\r\n", ch);
      return;
    case THIRST:
      send_to_char ("You are thirsty.\r\n", ch);
      return;
    case DRUNK:
      if (intoxicated)
	send_to_char ("You are now sober.\r\n", ch);
      return;
    default:
      break;
    }

}


void 
check_idling (struct char_data *ch)
{
  extern int free_rent;
  void Crash_rentsave (struct char_data *ch, int cost);
  void command_interpreter (struct char_data *ch, char *argument);

  if (PRF2_FLAGGED(ch, PRF2_LOCKOUT))
    ch->char_specials.timer = 0;

  if (++(ch->char_specials.timer) > 5  && PRF2_FLAGGED(ch, PRF2_MBUILDING)) {
    send_to_char("Build mode, my friend, was not made for you to idle in.\r\n", ch);
    command_interpreter(ch, "build off");
    return;
  }

  if (ch->char_specials.timer > 8) {
    if (GET_WAS_IN (ch) == NOWHERE && ch->in_room != NOWHERE)
      {
	GET_WAS_IN (ch) = ch->in_room;
	if (FIGHTING (ch))
	  {
	    stop_fighting (FIGHTING (ch));
	    stop_fighting (ch);
	  }
	act ("$n disappears into the void.", TRUE, ch, 0, 0, TO_ROOM);
	send_to_char ("You have been idle, and are pulled into a void.\r\n", ch);
	save_char (ch, NOWHERE);
	Crash_crashsave (ch);
	char_from_room (ch);
	char_to_room (ch, 1);
      }
    else if (ch->char_specials.timer > 48)
      {
	if (ch->in_room != NOWHERE)
	  char_from_room (ch);
	char_to_room (ch, 3);
	if (ch->desc)
	  close_socket (ch->desc);
	ch->desc = NULL;
	if (free_rent || (GET_LEVEL(ch) >= LVL_IMMORT))
	  Crash_rentsave (ch, 0);
	else
	  Crash_idlesave (ch);
	sprintf (buf, "%s force-rented and extracted (idle).", GET_NAME (ch));
	mudlog (buf, PFT, LVL_GOD, TRUE);
	extract_char (ch);
      }
  }
}


/* Update PCs, NPCs, and objects */
void 
point_update (void)
{
  void update_char_objects (struct char_data *ch);	/* handler.c */
  void extract_obj (struct obj_data *obj);	/* handler.c */
  void do_extract_char (struct char_data *ch, int type);	/* handler.c */
  struct descriptor_data *d;
  struct char_data *i, *next_char;
  struct obj_data *j, *next_thing, *jj, *next_thing2;
  char mybuf[1024];
  int enter_player_game (struct descriptor_data *d);

  /* characters */
  for (i = character_list; i; i = next_char)
    {
      next_char=i->next;
      if (!IS_NPC(i) && IS_SET(PRF2_FLAGS(i), PRF2_INTANGIBLE) && GET_LEVEL(i) < LVL_IMMORT) {
        if (PRF2_FLAGGED(i, PRF2_MBUILDING)) check_idling (i);
        else if (i->player_specials->saved.death_timer > 0) {
          i->player_specials->saved.death_timer--;
          if (!i->player_specials->saved.death_timer) {
            send_to_char("Death makes a cryptic gesture and you find yourself englufed in light!\r\n", i);
            act("Death makes a cryptic gesture and $n dissapears in a bright light!\r\n", FALSE, i, NULL, NULL, TO_ROOM);
            REMOVE_BIT(PRF2_FLAGS(i), PRF2_INTANGIBLE);
            d=i->desc;
            do_extract_char(i, 2);
            enter_player_game(d);
            look_at_room(d->character, 1);
            act("A white mist appears and $n steps out.\r\n", FALSE, i, NULL, NULL, TO_ROOM);
          }
        }
        continue;
      }
      gain_condition (i, FULL, -1);
      gain_condition (i, DRUNK, -1);
      gain_condition (i, THIRST, -1);

      if (GET_POS (i) >= POS_STUNNED)
	{
	  GET_HIT (i) = MIN (GET_HIT (i) + hit_gain (i), GET_MAX_HIT (i));
	  GET_MANA (i) = MIN (GET_MANA (i) + mana_gain (i), GET_MAX_MANA (i));
	  GET_MOVE (i) = MIN (GET_MOVE (i) + move_gain (i), GET_MAX_MOVE (i));

	  /* This is the flee-protection code for arena */
	  /* A person who flee's from combat must wait for the flee_timer */
	  /* to expire before recalling, or else it's considered as       */
	  /* concedeing a lost match. -Thargor- */
	  if (IS_ARENACOMBATANT(i)){
	    if (GET_ARENAFLEETIMER(i) >= (1 + arena_flee_timeout)){
	      send_to_char("Flee-Recall timer expired. You may now"
			   "recall without conceding the match.\r\n", i);
	      GET_ARENAFLEETIMER(i) = 0;
	    }else if (GET_ARENAFLEETIMER(i) >= 1){
	      GET_ARENAFLEETIMER(i) += 1;
	      sprintf(mybuf, "Flee-Recall timer in tic #%d. "
		      "%d tic(s) to go.\r\n", 
		      (GET_ARENAFLEETIMER(i)-1), 
		      (arena_flee_timeout+1)-GET_ARENAFLEETIMER(i));
	      send_to_char(mybuf, i);
	    }
	  }

	  if (IS_AFFECTED (i, AFF_POISON))
	    damage (i, i, 2, SPELL_POISON);
	  if (GET_POS (i) <= POS_STUNNED)
	    update_pos (i);
	  if (IS_AFFECTED (i, AFF_PLAGUED))
	    send_to_char("&RYou feel the effects of a deadly plague ravage through your body.&n", i);
	}
      else if (GET_POS (i) == POS_INCAP)
	damage (i, i, 1, TYPE_SUFFERING);
      else if (GET_POS (i) == POS_MORTALLYW)
	damage (i, i, 2, TYPE_SUFFERING);
      if (!IS_NPC (i))
	{
	  update_char_objects (i);

	  if (GET_LEVEL (i) < LVL_IMMORT)
	    check_idling (i);

      if (GET_LEVEL(i) < LVL_IMMORT)
	if (SECT(i->in_room) == SECT_WATER_NOSWIM && !has_boat(i)) {
	  act("$n thrashes about in the water straining to stay afloat.", FALSE,
		i, 0, 0, TO_ROOM);
	  send_to_char("You are drowning!\r\n", i);
	  damage (i, i, GET_MAX_HIT(i) / 5, TYPE_DROWNING); /* TYPE_DROWNING ? */
	}
      if (GET_LEVEL(i) < LVL_IMMORT) {

      if ((GET_COND(i, THIRST) < 0) && (GET_COND(i, THIRST) > -12))
      send_to_char("You are extremely thirsty.\r\n", i);
      else
      if ((GET_COND(i, THIRST) <= -12) && (GET_COND(i, THIRST) >= -20))
      send_to_char("You are suffering from dehydration and must get something to drink.\r\n", i); 
     else
    if ((GET_COND(i, THIRST) < -20) && (GET_COND(i, THIRST) > -24)) {
    send_to_char("You are now dying of thirst! You must get something to drink quickly.\r\n", i);
     damage (i, i, GET_MAX_HIT(i) / 4, TYPE_STARVING);
     update_pos(i);
  }
  else
  if ((GET_COND(i, THIRST) <= -24) && (GET_COND(i, THIRST) > -100)) {
  send_to_char("&RYou have died of extreme thirst!&n\r\n", i);
  die(i, NULL);
  }

      if ((GET_COND(i, FULL) < 0) && (GET_COND(i, FULL) >= -23))
      send_to_char("You are very hungry.\r\n", i);
      if ((GET_COND(i, FULL) < -23) && (GET_COND(i, FULL) > -36))       
      send_to_char("You are extremely hungry.\r\n", i);                                            
      else
      if ((GET_COND(i, FULL) <= -36) && (GET_COND(i, FULL) >= -48))  
      send_to_char("You are suffering from starvation and must get something to eat.\r\n", i);
     else
    if ((GET_COND(i, FULL) < -48) && (GET_COND(i, FULL) > -60)) {
    send_to_char("You are now dying of hunger! You must get something to eat soon.\r\n", i);
     damage (i, i, GET_MAX_HIT(i) / 8, TYPE_STARVING);
     update_pos(i);
  }
  else
  if ((GET_COND(i, FULL) <= -60) && (GET_COND(i, FULL) > -100)) {
  send_to_char("&RYou have died of extreme hunger!&n\r\n", i);
  die(i, NULL);
  }
 }

	}
    }

  /* objects */
  for (j = object_list; j; j = next_thing)
    {
      next_thing = j->next;	/* Next in object list */

    /* If this object is in water. */
    if (j->in_room != NOWHERE && (SECT(j->in_room) == SECT_WATER_NOSWIM ||
		SECT(j->in_room) == SECT_WATER_SWIM)) {
      /* Give everything a random chance of sinking, some may never. */
       if (GET_OBJ_TYPE(j) != ITEM_BOAT && number(0, GET_OBJ_WEIGHT(j)) > 0) {
	act("$p sinks into the murky depths.", FALSE, 0, j, 0, TO_ROOM);
	extract_obj(j);
	continue;
      } else
	act("$p floats unsteadily in the area.", FALSE, 0, j, 0, TO_ROOM);
    }
    /* Portals */
    if (GET_OBJ_TYPE(j) == ITEM_PORTAL && !j->carried_by && !j->in_obj) {
      // Ignore portals in people's inv or in other objects.
      if (GET_OBJ_TIMER(j)>0)
        GET_OBJ_TIMER(j)--;
      if (GET_OBJ_TIMER(j)==0) {
	act ("$p dissapears in a puff of smoke!", FALSE, 0, j, 0, TO_ROOM);
        extract_obj(j);
        continue;
      }
    }    

	/* If this is a corpse */
      if ((GET_OBJ_TYPE (j) == ITEM_CONTAINER) && GET_OBJ_VAL (j, 3))
	{
	  /* timer count down */
	  if (GET_OBJ_TIMER (j) > 0)
	    GET_OBJ_TIMER (j)--;

	  if (!GET_OBJ_TIMER (j))
	    {

	      if (j->carried_by)
		act ("$p decays in your hands.", FALSE, j->carried_by, j, 0, TO_CHAR);
	      else if ((j->in_room != NOWHERE) && (world[j->in_room].people))
		{
		  act ("A quivering horde of maggots consumes $p.",
		       TRUE, world[j->in_room].people, j, 0, TO_ROOM);
		  act ("A quivering horde of maggots consumes $p.",
		       TRUE, world[j->in_room].people, j, 0, TO_CHAR);
		}

	      for (jj = j->contains; jj; jj = next_thing2)
		{
		  next_thing2 = jj->next_content;	/* Next in inventory */
		  obj_from_obj (jj);

		  if (j->in_obj)
		    obj_to_obj (jj, j->in_obj);
		  else if (j->carried_by)
		    obj_to_room (jj, j->carried_by->in_room);
		  else if (j->in_room != NOWHERE)
		    obj_to_room (jj, j->in_room);
		  else
		    assert (FALSE);
       }
        extract_obj(j);
      }
     }
    /* If the timer is set, count it down and at 0, try the trigger */
    /* note to .rej hand-patchers: make this last in your point-update() */
    else if (GET_OBJ_TIMER(j)>0) {
      GET_OBJ_TIMER(j)--;
      if (!GET_OBJ_TIMER(j))
        timer_otrigger(j);
    }
   }
 }
