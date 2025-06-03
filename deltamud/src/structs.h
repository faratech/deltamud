/* ************************************************************************ * File: structs.h Part of CircleMUD * *
Usage:
header file for central structures and contstants * * * * All rights reserved.  See license.doc for complete information.  * *
* * Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University * * CircleMUD is based on DikuMUD, Copyright (C)
1990, 1991.  * ************************************************************************ */


/* preamble *************************************************************/

#define NOWHERE    -1    /* nil reference for room-database	*/
#define NOTHING	   -1    /* nil reference for objects		*/
#define NOBODY	   -1    /* nil reference for mobiles		*/


#define MAX_STAT 25
#define MAX_PLAYER_STAT 18

#define SPECIAL(name) \
   int (name)(struct char_data *ch, void *me, int cmd, char *argument)

/* misc editor defines **************************************************/
/* ignore type defines */
#define IGNORE_PUBLIC (1 << 0)
#define IGNORE_PRIVATE (1 << 1)
#define IGNORE_EMOTE (1 << 2)

/* format modes for format_text */
#define FORMAT_INDENT		(1 << 0)


#define GOD_CMD           -1    /* command is usable only by gods (godcmd1) */
#define GOD_CMD2          -2    /* command is usable only by gods (godcmd2) */
#define MOB_CMD           -3    /* command is usable only by mobs */
#define DISABLED_CMD      -4    /* command is disabled */
#define GOD_CMD3          -5    /* command is usable only by gods (godcmd3) */
#define GOD_CMD4          -6    /* command is usable only by gods (godcmd4) */

#define ARENA_NOT         0
#define ARENA_COMBATANT1  1     /* Arena combatant about to do match #1   */
#define ARENA_COMBATANT1w 2     /* Arena combatant who's done > 1 match   */
#define ARENA_COMBATANT2  3     /* Arena combatant about to do match #2   */
#define ARENA_COMBATANT3  4     /* Arena combatant about to do match #2   */
                       /* Note: If you want to add more matches, put here */
#define ARENA_COMBATANTZ  99    /* Arena combatant who's done all matches */
#define ARENA_OBSERVER    100

/* room-related defines *************************************************/


/* The cardinal directions: used as index to room_data.dir_option[] */
#define NORTH          0
#define EAST           1
#define SOUTH          2
#define WEST           3
#define UP             4
#define DOWN           5


/* Room flags: used in room_data.room_flags */
/* WARNING: In the world files, NEVER set the bits marked "R" ("Reserved") */
#define ROOM_DARK		(1 << 0)   /* Dark			*/
#define ROOM_DEATH		(1 << 1)   /* Death trap		*/
#define ROOM_NOMOB		(1 << 2)   /* MOBs not allowed		*/
#define ROOM_INDOORS		(1 << 3)   /* Indoors			*/
#define ROOM_PEACEFUL		(1 << 4)   /* Violence not allowed	*/
#define ROOM_SOUNDPROOF		(1 << 5)   /* Shouts, gossip blocked	*/
#define ROOM_NOTRACK		(1 << 6)   /* Track won't go through	*/
#define ROOM_NOMAGIC		(1 << 7)   /* Magic not allowed		*/
#define ROOM_TUNNEL		(1 << 8)   /* room for only 1 pers	*/
#define ROOM_PRIVATE		(1 << 9)   /* Can't teleport in		*/
#define ROOM_GODROOM		(1 << 10)  /* LVL_GOD+ only allowed	*/
#define ROOM_HOUSE		(1 << 11)  /* (R) Room is a house	*/
#define ROOM_HOUSE_CRASH	(1 << 12)  /* (R) House needs saving	*/
#define ROOM_ATRIUM		(1 << 13)  /* (R) The door to a house	*/
#define ROOM_OLC		(1 << 14)  /* (R) Modifyable/!compress	*/
#define ROOM_BFS_MARK		(1 << 15)  /* (R) breath-first srch mrk	*/
#define ROOM_IMPROOM            (1 << 16)  /* LVL_IMPL only allowed     */
#define ROOM_BAD_REGEN          (1 << 17)  /* Room is cursed with bad regeneration */
#define ROOM_WALL               (1 << 18)  /* Can't enter if on map */
#define ROOM_CLAN_ROOM          (1 << 19)  /* room is owned by clan */
#define ROOM_GOOD_REGEN         (1 << 20)  /* Produces better regen */

 /* Exit info: used in room_data.dir_option.exit_info *
  * and room_data.special_exit.exit_info              */
#define EX_ISDOOR		(1 << 0)   /* Exit is a door		*/
#define EX_CLOSED		(1 << 1)   /* The door is closed	*/
#define EX_LOCKED		(1 << 2)   /* The door is locked	*/
#define EX_PICKPROOF		(1 << 3)   /* Lock can't be picked	*/
#define EX_HIDDEN		(1 << 4)   /* Exit is hidden */

/* Sector types: used in room_data.sector_type */
#define SECT_INSIDE          0		   /* Indoors			*/
#define SECT_CITY            1		   /* In a city			*/
#define SECT_FIELD           2		   /* In a field		*/
#define SECT_FOREST          3		   /* In a forest		*/
#define SECT_HILLS           4		   /* In the hills		*/
#define SECT_MOUNTAIN        5		   /* On a mountain		*/
#define SECT_WATER_SWIM      6		   /* Swimmable water		*/
#define SECT_WATER_NOSWIM    7		   /* Water - need a boat	*/
#define SECT_UNDERWATER	     8		   /* Underwater		*/
#define SECT_FLYING	     9		   /* Wheee!			*/
#define SECT_ROAD            10            /* On a road                 */
#define SECT_WALL            11            /* Wall (used for map)       */
#define SECT_SWAMP           12            /* In a swamp                */
#define SECT_DESERT          13            /* In a desert               */
#define SECT_BRIDGE          14            /* Bridge (used for map)     */

/* char and mob-related defines *****************************************/


/* PC classes */
#define CLASS_UNDEFINED	  -1
#define CLASS_MAGIC_USER  0
#define CLASS_CLERIC      1
#define CLASS_THIEF       2
#define CLASS_WARRIOR     3
#define CLASS_ARTISAN     4

#define NUM_CLASSES	  5  /* This must be the number of classes!! */
#define NUM_STARTROOMS    3
#define NUM_RACES         9
#define NUM_DEITIES       15
#define TOWN_UNDEFINED    -1

/* NPC classes (currently unused - feel free to implement!) */
#define CLASS_OTHER       0
#define CLASS_UNDEAD      1
#define CLASS_HUMANOID    2
#define CLASS_ANIMAL      3
#define CLASS_DRAGON      4
#define CLASS_GIANT       5

#define RACE_UNDEFINED   -1
#define RACE_HUMAN        0
#define RACE_ELF          1
#define RACE_GNOME        2
#define RACE_DWARF        3
#define RACE_TROLL        4
#define RACE_GOBLIN       5
#define RACE_DROW         6
#define RACE_ORC          7
#define RACE_MINOTAUR     8

/* Sex */
#define SEX_NEUTRAL   0
#define SEX_MALE      1
#define SEX_FEMALE    2

/* God */

