/* ************************************************************************
   *   File: act.offensive.c                               Part of CircleMUD *
   *  Usage: player-level commands of an offensive nature                    *
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
#include "olc.h"

/* extern variables */
extern byte pk_victim_min;       /* see config.c */
extern struct room_data *world;
extern struct descriptor_data *descriptor_list;
extern struct room_data *world;
extern int pk_allowed;
extern struct dex_skill_type dex_app_skill[];

/* extern functions */
bool check_perm_duration(struct char_data *ch, long bitvector);
void trans_to_preproom(struct char_data *);
void match_over(struct char_data *, struct char_data *, char *, int);
void raw_kill(struct char_data * ch, struct char_data * killer);
void check_killer (struct char_data *ch, struct char_data *vict);


ACMD (do_assist)
{
  struct char_data *helpee, *opponent;

  if (FIGHTING (ch))
    {
      send_to_char ("You're already fighting!  How can you assist someone else?\r\n", ch);
      return;
    }
  one_argument (argument, arg);

  if (!*arg)
    send_to_char ("Whom do you wish to assist?\r\n", ch);
  else if (!(helpee = get_char_room_vis (ch, arg)))
    send_to_char (NOPERSON, ch);
  else if (helpee == ch)
    send_to_char ("You can't help yourself any more than this!\r\n", ch);
  else
    {
      for (opponent = world[ch->in_room].people;
	   opponent && (FIGHTING (opponent) != helpee);
	   opponent = opponent->next_in_room)
	;

      if (!opponent)
	act ("But nobody is fighting $M!", FALSE, ch, 0, helpee, TO_CHAR);
      else if (!CAN_SEE (ch, opponent))
	act ("You can't see who is fighting $M!", FALSE, ch, 0, helpee, TO_CHAR);
      else if (!pk_allowed && !IS_NPC (opponent))	/* prevent accidental pkill */
	act ("Use 'murder' if you really want to attack $N.", FALSE,
	     ch, 0, opponent, TO_CHAR);
      else
	{
	  send_to_char ("You join the fight!\r\n", ch);
	  act ("$N assists you!", 0, helpee, 0, ch, TO_CHAR);
	  act ("$n assists $N.", FALSE, ch, 0, helpee, TO_NOTVICT);
	  hit (ch, opponent, TYPE_UNDEFINED);
	}
    }
}


ACMD (do_hit)
{
  struct char_data *vict;
  void deathblow (struct char_data *ch, struct char_data *victim, int dam, int attacktype);

  one_argument (argument, arg);

  if (!*arg)
    send_to_char ("Hit who?\r\n", ch);
  else if (!(vict = get_char_room_vis (ch, arg)))
    send_to_char ("They don't seem to be here.\r\n", ch);
  else if (vict == ch)
    {
      send_to_char ("You hit yourself...OUCH!.\r\n", ch);
      act ("$n hits $mself, and says OUCH!", FALSE, ch, 0, vict, TO_ROOM);
    }
  else if (IS_AFFECTED (ch, AFF_CHARM) && (ch->master == vict))
    act ("$N is just such a good friend, you simply can't hit $M.", FALSE, ch, 0, vict, TO_CHAR);
  else
    {
      if (!pk_allowed)
	{
	  if (!IS_NPC (vict) && !IS_NPC (ch))
	    {
	      /* Arena Mod - Thargor */
	      if (IS_ARENACOMBATANT(ch) && IS_ARENACOMBATANT(vict))
		subcmd = SCMD_MURDER;

	      if (subcmd != SCMD_MURDER && subcmd != SCMD_DEATHBLOW)
		{
		  send_to_char ("Use 'murder' to hit another player.\r\n", ch);
		  return;
		}
	      else
		{
		  if (!(!pk_allowed && !IS_NPC(vict) && !IS_NPC(ch)
			&& ((GET_LEVEL(vict) < pk_victim_min) 
			    || (GET_LEVEL(ch) < pk_victim_min))))
		    check_killer (ch, vict);
		}
	    }
	  if (IS_AFFECTED (ch, AFF_CHARM) && !IS_NPC (ch->master) && !IS_NPC (vict))
	    return;		/* you can't order a charmed pet to attack a
				 * player */
	}
      if ((GET_POS (ch) == POS_STANDING) && (vict != FIGHTING (ch)))
	{
          if (subcmd == SCMD_DEATHBLOW) {
            if (GET_HIT(vict) < 0 && GET_POS(vict) < POS_SLEEPING)
              deathblow (ch, vict, 100, GET_ATTACKTYPE(ch));
            else
              send_to_char("They aren't near death!\r\n", ch);
          }
          else {
	    hit (ch, vict, TYPE_UNDEFINED);
	    WAIT_STATE (ch, PULSE_VIOLENCE + 2);
          }
	}
      else
	send_to_char ("You do the best you can!\r\n", ch);
    }
}



