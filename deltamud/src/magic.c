/* ************************************************************************
   *   File: magic.c                                       Part of CircleMUD *
   *  Usage: low-level functions for magic; spell template code              *
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

extern struct room_data *world;
extern struct obj_data *object_list;
extern struct char_data *character_list;
extern struct index_data *obj_index;

extern struct descriptor_data *descriptor_list;
extern struct zone_data *zone_table;

extern int mini_mud;
extern int pk_allowed;

extern struct default_mobile_stats *mob_defaults;
extern char weapon_verbs[];
extern int *max_ac_applys;
extern struct apply_mod_defaults *apmd;

void clearMemory (struct char_data *ch);
void act (char *str, int i, struct char_data *c, struct obj_data *o,
	  void *vict_obj, int j);

void damage (struct char_data *ch, struct char_data *victim,
	     int damage, int weapontype);

void weight_change_object (struct obj_data *obj, int weight);
void add_follower (struct char_data *ch, struct char_data *leader);
int dice (int number, int size);
extern struct spell_info_type spell_info[];


struct char_data *read_mobile (int, int);

int mag_savingthrow (struct char_data *ch, struct char_data *victim)
{
  /* Ch casts a bad spell on victim, does the victim avoid it? */
  if (number(0, 100) > chance(ch, victim, 1))
    return TRUE;
  else
    return FALSE;
}


/* affect_update: called from comm.c (causes spells to wear off) */
void 
affect_update (void)
{
  static struct affected_type *af, *next;
  static struct char_data *i;
  extern char *spell_wear_off_msg[];

  for (i = character_list; i; i = i->next) {
    if (!IS_NPC(i))
      if (IS_SET(PRF2_FLAGS(i), PRF2_INTANGIBLE))
        continue;
    for (af = i->affected; af; af = next)
      {
	next = af->next;
	if (af->duration >= 1) {
	  af->duration--;
          if (af->type==SKILL_ADRENALINE && !FIGHTING(i)) {
            if (GET_POS(i) < POS_STANDING) {
              if (GET_POS(i) < POS_SLEEPING) {
                send_to_char("The &Radrenaline&n flowing through your veins sustains you on the brink of &Kdeath&n!\r\n", i);
                GET_HIT(i)+=number(1, 500);
                GET_POS(i)=POS_STANDING;
              } else {
                send_to_char("Your &Radrenaline&n rush completely wears off, leaving you exhausted.\r\n", i);
                GET_HIT(i)=MAX(10, GET_HIT(i)-(af->duration*100));
                GET_MOVE(i)=MAX(10, GET_MOVE(i)-(af->duration*15));
              }
              affect_remove (i, af);
            } else {
              send_to_char("Your &Radrenaline&n rush slowly wears off, leaving you tired.\r\n", i);
              GET_HIT(i)=MAX(10, GET_HIT(i)-100);
              GET_MOVE(i)=MAX(0, GET_MOVE(i)-15);
            }
          }
        }
	else if (af->duration == -1)	/* No action */
	  af->duration = -1;	/* GODs only! unlimited */
	else
	  {
	    if ((af->type > 0) && (af->type <= MAX_SPELLS))
	      if (!af->next || (af->next->type != af->type) ||
		  (af->next->duration > 0))
		if (*spell_wear_off_msg[af->type] && af->type <= 499)
		  {
		    send_to_char (spell_wear_off_msg[af->type], i);
		    send_to_char ("\r\n", i);
                    if (af->bitvector == AFF_R_CHARGED)
                      damage (i, i, af->modifier, TYPE_UNDEFINED);
		  }
	    affect_remove (i, af);
	  }
      }
    }
}


/*
 *  mag_materials:
 *  Checks for up to 3 vnums (spell reagents) in the player's inventory.
 *
 * No spells implemented in Circle 3.0 use mag_materials, but you can use
 * it to implement your own spells which require ingredients (i.e., some
 * heal spell which requires a rare herb or some such.)
 */
