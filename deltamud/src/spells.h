/* ************************************************************************
*   File: spells.h                                      Part of CircleMUD *
*  Usage: header file: constants and fn prototypes for spell system       *
*                                                                         *
*  All rights reserved.  See license.doc for complete information.        *
*                                                                         *
*  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
*  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
************************************************************************ */

#define DEFAULT_STAFF_LVL	12
#define DEFAULT_WAND_LVL	12

#define MAG_DAMAGE	(1 << 0)
#define MAG_AFFECTS	(1 << 1)
#define MAG_UNAFFECTS	(1 << 2)
#define MAG_POINTS	(1 << 3)
#define MAG_ALTER_OBJS	(1 << 4)
#define MAG_GROUPS	(1 << 5)
#define MAG_MASSES	(1 << 6)
#define MAG_AREAS	(1 << 7)
#define MAG_SUMMONS	(1 << 8)
#define MAG_CREATIONS	(1 << 9)
#define MAG_MANUAL	(1 << 10)


#define TYPE_UNDEFINED               -1
#define SPELL_RESERVED_DBC            0  /* SKILL NUMBER ZERO -- RESERVED */

/* PLAYER SPELLS -- Numbered from 1 to MAX_SPELLS */

#define SPELL_ARMOR                   1 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_TELEPORT                2 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_BLESS                   3 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_BLINDNESS               4 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CHARM                   5 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CLONE                   6 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CONTROL_WEATHER         7 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CREATE_FOOD             8 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CREATE_WATER            9 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CURE_BLIND             10 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CURE_CRITIC            11 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CURE_LIGHT             12 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_CURSE                  13 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_DETECT_ALIGN           14 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_DETECT_INVIS           15 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_DETECT_MAGIC           16 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_DETECT_POISON          17 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_EARTHQUAKE             18 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_ENCHANT_WEAPON         19 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_HEAL                   20 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_INVISIBLE              21 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_LOCATE_OBJECT          22 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_POISON                 23 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_REMOVE_CURSE           24 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_SANCTUARY              25 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_SLEEP                  26 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_STRENGTH               27 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_SUMMON                 28 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_WORD_OF_RECALL         29 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_REMOVE_POISON          30 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_SENSE_LIFE             31 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_ANIMATE_DEAD	     32 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_GROUP_ARMOR	     33 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_GROUP_HEAL	     34 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_GROUP_RECALL	     35 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_INFRAVISION	     36 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_WATERWALK		     37 /* Reserved Skill[] DO NOT CHANGE */
#define SPELL_STONE_SKIN  	     38 /* Implemented by Mulder */
#define SPELL_FEAR                   39 /* Implemented by Mulder */
#define SPELL_RECHARGE               40 /* Implemented by Mulder */
#define SPELL_PORTAL                 41 /* Implemented by Mulder */
#define SPELL_GROUP_STONE_SKIN       42 /* Implemented by Mulder */
#define SPELL_LOCATE_TARGET          43 /* Implemented by Mulder */
#define SPELL_CONVERGENCE            44 /* Implemented by Thargor */
#define SPELL_AUTUS                  45 /* Implemented by Thargor */
#define SPELL_RESIST_PORTAL          46 /* Implemented by Mulder */
#define SPELL_REGEN_MANA             47 /* Implemented by Thargor */
#define SPELL_HOME                   48 /* Implemented by Thargor */
#define SPELL_WORD_OF_RETREAT        49 /* Implemented by Mulder */
#define SKILL_CHAIN_FOOTING          50 /* Implemented by Storm - This skill is here because it gives -player- an affect and is not a *normal* skill, heh. */
#define SPELL_REDIRECT_CHARGE	     51 /* Implemented by Storm */

/* Insert new spells here, up to MAX_SPELLS */
#define MAX_SPELLS		    500