ACMD (do_kill)
{
  struct char_data *vict;

  if ((GET_LEVEL (ch) < LVL_IMPL) || IS_NPC (ch))
    {
      do_hit (ch, argument, cmd, subcmd);
      return;
    }
  one_argument (argument, arg);

  if (!*arg)
    {
      send_to_char ("Kill who?\r\n", ch);
    }
  else
    {
      if (!(vict = get_char_room_vis (ch, arg)))
	send_to_char ("They aren't here.\r\n", ch);
      else if (ch == vict)
	send_to_char ("Your mother would be so sad.. :(\r\n", ch);
      else
	{
	  act ("You chop $M to pieces!  Ah!  The blood!", FALSE, ch, 0, vict, TO_CHAR);
	  act ("$N chops you to pieces!", FALSE, vict, 0, ch, TO_CHAR);
	  act ("$n brutally slays $N!", FALSE, ch, 0, vict, TO_NOTVICT);
         raw_kill(vict, ch);
	}
    }
}


/* Switch from fighting one character to fighting another character */
ACMD(do_target)
{
  int percent, prob;
  struct char_data *vict;
         
  one_argument(argument, arg);
  
  if (IS_NPC(ch) && IS_AFFECTED(ch, AFF_CHARM)) {
    send_to_char("You'd better leave this skill to players.\r\n", ch);
    return;
  } else if (!IS_NPC(ch) && !GET_SKILL(ch, SKILL_TARGET)) {
    send_to_char("You don't know of that skill.\r\n", ch);
    return;
  }
     
//  if(check_state(ch) != 0) {
//    send_to_char("You are too engaged in combat to switch now!\r\n", ch);
//    return;
//        }
  
  if (!*arg)
    send_to_char("Hit who?\r\n", ch);
  else if (!(vict = get_char_room_vis(ch, arg)))
    send_to_char("They don't seem to be here.\r\n", ch);
  else if (vict == ch) {
    send_to_char("You hit yourself...OUCH!.\r\n", ch);
    act("$n hits $mself, and says OUCH!", FALSE, ch, 0, vict, TO_ROOM);
  } else if (IS_AFFECTED(ch, AFF_CHARM) && (ch->master == vict))
    act("$N is just such a good friend, you simply can't hit $M.", FALSE, ch, 0, vict, TO_CHAR);
  else {
    if (!pk_allowed) {
      if (!IS_NPC(vict) && !IS_NPC(ch) && (subcmd != SCMD_MURDER)) {
        send_to_char("Use 'murder' to hit another player.\r\n", ch);
        return;
      }
      if (IS_AFFECTED(ch, AFF_CHARM) && !IS_NPC(ch->master) && !IS_NPC(vict))
        return;                 /* you can't order a charmed pet to attack a
                                 * player */
          }
      percent = number(1, 401);     /* 101% is a complete failure */
      if (IS_NPC(ch))
         prob = 90;
       else
         prob = GET_SKILL(ch, SKILL_TARGET); /* very low success probability */
      prob+=chance(ch, vict, 0)*3; /* Bonuses are three times as important as how well you're learned in that skill */
      if (percent > prob) {
         act("You try to switch target, but your current opponent keep you busy!", FALSE, ch, 0, 0, TO_CHAR);
         act("$n makes some desperate attempts to attack another opponent!", FALSE, ch, 0, 0, TO_ROOM );
         WAIT_STATE (ch, PULSE_VIOLENCE * 2);
         return;
      }
      act("You start fighting $N!", FALSE, ch, 0, vict, TO_CHAR);
      act("$n starts fighting $N!", FALSE, ch, 0, vict, TO_NOTVICT);   
      act("$n starts fighting you!", FALSE, ch, 0, vict, TO_VICT);
      hit(ch, vict, TYPE_UNDEFINED);
      WAIT_STATE (ch, PULSE_VIOLENCE * 2);    
    }
        
}

ACMD (do_backstab)
{
  struct char_data *vict;
  int percent, prob;

  one_argument (argument, buf);

  if (!(vict = get_char_room_vis (ch, buf)))
    {
      send_to_char ("Backstab who?\r\n", ch);
      return;
    }
  if (vict == ch)
    {
      send_to_char ("How can you sneak up on yourself?\r\n", ch);
      return;
    }
  if (!GET_EQ (ch, WEAR_WIELD))
    {
      send_to_char ("You need to wield a weapon to make it a success.\r\n", ch);
      return;
    }
  if (GET_OBJ_VAL (GET_EQ (ch, WEAR_WIELD), 3) != TYPE_PIERCE - TYPE_HIT)
    {
      send_to_char ("Only piercing weapons can be used for backstabbing.\r\n", ch);
      return;
    }
  if (FIGHTING (vict))
    {
      send_to_char ("You can't backstab a fighting person -- they're too alert!\r\n", ch);
      return;
    }

  if (MOB_FLAGGED (vict, MOB_AWARE))
    {
      act ("You notice $N lunging at you!", FALSE, vict, 0, ch, TO_CHAR);
      act ("$e notices you lunging at $m!", FALSE, vict, 0, ch, TO_VICT);
      act ("$n notices $N lunging at $m!", FALSE, vict, 0, ch, TO_NOTVICT);
      hit (vict, ch, TYPE_UNDEFINED);
      return;
    }

  percent = number(1, 101);
  if (IS_NPC(ch))
     prob = 90;
   else
     prob = GET_SKILL(ch, SKILL_BACKSTAB);

  if (AWAKE (vict) && (percent > prob))
    damage (ch, vict, 0, SKILL_BACKSTAB);
  else
    hit (ch, vict, SKILL_BACKSTAB);

  WAIT_STATE(ch, PULSE_VIOLENCE * 2)
}



