/* ************************************************************************
   *   File: fight.c                                       Part of CircleMUD *
   *  Usage: Combat system                                                   *
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
#include "handler.h"
#include "interpreter.h"
#include "db.h"
#include "spells.h"
#include "screen.h"
#include "dg_scripts.h"
#include "olc.h"

/* Structures */
extern int arena_preproom;
extern int arena_entrance;
extern struct char_data *arenamaster;
struct char_data *combat_list = NULL;	/* head of l-list of fighting chars */
struct char_data *next_combat_list = NULL;

/* External structures */
extern struct room_data *world;
extern struct message_list fight_messages[MAX_MESSAGES];
extern struct obj_data *object_list;
extern struct index_data *mob_index;	/* LJ autoquest */
extern int pk_allowed;		/* see config.c */
extern int jail_num;		/* see config.c */
extern int bail_multiplier;	/* see config.c */
extern byte pk_victim_min;      /* see config.c */
extern int auto_save;		/* see config.c */
extern int max_exp_gain;	/* see config.c */
extern int max_exp_loss;	/* see config.c */
ACMD(do_use);
void write_aliases(struct char_data *ch);
extern struct str_app_type str_app[];
extern struct dex_app_type dex_app[];

/* External procedures */
char *numdisplay(int);
void match_over(struct char_data *, struct char_data *, char *, int);
void deobserve(struct char_data *);
void clearobservers(struct char_data *);
void inc_matchcount(struct char_data *);
char *fread_action (FILE * fl, int nr);
char *fread_string (FILE * fl, char *error);
void stop_follower (struct char_data *ch);
ACMD (do_flee);
void hit (struct char_data *ch, struct char_data *victim, int type);
void forget (struct char_data *ch, struct char_data *victim);
void remember (struct char_data *ch, struct char_data *victim);
void diag_char_to_char (struct char_data *i, struct char_data *ch);
int ok_damage_shopkeeper (struct char_data *ch, struct char_data *victim);
int estimate_difficulty(struct char_data *ch, struct char_data *victim);
int exp_to_level(int arg);
ACMD (do_gen_comm);

/* Weapon attack texts */
struct attack_hit_type attack_hit_text[] =
{
  {"hit", "hits"},		/* 0 */
  {"sting", "stings"},
  {"whip", "whips"},
  {"slash", "slashes"},
  {"bite", "bites"},
  {"bludgeon", "bludgeons"},	/* 5 */
  {"crush", "crushes"},
  {"pound", "pounds"},
  {"claw", "claws"},
  {"maul", "mauls"},
  {"thrash", "thrashes"},	/* 10 */
  {"pierce", "pierces"},
  {"blast", "blasts"},
  {"punch", "punches"},
  {"stab", "stabs"}
};

#define IS_WEAPON(type) (((type) >= TYPE_HIT) && ((type) < TYPE_STARVING))

/* The Fight related routines */

void 
appear (struct char_data *ch)
{
  extern bool check_perm_duration(struct char_data *ch, long bitvector);
  if (!check_perm_duration(ch, AFF_INVISIBLE)) {
    if (affected_by_spell (ch, SPELL_INVISIBLE))
      affect_from_char (ch, SPELL_INVISIBLE);
    REMOVE_BIT (AFF_FLAGS (ch), AFF_INVISIBLE);
  }
  if (!check_perm_duration(ch, AFF_HIDE))
    REMOVE_BIT (AFF_FLAGS (ch), AFF_HIDE);

  if (!IS_SET(AFF_FLAGS(ch), AFF_HIDE) && !IS_SET(AFF_FLAGS(ch), AFF_INVISIBLE)) {
    if (GET_LEVEL (ch) < LVL_IMMORT)
      act ("$n slowly fades into existence.", FALSE, ch, 0, 0, TO_ROOM);
    else
      act ("You feel a strange presence as $n appears, seemingly from nowhere.",
	 FALSE, ch, 0, 0, TO_ROOM);
  }
}



void 
load_messages (void)
{
  FILE *fl;
  int i, type=0;
  struct message_type *messages;
  char chk[128];

  if (!(fl = fopen (MESS_FILE, "r")))
    {
      sprintf (buf2, "SYSERR: Error reading combat message file %s", MESS_FILE);
      perror (buf2);
      exit (1);
    }
  for (i = 0; i < MAX_MESSAGES; i++)
    {
      fight_messages[i].a_type = 0;
      fight_messages[i].number_of_attacks = 0;
      fight_messages[i].msg = 0;
    }


  fgets (chk, 128, fl);
  while (!feof (fl) && (*chk == '\n' || *chk == '*'))
    fgets (chk, 128, fl);

  while (*chk == 'M')
    {
      fgets (chk, 128, fl);
      sscanf (chk, " %[^\n]\n", buf);
      type=0;
        for (i=0; i < NUM_ATTACK_TYPES; i++)
          if (is_abbrev(buf, attack_hit_text[i].singular)) {
            type=i+1100;
            break;
          }
        if (!type)
          if ((type = find_skill_num (buf))==-1) {
            for (i=0; i<=12; i++) fgets (chk, 128, fl);
            while (!feof (fl) && (*chk == '\n' || *chk == '*'))
  	      fgets (chk, 128, fl);
            continue;
          }
      for (i = 0; (i < MAX_MESSAGES) && (fight_messages[i].a_type != type) &&
	   (fight_messages[i].a_type); i++);
      if (i >= MAX_MESSAGES)
	{
	  fprintf (logfile, "SYSERR: Too many combat messages.  Increase MAX_MESSAGES and recompile.");
	  exit (1);
	}
      CREATE (messages, struct message_type, 1);
      fight_messages[i].number_of_attacks++;
      fight_messages[i].a_type = type;
      messages->next = fight_messages[i].msg;
      fight_messages[i].msg = messages;

      messages->die_msg.attacker_msg = fread_action (fl, i);
      messages->die_msg.victim_msg = fread_action (fl, i);
      messages->die_msg.room_msg = fread_action (fl, i);
      messages->miss_msg.attacker_msg = fread_action (fl, i);
      messages->miss_msg.victim_msg = fread_action (fl, i);
      messages->miss_msg.room_msg = fread_action (fl, i);
      messages->hit_msg.attacker_msg = fread_action (fl, i);
      messages->hit_msg.victim_msg = fread_action (fl, i);
      messages->hit_msg.room_msg = fread_action (fl, i);
      messages->god_msg.attacker_msg = fread_action (fl, i);
      messages->god_msg.victim_msg = fread_action (fl, i);
      messages->god_msg.room_msg = fread_action (fl, i);
      fgets (chk, 128, fl);
      while (!feof (fl) && (*chk == '\n' || *chk == '*'))
	fgets (chk, 128, fl);
    }

  fclose (fl);
}

/* add more blood to room (Mulder) */

void increase_blood(int rm)
{
  world[rm].blood = MIN(RM_BLOOD(rm) + 1, 10);
}
void increase_snow(int rm) {
 world[rm].snow = MIN(RM_SNOW(rm) + 1, 10);
}