#define DEITY_UNDEFINED   -1
#define DEITY_AETOS        0
#define DEITY_CORGUS       1
#define DEITY_LYTHERN      2
#define DEITY_PALLAS       3 
#define DEITY_EROS         4
#define DEITY_CAILIA       5
#define DEITY_MARBIN       6 
#define DEITY_POSEIDON     7
#define DEITY_LELU         8
#define DEITY_CHAOS	   9
#define DEITY_ELESTRA  	  10
#define DEITY_NEMONICA    11
#define DEITY_DURN	  12
#define DEITY_RANUS   	  13
#define DEITY_INCUBUS 	  14

/* Positions */
#define POS_DEAD       0	/* dead			*/
#define POS_MORTALLYW  1	/* mortally wounded	*/
#define POS_INCAP      2	/* incapacitated	*/
#define POS_STUNNED    3	/* stunned		*/
#define POS_SLEEPING   4	/* sleeping		*/
#define POS_RESTING    5	/* resting		*/
#define POS_MEDITATING 6	/* meditating		*/
#define POS_SITTING    7	/* sitting		*/
#define POS_FIGHTING   8	/* fighting		*/
#define POS_STANDING   9	/* standing		*/


/* Player flags: used by char_data.char_specials.act */
#define PLR_KILLER	(1 << 0)   /* Player is a player-killer		*/
#define PLR_THIEF	(1 << 1)   /* Player is a player-thief		*/
#define PLR_FROZEN	(1 << 2)   /* Player is frozen			*/
#define PLR_DONTSET     (1 << 3)   /* Don't EVER set (ISNPC bit)	*/
#define PLR_WRITING	(1 << 4)   /* Player writing (board/mail/olc)	*/
#define PLR_MAILING	(1 << 5)   /* Player is writing mail		*/
#define PLR_CRASH	(1 << 6)   /* Player needs to be crash-saved	*/
#define PLR_SITEOK	(1 << 7)   /* Player has been site-cleared	*/
#define PLR_NOSHOUT	(1 << 8)   /* Player not allowed to shout/goss	*/
#define PLR_NOTITLE	(1 << 9)   /* Player not allowed to set title	*/
#define PLR_DELETED	(1 << 10)  /* Player deleted - space reusable	*/
#define PLR_UNUSED01	(1 << 11)  /* Player uses nonstandard loadroom	*/
#define PLR_NOWIZLIST	(1 << 12)  /* Player shouldn't be on wizlist	*/
#define PLR_NODELETE	(1 << 13)  /* Player shouldn't be deleted	*/
#define PLR_INVSTART	(1 << 14)  /* Player should enter game wizinvis	*/
#define PLR_CRYO	(1 << 15)  /* Player is cryo-saved (purge prog)	*/
#define PLR_QUESTOR     (1 << 16)  /* Player is currently on a quest    */
#define PLR_MULTIOK     (1 << 17)  /* Player can multi-play             */
#define PLR_MBUILDER    (1 << 18)  /* Player is a mortal builder        */

/* Mobile flags: used by char_data.char_specials.act */
#define MOB_SPEC         (1 << 0)  /* Mob has a callable spec-proc	*/
#define MOB_SENTINEL     (1 << 1)  /* Mob should not move		*/
#define MOB_SCAVENGER    (1 << 2)  /* Mob picks up stuff on the ground	*/
#define MOB_ISNPC        (1 << 3)  /* (R) Automatically set on all Mobs	*/
#define MOB_AWARE	 (1 << 4)  /* Mob can't be backstabbed		*/
#define MOB_AGGRESSIVE   (1 << 5)  /* Mob hits players in the room	*/
#define MOB_STAY_ZONE    (1 << 6)  /* Mob shouldn't wander out of zone	*/
#define MOB_WIMPY        (1 << 7)  /* Mob flees if severely injured	*/
#define MOB_AGGR_EVIL	 (1 << 8)  /* auto attack evil PC's		*/
#define MOB_AGGR_GOOD	 (1 << 9)  /* auto attack good PC's		*/
#define MOB_AGGR_NEUTRAL (1 << 10) /* auto attack neutral PC's		*/
#define MOB_MEMORY	 (1 << 11) /* remember attackers if attacked	*/
#define MOB_HELPER	 (1 << 12) /* attack PCs fighting other NPCs	*/
#define MOB_NOCHARM	 (1 << 13) /* Mob can't be charmed		*/
#define MOB_NOSUMMON	 (1 << 14) /* Mob can't be summoned		*/
#define MOB_NOSLEEP	 (1 << 15) /* Mob can't be slept		*/
#define MOB_NOBASH	 (1 << 16) /* Mob can't be bashed (e.g. trees)	*/
#define MOB_NOBLIND	 (1 << 17) /* Mob can't be blinded		*/
#define MOB_QUESTMASTER  (1 << 18) /* Mob is a questmaster              */
#define MOB_QUEST        (1 << 19) /* Mob is a quest target             */
#define MOB_MOUNTABLE    (1 << 20) /* Mob can be mounted                */
#define MOB_CASTER       (1 << 21) /* Mob is a spell caster             */
#define MOB_DBLATTACK    (1 << 22) /* Mob can do second attack          */
#define MOB_NOTALK       (1 << 23) /* Mob can't talk at all */
#define MOB_MERCY        (1 << 24) /* Mob won't kill your ass */
#define MOB_DEITY	 (1 << 25) /* Mob is a god. */
#define MOB_PCHELPER_GOOD (1 << 26) /* Mob assists good PC's instead of mobs */
#define MOB_PCHELPER_NEUT (1 << 27) /* Mob assists neutral PC's instead of mobs */
#define MOB_PCHELPER_EVIL (1 << 28) /* Mob assists evil PC's instead of mobs */
#define MOB_BANKER	(1 << 29)  /* Mob is a banker */

/* Preference flags: used by char_data.player_specials.pref */
#define PRF_BRIEF       (1 << 0)  /* Room descs won't normally be shown	*/
#define PRF_COMPACT     (1 << 1)  /* No extra CRLF pair before prompts	*/
#define PRF_DEAF	(1 << 2)  /* Can't hear shouts			*/
#define PRF_NOTELL	(1 << 3)  /* Can't receive tells		*/
#define PRF_DISPHP	(1 << 4)  /* Display hit points in prompt	*/
#define PRF_DISPMANA	(1 << 5)  /* Display mana points in prompt	*/
#define PRF_DISPMOVE	(1 << 6)  /* Display move points in prompt	*/
#define PRF_AUTOEXIT	(1 << 7)  /* Display exits in a room		*/
#define PRF_NOHASSLE	(1 << 8)  /* Aggr mobs won't attack		*/
#define PRF_NOARENA	(1 << 9) /* Can't hear arena channel		*/
#define PRF_SUMMONABLE	(1 << 10) /* Can be summoned			*/
#define PRF_NOREPEAT	(1 << 11) /* No repetition of comm commands	*/
#define PRF_HOLYLIGHT	(1 << 12) /* Can see in dark			*/
#define PRF_COLOR_1	(1 << 13) /* Color (low bit)			*/
#define PRF_COLOR_2	(1 << 14) /* Color (high bit)			*/
#define PRF_NOWIZ	(1 << 15) /* Can't hear wizline			*/
#define PRF_LOG1	(1 << 16) /* On-line System Log (low bit)	*/
#define PRF_LOG2	(1 << 17) /* On-line System Log (mid bit)	*/
#define PRF_NOAUCT	(1 << 18) /* Can't hear auction channel		*/
#define PRF_NOGOSS	(1 << 19) /* Can't hear gossip channel		*/
#define PRF_NOGRATZ	(1 << 20) /* Can't hear grats channel		*/
#define PRF_ROOMFLAGS	(1 << 21) /* Can see room flags (ROOM_x)	*/
#define PRF_AFK         (1 << 22) /* Away from Keyboard                 */
#define PRF_AUTOSPLIT   (1 << 23) /* Automatically Split gold           */
#define PRF_AUTOLOOT    (1 << 24) /* Automatically Loot corpse          */
#define PRF_AUTOGOLD    (1 << 25) /* Automatically Loot gold            */
#define PRF_DISPEXP     (1 << 26) /* Display experience on prompt       */
#define PRF_NOTIC       (1 << 27) /* Can't hear bells of Anacreon       */
#define PRF_UNUSED       (1 << 28) /* Currently Unused */
#define PRF_LOG3        (1 << 29) /* On-line System Log (high bit)      */
#define PRF_NOLOOKSTACK (1 << 30) /* No mob stacking                    */

     /* Reminder: only up to 0-30 in a (signed) bitvector. need a new one */