ACMD (do_order)
{
  char name[100], message[256];
  char buf[256];
  bool found = FALSE;
  int org_room;
  struct char_data *vict;
  struct follow_type *k;

  half_chop (argument, name, message);

  if (!*name || !*message)
    send_to_char ("Order who to do what?\r\n", ch);
  else if (!(vict = get_char_room_vis (ch, name)) && !is_abbrev (name, "followers"))
    send_to_char ("That person isn't here.\r\n", ch);
  else if (ch == vict)
    send_to_char ("You obviously suffer from skitzofrenia.\r\n", ch);

  else
    {
      if (IS_AFFECTED (ch, AFF_CHARM))
	{
	  send_to_char ("Your superior would not aprove of you giving orders.\r\n", ch);
	  return;
	}
      if (vict)
	{
	  sprintf (buf, "$N orders you to '%s'", message);
	  act (buf, FALSE, vict, 0, ch, TO_CHAR);
	  act ("$n gives $N an order.", FALSE, ch, 0, vict, TO_ROOM);

	  if ((vict->master != ch) || !IS_AFFECTED (vict, AFF_CHARM))
	    act ("$n has an indifferent look.", FALSE, vict, 0, 0, TO_ROOM);
	  else
	    {
	      send_to_char (OK, ch);
	      command_interpreter (vict, message);
	    }
	}
      else
	{			/* This is order "followers" */
	  sprintf (buf, "$n issues the order '%s'.", message);
	  act (buf, FALSE, ch, 0, vict, TO_ROOM);

	  org_room = ch->in_room;

	  for (k = ch->followers; k; k = k->next)
	    {
	      if (org_room == k->follower->in_room)
		if (IS_AFFECTED (k->follower, AFF_CHARM))
		  {
		    found = TRUE;
		    command_interpreter (k->follower, message);
		  }
	    }
	  if (found)
	    send_to_char (OK, ch);
	  else
	    send_to_char ("Nobody here is a loyal subject of yours!\r\n", ch);
	}
    }
}



ACMD (do_flee)
{
  int i, attempt, loss, minlevel;
  struct char_data *was_fighting;
  extern char *numdisplay(int number);

  if (GET_POS (ch) < POS_FIGHTING)
    {
      send_to_char ("You are in pretty bad shape, unable to flee!\r\n", ch);
      return;
    }

  was_fighting = FIGHTING(ch);

  if (IS_ARENACOMBATANT(ch))
    minlevel= 1;
  else
    minlevel = 15;

  for (i = 0; i < 6; i++)
    {
      attempt = number (0, NUM_OF_DIRS - 1);	/* Select a random direction */
      if (CAN_GO (ch, attempt) &&
	  !IS_SET (ROOM_FLAGS (EXIT (ch, attempt)->to_room), ROOM_DEATH))
	{
	  act ("$n panics, and attempts to flee!", TRUE, ch, 0, 0, TO_ROOM);
	  if (do_simple_move (ch, attempt, TRUE))
	    {
	      send_to_char ("You flee head over heels.\r\n", ch);
	      if (was_fighting && !IS_NPC(ch) && GET_LEVEL(ch) >= minlevel) {
		loss = GET_MAX_HIT(was_fighting) - GET_HIT(was_fighting);
		loss *= GET_LEVEL(was_fighting);
		if (!IS_NPC(ch) 
		    && IS_ARENACOMBATANT(ch)){
		  sprintf(buf, "&RYou would have lost %s experience points "
			  "for fleeing if this wasn't an arena.&n\r\n", numdisplay(loss));
		  send_to_char(buf, ch);
		  LASTFIGHTING(ch) = was_fighting;
		  GET_ARENAFLEETIMER(ch) = 1; /* start flee timer */
		  send_to_char("Starting Flee-Recall timer. If you recall before the timer expires, you concede the match!\r\n", ch);
		  /* match_over(was_fighting, ch, "(Fled)", 0);
		  if (GET_ARENASTAT(ch) == ARENA_COMBATANTZ)
		    trans_to_preproom(ch);
		    */
		}else{
		  gain_exp(ch, -loss);
		  sprintf(buf, "&RYou lost %s experience points for "
			  "fleeing!&n\r\n", numdisplay(loss));
		  send_to_char(buf, ch);
		}
	      }
	    }
	  else
	    {
	      act ("$n tries to flee, but can't!", TRUE, ch, 0, 0, TO_ROOM);
	    }
	  return;
	}
    }
  send_to_char ("PANIC!  You couldn't escape!\r\n", ch);
}