int 
mag_materials (struct char_data *ch, int item0, int item1, int item2,
	       int extract, int verbose)
{
  struct obj_data *tobj;
  struct obj_data *obj0 = NULL, *obj1 = NULL, *obj2 = NULL;

  for (tobj = ch->carrying; tobj; tobj = tobj->next_content)
    {
      if ((item0 > 0) && (GET_OBJ_VNUM (tobj) == item0))
	{
	  obj0 = tobj;
	  item0 = -1;
	}
      else if ((item1 > 0) && (GET_OBJ_VNUM (tobj) == item1))
	{
	  obj1 = tobj;
	  item1 = -1;
	}
      else if ((item2 > 0) && (GET_OBJ_VNUM (tobj) == item2))
	{
	  obj2 = tobj;
	  item2 = -1;
	}
    }
  if ((item0 > 0) || (item1 > 0) || (item2 > 0))
    {
      if (verbose)
	{
	  switch (number (0, 2))
	    {
	    case 0:
	      send_to_char ("A wart sprouts on your nose.\r\n", ch);
	      break;
	    case 1:
	      send_to_char ("Your hair falls out in clumps.\r\n", ch);
	      break;
	    case 2:
	      send_to_char ("A huge corn develops on your big toe.\r\n", ch);
	      break;
	    }
	}
      return (FALSE);
    }
  if (extract)
    {
      if (item0 < 0)
	{
	  obj_from_char (obj0);
	  extract_obj (obj0);
	}
      if (item1 < 0)
	{
	  obj_from_char (obj1);
	  extract_obj (obj1);
	}
      if (item2 < 0)
	{
	  obj_from_char (obj2);
	  extract_obj (obj2);
	}
    }
  if (verbose)
    {
      send_to_char ("A puff of smoke rises from your pack.\r\n", ch);
      act ("A puff of smoke rises from $n's pack.", TRUE, ch, NULL, NULL, TO_ROOM);
    }
  return (TRUE);
}




/*
 * Every spell that does damage comes through here.  This calculates the
 * amount of damage, adds in any modifiers, determines what the saves are,
 * tests for save and calls damage().
 */

void 
mag_damage (int level, struct char_data *ch, struct char_data *victim,
	    int spellnum)
{
  int is_mage = 0, is_cleric = 0;
  int dam = 0;

  if (victim == NULL || ch == NULL)
    return;

  is_mage = (GET_CLASS (ch) == CLASS_MAGIC_USER);
  is_cleric = (GET_CLASS (ch) == CLASS_CLERIC);

  switch (spellnum)
    {
      /* Mostly mages */
/*    case SPELL_ENERGY_DRAIN:
      if (GET_LEVEL (victim) <= 2)
	dam = 100;
      else
	dam = dice (1, 10);
      break;
Just for an example. 
*/
      /* Area spells */
    case SPELL_EARTHQUAKE:
      dam = dice (2, 8) + level;
      break;

    }				/* switch(spellnum) */

  /* divide damage by two if victim makes his saving throw */
  if (mag_savingthrow (ch, victim))
    dam >>= 1;

  /* If char is affected by convergence of power, double the damage */
  if (IS_AFFECTED(ch, AFF_CONVERGENCE))
    dam <<= 1;

  dam*=dam_multi(ch, victim, 1);

  /* and finally, inflict the damage */
  damage (ch, victim, dam, spellnum);
}


/*
 * Every spell that does an affect comes through here.  This determines
 * the effect, whether it is added or replacement, whether it is legal or
 * not, etc.
 *
 * affect_join(vict, aff, add_dur, avg_dur, add_mod, avg_mod)
 */

#define MAX_SPELL_AFFECTS 5	/* change if more needed */

