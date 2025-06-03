/* ************************************************************************
   *   File: arena.c                                                         *
   *  Usage: DeltaMUD Arena                                                  *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1998 By Michael "Mulder" Fara, Kurt "Frak" Hopfer        *
   *                        and Lance "Thargor"                              *
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
#include "screen.h"

/* extern variables */
extern int arena_zone;
extern int arena_preproom;
extern int arena_observeroom;
extern int arena_combatant;
extern int arena_observer;
extern struct char_data *arenamaster;
extern struct char_data *defaultobserve;

extern struct room_data *world;
extern struct descriptor_data *descriptor_list;
extern struct char_data *character_list;

/* extern functions */
void deobserve(struct char_data *);
void clearobservers(struct char_data*);
void linkobserve(struct char_data *who, struct char_data *to);
ACMD(do_tell);
ACMD(do_action);
ACMD (do_gen_comm);

void inc_matchcount(struct char_data *ch)
{
  if (GET_ARENASTAT(ch) == ARENA_COMBATANT1){
    GET_ARENASTAT(ch) = ARENA_COMBATANT2;
    send_to_char ("\r\nYou've used up one of your arena matches. "
		  "Two left.\r\n\r\n", ch);
  } else if (GET_ARENASTAT(ch) == ARENA_COMBATANT1w){
    GET_ARENASTAT(ch) = ARENA_COMBATANT2;
    send_to_char ("\r\nYou've used up one of your arena matches. "
		  "One left.\r\n\r\n", ch);
  } else if (GET_ARENASTAT(ch) == ARENA_COMBATANT2){
    GET_ARENASTAT(ch) = ARENA_COMBATANT3;
    send_to_char ("\r\nYou've used up two of your arena matches. "
		  "One left.\r\n\r\n", ch);
  } else if (GET_ARENASTAT(ch) == ARENA_COMBATANT3){
    GET_ARENASTAT(ch) = ARENA_COMBATANTZ;
    send_to_char ("\r\nYou've used up all three of your arena matches!\r\n"
		  "Thank you. Come again.\r\n\r\n", ch);
  } else {
    mudlog ("DEBUG: arena combatant but not flagged as such?", 
	    BRF, LVL_IMMORT, TRUE);
    send_to_char ("Hmmm, your arena matches are screwed!\r\n", ch);
  }	  	  

}

void trans_to_preproom(struct char_data *ch)
{
    GET_HIT(ch) = 1;
    char_from_room (ch);
    char_to_room (ch, real_room(arena_preproom));
    act ("$n has entered the Arena Prep Room.", FALSE, 
	 ch, 0, 0, TO_NOTVICT);
    look_at_room (ch, 0);
}

void match_over(struct char_data *winner, struct char_data *loser, 
		char *msg, int loser_to_preproom)
{
  int winnings;

  if (winner == NULL)
    return;

  if (loser == NULL)
    return;

  if (IS_NPC(winner) || IS_NPC(loser))
    return;

  if (!IS_ARENACOMBATANT(winner)){
      sprintf (buf2, "DEBUG: match_over called but %s (winner) is not flagged an arena combatant?!\r\n", GET_NAME(winner));
      mudlog (buf2, BRF, LVL_GRGOD, TRUE);
      return;
  }
  if (!IS_ARENACOMBATANT(loser)){
      sprintf (buf2, "DEBUG: match_over called but %s (loser) is not flagged an arena combatant?!\r\n", GET_NAME(loser));
      mudlog (buf2, BRF, LVL_GRGOD, TRUE);
      return;
  }

  winnings = (int) (GET_LEVEL(loser) * arena_combatant * 
		    number(5,15) * 0.1); 
  
  act ("$n has WON this match!", FALSE, winner, 0, 0, TO_NOTVICT);
  sprintf(buf2,"\r\n&RYou are victorious!!! "
	  "You have been rewarded %d coins for winning.&n\r\n\r\n",
	  winnings);
  send_to_char (buf2, winner);
  GET_GOLD(winner) += winnings;
  if (GET_ARENAWINS(winner) < 254)
    GET_ARENAWINS(winner) += 1;
  GET_ARENAFLEETIMER(winner) = 0;

  act ("$n has lost this match!", FALSE, loser, 0, 0, TO_NOTVICT);
  send_to_char ("\r\n&RYou have lost the match!  Sorry...&n\r\n\r\n",
			  loser);
  
  if (GET_ARENALOSSES(loser) < 254)
    GET_ARENALOSSES(loser) += 1;
  GET_ARENAFLEETIMER(loser) = 0;

  if (FIGHTING(winner))
    stop_fighting(winner);
  if (FIGHTING(loser))
    stop_fighting(FIGHTING(loser));


  GET_POS(loser) = POS_STANDING;

  if (GET_ARENASTAT(winner) == ARENA_COMBATANT1)
    GET_ARENASTAT(winner) = ARENA_COMBATANT1w;

  inc_matchcount(loser);

  sprintf(buf2, "%s has won a match against %s! %s", 
	  GET_NAME(winner), GET_NAME(loser), msg);
  do_gen_comm (arenamaster, buf2, 1, SCMD_ARENA);

  log(buf2);

  if (loser_to_preproom == TRUE)
    trans_to_preproom(loser);

}

void bup_affects(struct char_data *ch)
{
  if (IS_NPC(ch))
    return;

  BUP_WIMP_LEV(ch) = GET_WIMP_LEV(ch);
  GET_WIMP_LEV(ch) = 0;
  BUP_RECALL_LEV(ch) = GET_RECALL_LEV(ch);  
  GET_RECALL_LEV(ch) = 0;

  BUP_AFF_FLAGS(ch) = AFF_FLAGS(ch);
  BUP_AFFECTED(ch) = ch->affected;
  AFF_FLAGS(ch) = 0;
  ch->affected = NULL;
}

void restore_bup_affects(struct char_data *ch)
{
  if (IS_NPC(ch))
    return;

  /* First clearoff the arena affects */
  while (ch->affected)
    affect_remove(ch, ch->affected);

  /* Now restore the backed up one */
  AFF_FLAGS(ch) = BUP_AFF_FLAGS(ch);
  ch->affected = BUP_AFFECTED(ch);

  GET_WIMP_LEV(ch) = BUP_WIMP_LEV(ch);
  GET_RECALL_LEV(ch) = BUP_RECALL_LEV(ch);  

}