void 
update_pos (struct char_data *victim)
{
  if ((GET_HIT (victim) > 0) && (GET_POS (victim) > POS_STUNNED))
    return;
  else if (GET_HIT (victim) > 0)
    GET_POS (victim) = POS_STANDING;
  else if (GET_HIT (victim) <= -11)
    GET_POS (victim) = POS_DEAD;
  else if (GET_HIT (victim) <= -6)
    GET_POS (victim) = POS_MORTALLYW;
  else if (GET_HIT (victim) <= -3)
    GET_POS (victim) = POS_INCAP;
  else
    GET_POS (victim) = POS_STUNNED;
}


void 
check_killer (struct char_data *ch, struct char_data *vict)
{

  /* peaceful rooms - but let imps attack */
  if (!PLR_FLAGGED (vict, PLR_KILLER) && !PLR_FLAGGED (vict, PLR_THIEF)
      && !PLR_FLAGGED (ch, PLR_KILLER) && !IS_NPC (ch) && !IS_NPC (vict) &&
      (ch != vict) && IS_JURISDICTED(ch->in_room) && 
      !ROOM_FLAGGED (ch->in_room, ROOM_PEACEFUL))
    {
      char buf[256];

      sprintf (buf, "PC Killer bit set on %s for initiating attack on %s at %s.",
	       GET_NAME (ch), GET_NAME (vict), world[vict->in_room].name);
      if (!PLR_FLAGGED (ch, PLR_THIEF))
	mudlog (buf, BRF, LVL_IMMORT, TRUE);

      SET_BIT (PLR_FLAGS (ch), PLR_KILLER);
      GET_ALIGNMENT (ch) = -1000;

      send_to_char ("This is a jurisdicted area. If you want to be a PLAYER KILLER, so be it...\r\n", ch);
    }
}


/* start one char fighting another (yes, it is horrible, I know... )  */
void 
set_fighting (struct char_data *ch, struct char_data *vict)
{
  if (ch == vict)
    return;

  assert (!FIGHTING (ch));

  ch->next_fighting = combat_list;
  combat_list = ch;

  if (IS_AFFECTED (ch, AFF_SLEEP))
    affect_from_char (ch, SPELL_SLEEP);

  FIGHTING (ch) = vict;
  GET_POS (ch) = POS_FIGHTING;

  if (!pk_allowed)
    check_killer (ch, vict);
}



/* remove a char from the list of fighting chars */
void 
stop_fighting (struct char_data *ch)
{
  struct char_data *temp;

  if (ch == next_combat_list)
    next_combat_list = ch->next_fighting;

  REMOVE_FROM_LIST (ch, combat_list, next_fighting);
  ch->next_fighting = NULL;
  FIGHTING (ch) = NULL;
  GET_POS (ch) = POS_STANDING;
  update_pos (ch);
}



void 
make_corpse (struct char_data *ch)
{
  struct obj_data *corpse, *o;
  struct obj_data *money;
  int i;
  extern int max_npc_corpse_time, max_pc_corpse_time;

  struct obj_data *create_money (int amount);

  corpse = create_obj ();

  corpse->item_number = NOTHING;
  corpse->in_room = NOWHERE;
  corpse->name = str_dup ("corpse");

  sprintf (buf2, "The corpse of %s is lying here.", GET_NAME (ch));
  corpse->description = str_dup (buf2);

  sprintf (buf2, "the corpse of %s", GET_NAME (ch));
  corpse->short_description = str_dup (buf2);

  GET_OBJ_TYPE (corpse) = ITEM_CONTAINER;
  GET_OBJ_WEAR (corpse) = ITEM_WEAR_TAKE;
  GET_OBJ_EXTRA (corpse) = ITEM_NODONATE;
  GET_OBJ_VAL (corpse, 0) = 0;	/* You can't store stuff in a corpse */
  GET_OBJ_VAL (corpse, 3) = 1;	/* corpse identifier */
  GET_OBJ_WEIGHT (corpse) = GET_WEIGHT (ch) + IS_CARRYING_W (ch);
  GET_OBJ_RENT (corpse) = 100000;
  if (IS_NPC (ch))
    GET_OBJ_TIMER (corpse) = max_npc_corpse_time;
  else
    GET_OBJ_TIMER (corpse) = max_pc_corpse_time;

  /* transfer character's inventory to the corpse */
  corpse->contains = ch->carrying;
  for (o = corpse->contains; o != NULL; o = o->next_content)
    o->in_obj = corpse;
  object_list_new_owner (corpse, NULL);

  /* transfer character's equipment to the corpse */
  for (i = 0; i < NUM_WEARS; i++)
    if (GET_EQ (ch, i))
      obj_to_obj (unequip_char (ch, i), corpse);

  /* transfer gold */
  if (GET_GOLD (ch) > 0)
    {
      /* following 'if' clause added to fix gold duplication loophole */
      if (IS_NPC (ch) || (!IS_NPC (ch) && ch->desc))
	{
	  money = create_money (GET_GOLD (ch));
	  obj_to_obj (money, corpse);
	}
      GET_GOLD (ch) = 0;
    }
  ch->carrying = NULL;
  IS_CARRYING_N (ch) = 0;
  IS_CARRYING_W (ch) = 0;

  obj_to_room (corpse, ch->in_room);
}


/* When ch kills victim */
void 
change_alignment (struct char_data *ch, struct char_data *victim)
{
  /*
   * new alignment change algorithm: if you kill a monster with alignment A,
   * you move 1/16th of the way to having alignment -A.  Simple and fast.
   */
  if (PRF2_FLAGGED(ch, PRF2_INTANGIBLE)) return;
  GET_ALIGNMENT (ch) += (-GET_ALIGNMENT (victim) - GET_ALIGNMENT (ch)) >> 4;

  if (IS_GOOD (ch))
    REMOVE_BIT (PLR_FLAGS (ch), PLR_THIEF | PLR_KILLER);

}



void 
death_cry (struct char_data *ch)
{
  int door, was_in;

  act ("Your blood freezes as you hear $n's death cry.", FALSE, ch, 0, 0, TO_ROOM);
  was_in = ch->in_room;

  for (door = 0; door < NUM_OF_DIRS; door++)
    {
      if (CAN_GO (ch, door))
	{
	  ch->in_room = world[was_in].dir_option[door]->to_room;
	  act ("Your blood freezes as you hear someone's death cry.", FALSE, ch, 0, 0, TO_ROOM);
	  ch->in_room = was_in;
	}
    }
}

void raw_kill(struct char_data * ch, struct char_data * killer)
{
  void do_extract_char (struct char_data *ch, int type);
  if (FIGHTING (ch))
    stop_fighting (ch);

  while (ch->affected)
    affect_remove (ch, ch->affected);

   if (killer) {
     if (death_mtrigger(ch, killer))
       death_cry(ch);
   } else
       death_cry(ch);

  make_corpse (ch);
  if (GET_LEVEL(ch) < 30 || IS_NPC(ch))
    extract_char (ch);
  else
    do_extract_char (ch, 1);
}