void 
mag_affects (int level, struct char_data *ch, struct char_data *victim,
	     int spellnum)
{
  struct affected_type af[MAX_SPELL_AFFECTS];
  int is_mage = FALSE, is_cleric = FALSE;
  bool accum_affect = FALSE, accum_duration = FALSE;
  char *to_vict = NULL, *to_room = NULL;
  int i;
  extern const char *spell_affect_msg[];


  if (victim == NULL || ch == NULL)
    return;

  is_mage = (GET_CLASS (ch) == CLASS_MAGIC_USER);
  is_cleric = (GET_CLASS (ch) == CLASS_CLERIC);

  for (i = 0; i < MAX_SPELL_AFFECTS; i++)
    {
      af[i].type = spellnum;
      af[i].bitvector = 0;
      af[i].modifier = 0;
      af[i].location = APPLY_NONE;
    }

  switch (spellnum)
    {

    case SPELL_ARMOR:
      af[0].location = APPLY_DEFENSE;
      af[0].modifier = 10;
      af[0].duration = 24;
      accum_duration = TRUE;
      break;

    case SPELL_BLESS:
      af[0].location = APPLY_TECHNIQUE;
      af[0].modifier = 8;
      af[0].duration = 6;

      af[1].location = APPLY_MDEFENSE;
      af[1].modifier = 5;
      af[1].duration = 6;

      accum_duration = TRUE;
      break;

    case SPELL_BLINDNESS:
      if (MOB_FLAGGED (victim, MOB_NOBLIND) || mag_savingthrow (ch, victim))
	{
	  send_to_char ("You fail.\r\n", ch);
	  return;
	}

      af[0].location = APPLY_TECHNIQUE;
      af[0].modifier = -7;
      af[0].duration = 2;
      af[0].bitvector = AFF_BLIND;

      af[1].location = APPLY_DEFENSE;
      af[1].modifier = -10;
      af[1].duration = 2;
      af[1].bitvector = AFF_BLIND;

      to_room = "$n seems to be blinded!";
      break;

    case SPELL_CURSE:
      if (mag_savingthrow (ch, victim))
	{
	  send_to_char (NOEFFECT, ch);
	  return;
	}

      af[0].location = APPLY_TECHNIQUE;
      af[0].duration = 1 + (GET_LEVEL (ch) >> 1);
      af[0].modifier = -3;
      af[0].bitvector = AFF_CURSE;

      af[1].location = APPLY_POWER;
      af[1].duration = 1 + (GET_LEVEL (ch) >> 1);
      af[1].modifier = -4;
      af[1].bitvector = AFF_CURSE;

      accum_duration = TRUE;
      accum_affect = TRUE;
      to_room = "$n briefly glows red!";
      break;

    case SPELL_DETECT_ALIGN:
      af[0].duration = 12 + level;
      af[0].bitvector = AFF_DETECT_ALIGN;
      accum_duration = TRUE;
      break;

    case SPELL_DETECT_INVIS:
      af[0].duration = 12 + level;
      af[0].bitvector = AFF_DETECT_INVIS;
      accum_duration = TRUE;
      break;

    case SPELL_DETECT_MAGIC:
      af[0].duration = 12 + level;
      af[0].bitvector = AFF_DETECT_MAGIC;
      accum_duration = TRUE;
      break;

    case SPELL_INFRAVISION:
      af[0].duration = 12 + level;
      af[0].bitvector = AFF_INFRAVISION;
      accum_duration = TRUE;
      to_room = "$n's eyes glow red.";
      break;

    case SPELL_INVISIBLE:
      if (!victim)
	victim = ch;

      af[0].duration = 12 + (GET_LEVEL (ch) >> 2);
      af[0].modifier = 10;
      af[0].location = APPLY_DEFENSE;
      af[0].bitvector = AFF_INVISIBLE;
      accum_duration = TRUE;
      to_room = "$n slowly fades out of existence.";
      break;

    case SPELL_POISON:
      if (mag_savingthrow (ch, victim))
	{
	  send_to_char (NOEFFECT, ch);
	  return;
	}

      af[0].location = APPLY_STR;
      af[0].duration = GET_LEVEL (ch);
      af[0].modifier = -2;
      af[0].bitvector = AFF_POISON;
      to_room = "$n gets violently ill!";
      break;

    case SPELL_SANCTUARY:
      af[0].duration = 4;
      af[0].bitvector = AFF_SANCTUARY;

      accum_duration = TRUE;
      to_room = "$n is surrounded by a white aura.";
      break;

    case SPELL_CONVERGENCE:
      if (IS_AFFECTED(victim, AFF_AUTUS)) {
	send_to_char("A green aura nullifies your magick!\r\n", ch);
	return;
      }else{
	af[0].duration = 4;
	af[0].bitvector = AFF_CONVERGENCE;

	accum_duration = TRUE;
	to_room = "$n is surrounded by a red aura.";
	break;
      }

    case SPELL_AUTUS:
      if (IS_AFFECTED(victim, AFF_CONVERGENCE)){
	send_to_char("A red aura nullifies your magick!\r\n", ch);
	return;
      }else{
	af[0].duration = 4;
	af[0].bitvector = AFF_AUTUS;

	accum_duration = TRUE;
	to_room = "$n is surrounded by a green aura.";
	break;
      }

    case SPELL_SLEEP:
      if (!pk_allowed && !IS_NPC (ch) && !IS_NPC (victim))
	return;
      if (MOB_FLAGGED (victim, MOB_NOSLEEP))
	return;
      if (mag_savingthrow (ch, victim))
	return;

      af[0].duration = 4 + (GET_LEVEL (ch) >> 2);
      af[0].bitvector = AFF_SLEEP;

      if (GET_POS (victim) > POS_SLEEPING)
	{
	  act ("$n goes to sleep.", TRUE, victim, 0, 0, TO_ROOM);
	  GET_POS (victim) = POS_SLEEPING;
	}
      break;

    case SPELL_STRENGTH:
      af[0].location = APPLY_STR;
      af[0].duration = (GET_LEVEL (ch) >> 1) + 4;
      af[0].modifier = 1 + (level > MAX_PLAYER_STAT);
      accum_duration = TRUE;
      accum_affect = TRUE;
      break;

    case SPELL_SENSE_LIFE:
      af[0].duration = GET_LEVEL (ch);
      af[0].bitvector = AFF_SENSE_LIFE;
      accum_duration = TRUE;
      break;

    case SPELL_WATERWALK:
      af[0].duration = 24;
      af[0].bitvector = AFF_WATERWALK;
      accum_duration = TRUE;
      break;

    case SPELL_STONE_SKIN:
      af[0].location = APPLY_DEFENSE;
      af[0].modifier = 20;
      af[0].duration = 24;
      accum_duration = FALSE;

      to_room = "You watch in fascination as $n's skin turns to stone!";
      break;

    case SPELL_RESIST_PORTAL:
        af[0].duration = 16;
        af[0].bitvector = AFF_NOPORTAL;

        accum_duration = TRUE;
        break;

    case SPELL_REDIRECT_CHARGE:
      if (AFF_FLAGGED(victim, AFF_R_CHARGED)) {
        send_to_char("Target is already carrying a charge.\r\n", ch);
        return;
      }
      af[0].location = APPLY_NONE;
      af[0].duration = 24;
      af[0].modifier = 0;
      af[0].bitvector = AFF_REDIRECT_CHARGE;
      to_room = "$n suddenly &Kglows&n.";
      accum_duration = FALSE;
      break;

    default: return;
    }
    to_vict=(char *) spell_affect_msg[spellnum];
  /*
   * If this is a mob that has this affect set in its mob file, do not
   * perform the affect.  This prevents people from un-sancting mobs
   * by sancting them and waiting for it to fade, for example.
   */
  if (IS_NPC (victim) && !affected_by_spell (victim, spellnum))
    for (i = 0; i < MAX_SPELL_AFFECTS; i++)
      if (IS_AFFECTED (victim, af[i].bitvector))
	{
	  send_to_char (NOEFFECT, ch);
	  return;
	}

  /*
   * If the victim is already affected by this spell, and the spell does
   * not have an accumulative effect, then fail the spell.
   */
  if (affected_by_spell (victim, spellnum) && !(accum_duration || accum_affect))
    {
      send_to_char (NOEFFECT, ch);
      return;
    }

  for (i = 0; i < MAX_SPELL_AFFECTS; i++)
    if (af[i].bitvector || (af[i].location != APPLY_NONE))
      affect_join (victim, af + i, accum_duration, FALSE, accum_affect, FALSE);

  if (to_vict != NULL && spellnum!=SPELL_SLEEP)
    act (to_vict, FALSE, victim, 0, ch, TO_CHAR);
  if (to_room != NULL)
    act (to_room, TRUE, victim, 0, ch, TO_ROOM);
}