#define PRF2_QCHAN	(1 << 0)  /* On quest channel 		        */
#define PRF2_LOCKOUT	(1 << 1)  /* Keyboard lockout			*/
#define PRF2_NOMAP      (1 << 2)  /* Don't display the world map       */
#define PRF2_CTITLE      (1 << 3)  /* Show clan name in who list */
#define PRF2_CLAN_TALK   (1 << 4)  /* Can hear clan channel */
#define PRF2_DISPMOB     (1 << 5)  /* Display fight info */
#define PRF2_MBUILDING   (1 << 6)  /* Player is building */
#define PRF2_MERCY       (1 << 7)  /* Player is merciful to enemies, what a nice guy :) */
#define PRF2_ADVANCEDMAP (1 << 8)  /* Player sees the advanced map. */
#define PRF2_INTANGIBLE  (1 << 9)  /* Player is a ghost. */

/* Affect bits: used in char_data.char_specials.saved.affected_by */
/* WARNING: In the world files, NEVER set the bits marked "R" ("Reserved") */
#define AFF_BLIND             (1 << 0)	   /* (R) Char is blind		*/
#define AFF_INVISIBLE         (1 << 1)	   /* Char is invisible		*/
#define AFF_DETECT_ALIGN      (1 << 2)	   /* Char is sensitive to align*/
#define AFF_DETECT_INVIS      (1 << 3)	   /* Char can see invis chars  */
#define AFF_DETECT_MAGIC      (1 << 4)	   /* Char is sensitive to magic*/
#define AFF_SENSE_LIFE        (1 << 5)	   /* Char can sense hidden life*/
#define AFF_WATERWALK	      (1 << 6)	   /* Char can walk on water	*/
#define AFF_SANCTUARY         (1 << 7)	   /* Char protected by sanct.	*/
#define AFF_GROUP             (1 << 8)	   /* (R) Char is grouped	*/
#define AFF_CURSE             (1 << 9)	   /* Char is cursed		*/
#define AFF_INFRAVISION       (1 << 10)	   /* Char can see in dark	*/
#define AFF_POISON            (1 << 11)	   /* (R) Char is poisoned	*/
#define AFF_UNUSED_1          (1 << 12)	   /* Char protected from evil  */
#define AFF_UNUSED_2          (1 << 13)	   /* Char protected from good  */
#define AFF_SLEEP             (1 << 14)	   /* (R) Char magically asleep	*/
#define AFF_NOTRACK	      (1 << 15)	   /* Char can't be tracked	*/
#define AFF_TAMED   	      (1 << 16)	   // Char has been tamed (DAK)
#define AFF_CONVERGENCE	      (1 << 17)	   /* Char has convergence	*/
#define AFF_SNEAK             (1 << 18)	   /* Char can move quietly	*/
#define AFF_HIDE              (1 << 19)	   /* Char is hidden		*/
#define AFF_AUTUS	      (1 << 20)	   /* Char has mana autus 	*/
#define AFF_CHARM             (1 << 21)	   /* Char is charmed		*/
#define AFF_NOPORTAL          (1 << 22)    /* Char is resisting portal  */
#define AFF_PLAGUED           (1 << 23)    /* Char has the plague       */
#define AFF_CHAINED           (1 << 24)    /* Char is chained to the ground */
#define AFF_REDIRECT_CHARGE   (1 << 25)    /* Char will accept lightning strikes */
#define AFF_R_CHARGED         (1 << 26)    /* Char will GIVE lightning strikes *g* */

/* Modes of connectedness: used by descriptor_data.state */
#define CON_PLAYING	 0		/* Playing - Nominal state	*/
#define CON_CLOSE	 1		/* Disconnecting		*/
#define CON_GET_NAME	 2		/* By what name ..?		*/
#define CON_NAME_CNFRM	 3		/* Did I get that right, x?	*/
#define CON_PASSWORD	 4		/* Password:			*/
#define CON_NEWPASSWD	 5		/* Give me a password for x	*/
#define CON_CNFPASSWD	 6		/* Please retype password:	*/
#define CON_QSEX	 7		/* Sex?				*/
#define CON_QCLASS	 8		/* Class?			*/
#define CON_RMOTD	 9		/* PRESS RETURN after MOTD	*/
#define CON_MENU	 10		/* Your choice: (main menu)	*/
#define CON_EXDESC	 11		/* Enter a new description:	*/
#define CON_CHPWD_GETOLD 12		/* Changing passwd: get old	*/
#define CON_CHPWD_GETNEW 13		/* Changing passwd: get new	*/
#define CON_CHPWD_VRFY   14		/* Verify new password		*/
#define CON_DELCNF1	 15		/* Delete confirmation 1	*/
#define CON_DELCNF2	 16		/* Delete confirmation 2	*/
#define CON_OEDIT	 17		/*. OLC mode - object edit     .*/
#define CON_REDIT	 18		/*. OLC mode - room edit       .*/
#define CON_ZEDIT	 19		/*. OLC mode - zone info edit  .*/
#define CON_MEDIT	 20		/*. OLC mode - mobile edit     .*/
#define CON_SEDIT	 21		/*. OLC mode - shop edit       .*/
#define CON_QANSI        22             /* Verify ansi color standard   */
#define CON_QRACE        23             /* Race?                        */
#define CON_QROLLSTATS   24             /* Roll stats                   */
#define CON_QHOMETOWN    25             /* Find hometown                */
#define CON_TRIGEDIT     26             /*. OLC mode - trigger edit    .*/
#define CON_NEWBIE       27             /* Total Newbie?                */
#define CON_HEDIT        28             /*. OLC mode - help editor     .*/
#define CON_AEDIT        29             /*. OLC mode - action editor   .*/
#define CON_TEXTED       30             /*. OLC mode - text editor     .*/
#define CON_QDEITY	 31		/* Deity Selection		*/