void die(struct char_data * ch, struct char_data * killer)
{
  increase_blood(ch->in_room); /* now add the blood (Mulder) */
  gain_exp (ch, (-(GET_EXP(ch)-exp_to_level(GET_LEVEL(ch)-1))/4));
  //Subtract the difference of the current player exp and how much exp it took to get to their level
  //divided by 4.
  if (!IS_NPC (ch)) {
    REMOVE_BIT (PLR_FLAGS (ch), PLR_KILLER | PLR_THIEF);
    write_aliases(ch);
    GET_COND(ch, FULL) = 0;
    GET_COND(ch, THIRST) = 0;
    GET_COND(ch, DRUNK) = 0;
  }
  raw_kill(ch, killer);
}



void 
perform_group_gain (struct char_data *ch, int base,
		    struct char_data *victim)
{
  if (base > 1)
    {
      sprintf (buf2, "You receive your share of experience -- %s points.\r\n", numdisplay(base));
      send_to_char (buf2, ch);
    }
  else
    send_to_char ("You receive your share of experience -- one measly little point!\r\n", ch);

  gain_exp (ch, base);
  change_alignment (ch, victim);
}


void 
group_gain (struct char_data *ch, struct char_data *victim)
{
  int tot_members, tot_gain;
  struct char_data *k;
  struct follow_type *f;

  if (IS_NPC(ch))
    return;

  if (!(k = ch->master))
    k = ch;

  if (IS_AFFECTED (k, AFF_GROUP) && (k->in_room == ch->in_room))
    tot_members = 1;
  else
    tot_members = 0;

  for (f = k->followers; f; f = f->next)
    if (IS_AFFECTED (f->follower, AFF_GROUP) && f->follower->in_room == ch->in_room)
      tot_members++;

  if (tot_members<=1)
    tot_gain = MAX(MIN(GET_EXP (victim) * 0.666, max_exp_gain), 1);
  else
    tot_gain = MAX(MIN(GET_EXP (victim) / tot_members, max_exp_gain), 1);

  if (!IS_NPC(victim))
    tot_gain=1;

  if (IS_AFFECTED (k, AFF_GROUP) && k->in_room == ch->in_room)
    perform_group_gain (k, tot_gain, victim);

  for (f = k->followers; f; f = f->next)
    if (IS_AFFECTED (f->follower, AFF_GROUP) && f->follower->in_room == ch->in_room)
      perform_group_gain (f->follower, tot_gain, victim);
}


void 
solo_gain (struct char_data *ch, struct char_data *victim)
{
  int exp;

  exp = MAX(MIN(GET_EXP (victim) * 0.666, max_exp_gain), 1);

  /* Calculate level-difference bonus */
  if (!IS_NPC (victim))
    exp = 1;

  if (IS_NPC (ch))
    return; // Mobs don't get exp.

  if (exp > 1)
    {
      sprintf (buf2, "You receive %s experience points.\r\n", numdisplay(exp));
      send_to_char (buf2, ch);
    }
  else
    send_to_char ("You receive one lousy experience point.\r\n", ch);

  gain_exp (ch, exp);
  change_alignment (ch, victim);
}


char *
replace_string (char *str, char *weapon_singular, char *weapon_plural)
{
  static char buf[256];
  char *cp;

  cp = buf;

  for (; *str; str++)
    {
      if (*str == '#')
	{
	  switch (*(++str))
	    {
	    case 'W':
	      for (; *weapon_plural; *(cp++) = *(weapon_plural++));
	      break;
	    case 'w':
	      for (; *weapon_singular; *(cp++) = *(weapon_singular++));
	      break;
	    default:
	      *(cp++) = '#';
	      break;
	    }
	}
      else
	*(cp++) = *str;

      *cp = 0;
    }				/* For */

  return (buf);
}


/* message for doing damage with a weapon */
void 
dam_message (int dam, struct char_data *ch, struct char_data *victim,
	     int w_type)
{
  char *buf;
  int msgnum;

  static struct dam_weapon_type
    {
      char *to_room;
      char *to_char;
      char *to_victim;
    }
  dam_weapons[] =
  {

    /* use #w for singular (i.e. "slash") and #W for plural (i.e. "slashes") */

    {
      "$n tries to #w $N, but misses.",		/* 0: 0     */
	"You try to #w $N, but miss.",
	"$n tries to #w you, but misses."
    }
    ,

    {
      "$n tickles $N as $e #W $M.",	/* 1: 1..2  */
	"You tickle $N as you #w $M.",
	"$n tickles you as $e #W you."
    }
    ,

    {
      "$n barely #W $N.",	/* 2: 3..4  */
	"You barely #w $N.",
	"$n barely #W you."
    }
    ,

    {
      "$n #W $N.",		/* 3: 5..6  */
	"You #w $N.",
	"$n #W you."
    }
    ,

    {
      "$n #W $N hard.",		/* 4: 7..10  */
	"You #w $N hard.",
	"$n #W you hard."
    }
    ,

    {
      "$n #W $N very hard.",	/* 5: 11..14  */
	"You #w $N very hard.",
	"$n #W you very hard."
    }
    ,

    {
      "$n #W $N extremely hard.",	/* 6: 15..19  */
	"You #w $N extremely hard.",
	"$n #W you extremely hard."
    }
    ,

    {
      "$n massacres $N to small fragments with $s #w.",		/* 7: 19..23 */
	"You massacre $N to small fragments with your #w.",
	"$n massacres you to small fragments with $s #w."
    }
    ,

    {
      "$n OBLITERATES $N with $s deadly #w!!",	/* 8: 24..28   */
	"You OBLITERATE $N with your deadly #w!!",
	"$n OBLITERATES you with $s deadly #w!!"
    }
    ,
    {
      "$n PULVERIZES $N to bits with $s deadly #w!!",	/* 9: 29..33   */
	"You PULVERIZE $N to bits with your deadly #w!!",
	"$n PULVERIZES you to bits with $s deadly #w!!"
    }
    ,
    {
      "$n VAPORIZES $N with $s deadly #w!!",	/*10: 34..38   */
	"You VAPORIZE $N with your deadly #w!!",
	"$n VAPORIZES you with $s deadly #w!!"
    }
    ,
    {
      "$n ANNIHILATES $N to smithereens with $s deadly #w!!", /*11: > 38 */
	"You &RANNIHILATE&Y $N to smithereens with your deadly #w!!",
	"$n &RANNIHILATES&Y you to smithereens with $s deadly #w!!"
    }
  };


  w_type -= TYPE_HIT;		/* Change to base of table with text */

  if (dam == 0)
    msgnum = 0;
  else if (dam <= 27)
    msgnum = 1;
  else if (dam <= 54)
    msgnum = 2;
  else if (dam <= 81)
    msgnum = 3;
  else if (dam <= 108)
    msgnum = 4;
  else if (dam <= 135)
    msgnum = 5;
  else if (dam <= 162)
    msgnum = 6;
  else if (dam <= 189)
    msgnum = 7;
  else if (dam <= 216)
    msgnum = 8;
  else if (dam <= 243)
    msgnum = 9;
  else if (dam <= 270)
    msgnum = 10;
  else
    msgnum = 11;

  /* damage message to onlookers */
  buf = replace_string (dam_weapons[msgnum].to_room,
	  attack_hit_text[w_type].singular, attack_hit_text[w_type].plural);
  act (buf, FALSE, ch, NULL, victim, TO_NOTVICT);

  /* damage message to damager */
  send_to_char (CCYEL (ch, C_CMP), ch);
  buf = replace_string (dam_weapons[msgnum].to_char,
	  attack_hit_text[w_type].singular, attack_hit_text[w_type].plural);
  act (buf, FALSE, ch, NULL, victim, TO_CHAR);
  send_to_char (CCNRM (ch, C_CMP), ch);

  /* damage message to damagee */
  send_to_char (CCRED (victim, C_CMP), victim);
  buf = replace_string (dam_weapons[msgnum].to_victim,
	  attack_hit_text[w_type].singular, attack_hit_text[w_type].plural);
  act (buf, FALSE, ch, NULL, victim, TO_VICT | TO_SLEEP);
  send_to_char (CCNRM (victim, C_CMP), victim);


}