/*
 * This function is used to provide services to mag_groups.  This function
 * is the one you should change to add new group spells.
 */

void 
perform_mag_groups (int level, struct char_data *ch,
		    struct char_data *tch, int spellnum)
{
  switch (spellnum)
    {
    case SPELL_GROUP_HEAL:
      mag_points (level, ch, tch, SPELL_HEAL);
      break;
    case SPELL_GROUP_ARMOR:
      mag_affects (level, ch, tch, SPELL_ARMOR);
      break;
    case SPELL_GROUP_RECALL:
      spell_recall (level, ch, tch, NULL);
      break;
    case SPELL_GROUP_STONE_SKIN:
      mag_affects (level, ch, tch, SPELL_STONE_SKIN);
      break;
    }
}


/*
 * Every spell that affects the group should run through here
 * perform_mag_groups contains the switch statement to send us to the right
 * magic.
 *
 * group spells affect everyone grouped with the caster who is in the room,
 * caster last.
 *
 * To add new group spells, you shouldn't have to change anything in
 * mag_groups -- just add a new case to perform_mag_groups.
 */

void 
mag_groups (int level, struct char_data *ch, int spellnum)
{
  struct char_data *tch, *k;
  struct follow_type *f, *f_next;

  if (ch == NULL)
    return;

  if (!IS_AFFECTED (ch, AFF_GROUP))
    return;
  if (ch->master != NULL)
    k = ch->master;
  else
    k = ch;
  for (f = k->followers; f; f = f_next)
    {
      f_next = f->next;
      tch = f->follower;
      if (tch->in_room != ch->in_room)
	continue;
      if (!IS_AFFECTED (tch, AFF_GROUP))
	continue;
      if (ch == tch)
	continue;
      perform_mag_groups (level, ch, tch, spellnum);
    }

  if ((k != ch) && IS_AFFECTED (k, AFF_GROUP))
    perform_mag_groups (level, ch, k, spellnum);
  perform_mag_groups (level, ch, ch, spellnum);
}