ACMD (do_chain_footing)
{
  struct char_data *vict;
  int percent, prob;
  struct affected_type *h;
   
  one_argument (argument, arg);

  if (GET_CLASS (ch) != CLASS_WARRIOR && GET_LEVEL(ch) < LVL_IMMORT)
    {
      send_to_char ("You'd better leave all the martial arts to fighters.\r\n", ch);
      return;
    }
  if (!(vict = get_char_room_vis (ch, arg)))
    {
      if (FIGHTING (ch))
        vict = FIGHTING (ch);
      else {
	  send_to_char ("Chain who's feet?\r\n", ch);
	  return;
      }
    }
  if (FIGHTING(ch) && FIGHTING(ch)!=vict) {
    send_to_char ("You're too busy fighting to throw yourself at ANOTHER person!\r\n", ch);
    return;
  }
  if (vict == ch)
    {
      send_to_char ("You chain your own feet! Wait... AHHHHHHh\r\n", ch);
      return;
    }
  
  if (ROOM_FLAGGED(ch->in_room, ROOM_PEACEFUL)) {
    send_to_char("This room just has such a peaceful, easy feeling...\r\n", ch);
    return;
  }

  percent = number(1, 401);
  if (IS_NPC(ch))
     prob = 90;
   else
     prob = GET_SKILL(ch, SKILL_CHAIN_FOOTING);
  prob+=chance(ch, vict, 0)*3;

  for (h = vict->affected; h; h = h->next) {
    if (h->type == SKILL_CHAIN_FOOTING) {
      send_to_char ("Whoops, they're already chained. Doh.\r\n", ch);
      return;
    }
    if (!(h->next)) break;
  }

  if (GET_MOVE(ch) < 25 && GET_LEVEL(ch) < LVL_IMMORT) {
    send_to_char("You're too exhausted for such action!\r\n", ch);
    return;
  }
  else if (GET_LEVEL(ch) < LVL_IMMORT)
    GET_MOVE(ch)-=25;

  if (percent > prob)
    {
      act ("$N lunges at you, but you step aside and laugh at $e.", FALSE, vict, 0, ch, TO_CHAR);
      act ("$n lunges at $N, but gets a face full of dirt instead!", FALSE, ch, 0, vict, TO_NOTVICT);
      act ("You lunge at $N, but get a face full of dirt instead!", FALSE, ch, 0, vict, TO_CHAR);
      GET_POS (ch) = POS_SITTING;
    }
  else
    {
      act ("$N lunges at you and skillfully chains your feet together!", FALSE, vict, 0, ch, TO_CHAR);
      act ("$n lunges at $N and skillfully chains $S feet together!", FALSE, ch, 0, vict, TO_NOTVICT);
      act ("You lunge at $N and skillfully chain $S feet together!", FALSE, ch, 0, vict, TO_CHAR);
      if (h) {
        CREATE(h->next, struct affected_type, 1);
        h=h->next;
      }
      else {
        CREATE(h, struct affected_type, 1);
        vict->affected=h;
      }
      h->duration = 2;
      h->bitvector = AFF_CHAINED;
      h->type = SKILL_CHAIN_FOOTING;
      SET_BIT(AFF_FLAGS(vict), AFF_CHAINED);
    }
  if (!FIGHTING(ch))
  set_fighting (ch, vict);
  WAIT_STATE (ch, PULSE_VIOLENCE * 2);
}

ACMD (do_bash)
{
  struct char_data *vict;
  int percent, prob;

  one_argument (argument, arg);

  if (GET_CLASS (ch) != CLASS_WARRIOR && GET_LEVEL(ch) < LVL_IMMORT)
    {
      send_to_char ("You'd better leave all the martial arts to fighters.\r\n", ch);
      return;
    }
  if (!(vict = get_char_room_vis (ch, arg)))
    {
      if (FIGHTING (ch))
	{
	  vict = FIGHTING (ch);
	}
      else
	{
	  send_to_char ("Bash who?\r\n", ch);
	  return;
	}
    }
  if (vict == ch)
    {
      send_to_char ("Aren't we funny today...\r\n", ch);
      return;
    }
  if (!GET_EQ (ch, WEAR_WIELD))
    {
      send_to_char ("You need to wield a weapon to make it a success.\r\n", ch);
      return;
    }
  
  if (ROOM_FLAGGED(ch->in_room, ROOM_PEACEFUL)) {
  send_to_char("This room just has such a peaceful, easy feeling...\r\n", ch);
  return;
  }

  percent = number(1, 401);
  if (IS_NPC(ch))
     prob = 90;
   else
     prob = GET_SKILL(ch, SKILL_BASH);
  prob+=chance(ch, vict, 0)*3;

  if (MOB_FLAGGED (vict, MOB_NOBASH))
    percent = 401;

  if (percent > prob)
    {
      damage (ch, vict, 0, SKILL_BASH);
      GET_POS (ch) = POS_SITTING;
    }
  else
    {
      damage (ch, vict, 1, SKILL_BASH);
      GET_POS (vict) = POS_SITTING;
      WAIT_STATE (vict, PULSE_VIOLENCE);
    }
  WAIT_STATE (ch, PULSE_VIOLENCE * 2);
}