/*
 * message for doing damage with a spell or skill
 *  C3.0: Also used for weapon damage on miss and death blows
 */
int 
skill_message (int dam, struct char_data *ch, struct char_data *vict,
	       int attacktype)
{
  int i, j, nr;
  struct message_type *msg;

  struct obj_data *weap = GET_EQ (ch, WEAR_WIELD);

  for (i = 0; i < MAX_MESSAGES; i++)
    {
      if (fight_messages[i].a_type == attacktype)
	{
	  nr = dice (1, fight_messages[i].number_of_attacks);
	  for (j = 1, msg = fight_messages[i].msg; (j < nr) && msg; j++)
	    msg = msg->next;

	  if (!IS_NPC (vict) && (GET_LEVEL (vict) >= LVL_IMMORT))
	    {
	      act (msg->god_msg.attacker_msg, FALSE, ch, weap, vict, TO_CHAR);
	      act (msg->god_msg.victim_msg, FALSE, ch, weap, vict, TO_VICT);
	      act (msg->god_msg.room_msg, FALSE, ch, weap, vict, TO_NOTVICT);
	    }
	  else if (dam != 0)
	    {
	      if (GET_POS (vict) == POS_DEAD)
		{
		  send_to_char (CCYEL (ch, C_CMP), ch);
		  act (msg->die_msg.attacker_msg, FALSE, ch, weap, vict, TO_CHAR);
		  send_to_char (CCNRM (ch, C_CMP), ch);

		  send_to_char (CCRED (vict, C_CMP), vict);
		  act (msg->die_msg.victim_msg, FALSE, ch, weap, vict, TO_VICT | TO_SLEEP);
		  send_to_char (CCNRM (vict, C_CMP), vict);

		  act (msg->die_msg.room_msg, FALSE, ch, weap, vict, TO_NOTVICT);
		}
	      else
		{
		  send_to_char (CCYEL (ch, C_CMP), ch);
		  act (msg->hit_msg.attacker_msg, FALSE, ch, weap, vict, TO_CHAR);
		  send_to_char (CCNRM (ch, C_CMP), ch);

		  send_to_char (CCRED (vict, C_CMP), vict);
		  act (msg->hit_msg.victim_msg, FALSE, ch, weap, vict, TO_VICT | TO_SLEEP);
		  send_to_char (CCNRM (vict, C_CMP), vict);

		  act (msg->hit_msg.room_msg, FALSE, ch, weap, vict, TO_NOTVICT);
		}
	    }
	  else if (ch != vict)
	    {			/* Dam == 0 */
	      send_to_char (CCYEL (ch, C_CMP), ch);
	      act (msg->miss_msg.attacker_msg, FALSE, ch, weap, vict, TO_CHAR);
	      send_to_char (CCNRM (ch, C_CMP), ch);

	      send_to_char (CCRED (vict, C_CMP), vict);
	      act (msg->miss_msg.victim_msg, FALSE, ch, weap, vict, TO_VICT | TO_SLEEP);
	      send_to_char (CCNRM (vict, C_CMP), vict);

	      act (msg->miss_msg.room_msg, FALSE, ch, weap, vict, TO_NOTVICT);
	    }
	  return 1;
	}
    }
  return 0;
}

#define PHP 5 /* Percent of a player's hp that needs to be lost by ch for him to get +1 POWER */

void adrenaline_rush(struct char_data *ch, sh_int damage) {
  struct affected_type af;

  if (IS_NPC(ch) || GET_SKILL(ch, SKILL_ADRENALINE) < 70 || damage < PHP*GET_MAX_HIT(ch)/100) return;

  af.location = APPLY_POWER;
  af.modifier = damage/(PHP*GET_MAX_HIT(ch)/100);
  af.duration = damage/(PHP*GET_MAX_HIT(ch)/100);
  af.bitvector = 0;
  af.type=SKILL_ADRENALINE + (GET_SKILL(ch, SKILL_BLOODLUST) > 70) + (GET_SKILL(ch, SKILL_CARNALRAGE) > 70); // These skills MUST be in succession to each other for this to work!
  if (af.type==SKILL_CARNALRAGE)
    send_to_char("You feel the &RCarnal &rRage&n build within you!!!\r\n", ch);
  else if (af.type==SKILL_BLOODLUST)
    send_to_char("You &rlust&n for more &RBLOOD&n!!\r\n", ch);
  else if (af.type==SKILL_ADRENALINE)
    send_to_char("You feel a surge of &RADRENALINE&n!\r\n", ch);
  affect_join (ch, &af, TRUE, FALSE, TRUE, FALSE);
}

#define VALID_ATTACKTYPE (attacktype < SELF_DAMAGE)