/*
 * mass spells affect every creature in the room except the caster.
 *
 * No spells of this class currently implemented as of Circle 3.0.
 */

void 
mag_masses (int level, struct char_data *ch, int spellnum)
{
  struct char_data *tch, *tch_next;

  for (tch = world[ch->in_room].people; tch; tch = tch_next)
    {
      tch_next = tch->next_in_room;
      if (tch == ch)
	continue;

      switch (spellnum)
	{
	}
    }
}


/*
 * Every spell that affects an area (room) runs through here.  These are
 * generally offensive spells.  This calls mag_damage to do the actual
 * damage -- all spells listed here must also have a case in mag_damage()
 * in order for them to work.
 *
 *  area spells have limited targets within the room.
 */

void 
mag_areas (int level, struct char_data *ch, int spellnum)
{
  struct char_data *tch, *next_tch;
  char *to_char = NULL;
  char *to_room = NULL;

  if (ch == NULL)
    return;

  /*
   * to add spells to this fn, just add the message here plus an entry
   * in mag_damage for the damaging part of the spell.
   */
  switch (spellnum)
    {
    case SPELL_EARTHQUAKE:
      to_char = "You gesture and the earth begins to shake all around you!";
      to_room = "$n gracefully gestures and the earth begins to shake violently!";
      break;
    }

  if (to_char != NULL)
    act (to_char, FALSE, ch, 0, 0, TO_CHAR);
  if (to_room != NULL)
    act (to_room, FALSE, ch, 0, 0, TO_ROOM);


  for (tch = world[ch->in_room].people; tch; tch = next_tch)
    {
      next_tch = tch->next_in_room;

      /*
       * The skips: 1: the caster
       *            2: immortals
       *            3: if no pk on this mud, skips over all players
       *            4: pets (charmed NPCs)
       * players can only hit players in CRIMEOK rooms 4) players can only hit
       * charmed mobs in CRIMEOK rooms
       */

      if (tch == ch)
	continue;
      if (!IS_NPC (tch) && GET_LEVEL (tch) >= LVL_IMMORT)
	continue;
      if (!pk_allowed && !IS_NPC (ch) && !IS_NPC (tch))
	continue;
      if (!IS_NPC (ch) && IS_NPC (tch) && IS_AFFECTED (tch, AFF_CHARM))
	continue;

      mag_damage (GET_LEVEL (ch), ch, tch, spellnum);
    }
}