/* Character equipment positions: used as index for char_data.equipment[] */
/* NOTE: Don't confuse these constants with the ITEM_ bitvectors
   which control the valid places you can wear a piece of equipment */
#define WEAR_LIGHT      0
#define WEAR_FINGER_R   1
#define WEAR_FINGER_L   2
#define WEAR_NECK_1     3
#define WEAR_NECK_2     4
#define WEAR_BODY       5
#define WEAR_HEAD       6
#define WEAR_LEGS       7
#define WEAR_FEET       8
#define WEAR_HANDS      9
#define WEAR_ARMS      10
#define WEAR_SHIELD    11
#define WEAR_ABOUT     12
#define WEAR_WAIST     13
#define WEAR_WRIST_R   14
#define WEAR_WRIST_L   15
#define WEAR_WIELD     16
#define WEAR_HOLD      17
#define WEAR_SHOULDERS 18
#define WEAR_ANKLE_R   19
#define WEAR_ANKLE_L   20
#define WEAR_FACE      21

#define NUM_WEARS      22	/* This must be the # of eq positions!! */


/* object-related defines ********************************************/


/* Item types: used by obj_data.obj_flags.type_flag */
#define ITEM_LIGHT      1		/* Item is a light source	*/
#define ITEM_SCROLL     2		/* Item is a scroll		*/
#define ITEM_WAND       3		/* Item is a wand		*/
#define ITEM_STAFF      4		/* Item is a staff		*/
#define ITEM_WEAPON     5		/* Item is a weapon		*/
#define ITEM_FIREWEAPON 6		/* Unimplemented		*/
#define ITEM_MISSILE    7		/* Unimplemented		*/
#define ITEM_TREASURE   8		/* Item is a treasure, not gold	*/
#define ITEM_ARMOR      9		/* Item is armor		*/
#define ITEM_POTION    10 		/* Item is a potion		*/
#define ITEM_WORN      11		/* Unimplemented		*/
#define ITEM_OTHER     12		/* Misc object			*/
#define ITEM_TRASH     13		/* Trash - shopkeeps won't buy	*/
#define ITEM_TRAP      14		/* Unimplemented		*/
#define ITEM_CONTAINER 15		/* Item is a container		*/
#define ITEM_NOTE      16		/* Item is note 		*/
#define ITEM_DRINKCON  17		/* Item is a drink container	*/
#define ITEM_KEY       18		/* Item is a key		*/
#define ITEM_FOOD      19		/* Item is food			*/
#define ITEM_MONEY     20		/* Item is money (gold)		*/
#define ITEM_PEN       21		/* Item is a pen		*/
#define ITEM_BOAT      22		/* Item is a boat		*/
#define ITEM_FOUNTAIN  23		/* Item is a fountain		*/
#define ITEM_PORTAL    24               /* Item is a portal             */
#define ITEM_HP_REGEN  25               /* Increase hit regen rate      */
#define ITEM_MP_REGEN  26               /* Increase mana regen rate     */
#define ITEM_MV_REGEN  27               /* Increase move regen rate     */
#define ITEM_ATM       28		/* Item is a bank machine       */

 
/* Take/Wear flags: used by obj_data.obj_flags.wear_flags */
#define ITEM_WEAR_TAKE		(1 << 0)  /* Item can be takes		*/
#define ITEM_WEAR_FINGER	(1 << 1)  /* Can be worn on finger	*/
#define ITEM_WEAR_NECK		(1 << 2)  /* Can be worn around neck 	*/
#define ITEM_WEAR_BODY		(1 << 3)  /* Can be worn on body 	*/
#define ITEM_WEAR_HEAD		(1 << 4)  /* Can be worn on head 	*/
#define ITEM_WEAR_LEGS		(1 << 5)  /* Can be worn on legs	*/
#define ITEM_WEAR_FEET		(1 << 6)  /* Can be worn on feet	*/
#define ITEM_WEAR_HANDS		(1 << 7)  /* Can be worn on hands	*/
#define ITEM_WEAR_ARMS		(1 << 8)  /* Can be worn on arms	*/
#define ITEM_WEAR_SHIELD	(1 << 9)  /* Can be used as a shield	*/
#define ITEM_WEAR_ABOUT		(1 << 10) /* Can be worn about body 	*/
#define ITEM_WEAR_WAIST 	(1 << 11) /* Can be worn around waist 	*/
#define ITEM_WEAR_WRIST		(1 << 12) /* Can be worn on wrist 	*/
#define ITEM_WEAR_WIELD		(1 << 13) /* Can be wielded		*/
#define ITEM_WEAR_HOLD		(1 << 14) /* Can be held		*/
#define ITEM_WEAR_SHOULDERS     (1 << 15) /* Can be worn on shoulders   */
#define ITEM_WEAR_ANKLE         (1 << 16) /* Can be worn on ankle       */
#define ITEM_WEAR_FACE          (1 << 17) /* Can be worn on face        */

/* Extra object flags: used by obj_data.obj_flags.extra_flags */
#define ITEM_GLOW          (1 << 0)	/* Item is glowing		*/
#define ITEM_HUM           (1 << 1)	/* Item is humming		*/
#define ITEM_NORENT        (1 << 2)	/* Item cannot be rented	*/
#define ITEM_NODONATE      (1 << 3)	/* Item cannot be donated	*/
#define ITEM_NOINVIS	   (1 << 4)	/* Item cannot be made invis	*/
#define ITEM_INVISIBLE     (1 << 5)	/* Item is invisible		*/
#define ITEM_MAGIC         (1 << 6)	/* Item is magical		*/
#define ITEM_NODROP        (1 << 7)	/* Item is cursed: can't drop	*/
#define ITEM_BLESS         (1 << 8)	/* Item is blessed		*/
#define ITEM_ANTI_GOOD     (1 << 9)	/* Not usable by good people	*/
#define ITEM_ANTI_EVIL     (1 << 10)	/* Not usable by evil people	*/
#define ITEM_ANTI_NEUTRAL  (1 << 11)	/* Not usable by neutral people	*/
#define ITEM_ANTI_MAGIC_USER (1 << 12)	/* Not usable by mages		*/
#define ITEM_ANTI_CLERIC   (1 << 13)	/* Not usable by clerics	*/
#define ITEM_ANTI_THIEF	   (1 << 14)	/* Not usable by thieves	*/
#define ITEM_ANTI_WARRIOR  (1 << 15)	/* Not usable by warriors	*/
#define ITEM_NOSELL	   (1 << 16)	/* Shopkeepers won't touch it	*/
#define ITEM_ANTI_ARTISAN  (1 << 17)    /* Not usable by artisans       */