void do_actual_damage (struct char_data *ch, struct char_data *victim, int dam, int attacktype,
int deathblow) {
  int j, gold_before, gold_after, questdiff;
  struct obj_data *tobj;
  int rip_dam;
  struct char_data *questtarget = NULL;
  ACMD (do_get);
  ACMD (do_split);
  ACMD (do_stand);
  /*  int exp; */
  long local_gold = 0;
  char local_buf[256];
  char mybuf[1024];

  if (GET_POS (victim) <= POS_DEAD)
    {
      log ("SYSERR: Attempt to damage a corpse.");
      die(victim, ch);
      return;			/* -je, 7/7/92 */
    }

  if (IS_SET(PRF2_FLAGS(ch), PRF2_MERCY) && !IS_NPC(ch) && GET_HIT(victim) < 0 && !deathblow && VALID_ATTACKTYPE) {
    act("$N is almost dead, and you decide to have mercy on them.", FALSE, ch, 0, victim, TO_CHAR);
    stop_fighting(ch);
    return;
  }

  if (!(IS_ARENACOMBATANT(ch) && IS_ARENACOMBATANT(victim))){
    if (!pk_allowed && !IS_NPC(ch) && !IS_NPC(victim)
        && (GET_LEVEL(victim) < pk_victim_min)
        && !PLR_FLAGGED (victim, PLR_THIEF)
        && !(ch == victim)){
      send_to_char ("Ack! But he's a newbie!\r\n", ch);
      return;
    }
    if (!pk_allowed && !IS_NPC(ch) && !IS_NPC(victim) 
	&& (GET_LEVEL(ch) < pk_victim_min)
        && !(ch == victim)){

      send_to_char ("Wait till you're level 10 at least before becoming a PLAYER KILLER.\r\n", ch);
      return;
    }
  }

  if (ch->in_room != victim->in_room){
    sprintf(mybuf, "DEBUG: Timing Bug Trigger - %s (ch) and %s (victim) are not in same room.",GET_NAME(ch), GET_NAME(victim));
    mudlog(mybuf, CMP, LVL_GRGOD, TRUE);
    return;
  }

  /* peaceful rooms - but let imps attack */
  if (ch != victim && ROOM_FLAGGED (ch->in_room, ROOM_PEACEFUL) && GET_LEVEL (ch) < LVL_IMPL)
    {
      send_to_char ("This room just has such a peaceful, easy feeling...\r\n", ch);
      return;
    }

  /* shopkeeper protection */
  if (!ok_damage_shopkeeper (ch, victim))
    return;

  /* You can't damage an immortal or a builder */
  if ((!IS_NPC (victim) && (GET_LEVEL (victim) >= LVL_IMMORT)) || ((GET_LEVEL(ch)<LVL_IMMORT || IS_NPC(ch)) && IS_SET(PRF2_FLAGS(victim), PRF2_INTANGIBLE)))
    dam = 0;

  if ((PRF2_FLAGGED(ch, PRF2_INTANGIBLE) && !PRF2_FLAGGED(ch, PRF2_MBUILDING) && GET_LEVEL(victim)<LVL_IMMORT) ||
      (PRF2_FLAGGED(victim, PRF2_INTANGIBLE) && !PRF2_FLAGGED(victim, PRF2_MBUILDING) && GET_LEVEL(ch)<LVL_IMMORT)) {
    stop_fighting(ch);
    stop_fighting(victim);
    return; /* Intangibles can't fight. */
  }

  if (victim != ch)
    {
      /* Start the attacker fighting the victim */
      if (GET_POS (ch) > POS_STUNNED && (FIGHTING (ch) == NULL))
	set_fighting (ch, victim);

      /* Start the victim fighting the attacker */
      if (GET_POS (victim) > POS_STUNNED && (FIGHTING (victim) == NULL))
	{
	  set_fighting (victim, ch);
	  if (MOB_FLAGGED (victim, MOB_MEMORY) && !IS_NPC (ch))
	    remember (victim, ch);
	}
    }

  /* If you attack a pet, it hates your guts */
  if (victim->master == ch)
    stop_follower (victim);

  /* If the attacker is invisible, he becomes visible */
  if (IS_AFFECTED (ch, AFF_INVISIBLE | AFF_HIDE))
    appear (ch);

  /* Cut damage in half if victim has sanct, to a minimum 1 */
  if (IS_AFFECTED (victim, AFF_SANCTUARY) && dam >= 2)
    dam /= 2;

  /* Check for PK if this is not a PK MUD */
  if (!pk_allowed) {
      check_killer (ch, victim);
      /* Uncomment this part ONLY if COMPLETELY NO PKing is allowed
      if (PLR_FLAGGED (ch, PLR_KILLER) && (ch != victim))
	dam = 0;
	*/
  }

  /* Damage multiplier. */
  dam*=dam_multi(ch, victim, attacktype > 0 && attacktype <= MAX_SPELLS ? 1 : 0);

  /* New Skills here - Dodge, Parry, Avoid, Riposte */
  if (attacktype <= TYPE_STAB && attacktype >= TYPE_HIT && GET_POS(ch) > POS_STANDING-1) {
      /* come the more powerful skills... Riposte first */
      if ((number(1,100)*AVOID_FACTOR) <= GET_SKILL(victim, SKILL_RIPOSTE)) {
        
        /* Send out the lovely messages */
        sprintf(buf, "You anticipate $N's attack, avoiding it, and striking back!");
        act(buf, TRUE, victim, 0, ch, TO_CHAR);
        
        sprintf(buf, "$n anticipates your attack, and strikes back at you!");
        act(buf, FALSE, victim, 0, ch, TO_VICT);
        
        sprintf(buf, "$n anticipates $N's ameteur attack, and strikes back expertly.");
        act(buf, FALSE, victim, 0, ch, TO_NOTVICT);
    
        /* How much damage to do on a riposte? */
        tobj = victim->equipment[WEAR_WIELD];
        /* If we aren't wielding a weapon, strike back with minimal
         * barehand damage.
         */
        if(!tobj || GET_OBJ_TYPE(tobj) != ITEM_WEAPON){
          damage(victim, ch, 2, SKILL_RIPOSTE);
          return;
        }
        
        rip_dam = dice(GET_OBJ_VAL(tobj, 1), GET_OBJ_VAL(tobj, 2));
        rip_dam *= 1 + (POS_FIGHTING - GET_POS(ch)) / 3;
      
        damage(victim, ch, rip_dam, SKILL_RIPOSTE);
        return;
        
      } else if((number(1,100)*AVOID_FACTOR) <= GET_SKILL(victim, SKILL_AVOID)) {
          
        /* Send out the lovely messages */
        sprintf(buf, "You avoid $N's attack, tossing $M to the ground.");
        act(buf, TRUE, victim, 0, ch, TO_CHAR);
        
        sprintf(buf, "$n avoids your attack, trips you, sending you to the ground.");
        act(buf, FALSE, victim, 0, ch, TO_VICT);
        
        sprintf(buf, "$n avoids $N's pathetic attack and sends $M sprawling.");
        act(buf, FALSE, victim, 0, ch, TO_NOTVICT);
        
        GET_POS(ch) = POS_SITTING;
        WAIT_STATE(ch, PULSE_VIOLENCE);
      
        return;
        
      } else if((number(1,100)*AVOID_FACTOR) <= GET_SKILL(victim, SKILL_PARRY)) {
        
          /* Send out the lovely messages */
          sprintf(buf, "You parry $N's viscious attack upon your person.");
          act(buf, TRUE, victim, 0, ch, TO_CHAR);
        
          sprintf(buf, "$n spoils your attack with a deft parrying move.");
          act(buf, FALSE, victim, 0, ch, TO_VICT);
        
          sprintf(buf, "$n parries $N's attack with a series of skillful maneuvers.");
          act(buf, FALSE, victim, 0, ch, TO_NOTVICT);
        
          return;

      } else if((number(1,100)*AVOID_FACTOR) <= GET_SKILL(victim, SKILL_DODGE)) {
          
        /* Send out the lovely messages */
        sprintf(buf, "You narrowly dodge $N's masterful attack.");
        act(buf, TRUE, victim, 0, ch, TO_CHAR);
          
        sprintf(buf, "$n narrowly dodges your skillful attack, just avoiding your intended blow.");
        act(buf, FALSE, victim, 0, ch, TO_VICT);
          
        sprintf(buf, "$n narrowly dodges $N's strike.");
        act(buf, FALSE, victim, 0, ch, TO_NOTVICT);
          
        return;
      } 
  }
          
  tobj = NULL;
        
        
  if(!IS_NPC(ch) && !CHECK_WAIT(ch) && GET_POS(ch) < POS_FIGHTING)
    do_stand(ch, "", 0, 0);
        
  if(IS_NPC(ch) && !GET_MOB_WAIT(ch) && GET_POS(ch) < POS_FIGHTING)
    do_stand(ch, "", 0, 0);
        
        
  /* Set the maximum damage per round and subtract the hit points */
  dam = MAX (MIN (dam, 1000), 0);
  GET_HIT (victim) -= dam;
  if (VALID_ATTACKTYPE) adrenaline_rush(victim, dam);

  /* Gain exp for the hit */
//  if (ch != victim)
//    gain_exp (ch, GET_LEVEL (victim) * dam);

  update_pos (victim);

  /*
   * skill_message sends a message from the messages file in lib/misc.
   * dam_message just sends a generic "You hit $n extremely hard.".
   * skill_message is preferable to dam_message because it is more
   * descriptive.
   * 
   * If we are _not_ attacking with a weapon (i.e. a spell), always use
   * skill_message. If we are attacking with a weapon: If this is a miss or a
   * death blow, send a skill_message if one exists; if not, default to a
   * dam_message. Otherwise, always send a dam_message.
   */
  if (attacktype != -1 && VALID_ATTACKTYPE && attacktype!=SKILL_RIPOSTE) {
    if (!IS_WEAPON(attacktype))
      skill_message(dam, ch, victim, attacktype);
    else {
      if ((GET_POS(victim) == POS_DEAD && !deathblow && !IS_SET(PRF2_FLAGS(ch), PRF2_MERCY) &&
          !IS_NPC(ch)) || dam == 0) {
        if (!skill_message(dam, ch, victim, attacktype))
          dam_message(dam, ch, victim, attacktype);
      } else
        dam_message(dam, ch, victim, attacktype);
    }
  }

  if (!IS_NPC(ch) && IS_SET(PRF2_FLAGS(ch), PRF2_MERCY) && GET_HIT(victim) < 0 && !deathblow && VALID_ATTACKTYPE) {
    stop_fighting(ch);
    stop_fighting(victim);
    GET_POS(victim)=POS_STANDING;
    sprintf(buf, "You have mercy on $N, and spare %s life... for now.", HSHR(victim));
    act(buf, TRUE, ch, 0, victim, TO_CHAR);
    
    sprintf(buf, "$n spares your life, thank the gods!");
    act(buf, FALSE, ch, 0, victim, TO_VICT);
  
    sprintf(buf, "$N is about to deliver the death blow, but suddenly spares $n's life!");
    act(buf, FALSE, victim, 0, ch, TO_NOTVICT);
    GET_HIT(victim)=-1; update_pos(victim);
  }

  /* Use send_to_char -- act() doesn't send message if you are DEAD. */
  switch (GET_POS (victim))
    {
    case POS_MORTALLYW:
      act ("$n is mortally wounded, and will die soon, if not aided.", TRUE, victim, 0, 0, TO_ROOM);
      send_to_char ("You are mortally wounded, and will die soon, if not aided.\r\n", victim);
      break;
    case POS_INCAP:
      act ("$n is incapacitated and will slowly die, if not aided.", TRUE, victim, 0, 0, TO_ROOM);
      send_to_char ("You are incapacitated an will slowly die, if not aided.\r\n", victim);
      break;
    case POS_STUNNED:
      act ("$n is stunned, but will probably regain consciousness again.", TRUE, victim, 0, 0, TO_ROOM);
      send_to_char ("You're stunned, but will probably regain consciousness again.\r\n", victim);
      break;
    case POS_DEAD:
      if (IS_ARENACOMBATANT(victim)){
	match_over(ch, victim, "(Fatality)", TRUE);
	return ;
      }else{
	act ("$n is dead!  R.I.P.", FALSE, victim, 0, 0, TO_ROOM);
	send_to_char ("You are dead!  Sorry...\r\n", victim);
      }
      break;

    default:			/* >= POSITION SLEEPING */
      if (dam > (GET_MAX_HIT (victim) / 4))
	act ("That really did HURT!", FALSE, victim, 0, 0, TO_CHAR);

      if (GET_HIT (victim) < (GET_MAX_HIT (victim) / 4))
	{
	  sprintf (buf2, "%sYou wish that your wounds would stop BLEEDING so much!%s\r\n",
		   CCRED (victim, C_SPR), CCNRM (victim, C_SPR));
	  send_to_char (buf2, victim);
     if (IS_NPC(victim) && (ch != victim) && MOB_FLAGGED(victim, MOB_WIMPY))
	    do_flee (victim, "", 0, 0);
	 }

      if (!IS_NPC (victim) && GET_RETREAT_LEV (victim) && (victim != ch) && GET_HIT(victim) <  GET_RETREAT_LEV(victim) && GET_HIT(victim) > 0)
        {
          send_to_char ("You wimp out, and attempt to retreat!\r\n", victim);
          /* Retreat check is done at spell_recall in spells.c */
          do_use (victim, "retreat", 0, SCMD_RECITE);
        }

      if (!IS_NPC (victim) && GET_RECALL_LEV (victim) && (victim != ch) && GET_HIT(victim) < GET_RECALL_LEV(victim) && GET_HIT(victim) > 0)
        {
          send_to_char ("You wimp out, and attempt to recall!\r\n", victim);
	  /* Recall check is done at spell_recall in spells.c */
	  do_use (victim, "recall", 0, SCMD_RECITE);
        }
      if (!IS_NPC (victim) && GET_WIMP_LEV (victim) && (victim != ch) && GET_HIT(victim) < GET_WIMP_LEV(victim) && GET_HIT(victim) > 0)
	{
	  send_to_char ("You wimp out, and attempt to flee!\r\n", victim);
	  /* Flee check is done at do_flee in act.offensive.c */
	  do_flee (victim, "", 0, 0);
	}
      break;
    }

  /* Help out poor linkless people who are attacked */
  if (!IS_NPC (victim) && !(victim->desc))
    {
      do_flee (victim, "", 0, 0);
      if (!FIGHTING (victim))
	{
	  act ("$n is rescued by divine forces.", FALSE, victim, 0, 0, TO_ROOM);
	  GET_WAS_IN (victim) = victim->in_room;
	  char_from_room (victim);
	  char_to_room (victim, 0);
	}
    }

  /* stop someone from fighting if they're stunned or worse */
  if ((GET_POS (victim) <= POS_STUNNED) && (FIGHTING (victim) != NULL))
    stop_fighting (victim);

  /* Uh oh.  Victim died. */
  if (GET_POS (victim) == POS_DEAD)
    {
      if ((ch != victim) && (IS_NPC (victim) || victim->desc))
	{
	  if (IS_AFFECTED (ch, AFF_GROUP))
	    group_gain (ch, victim);
	  else
	    solo_gain (ch, victim);
	}
      if (PLR_FLAGGED (ch, PLR_QUESTOR) && IS_NPC (victim))
	{			/* LJ autoquest */
	  if (GET_QUESTMOB (ch) == GET_MOB_VNUM (victim))
	    {
	      send_to_char ("You have almost completed your QUEST!\n\r", ch);
	      send_to_char ("Return to the questmaster before your time runs out!\n\r", ch);
	      /* GET_QUESTMOB (ch) = 0; */
	      questtarget = get_char_num(real_mobile(GET_QUESTMOB(ch)));
	      questdiff = estimate_difficulty(ch, questtarget);
	      if (questdiff <= 0)
		questdiff = 1;
	      questdiff = (questdiff/5);
	      GET_QUESTMOB (ch) = -(questdiff);
	    }
	}


      if (!IS_NPC (victim))
	{
	  sprintf (buf2, "%s killed by %s at %s", GET_NAME (victim), GET_NAME (ch),
		   world[victim->in_room].name);
	  mudlog (buf2, BRF, LVL_IMMORT, TRUE);

          if (ch == victim) { /* suicide :( */
	    die(victim, ch);
            return;
          }
	  if ((!pk_allowed) && (PLR_FLAGGED (ch, PLR_KILLER))){
	    /* Oooh, goto JAIL! */

	 sprintf (buf, "&m[&YINFO&m]&n %s was killed by %s (jailed).\r\n",
       GET_NAME (victim), GET_NAME (ch));
	 send_to_all (buf);

	    send_to_char ("Oh now you've really gone and done it!\r\n", ch);
	    REMOVE_BIT(PRF_FLAGS(ch), PRF_SUMMONABLE);
	    SET_BIT(PRF_FLAGS(ch), PRF_NOAUCT);
	    GET_ALIGNMENT (ch) = -1000;

	    j = (int) GET_LEVEL (ch) - (int) GET_LEVEL (victim);
	    if (j == 0)
	      j = 1;
	    else
	      j = abs(j);
	    GET_BAIL_AMT (ch) =  (j* bail_multiplier);

	    char_from_room (ch);
	    char_to_room (ch, real_room(jail_num));
	    act ("$n suddenly appears in the room.", TRUE, ch, 0, 0, TO_ROOM);
	    look_at_room (ch, 0);
	  }else if ((!pk_allowed) && (PLR_FLAGGED (victim, PLR_KILLER) && !IS_NPC(ch))){
	    sprintf (buf,"&m[&YINFO&m]&n %s was killed by %s (defending).\r\n",
		     GET_NAME (victim), GET_NAME (ch));
	    send_to_all (buf);

// this was annoying, changed 8/23/98 --Mulder
	  }else if (IS_NPC(ch) || IS_NPC(victim)){
//
//	    sprintf (buf, "&m[&YINFO&m]&n %s was killed by %s.\r\n", 
//		     GET_NAME (victim), GET_NAME (ch));
//	    send_to_all (buf);
      
	  }else{
	    sprintf (buf, "&m[&YINFO&m]&n %s was killed by %s (offending).\r\n", 
		     GET_NAME (victim), GET_NAME (ch));
	    send_to_all (buf);
	  }

	  if (MOB_FLAGGED (ch, MOB_MEMORY))
	    forget (ch, victim);
	}

      /* Cant determine GET_GOLD on corpse, so do now and store */
      if (IS_NPC (victim))
	{
	  local_gold = GET_GOLD (victim);
	  sprintf (local_buf, "%ld", (long) local_gold);
	}
      die(victim, ch);
      gold_before = gold_after = 0;
      /* If Autoloot enabled, get all corpse */
      if (IS_NPC (victim) && !IS_NPC (ch) 
	  && PRF_FLAGGED (ch, PRF_AUTOLOOT) && !PLR_FLAGGED(ch, PLR_KILLER))
	{
	  gold_before = GET_GOLD(ch);
	  do_get (ch, "all corpse", 0, 0);
	  gold_after = GET_GOLD(ch);
	}

      /* If Autoloot AND AutoSplit AND we got money, split with group */
      if (IS_AFFECTED (ch, AFF_GROUP) && (local_gold > 0) &&
	  PRF_FLAGGED (ch, PRF_AUTOSPLIT) && PRF_FLAGGED (ch, PRF_AUTOLOOT)
	  && !PLR_FLAGGED(ch, PLR_KILLER))
	{
	  if (gold_after > gold_before)
	    do_split (ch, local_buf, 0, 0);
	}
      /* If Autogold enabled, get coins corpse */
      if (IS_NPC (victim) && !IS_NPC (ch) && PRF_FLAGGED (ch, PRF_AUTOGOLD)
	  && !PRF_FLAGGED(ch, PLR_KILLER))
	{
	  gold_before = GET_GOLD(ch);
	  do_get (ch, "coins corpse", 0, 0);
	  gold_after = GET_GOLD(ch);
	}

      /* If Autogold AND AutoSplit AND we got money, split with group */
      if (IS_AFFECTED (ch, AFF_GROUP) && (local_gold > 0) &&
	  PRF_FLAGGED (ch, PRF_AUTOSPLIT) && PRF_FLAGGED (ch, PRF_AUTOGOLD)
	  && !PLR_FLAGGED(ch, PLR_KILLER))
	{
	  if (gold_after > gold_before)
	    do_split (ch, local_buf, 0, 0);
	}
    }
}