/*
 *  Every spell which summons/gates/conjours a mob comes through here.
 *
 *  None of these spells are currently implemented in Circle 3.0; these
 *  were taken as examples from the JediMUD code.  Summons can be used
 *  for spells like clone, ariel servant, etc.
 *
 * 10/15/97 (gg) - Implemented Animate Dead and Clone.
 */

/*
 * These use act(), don't put the \r\n.
 */
static char *mag_summon_msgs[] =
{
  "\r\n",
  "$n makes a strange magical gesture; you feel a strong breeze!",
  "$n animates a corpse!",
  "$N appears from a cloud of thick blue smoke!",
  "$N appears from a cloud of thick green smoke!",
  "$N appears from a cloud of thick red smoke!",
  "$N disappears in a thick black cloud!"
  "As $n makes a strange magical gesture, you feel a strong breeze.",
  "As $n makes a strange magical gesture, you feel a searing heat.",
  "As $n makes a strange magical gesture, you feel a sudden chill.",
  "As $n makes a strange magical gesture, you feel the dust swirl.",
  "$n magically divides!",
  "$n animates a corpse!"
};

/*
 * Keep the \r\n because these use send_to_char.
 */
static char *mag_summon_fail_msgs[] =
{
  "\r\n",
  "There are no such creatures.\r\n",
  "Your attempt to raise the dead failed.\r\n",
  "The elemental forces were not powerful enough.\r\n",
  "It did not work..\r\n",
  "The elements resist!\r\n",
  "You failed.\r\n",
  "There is no corpse!\r\n"
};

/* These mobiles do not exist. */
#define MOB_MONSUM_I		130
#define MOB_MONSUM_II		140
#define MOB_MONSUM_III		150
#define MOB_GATE_I		160
#define MOB_GATE_II		170
#define MOB_GATE_III		180

/* Defined mobiles. */
#define MOB_ELEMENTAL_BASE	20	/* Only one for now. */
#define MOB_CLONE		10
#define MOB_ZOMBIE		11
#define MOB_AERIALSERVANT	19


void 
mag_summons (int level, struct char_data *ch, struct obj_data *obj,
	     int spellnum)
{
  struct char_data *mob = NULL;
  struct obj_data *tobj, *next_obj;
  int pfail = 0, msg = 0, fmsg = 0, mob_num = 0, num = 1, handle_corpse = FALSE, i;
  void load_mtrigger (struct char_data *);

  if (ch == NULL)
    return;

  switch (spellnum)
    {
    case SPELL_CLONE:
      msg = 10;
      fmsg = number (2, 6);	/* Random fail message. */
      mob_num = MOB_CLONE;
      pfail = 50;		/* 50% failure, should be based on something later. */
      break;

    case SPELL_ANIMATE_DEAD:
      if (obj == NULL || !IS_CORPSE (obj))
	{
	  act (mag_summon_fail_msgs[7], FALSE, ch, 0, 0, TO_CHAR);
	  return;
	}
      handle_corpse = TRUE;
      msg = 11;
      fmsg = number (2, 6);	/* Random fail message. */
      mob_num = MOB_ZOMBIE;
      pfail = 10;		/* 10% failure, should vary in the future. */
      break;

    default:
      return;
    }

  if (IS_AFFECTED (ch, AFF_CHARM))
    {
      send_to_char ("You are too giddy to have any followers!\r\n", ch);
      return;
    }
  if (number (0, 101) < pfail)
    {
      send_to_char (mag_summon_fail_msgs[fmsg], ch);
      return;
    }
  for (i = 0; i < num; i++)
    {
      if (!(mob = read_mobile (mob_num, VIRTUAL)))
	{
	  send_to_char ("You don't quite remember how to make that creature.\r\n", ch);
	  return;
	}
      char_to_room (mob, ch->in_room);
      IS_CARRYING_W (mob) = 0;
      IS_CARRYING_N (mob) = 0;
      SET_BIT (AFF_FLAGS (mob), AFF_CHARM);
      if (spellnum == SPELL_CLONE)
	{			/* Don't mess up the proto with strcpy. */
	  mob->player.name = str_dup (GET_NAME (ch));
	  mob->player.short_descr = str_dup (GET_NAME (ch));
	}
      act (mag_summon_msgs[msg], FALSE, ch, 0, mob, TO_ROOM);
      load_mtrigger(mob);
      add_follower (mob, ch);
    }
  if (handle_corpse)
    {
      for (tobj = obj->contains; tobj; tobj = next_obj)
	{
	  next_obj = tobj->next_content;
	  obj_from_obj (tobj);
	  obj_to_char (tobj, mob);
	}
      extract_obj (obj);
    }
}