ACMD (do_rescue)
{
  struct char_data *vict, *tmp_ch;
  int percent, prob;

  one_argument (argument, arg);

  if (!(vict = get_char_room_vis (ch, arg)))
    {
      send_to_char ("Whom do you want to rescue?\r\n", ch);
      return;
    }
  if (vict == ch)
    {
      send_to_char ("What about fleeing instead?\r\n", ch);
      return;
    }
  if (FIGHTING (ch) == vict)
    {
      send_to_char ("How can you rescue someone you are trying to kill?\r\n", ch);
      return;
    }
  for (tmp_ch = world[ch->in_room].people; tmp_ch &&
       (FIGHTING (tmp_ch) != vict); tmp_ch = tmp_ch->next_in_room);

  if (!tmp_ch)
    {
      act ("But nobody is fighting $M!", FALSE, ch, 0, vict, TO_CHAR);
      return;
    }
  if (GET_CLASS (ch) != CLASS_WARRIOR && GET_LEVEL(ch) < LVL_IMMORT)
    send_to_char ("But only true warriors can do this!", ch);
  else
    {
      percent = number (1, 101);	/* 101% is a complete failure */
      prob = GET_SKILL (ch, SKILL_RESCUE);

      if (percent > prob)
	{
	  send_to_char ("You fail the rescue!\r\n", ch);
	  return;
	}
      send_to_char ("Banzai!!  To the rescue!!!\r\n", ch);
      act ("You are rescued by $N, leaving you confused.", FALSE, vict, 0, ch, TO_CHAR);
      act ("$n heroically rescues $N!", FALSE, ch, 0, vict, TO_NOTVICT);

      if (FIGHTING (vict) == tmp_ch)
	stop_fighting (vict);
      if (FIGHTING (tmp_ch))
	stop_fighting (tmp_ch);
      if (FIGHTING (ch))
	stop_fighting (ch);

      set_fighting (ch, tmp_ch);
      set_fighting (tmp_ch, ch);

      WAIT_STATE (vict, 2 * PULSE_VIOLENCE);
    }

}



ACMD (do_kick)
{
  struct char_data *vict;
  int percent, prob;

  if (GET_CLASS (ch) != CLASS_WARRIOR && GET_LEVEL(ch) < LVL_IMMORT)
    {
      send_to_char ("You'd better leave all the martial arts to fighters.\r\n", ch);
      return;
    }
  one_argument (argument, arg);

  if (!(vict = get_char_room_vis (ch, arg)))
    {
      if (FIGHTING (ch))
	{
	  vict = FIGHTING (ch);
	}
      else
	{
	  send_to_char ("Kick who?\r\n", ch);
	  return;
	}
    }
  if (vict == ch)
    {
      send_to_char ("Aren't we funny today...\r\n", ch);
      return;
    }
  percent = number(1, 401);
  if (IS_NPC(ch))
     prob = 90;
   else
     prob = GET_SKILL(ch, SKILL_KICK);
  prob+=chance(ch, vict, 0)*3;

  if (percent > prob)
    {
      damage (ch, vict, 0, SKILL_KICK);
    }
  else
    damage (ch, vict, GET_LEVEL (ch) >> 1, SKILL_KICK);
  WAIT_STATE (ch, PULSE_VIOLENCE * 3);
}