void damage (struct char_data *ch, struct char_data *victim, int dam, int attacktype) {
  do_actual_damage (ch, victim, dam, attacktype, 0);
}

void deathblow (struct char_data *ch, struct char_data *victim, int dam, int attacktype) {
  do_actual_damage (ch, victim, dam, attacktype, 1);
}

void hit (struct char_data *ch, struct char_data *victim, int type)
{
  static struct affected_type *af;
  struct obj_data *wielded = GET_EQ (ch, WEAR_WIELD);
  int dam=0, diceroll;

  int backstab_mult (int level);

  /* check if the character has a fight trigger */
  fight_mtrigger(ch);
  fight_otrigger(ch);

  /* Do some sanity checking, in case someone flees, etc. */
  if (ch->in_room != victim->in_room)
    {
      if (FIGHTING (ch) && FIGHTING (ch) == victim)
	stop_fighting (ch);
      return;
    }
  // No hits in a peaceful room! -Storm
  if (ROOM_FLAGGED(ch->in_room, ROOM_PEACEFUL) && GET_LEVEL(ch) < LVL_IMPL) {
    send_to_char("This room just has such a peaceful, easy feeling...\r\n", ch);
    return;
  }

  diceroll = number (0, 100);
  /* decide whether this is a hit or a miss */
  if (diceroll > chance(ch, victim, 0) && AWAKE (victim))
    {
      /* the attacker missed the victim */
      damage (ch, victim, 0, type);
    }
  else
    {
      /* okay, we know the guy has been hit.  now calculate damage. */

      if (wielded)
	{
	  /* Add weapon-based damage if a weapon is being wielded */
	  dam += dice (GET_OBJ_VAL (wielded, 1), GET_OBJ_VAL (wielded, 2));
	}
      else
	{
	  /* If no weapon, add bare hand damage instead */
	  if (IS_NPC (ch))
	    {
	      dam += dice (ch->mob_specials.damnodice, ch->mob_specials.damsizedice);
	    }
	  else
	    {
	      dam += number (0, 2);	/* Max 2 bare hand damage for players */
	    }
	}

      /*
       * Include a damage multiplier if victim isn't ready to fight:
       *
       * Position sitting  1.33 x normal
       * Position resting  1.66 x normal
       * Position sleeping 2.00 x normal
       * Position stunned  2.33 x normal
       * Position incap    2.66 x normal
       * Position mortally 3.00 x normal
       *
       * Note, this is a hack because it depends on the particular
       * values of the POSITION_XXX constants.
       */
      if (GET_POS (victim) < POS_FIGHTING)
	dam *= 1 + (POS_FIGHTING - GET_POS (victim)) / 3;

      /* at least 1 hp damage min per hit */
      dam = MAX (1, dam);

      if ((type == SKILL_BACKSTAB))
	{
	  dam *= backstab_mult (GET_LEVEL (ch));
	  damage (ch, victim, dam, SKILL_BACKSTAB);
	}
      else
	damage (ch, victim, dam, type);
      if (AFF_FLAGGED(ch, AFF_R_CHARGED))
         for (af = ch->affected; af; af = af->next)
           if (af->type==SPELL_REDIRECT_CHARGE && af->bitvector==AFF_R_CHARGED) {
	     act ("You momentarily run your finger against $N's skin and a charge of electricity jumps from your body into theirs!\r\n$N CRISPS AND FRIES!!", FALSE, ch, NULL, victim, TO_CHAR);
	     act ("$n touches you and you find it somewhat... &KELECTRIFYING&n!\r\nYour skin chars and crisps!", FALSE, ch, NULL, victim, TO_VICT);
	     act ("You momentarily see a flash of light and $N FRIES to a CRISP!", FALSE, ch, NULL, victim, TO_NOTVICT);
             damage(ch, victim, af->modifier, TYPE_UNDEFINED);
             affect_remove(ch, af);
             break;
           }
    }
   /* check if the victim has a hitprcnt trigger */
   hitprcnt_mtrigger(victim);
}