void 
mag_points (int level, struct char_data *ch, struct char_data *victim,
	    int spellnum)
{
  int hit = 0;
  int move = 0;
  int mana = 0;

  if (victim == NULL)
    return;

  switch (spellnum)
    {
    case SPELL_CURE_LIGHT:
      hit = dice (1, 8) + 1 + (level >> 2);
      send_to_char ("You feel better.\r\n", victim);
      break;
    case SPELL_CURE_CRITIC:
      hit = dice (3, 8) + 3 + (level >> 2);
      send_to_char ("You feel a lot better!\r\n", victim);
      break;
    case SPELL_HEAL:
      hit = 100 + dice (3, 8);
      send_to_char ("A warm feeling floods your body.\r\n", victim);
      break;
    case SPELL_REGEN_MANA:
      mana = 150;
      send_to_char ("A tingling sensation floods your body.\r\n", victim);
      break;
    }
  GET_HIT (victim) = MIN (GET_MAX_HIT (victim), GET_HIT (victim) + hit);
  GET_MANA (victim) = MIN (GET_MAX_MANA (victim), GET_MANA (victim) + mana);
  GET_MOVE (victim) = MIN (GET_MAX_MOVE (victim), GET_MOVE (victim) + move);
  update_pos (victim);
}


void 
mag_unaffects (int level, struct char_data *ch, struct char_data *victim,
	       int spellnum)
{
  int spell = 0;
  char *to_vict = NULL, *to_room = NULL;
  extern bool check_perm_duration (struct char_data *ch, long bitvector);
  if (victim == NULL)
    return;

  switch (spellnum)
    {
    case SPELL_CURE_BLIND:
    case SPELL_HEAL:
      if (check_perm_duration(ch, AFF_BLIND)) { if (spellnum!=SPELL_HEAL) send_to_char (NOEFFECT, ch);
return; }
      spell = SPELL_BLINDNESS;
      to_vict = "Your vision returns!";
      to_room = "There's a momentary gleam in $n's eyes.";
      break;
    case SPELL_REMOVE_POISON:
      if (check_perm_duration(ch, AFF_POISON)) { send_to_char (NOEFFECT, ch); return; }
      spell = SPELL_POISON;
      to_vict = "A warm feeling runs through your body!";
      to_room = "$n looks better.";
      break;
    case SPELL_REMOVE_CURSE:
      if (check_perm_duration(ch, AFF_CURSE)) { send_to_char (NOEFFECT, ch); return; }
      spell = SPELL_CURSE;
      to_vict = "You don't feel so unlucky.";
      break;
    default:
      sprintf (buf, "SYSERR: unknown spellnum %d passed to mag_unaffects", spellnum);
      log (buf);
      return;
      break;
    }

  if (!affected_by_spell (victim, spell))
    {
      send_to_char (NOEFFECT, ch);
      return;
    }

  affect_from_char (victim, spell);
  if (to_vict != NULL)
    act (to_vict, FALSE, victim, 0, ch, TO_CHAR);
  if (to_room != NULL)
    act (to_room, TRUE, victim, 0, ch, TO_ROOM);

}