ACMD (do_berserk)
{
  struct char_data *vict;
  int percent, prob, dmg, multiplier;
  float factor;
  struct obj_data *wielded = GET_EQ (ch, WEAR_WIELD);

  if ((GET_CLASS (ch) != CLASS_WARRIOR) && (GET_LEVEL(ch) < LVL_IMMORT))
    {
      send_to_char ("But only true warriors can do this!", ch);
      return;
    }
  if (GET_MOVE(ch) < 50)
    {
      send_to_char ("But you're out of breath!", ch);
      return;
    }

  one_argument (argument, arg);

  if (!(vict = get_char_room_vis (ch, arg)))
    {
      if (FIGHTING (ch))
	{
	  vict = FIGHTING (ch);
	}
      else
	{
	  send_to_char ("Berserk attack who?\r\n", ch);
	  return;
	}
    }
  if (vict == ch)
    {
      send_to_char ("You can't possibly be that insane???\r\n", ch);
      return;
    } 

  GET_MOVE(ch) -= 50;
  /* 101% is a complete failure */
  prob = GET_SKILL(ch, SKILL_BERSERK);
  percent = number((int)(prob * 0.4), 101) 
    + (GET_LEVEL(vict)/number(6, 12))
    - (GET_LEVEL(ch)/number(4, 10));

  /* prob is also a factor of hp/maxhp */
  factor = ((float)GET_HIT(ch))/((float)GET_MAX_HIT(ch));  
  if (factor < 0.6)
    factor = 0.6; /* lowest factor = 0.6 */
  prob = (int) (prob*factor);

  if (percent < 0)
    percent = 0;

  dmg = 1;
  if (wielded && GET_OBJ_TYPE (wielded) == ITEM_WEAPON){
    /* Add weapon-based damage if a weapon is being wielded */
    dmg += dice (GET_OBJ_VAL (wielded, 1), GET_OBJ_VAL (wielded, 2));
  }

  multiplier = number (2,5); 
  dmg *= multiplier;
  if (dmg > 200)
    dmg = 250;


  /*
  sprintf(buf, "%s: prob %d (factor %g) percent %d - Berserk %s."
	  " Berserk dmg %d (multiplier %d), normal dmg %d\r\n",
	  GET_NAME(ch), prob, factor, percent,
	  (percent>prob)?"FAILED":"SUCCEEDED",
	  dmg, multiplier, (int)(dmg/multiplier));
  log(buf);
  */

  if (percent > prob){
    damage (ch, vict, 0, SKILL_BERSERK);
    WAIT_STATE (ch, PULSE_VIOLENCE * 4);
  }else{
    damage (ch, vict, dmg, SKILL_BERSERK);
    WAIT_STATE (ch, PULSE_VIOLENCE * 2);
  }
}

/* Remove the victim's weapon */
ACMD(do_disarm)
{
  struct obj_data *obj;   
  struct char_data *vict;
  int percent, prob;
      
  one_argument(argument, arg);
   
  if(!ch)
    return;
    
  /* Who are we trying to disarm? */
  if (!(vict = get_char_room_vis(ch, arg))) {
    if (FIGHTING(ch)) {
      vict = FIGHTING(ch);
    } else {
      send_to_char("Disarm who?\r\n", ch);
      return;
    }
  }
  
  /* Can't order charmed mobs to disarm. */
  if (IS_NPC(ch) && IS_AFFECTED(ch, AFF_CHARM)) {
    send_to_char("You'd better leave this skill to players.\r\n", ch);
    return;
    
  } else
    /* Doesn't know how */
    if (   !IS_NPC(ch)
        && !GET_SKILL(ch, SKILL_DISARM)) {
     
      /* If the disarmer is wielding, then he loses his weapon */
      if(ch->equipment[WEAR_WIELD]) {
        obj=unequip_char(ch, WEAR_WIELD);  
        obj_to_room(obj, ch->in_room);
        act("While trying to disarm $N, you lose your own weapon.",
            TRUE, ch, 0, vict, TO_CHAR);
        act("While trying to disarm you, $n loses $s own weapon.",
            TRUE, ch, 0, vict, TO_VICT);
        act("While trying to disarm $N, $n loses $s own weapon.",
            TRUE, ch, 0, vict, TO_NOTVICT);
      } else
        /* If he's not wielding, he just gets tooled on */
        send_to_char("You dont know of that skill.\r\n", ch);
     
      return;
  }
        
  /* You are your own victim? */
  if (vict == ch) {
    send_to_char("Aren't we funny today...\r\n", ch);
    return;
  }
        
  /* Victim isn't wielding... */
  if (!vict->equipment[WEAR_WIELD]) {
    send_to_char("Disarm what weapon?!\r\n", ch);
    return;
  }  
      
  /* Can't disarm while whomping. */
  /* if(CHECK_FIGHT(ch) != 0) {
    send_to_char("You are too engaged in combat to disarm now!\r\n", ch);
    return;
  }*/
        
  percent = number(1, 401);
  if (IS_NPC(ch))
     prob = 90;
   else
     prob = GET_SKILL(ch, SKILL_DISARM);
  prob+=chance(ch, vict, 0)*3;
    
  if (   percent > prob
      && GET_POS(vict) > POS_SLEEPING) {
    act("$n tries to disarm $N but fails.", TRUE, ch, 0, vict, TO_NOTVICT);
    act("You try to disarm $N but fail.", TRUE, ch, 0, vict, TO_CHAR);
    act("$n tries to disarm you but fails.", TRUE, ch, 0, vict, TO_VICT);
    if (!FIGHTING(vict))
    hit(vict, ch, TYPE_UNDEFINED);   
  } else {
    act("$n makes $N drop his weapon to the ground with some fast moves.", 
        TRUE, ch, 0, vict, TO_NOTVICT);
    act("With some fast moves you manage to make $N drop the weapon.",
        TRUE, ch, 0, vict, TO_CHAR);
    act("$n performs some fast moves, and makes you drop your weapon.",
        TRUE, ch, 0, vict, TO_VICT);
    obj = unequip_char(vict, WEAR_WIELD);
    obj_to_room(obj, vict->in_room);
    if (!FIGHTING(vict))
    hit(vict, ch, TYPE_UNDEFINED);
  }
    
  WAIT_STATE(ch, PULSE_VIOLENCE);
}