/* Modifier constants used with obj affects ('A' fields) */
#define APPLY_NONE              0	/* No effect			*/
#define APPLY_STR               1	/* Apply to strength		*/
#define APPLY_DEX               2	/* Apply to dexterity		*/
#define APPLY_INT               3	/* Apply to constitution	*/
#define APPLY_WIS               4	/* Apply to wisdom		*/
#define APPLY_CON               5	/* Apply to constitution	*/
#define APPLY_CHA		6	/* Apply to charisma		*/
#define APPLY_CLASS             7	/* Reserved			*/
#define APPLY_LEVEL             8	/* Reserved			*/
#define APPLY_AGE               9	/* Apply to age			*/
#define APPLY_CHAR_WEIGHT      10	/* Apply to weight		*/
#define APPLY_CHAR_HEIGHT      11	/* Apply to height		*/
#define APPLY_MANA             12	/* Apply to max mana		*/
#define APPLY_HIT              13	/* Apply to max hit points	*/
#define APPLY_MOVE             14	/* Apply to max move points	*/
#define APPLY_GOLD             15	/* Reserved			*/
#define APPLY_EXP              16	/* Reserved			*/
#define APPLY_DEFENSE          17	
#define APPLY_MDEFENSE         18
#define APPLY_POWER            19	
#define APPLY_MPOWER           20	
#define APPLY_TECHNIQUE        21
#define APPLY_DAMAGE           22      /* This is BOGUS, it's only here to give the message "DAMAGE" in affects and stat, don't use it. */

/* Container flags - value[1] */
#define CONT_CLOSEABLE      (1 << 0)	/* Container can be closed	*/
#define CONT_PICKPROOF      (1 << 1)	/* Container is pickproof	*/
#define CONT_CLOSED         (1 << 2)	/* Container is closed		*/
#define CONT_LOCKED         (1 << 3)	/* Container is locked		*/


/* Some different kind of liquids for use in values of drink containers */
#define LIQ_WATER      0
#define LIQ_BEER       1
#define LIQ_WINE       2
#define LIQ_ALE        3
#define LIQ_DARKALE    4
#define LIQ_WHISKY     5
#define LIQ_LEMONADE   6
#define LIQ_FIREBRT    7
#define LIQ_LOCALSPC   8
#define LIQ_SLIME      9
#define LIQ_MILK       10
#define LIQ_TEA        11
#define LIQ_COFFE      12
#define LIQ_BLOOD      13
#define LIQ_SALTWATER  14
#define LIQ_CLEARWATER 15


/* other miscellaneous defines *******************************************/


/* Player conditions */
#define DRUNK        0
#define FULL         1
#define THIRST       2


/* Sun state for weather_data */
#define SUN_DARK	0
#define SUN_RISE	1
#define SUN_LIGHT	2
#define SUN_SET		3


/* Rent codes */
#define RENT_UNDEF      0
#define RENT_CRASH      1
#define RENT_RENTED     2
#define RENT_CRYO       3
#define RENT_FORCED     4
#define RENT_TIMEDOUT   5


/* other #defined constants **********************************************/

/*
 * **DO**NOT** blindly change the number of levels in your MUD merely by
 * changing these numbers and without changing the rest of the code to match.
 * Other changes throughout the code are required.  See coding.doc for
 * details.
 *
 * LVL_IMPL should always be the HIGHEST possible immortal level, and
 * LVL_IMMORT should always be the LOWEST immortal level.  The number of
 * mortal levels will always be LVL_IMMORT - 1.
 */


#define LVL_IMPL	105
#define LVL_GRGOD      104
#define LVL_GOD	        103
#define LVL_DEMIGOD	102
#define LVL_IMMORT      101
#define LVL_HERO	100
#define LVL_CLANLEADER  -1 /* not really a level, but used in boards.c */
#define LVL_GETSTUFF    3

/* Level of the 'freeze' command */
#define LVL_FREEZE	LVL_GRGOD

#define NUM_OF_DIRS	6	/* number of directions in a room (nsewud) */

#define OPT_USEC	100000	/* 10 passes per second */
#define PASSES_PER_SEC	(1000000 / OPT_USEC)
#define RL_SEC		* PASSES_PER_SEC

#define PULSE_ZONE      (10 RL_SEC)
#define PULSE_MOBILE    (10 RL_SEC)
#define PULSE_VIOLENCE  (2 RL_SEC)

/* Variables for the output buffering system */
#define MAX_SOCK_BUF            (12 * 1024) /* Size of kernel's sock buf   */
#define MAX_PROMPT_LENGTH       200          /* Max length of prompt */
#define GARBAGE_SPACE		32          /* Space for **OVERFLOW** etc  */
#define SMALL_BUFSIZE		1024        /* Static output buffer size   */
/* Max amount of output that can be buffered */
#define LARGE_BUFSIZE	   (MAX_SOCK_BUF - GARBAGE_SPACE - MAX_PROMPT_LENGTH)

//#define MAX_STRING_LENGTH	8192

#define MAX_STRING_LENGTH       16384

#define MAX_INPUT_LENGTH	256	/* Max length per *line* of input */
#define MAX_RAW_INPUT_LENGTH	512	/* Max size of *raw* input */
#define MAX_MESSAGES		60
#define MAX_NAME_LENGTH		20  /* Used in char_file_u *DO*NOT*CHANGE* */
#define MAX_PWD_LENGTH		128  /* Upgraded for SHA-256 hashes - was 10 for DES */
#define MAX_TITLE_LENGTH	40  /* Used in char_file_u *DO*NOT*CHANGE* */
#define HOST_LENGTH		30  /* Used in char_file_u *DO*NOT*CHANGE* */
#define EXDSCR_LENGTH		240 /* Used in char_file_u *DO*NOT*CHANGE* */
#define MAX_TONGUE		3   /* Used in char_file_u *DO*NOT*CHANGE* */
#define MAX_SKILLS		1000 /* Used in char_file_u *DO*NOT*CHANGE* */
#define MAX_AFFECT		32  /* Used in char_file_u *DO*NOT*CHANGE* */
#define MAX_OBJ_AFFECT		6 /* Used in obj_file_elem *DO*NOT*CHANGE* */
#define MAX_ROOM_VNUM           500000 /* how many rooms per zone */
#define MAX_ZONE_NUM            500000 /* how many zones mud can have */

#define MOB_DEFAULT_STAT 13

/**********************************************************************
* Structures                                                          *
**********************************************************************/


typedef signed char		sbyte;
typedef unsigned char		ubyte;
typedef signed short int	sh_int;
typedef unsigned short int	ush_int;
typedef char			bool;

#ifndef CIRCLE_WINDOWS
typedef char			byte;
#endif

typedef long	room_vnum;	/* A room's vnum type */
typedef long	obj_vnum;	/* An object's vnum type */
typedef long	mob_vnum;	/* A mob's vnum type */

typedef long	room_rnum;	/* A room's real (internel) number type */
typedef long	obj_rnum;	/* An object's real (internal) num type */
typedef long	mob_rnum;	/* A mobile's real (internal) num type */


/* Extra description: used in objects, mobiles, and rooms */
struct extra_descr_data {
   char	*keyword;                 /* Keyword in look/examine          */
   char	*description;             /* What to see                      */
   struct extra_descr_data *next; /* Next in list                     */
};


/* object-related structures ******************************************/


/* object flags; used in obj_data */
struct obj_flag_data {
   int  curr_slots;     // Current amount of SLOTS obj has
   int  total_slots;    // Total amount of SLOTS an obj has
   int	value[4];	/* Values of the item (see list)    */
   byte type_flag;	/* Type of item			    */
   int	wear_flags;	/* Where you can wear it	    */
   int	extra_flags;	/* If it hums, glows, etc.	    */
   int	weight;		/* Weigt what else                  */
   int	cost;		/* Value when sold (gp.)            */
   int	cost_per_day;	/* Cost to keep pr. real day        */
   int	timer;		/* Timer for object                 */
   long	bitvector;	/* To set chars bits                */

};