/* PLAYER SKILLS - Numbered from MAX_SPELLS+1 to MAX_SKILLS */
#define SKILL_BACKSTAB              501 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_BASH                  502 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_HIDE                  503 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_KICK                  504 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_PICK_LOCK             505 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_PUNCH                 506 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_RESCUE                507 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_SNEAK                 508 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_STEAL                 509 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_TRACK		    510 /* Reserved Skill[] DO NOT CHANGE */
#define SKILL_FORAGE                511 /* Implemented by Mulder */
#define SKILL_SCAN                  512 /* Implemented by Mulder */
#define SKILL_BREW                  513 /* Implemented by Mulder */
#define SKILL_FORGE                 514 /* Implemented by Mulder */
#define SKILL_SCRIBE                515 /* Implemented by Mulder */
#define SKILL_SPEED                 516 /* Implemented by Mulder */
#define SKILL_BERSERK               517 /* Implemented by Thargor */
#define SKILL_CAMOUFLAGE            518 /* Implemented by Thargor */
#define SKILL_BLANKET               519 /* Implemented by Thargor */
#define SKILL_RAM_DOOR              520 /* Implemented by Thargor */
#define SKILL_MOUNT                 521 /* Implemented by Mulder */
#define SKILL_RIDING                522 /* Implemented by Mulder */
#define SKILL_TAME                  523 /* Implemented by Mulder */
#define SKILL_SECOND_ATTACK         524 /* Implemented by Mulder */
#define SKILL_THIRD_ATTACK          525 /* Implemented by Mulder */
#define SKILL_LISTEN                526 /* Implemented by Mulder */
#define SKILL_MEDITATE              527 /* Implemented by Thargor */
#define SKILL_REPAIR                528 /* Implemented by Mulder */
#define SKILL_TAN		    529 /* Implemented by Mulder */
#define SKILL_FILLET		    530 /* Implemented by Mulder */
#define SKILL_CARVE		    531 /* Implemented by Mulder */
#define SKILL_DODGE                 532 /* Implemented by Mulder */
#define SKILL_PARRY                 533 /* Implemented by Mulder */
#define SKILL_AVOID                 534 /* Implemented by Mulder */
#define SKILL_RIPOSTE               535 /* Implemented by Mulder */
#define SKILL_CIRCLE                536 /* Implemented by Mulder */  
#define SKILL_TRIP                  537 /* Implemented by Mulder */
#define SKILL_DISARM                538 /* Implemented by Mulder */
#define SKILL_TARGET                539 /* Implemented by Mulder */
#define SKILL_ADRENALINE            540 /* These skills MUST be  */
#define SKILL_BLOODLUST             541 /* in succession to each */
#define SKILL_CARNALRAGE            542 /* other!                */

/* New skills may be added here up to MAX_SKILLS (1000) */

#define AVOID_FACTOR 20

/*
 *  NON-PLAYER AND OBJECT SPELLS AND SKILLS
 *  The practice levels for the spells and skills below are _not_ recorded
 *  in the playerfile; therefore, the intended use is for spells and skills
 *  associated with objects (such as SPELL_IDENTIFY used with scrolls of
 *  identify) or non-players (such as NPC-only spells).
 */

#define SPELL_IDENTIFY               1001
#define SPELL_FIRE_BREATH            1002
#define SPELL_GAS_BREATH             1003
#define SPELL_FROST_BREATH           1004
#define SPELL_ACID_BREATH            1005
#define SPELL_LIGHTNING_BREATH       1006

#define TOP_SPELL_DEFINE	     1099
/* NEW NPC/OBJECT SPELLS can be inserted here up to 1099 */


/* WEAPON ATTACK TYPES */

#define TYPE_HIT                     1100
#define TYPE_STING                   1101
#define TYPE_WHIP                    1102
#define TYPE_SLASH                   1103
#define TYPE_BITE                    1104
#define TYPE_BLUDGEON                1105
#define TYPE_CRUSH                   1106
#define TYPE_POUND                   1107
#define TYPE_CLAW                    1108
#define TYPE_MAUL                    1109
#define TYPE_THRASH                  1110
#define TYPE_PIERCE                  1111
#define TYPE_BLAST		     1112
#define TYPE_PUNCH		     1113
#define TYPE_STAB		     1114


/* new attack types can be added here - up to SELF_DAMAGE (damages inflicted from the game and not from another player) */
#define SELF_DAMAGE		     1197
#define TYPE_STARVING		     1197
#define TYPE_DROWNING		     1198
#define TYPE_SUFFERING		     1199