/* control the fights going on.  Called every 2 seconds from comm.c. */
void 
perform_violence (void)
{
  struct char_data *ch;
  extern struct index_data *mob_index;
  int apr, prob, percent;
  int condition;
  int j;

  for (ch = combat_list; ch; ch = next_combat_list)
    {
      next_combat_list = ch->next_fighting;
      apr = 0;

      if (FIGHTING (ch) == NULL || ch->in_room != FIGHTING (ch)->in_room)
	{
	  stop_fighting (ch);
	  continue;
	}

      if (IS_NPC (ch))
	{
	  if (GET_MOB_WAIT (ch) > 0)
	    {
	      GET_MOB_WAIT (ch) -= PULSE_VIOLENCE;
	      continue;
	    }
	  GET_MOB_WAIT (ch) = 0;
	  if (GET_POS (ch) < POS_FIGHTING)
	    {
	      GET_POS (ch) = POS_FIGHTING;
	      act ("$n scrambles to $s feet!", TRUE, ch, 0, 0, TO_ROOM);
	    }
         if ((GET_POS (FIGHTING(ch)) < POS_SLEEPING) && (MOB_FLAGGED(ch, MOB_MERCY))) {
         stop_fighting(FIGHTING(ch));
         stop_fighting(ch);
          }
	}

      if (GET_POS (ch) < POS_FIGHTING)
	{
	  send_to_char ("You can't fight while sitting!!\r\n", ch);
	  continue;
	}
      if (!IS_NPC (ch) && !PRF2_FLAGGED(ch, PRF2_DISPMOB))
	diag_char_to_char (FIGHTING (ch), ch);

      if (GET_CLASS(ch) == CLASS_WARRIOR || GET_CLASS(ch) == CLASS_THIEF){
	prob = GET_SKILL(ch, SKILL_SECOND_ATTACK);
	percent = number((int)(prob * 0.85), 101) 
	  - (GET_LEVEL(ch)/number(10, 20));
	if (percent <= 0)
	  percent = 1;
	if (prob > percent){
	  apr++;
	  prob = GET_SKILL(ch, SKILL_THIRD_ATTACK);
	  percent = number((int)(prob * 0.9), 151) 
	    - (GET_LEVEL(ch)/number(10, 30));
	  if (prob > percent && GET_CLASS(ch) == CLASS_WARRIOR)
	    apr++;
       }
     }

    if (IS_NPC(ch)){
      if (MOB_FLAGGED(ch, MOB_DBLATTACK)){
	percent = number(50, 101) 
	  - (GET_LEVEL(ch)/number(10, 20));
	if (percent <= 0)
	  percent = 1;
	if (number(1,100) > percent)
	  apr++;
      }
    }

      
    // increment apr by one for every attack they are supposed to get,
    // for the multiple attack skills, you should make sure they only
    // get a subsequent attack if they properly got the previous one.
    // For instance, you only get third attack if you are getting a
    // second attack.  This doesn't need to be skill based, you can
    // easily make it based upon class/level... see the second example
    // below.
    //
    //   if (AFF_FLAGGED(ch, AFF_HASTE))
    //     apr += number(0, 2);
    //    
    //   if (GET_CLASS(ch) == CLASS_WARRIOR && GET_LEVEL(ch) >= 10)
    //     apr++;
    //
    // If apr is negative they get no attacks, if apr is 0 they get
    // one attack.  APR has a range of -1 to 4, giving a minimum of
    // no attacks, to a maximum of 4.  See the below line for changing
    // that (eg., MAX(-1, MIN(apr, 6)) means a max of 6).
    

    apr = MAX(-1, MIN(apr, 4));
 
    if (apr >= 0) {
      for (; apr >= 0 && FIGHTING(ch); apr--) {
//        hit(ch, FIGHTING(ch), TYPE_UNDEFINED);
        hit(ch, FIGHTING(ch), GET_ATTACKTYPE(ch));
        if (FIGHTING(ch)) {
          condition = 20-(((GET_DEX(FIGHTING(ch))-12)*5)/3);
          MAX(MIN(condition,30),10); // Cap it to minimum of 10 and maximum of 30.
          if (!IS_NPC(FIGHTING(ch))) {
            for (j = 0; j < NUM_WEARS; j++) {
              if (FIGHTING(ch)->equipment[j]) {
                if (number(1, 100) <= condition) {
                  if (FIGHTING(ch)->equipment[j]->obj_flags.total_slots != 0) {
                    FIGHTING(ch)->equipment[j]->obj_flags.curr_slots =
                    FIGHTING(ch)->equipment[j]->obj_flags.curr_slots--;
                    sprintf(buf, "%s just got DAMAGED during the combat!\r\n",
                    FIGHTING(ch)->equipment[j]->short_description);
                    send_to_char(buf, FIGHTING(ch));
                  }
                }
                if ((FIGHTING(ch)->equipment[j]->obj_flags.curr_slots == 0) && (FIGHTING(ch)->equipment[j]->obj_flags.total_slots != 0)) {
                  sprintf(buf, "%s crumbles to dust as it wears out!\r\n",
                  FIGHTING(ch)->equipment[j]->short_description);
                  send_to_char(buf, FIGHTING(ch));
                  extract_obj(FIGHTING(ch)->equipment[j]);
                }
              }
            }
          }
        }
      }
      if ((MOB_FLAGGED(ch, MOB_SPEC) || MOB_FLAGGED(ch, MOB_CASTER))
          && mob_index[GET_MOB_RNUM(ch)].func != NULL)
        (mob_index[GET_MOB_RNUM(ch)].func) (ch, ch, 0, "");
    }
  }
}