/* Used in obj_file_elem *DO*NOT*CHANGE* */
struct obj_affected_type {
   byte location;      /* Which ability to change (APPLY_XXX) */
   sbyte modifier;     /* How much it changes by              */
};


/* ================== Memory Structure for Objects ================== */
struct obj_data {
   obj_vnum item_number;	/* Where in data-base			*/
   room_rnum in_room;		/* In what room -1 when conta/carr	*/

   struct obj_flag_data obj_flags;/* Object information               */
   struct obj_affected_type affected[MAX_OBJ_AFFECT];  /* affects */

   char	*name;                    /* Title of object :get etc.        */
   char	*description;		  /* When in room                     */
   char	*short_description;       /* when worn/carry/in cont.         */
   char	*action_description;      /* What to write when used          */
   struct extra_descr_data *ex_description; /* extra descriptions     */
   struct char_data *carried_by;  /* Carried by :NULL in room/conta   */
   struct char_data *worn_by;	  /* Worn by?			      */
   long worn_on;		  /* Worn where?		      */

   struct obj_data *in_obj;       /* In what object NULL when none    */
   struct obj_data *contains;     /* Contains objects                 */

   long id;                       /* used by DG triggers              */
   struct trig_proto_list *proto_script; /* list of default triggers  */
   struct script_data *script;    /* script info for the object       */

   struct obj_data *next_content; /* For 'contains' lists             */
   struct obj_data *next;         /* For the object list              */
   int min_level;                 /* Object's minimum level           */
   int obj_class;                 /* Object's intended class          */
};
/* ======================================================================= */


/* ====================== File Element for Objects ======================= */
/*                 BEWARE: Changing it will ruin rent files		   */
struct obj_file_elem {
   obj_vnum item_number;
    long locate;  /* that's the (1+)wear-location (when equipped) or
                     * (20+)index in obj file (if it's in a container) BK */ 

   int  curr_slots;     // Current amount of SLOTS obj has
   int  total_slots;    // Total amount of SLOTS an obj has
   int	value[4];
   int	extra_flags;
   int	weight;
   int	timer;
   long	bitvector;
   int min_level;
   struct obj_affected_type affected[MAX_OBJ_AFFECT];
};


/* header block for rent files.  BEWARE: Changing it will ruin rent files  */
struct rent_info {
   int	time;
   int	rentcode;
   int	net_cost_per_diem;
   int	gold;
   int	account;
   int	nitems;
   int	spare0;
   int	spare1;
   int	spare2;
   int	spare3;
   int	spare4;
   int	spare5;
   int	spare6;
   int	spare7;
};
/* ======================================================================= */


/* room-related structures ************************************************/


struct room_direction_data {
   char	*general_description;       /* When look DIR.			*/

   char	*keyword;		/* for open/close			*/

   long exit_info;		/* Exit info				*/
   obj_vnum key;		/* Key's number (-1 for no key)		*/
   room_rnum to_room;		/* Where direction leads (NOWHERE)	*/
};


struct room_special_exit_data {
   char	*general_description;       /* When look DIR.			*/

   char	*keyword;		/* for open/close			*/

   char *ex_name;		/* for command reference		*/

   char *leave_msg;		/* message when you exit the room       */
   long exit_info;		/* Exit info				*/
   obj_vnum key;		/* Key's number (-1 for no key)		*/
   room_rnum to_room;		/* Where direction leads (NOWHERE)	*/
};


/* ================== Memory Structure for room ======================= */
struct room_data {
   room_vnum number;		/* Rooms number	(vnum)		      */
   long zone;                 /* Room zone (for resetting)          */
   int	sector_type;            /* sector type (move/hide)            */
   char	*name;                  /* Rooms name 'You are ...'           */
   char	*description;           /* Shown when entered                 */
   struct extra_descr_data *ex_description; /* for examine/look       */
   struct room_special_exit_data *special_exit; /* for a special exit */
   struct room_direction_data *dir_option[NUM_OF_DIRS]; /* Directions */
   int room_flags;		/* DEATH,DARK ... etc                 */

   sh_int linkmapnum;           /* Map mods - Storm */
   sh_int linkrnum;
   char *id;
   int mapmv;
   int weather;                 /* How's the weather in the room? */
   sh_int wzonecontrol;         /* What zone's weather does it control? */

   byte light;                  /* Number of lightsources in room     */
   byte blood;                  /* Ammount of blood in room (Mulder) */
   byte snow;			/* Ammount of snow in room (Mulder) */

   SPECIAL(*func);

   struct trig_proto_list *proto_script; /* list of default triggers  */
   struct script_data *script;  /* script info for the object         */

   struct obj_data *contents;   /* List of items in room              */
   struct char_data *people;    /* List of NPC / PC in room           */
};
/* ====================================================================== */


/* char-related structures ************************************************/


/* memory structure for characters */
struct memory_rec_struct {
   long	id;
   struct memory_rec_struct *next;
};

typedef struct memory_rec_struct memory_rec;


/* This structure is purely intended to be an easy way to transfer */
/* and return information about time (real or mudwise).            */
struct time_info_data {
   int hours, day, month;
   long year;
};


/* These data contain information about a players time data */
struct time_data {
   time_t birth;    /* This represents the characters age                */
   time_t logon;    /* Time of the last logon (used to calculate played) */
   int	played;     /* This is the total accumulated time played in secs */
};


/* general player-related info, usually PC's and NPC's */
struct char_player_data {
   char	*passwd; /* character's password      */
   char	*name;	       /* PC / NPC s name (kill ...  )         */
   char	*short_descr;  /* for NPC 'actions'                    */
   char	*long_descr;   /* for 'look'			       */
   char	*description;  /* Extra descriptions                   */
   char	*title;        /* PC / NPC's title                     */
   byte sex;           /* PC / NPC's sex                       */
   byte class;         /* PC / NPC's class                     */
   byte race;          /* PC / NPC's race                      */
   byte deity;          /* PC / NPC's god                       */
   byte level;         /* PC / NPC's level                     */
   int	hometown;      /* PC s Hometown (zone)                 */
   struct time_data time;  /* PC's AGE in days                 */
   ubyte weight;       /* PC / NPC's weight                    */
   ubyte height;       /* PC / NPC's height                    */
};


/* Char's abilities.  Used in char_file_u *DO*NOT*CHANGE* */
struct char_ability_data {
   sbyte str;
   sbyte str_add;      /* 000 - 100 if strength 18             */
   sbyte intel;
   sbyte wis;
   sbyte dex;
   sbyte con;
   sbyte cha; 
};

/* Char's points.  Used in char_file_u *DO*NOT*CHANGE* */
struct char_point_data {
   long mana;
   long max_mana;     /* Max move for PC/NPC			   */
   long hit;
   long max_hit;      /* Max hit for PC/NPC                      */
   long move;
   long max_move;     /* Max move for PC/NPC                     */

   int	gold;           /* Money carried                           */
   int	bank_gold;	/* Gold the char has in a bank account	   */
   int	exp;            /* The experience of the player            */