#define TAR_IGNORE        1
#define TAR_CHAR_ROOM     2
#define TAR_CHAR_WORLD    4
#define TAR_FIGHT_SELF    8
#define TAR_FIGHT_VICT   16
#define TAR_SELF_ONLY    32 /* Only a check, use with i.e. TAR_CHAR_ROOM */
#define TAR_NOT_SELF     64 /* Only a check, use with i.e. TAR_CHAR_ROOM */
#define TAR_OBJ_INV     128
#define TAR_OBJ_ROOM    256
#define TAR_OBJ_WORLD   512
#define TAR_OBJ_EQUIP  1024

struct spell_info_type {
   byte min_position;	/* Position for caster	 */
   int mana_min;	/* Min amount of mana used by a spell (highest lev) */
   int mana_max;	/* Max amount of mana used by a spell (lowest lev) */
   int mana_change;	/* Change in mana used by spell from lev to lev */

   int min_level[NUM_CLASSES];
   int routines;
   byte violent;
   int targets;         /* See below for use with TAR_XXX  */
};

/* Possible Targets:

   bit 0 : IGNORE TARGET
   bit 1 : PC/NPC in room
   bit 2 : PC/NPC in world
   bit 3 : Object held
   bit 4 : Object in inventory
   bit 5 : Object in room
   bit 6 : Object in world
   bit 7 : If fighting, and no argument, select tar_char as self
   bit 8 : If fighting, and no argument, select tar_char as victim (fighting)
   bit 9 : If no argument, select self, if argument check that it IS self.

*/

#define SPELL_TYPE_SPELL   0
#define SPELL_TYPE_POTION  1
#define SPELL_TYPE_WAND    2
#define SPELL_TYPE_STAFF   3
#define SPELL_TYPE_SCROLL  4


/* Attacktypes with grammar */

struct attack_hit_type {
   char	*singular;
   char	*plural;
};


#define ASPELL(spellname) \
void	spellname(int level, struct char_data *ch, \
		  struct char_data *victim, struct obj_data *obj)

#define MANUAL_SPELL(spellname)	spellname(level, caster, cvict, ovict);

ASPELL(spell_create_water);
ASPELL(spell_recall);
ASPELL(spell_teleport);
ASPELL(spell_summon);
ASPELL(spell_locate_object);
ASPELL(spell_charm);
ASPELL(spell_information);
ASPELL(spell_identify);
ASPELL(spell_enchant_weapon);
ASPELL(spell_detect_poison);
ASPELL(spell_fear);
ASPELL(spell_recharge);
ASPELL(spell_portal);

/* basic magic calling functions */

int find_skill_num(char *name);

void mag_damage(int level, struct char_data *ch, struct char_data *victim,
  int spellnum);

void mag_affects(int level, struct char_data *ch, struct char_data *victim,
  int spellnum);

void mag_group_switch(int level, struct char_data *ch, struct char_data *tch, 
  int spellnum);

void mag_groups(int level, struct char_data *ch, int spellnum);

void mag_masses(int level, struct char_data *ch, int spellnum);

void mag_areas(int level, struct char_data *ch, int spellnum);

void mag_summons(int level, struct char_data *ch, struct obj_data *obj,
 int spellnum);

void mag_points(int level, struct char_data *ch, struct char_data *victim,
 int spellnum);

void mag_unaffects(int level, struct char_data *ch, struct char_data *victim,
  int spellnum);

void mag_alter_objs(int level, struct char_data *ch, struct obj_data *obj,
  int spellnum);

void mag_creations(int level, struct char_data *ch, int spellnum);

int	call_magic(struct char_data *caster, struct char_data *cvict,
  struct obj_data *ovict, int spellnum, int level);

void	mag_objectmagic(struct char_data *ch, struct obj_data *obj,
			char *argument);

int	cast_spell(struct char_data *ch, struct char_data *tch,
  struct obj_data *tobj, int spellnum);


/* other prototypes */
void spell_level(int spell, int class, int level);
void init_spell_levels(void);
char *skill_name(int num);