ACMD(do_trip)
{       
  struct char_data *vict;
  int percent, prob;
    
  one_argument(argument, arg);
        
  if (IS_NPC(ch) && IS_AFFECTED(ch, AFF_CHARM)) {
    send_to_char("You'd better leave this skill to players.\r\n", ch);
    return;
  } else if (!IS_NPC(ch) && !GET_SKILL(ch, SKILL_TRIP)) {
    send_to_char("You dont know of that skill.\r\n", ch);
    return;
  }

  if (!(vict = get_char_room_vis(ch, arg))) {
    if (FIGHTING(ch)) {  
      vict = FIGHTING(ch);
    } else {
      send_to_char("Trip who?\r\n", ch);
      return;
    }
  }
  if (vict == ch) {
    send_to_char("Aren't we funny today...\r\n", ch);
    return;
  }

  if (IS_NPC(vict) && MOB_FLAGGED(vict, MOB_NOBASH)) {
    act("You try to trip $N but can't.", TRUE, ch, 0, vict, TO_CHAR);
    act("$n tries to trip you, but you are unmovable.", TRUE, ch, 0, vict, TO_VICT);
    act("$n tries to trip $N, but can't.", TRUE, ch, 0, vict, TO_NOTVICT);
    return;
     }
  percent = number(1, 501);
  if (IS_NPC(ch))
     prob = 90;
   else
     prob = GET_SKILL(ch, SKILL_TRIP);
  prob+=chance(ch, vict, 0)*3;
    
  if (percent > prob) {
    damage(ch, vict, 0, SKILL_TRIP);
  } else {
    damage(ch, vict, 2, SKILL_TRIP);
    WAIT_STATE(vict, PULSE_VIOLENCE * 2);
    WAIT_STATE(ch, PULSE_VIOLENCE);
    return;
  }
  WAIT_STATE(ch, PULSE_VIOLENCE * 2);
}

ACMD (do_camouflage)
{
  byte percent, prob;
  struct char_data *vict;
  float factor;

  if ((GET_CLASS(ch) != CLASS_THIEF || GET_SKILL(ch, SKILL_CAMOUFLAGE) <= 0)
      && GET_LEVEL(ch) < LVL_IMMORT){
    send_to_char("You don't seem to have the camouflage ability.\r\n", ch);
    return;
  }

  if (GET_POS (ch) != POS_FIGHTING){
    send_to_char("You can only camouflage yourself when fighting.\r\n", ch);
    return;
  }

  if (GET_MOVE(ch) < 25)
    {
      send_to_char ("You're too tired to do that!", ch);
      return;
    }

  send_to_char ("You attempt to camouflage yourself with your surroundings.\r\n", ch);
  GET_MOVE(ch) -= 25;

  if (IS_AFFECTED (ch, AFF_HIDE) && !check_perm_duration(ch, AFF_HIDE))
    REMOVE_BIT (AFF_FLAGS (ch), AFF_HIDE);

  /* 101% is a complete failure */
  prob = GET_SKILL (ch, SKILL_CAMOUFLAGE) + dex_app_skill[GET_DEX (ch)].hide;
  percent = number ((int)(prob*0.6), 101) - (GET_LEVEL(ch)/number(10, 15));
  if (percent <= 0)
    percent = 0;

  /* prob is also a factor of hp/maxhp */
  factor = ((float)GET_HIT(ch))/((float)GET_MAX_HIT(ch));  
  if (factor < 0.6)
    factor = 0.6; /* lowest factor = 0.6 */
  prob = (int) (prob*factor);
  
  /*
  sprintf(buf, "%s: prob %d (factor %g) percent %d - Camouflage %s\r\n",
	  GET_NAME(ch), prob, factor, percent, 
	  (percent>prob)?"FAILED":"SUCCEEDED");
  log(buf);
  */

  if (percent > prob)
    return;

  sprintf(buf, "%s has disappeared from view!\r\n", GET_NAME(ch));
  act (buf, TRUE, ch, 0, 0, TO_ROOM);
  if (FIGHTING (ch)){
    vict = FIGHTING (ch);
    stop_fighting (vict);
    stop_fighting (ch);
    sprintf(buf, "%s is befuddled, having lost sight of his target.\r\n", 
	    GET_NAME(vict));
    send_to_room (buf , ch->in_room);
  }
  SET_BIT (AFF_FLAGS (ch), AFF_HIDE);
  WAIT_STATE(ch, PULSE_VIOLENCE*2);
}