   sh_int power;
   sh_int mpower;
   sh_int defense;
   sh_int mdefense;
   sh_int technique;
};


/* 
 * char_special_data_saved: specials which both a PC and an NPC have in
 * common, but which must be saved to the playerfile for PC's.
 *
 * WARNING:  Do not change this structure.  Doing so will ruin the
 * playerfile.  If you want to add to the playerfile, use the spares
 * in player_special_data.
 */
struct char_special_data_saved {
   int	alignment;		/* +-1000 for alignments                */
   long	idnum;			/* player's idnum; -1 for mobiles	*/
   long	act;			/* act flag for NPC's; player flag for PC's */

   long	affected_by;		/* Bitvector for spells/skills affected by */
};


/* Special playing constants shared by PCs and NPCs which aren't in pfile */
struct char_special_data {
   struct char_data *fighting;	/* Opponent				*/
   struct char_data *hunting;	/* Char hunted by this char		*/
   struct char_data *riding;	// Who are they riding? (DAK)
   struct char_data *ridden_by; // Who is riding them? (DAK)
   struct char_data *observing;    /* Who is this char observing	*/
   struct char_data *observe_by;   /* And who is observing this char    */
                                   /* Note: A tweak used here. see      */
                                   /* "Arena mods" in fight.c */
   byte position;		/* Standing, fighting, sleeping, etc.	*/

   int	carry_weight;		/* Carried weight			*/
   byte carry_items;		/* Number of items carried		*/
   int	timer;			/* Timer for update			*/
   int  flee_timer;             /* Arena flee timer */
   struct char_data *last_fighting; /* Last person fighting againstin arena */

   struct char_special_data_saved saved; /* constants saved in plrfile	*/
   long	bup_affected_by;	/* Bitvector for spells/skills affected by */
   struct affected_type *bup_affected;    /* affected by what spells    */
   int bup_wimp_level;
   int bup_recall_level;
   int bup_retreat_level;
};


/*
 *  If you want to add new values to the playerfile, do it here.  DO NOT
 * ADD, DELETE OR MOVE ANY OF THE VARIABLES - doing so will change the
 * size of the structure and ruin the playerfile.  However, you can change
 * the names of the spares to something more meaningful, and then use them
 * in your new code.  They will automatically be transferred from the
 * playerfile into memory when players log in.
 */
struct player_special_data_saved {
   byte skills[MAX_SKILLS+1];	/* array of skills plus skill 0		*/
   byte PADDING0;		/* used to be spells_to_learn		*/
   bool talks[MAX_TONGUE];	/* PC s Tongues 0 for NPC		*/
   int	wimp_level;		/* Below this # of hit points, flee!	*/
   byte freeze_level;		/* Level of god who froze char, if any	*/
   long invis_level;		/* level of invisibility		*/
   room_vnum load_room;		/* Which room to place char in		*/
   long	pref;			/* preference flags for PC's.		*/
   ubyte bad_pws;		/* number of bad password attemps	*/
   sbyte conditions[3];         /* Drunk, full, thirsty			*/
   struct char_data *questgiver;
   byte death_timer;
   ubyte citizen;
   ubyte training;
   ubyte newbie;
   ubyte arena;                 /* See ARENA_xxx defines */
   ubyte mclassvect;            /* multiclass vector */
   int spells_to_learn;		/* How many can you learn yet this level*/
   int questpoints;  /* LJ autoquest */
   int nextquest;  /* LJ autoquest */
   int countdown;  /* LJ autoquest */
   int questobj;  /* LJ autoquest */
   int questmob;  /* LJ autoquest */
   int recall_level;
   int retreat_level;
   int trust;
   int bail_amt;
   ubyte wins;
   ubyte losses;
   long	pref2;
   long	godcmds1;
   long	godcmds2;
   long	godcmds3;
   long	godcmds4; /* dont need these, but theres always room for jello */
   int clan;
   int clan_rank;

   long mapx;
   long mapy;
   long buildmodezone;
   long buildmoderoom;
   long tloadroom;
};

/*
 * Specials needed only by PCs, not NPCs.  Space for this structure is
 * not allocated in memory for NPCs, but it is for PCs and the portion
 * of it labelled 'saved' is saved in the playerfile.  This structure can
 * be changed freely; beware, though, that changing the contents of
 * player_special_data_saved will corrupt the playerfile.
 */
struct player_special_data {
   struct player_special_data_saved saved;

   char	*poofin;		/* Description on arrival of a god.     */
   char	*poofout;		/* Description upon a god's exit.       */
   char *email;     
   struct alias *aliases;	/* Character's aliases			*/
   long last_tell;		/* idnum of last tell from		*/
   void *last_olc_targ;		/* olc control				*/
   int last_olc_mode;		/* olc control				*/
};


/* Specials used by NPCs, not PCs */
struct mob_special_data {
   byte last_direction;     /* The last direction the monster went     */
   int	attack_type;        /* The Attack Type Bitvector for NPC's     */
   byte default_pos;        /* Default position for NPC                */
   memory_rec *memory;	    /* List of attackers to remember	       */
   byte damnodice;          /* The number of damage dice's	       */
   sh_int damsizedice;        /* The size of the damage dice's           */
   int wait_state;	    /* Wait state for bashed mobs	       */
};


/* An affect structure.  Used in char_file_u *DO*NOT*CHANGE* */
struct affected_type {
   long type;          /* The type of spell that caused this      */
   long duration;      /* For how long its effects will last      */
   long modifier;       /* This is added to apropriate ability     */
   byte location;        /* Tells which ability to change(APPLY_XXX)*/
   long	bitvector;       /* Tells which bits to set (AFF_XXX)       */

   struct affected_type *next;
};


/* Structure used for chars following other chars */
struct follow_type {
   struct char_data *follower;
   struct follow_type *next;
};


/* ================== Structure for player/non-player ===================== */
struct char_data {
   long nr;                            /* Mob's rnum			  */
   room_rnum in_room;                    /* Location (real room number)	  */
   room_rnum was_in_room;		 /* location for linkdead people  */

   struct char_player_data player;       /* Normal data                   */
   struct char_ability_data real_abils;	 /* Abilities without modifiers   */
   struct char_ability_data aff_abils;	 /* Abils with spells/stones/etc  */
   struct char_point_data points;        /* Points                        */
   struct char_special_data char_specials;	/* PC/NPC specials	  */
   struct player_special_data *player_specials; /* PC specials		  */
   struct mob_special_data mob_specials;	/* NPC specials		  */

   struct affected_type *affected;       /* affected by what spells       */
   struct obj_data *equipment[NUM_WEARS];/* Equipment array               */

   struct obj_data *carrying;            /* Head of list                  */
   struct descriptor_data *desc;         /* NULL for mobiles              */

   long id;                            /* used by DG triggers             */
   struct trig_proto_list *proto_script; /* list of default triggers      */
   struct script_data *script;         /* script info for the object      */
   struct script_memory *memory;       /* for mob memory triggers         */

   struct char_data *next_in_room;     /* For room->people - list         */
   struct char_data *next;             /* For either monster or ppl-list  */
   struct char_data *next_fighting;    /* For fighting list               */