void 
mag_alter_objs (int level, struct char_data *ch, struct obj_data *obj,
		int spellnum)
{
  char *to_char = NULL;
  char *to_room = NULL;

  if (obj == NULL)
    return;

  switch (spellnum)
    {
    case SPELL_BLESS:
      if (!IS_OBJ_STAT (obj, ITEM_BLESS) &&
	  (GET_OBJ_WEIGHT (obj) <= 5 * GET_LEVEL (ch)))
	{
	  SET_BIT (GET_OBJ_EXTRA (obj), ITEM_BLESS);
	  to_char = "$p glows briefly.";
	}
      break;
    case SPELL_CURSE:
      if (!IS_OBJ_STAT (obj, ITEM_NODROP))
	{
	  SET_BIT (GET_OBJ_EXTRA (obj), ITEM_NODROP);
	  if (GET_OBJ_TYPE (obj) == ITEM_WEAPON)
	    GET_OBJ_VAL (obj, 2)--;
	  to_char = "$p briefly glows red.";
	}
      break;
    case SPELL_INVISIBLE:
      if (!IS_OBJ_STAT (obj, ITEM_NOINVIS | ITEM_INVISIBLE))
	{
	  SET_BIT (obj->obj_flags.extra_flags, ITEM_INVISIBLE);
	  to_char = "$p vanishes.";
	}
      break;
    case SPELL_POISON:
      if (((GET_OBJ_TYPE (obj) == ITEM_DRINKCON) ||
	   (GET_OBJ_TYPE (obj) == ITEM_FOUNTAIN) ||
	   (GET_OBJ_TYPE (obj) == ITEM_FOOD)) && !GET_OBJ_VAL (obj, 3))
	{
	  GET_OBJ_VAL (obj, 3) = 1;
	  to_char = "$p steams briefly.";
	}
      break;
    case SPELL_REMOVE_CURSE:
      if (IS_OBJ_STAT (obj, ITEM_NODROP))
	{
	  REMOVE_BIT (obj->obj_flags.extra_flags, ITEM_NODROP);
	  if (GET_OBJ_TYPE (obj) == ITEM_WEAPON)
	    GET_OBJ_VAL (obj, 2)++;
	  to_char = "$p briefly glows blue.";
	}
      break;
    case SPELL_REMOVE_POISON:
      if (((GET_OBJ_TYPE (obj) == ITEM_DRINKCON) ||
	   (GET_OBJ_TYPE (obj) == ITEM_FOUNTAIN) ||
	   (GET_OBJ_TYPE (obj) == ITEM_FOOD)) && GET_OBJ_VAL (obj, 3))
	{
	  GET_OBJ_VAL (obj, 3) = 0;
	  to_char = "$p steams briefly.";
	}
      break;
    }

  if (to_char == NULL)
    send_to_char (NOEFFECT, ch);
  else
    act (to_char, TRUE, ch, obj, 0, TO_CHAR);

  if (to_room != NULL)
    act (to_room, TRUE, ch, obj, 0, TO_ROOM);
  else if (to_char != NULL)
    act (to_char, TRUE, ch, obj, 0, TO_ROOM);

}



void 
mag_creations (int level, struct char_data *ch, int spellnum)
{
  struct obj_data *tobj;
  int z;
  void load_otrigger (struct obj_data *);

  if (ch == NULL)
    return;
  level = MAX (MIN (level, LVL_IMPL), 1);

  switch (spellnum)
    {
    case SPELL_CREATE_FOOD:
      z = 10;
      break;
    default:
      send_to_char ("Spell unimplemented, it would seem.\r\n", ch);
      return;
      break;
    }

  if (!(tobj = read_object (z, VIRTUAL)))
    {
      send_to_char ("I seem to have goofed.\r\n", ch);
      sprintf (buf, "SYSERR: spell_creations, spell %d, obj %d: obj not found",
	       spellnum, z);
      log (buf);
      return;
    }
  obj_to_char (tobj, ch);
  act ("$n creates $p.", FALSE, ch, tobj, 0, TO_ROOM);
  act ("You create $p.", FALSE, ch, tobj, 0, TO_CHAR);
  load_otrigger(tobj);
}