ACMD (do_blanket)
{
  byte percent, prob;
  float factor;
  struct char_data *vict;
  struct char_data *k;
  struct follow_type *f;
  int grpsize = 0;
  int befuddled = 1;

  if ((GET_CLASS(ch) != CLASS_THIEF || GET_SKILL(ch, SKILL_BLANKET) <= 0)
      && GET_LEVEL(ch) < LVL_IMMORT){
    send_to_char("You don't seem to have the blanket camouflage ability.\r\n", ch);
    return;
  }

  if (GET_POS (ch) != POS_FIGHTING){
    send_to_char("You can only blanket comouflage your group when fighting.\r\n", ch);
    return;
  }

  if (GET_MOVE(ch) < 25)
    {
      send_to_char ("You're too tired to do that!", ch);
      return;
    }

  send_to_char ("You attempt to blanket camouflage your group.\r\n", ch);
  GET_MOVE(ch) -= 25;

  if (ch->master)
    k = ch->master;
  else
    k = ch;

  /* First un-hide everyone in the group */
  if ((get_char_room (GET_NAME(k), ch->in_room) != NULL) | (k == ch)){
    if (IS_AFFECTED (k, AFF_HIDE) && !check_perm_duration(k, AFF_HIDE))
      REMOVE_BIT (AFF_FLAGS (k), AFF_HIDE);
  }
  for (f = k->followers; f; f = f->next)
    if (IS_AFFECTED (f->follower, AFF_GROUP)){
      if (get_char_room (GET_NAME(f->follower), ch->in_room) != NULL){	
	grpsize++;
	if (IS_AFFECTED (f->follower, AFF_HIDE) && !check_perm_duration(f->follower, AFF_HIDE))
	  REMOVE_BIT (AFF_FLAGS(f->follower), AFF_HIDE);
      }
    }

  /* 101% is a complete failure */
  prob = GET_SKILL (ch, SKILL_CAMOUFLAGE) 
    + dex_app_skill[GET_DEX (ch)].hide - grpsize;
  percent = number ((int)(prob*0.6), 101) - (GET_LEVEL(ch)/number(10, 15));
  if (percent <= 0)
    percent = 0;

  /* prob is also a factor of hp/maxhp */
  factor = ((float)GET_HIT(ch))/((float)GET_MAX_HIT(ch));  
  if (factor < 0.55)
    factor = 0.55; /* lowest factor = 0.55 */
  prob = (int) (prob*factor);

  /*
  sprintf(buf, "%s: prob %d (factor %g) percent %d - Blanket %s\r\n",
	  GET_NAME(ch), prob, factor, percent, 
	  (percent>prob)?"FAILED":"SUCCEEDED");
  log(buf);
  */

  if (percent > prob)
    return;

  /* Now blanket is succesful, so everyone in the group is hidden */
  if ((get_char_room (GET_NAME(k), ch->in_room) != NULL) | (k == ch)){
    sprintf(buf, "%s has disappeared from view!\r\n", GET_NAME(k));
    act (buf, TRUE, k, 0, 0, TO_ROOM);
    if (FIGHTING (k)){
      vict = FIGHTING (k);
      stop_fighting (vict);
      stop_fighting (k);
      if (!befuddled){
	sprintf(buf, "%s is befuddled, having lost sight of his target.\r\n", 
		GET_NAME(vict));
	send_to_room (buf , ch->in_room);
	befuddled = 1;
      }
    }
    SET_BIT (AFF_FLAGS (k), AFF_HIDE);
  }

  for (f = k->followers; f; f = f->next)
    if (IS_AFFECTED (f->follower, AFF_GROUP)){
      if (get_char_room (GET_NAME(f->follower), ch->in_room) != NULL){	
	if (FIGHTING (f->follower)){
	  vict = FIGHTING (f->follower);
	  stop_fighting (vict);
	  stop_fighting (f->follower);
	  sprintf(buf, "%s has disappeared from view!\r\n", GET_NAME(f->follower));
	  act (buf, TRUE, f->follower, 0, 0, TO_ROOM);
	  if (!befuddled){
	    sprintf(buf, "%s is befuddled, having lost sight of his target.\r\n", 
		    GET_NAME(vict));
	    send_to_room (buf , ch->in_room);
	    befuddled = 1;
	  }
	}
	SET_BIT (AFF_FLAGS (f->follower), AFF_HIDE);
      }
    }
  WAIT_STATE(ch, PULSE_VIOLENCE*2);
}