   struct follow_type *followers;        /* List of chars followers       */
   struct char_data *master;             /* Who is char following?        */
   struct ignore_index_element *ignore_list;
};
/* ====================================================================== */


/* ==================== File Structure for Player ======================= */
/*             BEWARE: Changing it will ruin the playerfile		  */
/* struct char_file_u {
    char_player_data 
   char	name[MAX_NAME_LENGTH+1];
   char	description[EXDSCR_LENGTH];
   char	title[MAX_TITLE_LENGTH+1];
    These here are new spares you can use in future - Thargor 
    Note: You can't reorder them. They have to stay here 
   long nspare1;
   long nspare2;
   long nspare3;
   long nspare4;
   long nspare5;
   long nspare6;
   long nspare7;
   long nspare8;
   long nspare9;
   byte nspare10;
   byte nspare11;

   byte sex;
   byte class;
   byte race;
   byte deity;
   byte level;
   long hometown;
   time_t birth;   Time of birth of character
   int	played;    Number of secs played in total 
   ubyte weight;
   ubyte height;

   char	pwd[MAX_PWD_LENGTH+1];     character's password (SHA-256 hash) 

   struct char_special_data_saved char_specials_saved;
   struct player_special_data_saved player_specials_saved;
   struct char_ability_data abilities;
   struct char_point_data points;
   struct affected_type affected[MAX_AFFECT];

   time_t last_logon;		 Time (in secs) of last logon
   char host[HOST_LENGTH+1];	 host of last logon 
}; */
/* ====================================================================== */


/* descriptor-related structures ******************************************/


struct txt_block {
   char	*text;
   int aliased;
   struct txt_block *next;
};


struct txt_q {
   struct txt_block *head;
   struct txt_block *tail;
};


struct descriptor_data {
   socket_t	descriptor;	/* file descriptor for socket		*/
   char	host[HOST_LENGTH+1];	/* hostname				*/
   byte close_me;               /* flag: this desc. should be closed    */
   byte	bad_pws;		/* number of bad pw attemps this login	*/
   byte idle_tics;		/* tics idle at password prompt		*/
   int	connected;		/* mode of 'connectedness'		*/
   int	wait;			/* wait for how many loops		*/
   int	desc_num;		/* unique num assigned to desc		*/
   time_t login_time;		/* when the person connected		*/
   char *showstr_head;		/* for keeping track of an internal str	*/
   char **showstr_vector;	/* for paging through texts		*/
   int  showstr_count;		/* number of pages to page through	*/
   int  showstr_page;		/* which page are we currently showing?	*/
   char	**str;			/* for the modify-str system		*/
   size_t max_str;	        /*		-			*/
   char *backstr;		/* added for handling abort buffers	*/
   long	mail_to;		/* name for mail system			*/
   int	has_prompt;		/* is the user at a prompt?             */
   char	inbuf[MAX_RAW_INPUT_LENGTH];  /* buffer for raw input		*/
   char	last_input[MAX_INPUT_LENGTH]; /* the last input			*/
   char small_outbuf[SMALL_BUFSIZE];  /* standard output buffer		*/
   char *output;		/* ptr to the current output buffer	*/
   int  bufptr;			/* ptr to end of current output		*/
   int	bufspace;		/* space left in the output buffer	*/
   struct txt_block *large_outbuf; /* ptr to large buffer, if need it   */
   struct txt_q input;		/* q of unprocessed input		*/
   struct char_data *character;	/* linked to char			*/
   struct char_data *original;	/* original char if switched		*/
   struct descriptor_data *snooping; /* Who is this char snooping	*/
   struct descriptor_data *snoop_by; /* And who is snooping this char	*/
   struct descriptor_data *next; /* link to next descriptor		*/
   struct olc_data *olc;	     /*. OLC info - defined in olc.h   .*/
   struct obj_data *note;       /* is a note being written?             */
};


/* other miscellaneous structures ***************************************/


struct msg_type {
   char	*attacker_msg;  /* message to attacker */
   char	*victim_msg;    /* message to victim   */
   char	*room_msg;      /* message to room     */
};


struct message_type {
   struct msg_type die_msg;	/* messages when death			*/
   struct msg_type miss_msg;	/* messages when miss			*/
   struct msg_type hit_msg;	/* messages when hit			*/
   struct msg_type god_msg;	/* messages when hit on god		*/
   struct message_type *next;	/* to next messages of this kind.	*/
};


struct message_list {
   int	a_type;		        /* Attack type: skl/spl name		*/
   int	number_of_attacks;	/* How many attack messages to chose from. */
   struct message_type *msg;	/* List of messages.			*/
};


struct dex_skill_type {
   long p_pocket;
   long p_locks;
   long traps;
   long sneak;
   long hide;
};


struct dex_app_type {
   long reaction;
   long miss_att;
   long defensive;
};


struct str_app_type {
   long carry_w;  /* Maximum weight that can be carrried */
   long wield_w;  /* Maximum weight that can be wielded  */
};


struct wis_app_type {
   byte bonus;       /* how many practices player gains per lev */
};


struct int_app_type {
   byte learn;       /* how many % a player learns a spell/skill */
};


struct con_app_type {
   long hitp;
   long shock;
};

struct weather_data {
   int  sunlight;       /* And how much sun. */
};

struct title_type {
   char	*title_m;
   char	*title_f;
   int	exp;
};

struct citizen_type {
   char *citizen_m;
   char *citizen_f;
   char *citizen_n;  
};

/* ignore index element... the basic structure that shall be used by the 'ignore' command */
struct ignore_index_element {
  long id;
  long type;
  char *reason;
  struct ignore_index_element *next;
};

/* element in monster and object index-tables   */
struct index_data {
   int	vnum;    /* virtual number of this mob/obj           */
   int	number;     /* number of existing units of this mob/obj	*/
   SPECIAL(*func);

   char *farg;         /* string argument for special function     */
   struct trig_data *proto;     /* for triggers... the trigger     */
};

/* linked list for mob/object prototype trigger lists */
struct trig_proto_list {
  int vnum;                             /* vnum of the trigger   */
  struct trig_proto_list *next;         /* next trigger          */
};
struct time_write {
   int year, month, day;
 };

struct who_list{   /* Implemented by Thargor for new do_who function */
  int level;
  char desc[SMALL_BUFSIZE];
};
/* used in the socials */
struct social_messg {
  int act_nr;
  char *command;               /* holds copy of activating command */
  char *sort_as;             /* holds a copy of a similar command or  
                             * abbreviation to sort by for the parser */
  int hide;                  /* ? */
  int min_victim_position;   /* Position of victim */
  int min_char_position;     /* Position of char */
  int min_level_char;          /* Minimum level of socialing char */

  /* No argument was supplied */
  char *char_no_arg;
  char *others_no_arg;

  /* An argument was there, and a victim was found */
  char *char_found;
  char *others_found;
  char *vict_found;
                             
  /* An argument was there, as well as a body part, and a victim was found */
  char *char_body_found;
  char *others_body_found;
  char *vict_body_found;

  /* An argument was there, but no victim was found */
  char *not_found;  
  
  char *char_auto;
  char *others_auto;
  
  /* If the char cant be found search the char's inven and do these: */
  char *char_obj_found;      
  char *others_obj_found;
};
