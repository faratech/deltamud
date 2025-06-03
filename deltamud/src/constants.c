   /* ************************************************************************
   *   File: constants.c                                   Part of CircleMUD *
   *  Usage: Numeric and string contants used by the MUD                     *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "screen.h"

const char circlemud_version[] =
{
  "DeltaMUD v3.0 - Beta Test\r\nBased on CircleMUD v3.0bpl12\r\n"};

/* strings corresponding to ordinals/bitvectors in structs.h ********** */


/* (Note: strings for class definitions in class.c instead of here) */


/* cardinal directions */
const char *dirs[] =
{
  "north",
  "east",
  "south",
  "west",
  "up",
  "down",
  "\n"
};


/* ROOM_x */
const char *room_bits[] =
{
  "DARK",
  "DEATH",
  "!MOB",
  "INDOORS",
  "PEACEFUL",
  "SOUNDPROOF",
  "!TRACK",
  "!MAGIC",
  "TUNNEL",
  "PRIVATE",
  "GODROOM",
  "HOUSE",
  "HCRSH",
  "ATRIUM",
  "OLC",
  "*",				/* BFS MARK */
  "IMPROOM",
  "BAD_REGEN",
  "WALL",
  "CLANROOM",
  "GOOD_REGEN",
  "\n"
};


/* EX_x */
const char *exit_bits[] =
{
  "DOOR",
  "CLOSED",
  "LOCKED",
  "PICKPROOF",
  "\n"
};


/* SECT_ */
const char *sector_types[] =
{
  "Inside",
  "City",
  "Field",
  "Forest",
  "Hills",
  "Mountains",
  "Water (Swim)",
  "Water (No Swim)",
  "Underwater",
  "In Flight",
  "Road (Map)",
  "Wall (Map)",
  "Swamp",
  "Desert",
  "Bridge",
  "\n"
};


/* SEX_x */
const char *genders[] =
{
  "Neutral",
  "Male",
  "Female"
};


/* POS_x */
const char *position_types[] =
{
  "Dead",
  "Mortally wounded",
  "Incapacitated",
  "Stunned",
  "Sleeping",
  "Resting",
  "Meditating",
  "Sitting",
  "Fighting",
  "Standing",
  "\n"
};


/* PLR_x */
const char *player_bits[] =
{
  "KILLER",
  "THIEF",
  "FROZEN",
  "DONTSET",
  "WRITING",
  "MAILING",
  "CSH",
  "SITEOK",
  "NOSHOUT",
  "NOTITLE",
  "DELETED",
  "!UNUSED!",
  "!WIZL",
  "!DEL",
  "INVST",
  "CRYO",
  "QUESTOR",
  "MULTIOK",
  "MBUILDER",
  "\n"
};


/* MOB_x */
const char *action_bits[] =
{
  "SPEC",
  "SENTINEL",
  "SCAVENGER",
  "ISNPC",
  "AWARE",
  "AGGR",
  "STAY-ZONE",
  "WIMPY",
  "AGGR_EVIL",
  "AGGR_GOOD",
  "AGGR_NEUTRAL",
  "MEMORY",
  "HELPER",
  "!CHARM",
  "!SUMMN",
  "!SLEEP",
  "!BASH",
  "!BLIND",
  "QUESTMASTER",
  "QUESTMOB",
  "MOUNTABLE",
  "SPELLCASTER",
  "DBLATTACK",
  "!TALK",
  "MERCY",
  "DEITY",
  "PCHELPER_GOOD",
  "PCHELPER_NEUT",
  "PCHELPER_EVIL",
  "BANKER",
  "\n"
};


/* PRF_x */
const char *preference_bits[] =
{
  "BRIEF",
  "COMPACT",
  "DEAF",
  "!TELL",
  "D_HP",
  "D_MANA",
  "D_MOVE",
  "AUTOEX",
  "!HASS",
  "!ARENA",
  "SUMN",
  "!REP",
  "LIGHT",
  "C1",
  "C2",
  "!WIZ",
  "L1",
  "L2",
  "!AUC",
  "!GOS",
  "!GTZ",
  "RMFLG",
  "AFK",
  "AUTOSPLIT",
  "AUTOLOOT",
  "AUTOGOLD",
  "DISPEXP",
  "!TIC",
  "UNUSED",
  "L3",
  "!STACK",
  "\n"
};

/* PRF2_x */
const char *preference2_bits[] =
{
  "QUEST",
  "LOCKOUT",
  "!MAP",
  "CTITLE",
  "CLAN_TALK",
  "D_MOB",
  "MBUILDING",
  "MERCY",
  "ADVCANCEDMAP",
  "INTANGIBLE",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n",
  "\n"
};


/* AFF_x */
const char *affected_bits[] =
{
  "BLIND",
  "INVIS",
  "DETECT-ALIGN",
  "DETECT-INVIS",
  "DETECT-MAGIC",
  "SENSE-LIFE",
  "WATERWALK",
  "SANCTUARY",
  "GROUP",
  "CURSE",
  "INFRAVISION",
  "POISON",
  "UNUSED",
  "UNUSED",
  "SLEEP",
  "!TRACK",
  "TAMED",
  "CONVERGENCE",
  "SNEAK",
  "HIDE",
  "AUTUS",
  "CHARM",
  "!PORTAL",
  "PLAGUED",
  "CHAINED",
  "REDIRECT CHARGE",
  "CHARGED",
  "\n"
};


/* CON_x */
const char *connected_types[] =
{
  "Playing",
  "Disconnecting",
  "Get name",
  "Confirm name",
  "Get password",
  "Get new PW",
  "Confirm new PW",
  "Select sex",
  "Select class",
  "Reading MOTD",
  "Main Menu",
  "Get descript.",
  "Changing PW 1",
  "Changing PW 2",
  "Changing PW 3",
  "Self-Delete 1",
  "Self-Delete 2",
  "Object editor",
  "Room editor",
  "Zone editor",
  "Mobile editor",
  "Shop editor",
  "ANSI query",
  "Select race",
  "Roll stats",
  "Select town",
  "Trigger editor",
  "Newbie start",
  "Help editor",
  "Action editor",
  "Text editor",
  "Select deity",
  "\n"
};


/* WEAR_x - for eq list */
const char *where[] =
{
  "&b<&nused as light&b>&n      ",
  "&b<&nworn on finger&b>&n     ",
  "&b<&nworn on finger&b>&n     ",
  "&b<&nworn around neck&b>&n   ",
  "&b<&nworn around neck&b>&n   ",
  "&b<&nworn on body&b>&n       ",
  "&b<&nworn on head&b>&n       ",
  "&b<&nworn on legs&b>&n       ",
  "&b<&nworn on feet&b>&n       ",
  "&b<&nworn on hands&b>&n      ",
  "&b<&nworn on arms&b>&n       ",
  "&b<&nworn as shield&b>&n     ",
  "&b<&nworn about body&b>&n    ",
  "&b<&nworn about waist&b>&n   ",
  "&b<&nworn around wrist&b>&n  ",
  "&b<&nworn around wrist&b>&n  ",
  "&b<&nwielded&b>&n            ",
  "&b<&nheld&b>&n               ",
  "&b<&nworn on shoulders&b>&n  ",
  "&b<&nworn around ankle&b>&n  ",
  "&b<&nworn around ankle&b>&n  ",
  "&b<&nworn on face&b>&n       ",
};


/* WEAR_x - for stat */
const char *equipment_types[] =
{
  "Used as light",
  "Worn on right finger",
  "Worn on left finger",
  "First worn around Neck",
  "Second worn around Neck",
  "Worn on body",
  "Worn on head",
  "Worn on legs",
  "Worn on feet",
  "Worn on hands",
  "Worn on arms",
  "Worn as shield",
  "Worn about body",
  "Worn around waist",
  "Worn around right wrist",
  "Worn around left wrist",
  "Wielded",
  "Held",
  "Worn on shoulders",
  "Worn around right ankle",
  "Worn around left ankle",
  "Worn over face",
  "\n"
};


/* ITEM_x (ordinal object types) */
const char *item_types[] =
{
  "UNDEFINED",
  "LIGHT",
  "SCROLL",
  "WAND",
  "STAFF",
  "WEAPON",
  "FIRE WEAPON",
  "MISSILE",
  "TREASURE",
  "ARMOR",
  "POTION",
  "WORN",
  "OTHER",
  "TRASH",
  "TRAP",
  "CONTAINER",
  "NOTE",
  "LIQ CONTAINER",
  "KEY",
  "FOOD",
  "MONEY",
  "PEN",
  "BOAT",
  "FOUNTAIN",
  "PORTAL",
  "HIT_REGEN",
  "MANA_REGEN",
  "MOVE_REGEN",
  "ATM"
  "\n"
};


/* ITEM_WEAR_ (wear bitvector) */
const char *wear_bits[] =
{
  "TAKE",
  "FINGER",
  "NECK",
  "BODY",
  "HEAD",
  "LEGS",
  "FEET",
  "HANDS",
  "ARMS",
  "SHIELD",
  "ABOUT",
  "WAIST",
  "WRIST",
  "WIELD",
  "HOLD",
  "SHOULDERS",
  "ANKLE",
  "FACE",
  "\n"
};


/* ITEM_x (extra bits) */
const char *extra_bits[] =
{
  "GLOW",
  "HUM",
  "!RENT",
  "!DONATE",
  "!INVIS",
  "INVISIBLE",
  "MAGIC",
  "!DROP",
  "BLESS",
  "!GOOD",
  "!EVIL",
  "!NEUTRAL",
  "!MAGE",
  "!CLERIC",
  "!THIEF",
  "!WARRIOR",
  "!SELL",
  "!ARTISAN",
  "\n"
};


/* APPLY_x */
const char *apply_types[] =
{
  "NONE",
  "STR",
  "DEX",
  "INT",
  "WIS",
  "CON",
  "CHA",
  "CLASS",
  "LEVEL",
  "AGE",
  "CHAR_WEIGHT",
  "CHAR_HEIGHT",
  "MAXMANA",
  "MAXHIT",
  "MAXMOVE",
  "GOLD",
  "EXP",
  "DEFENSE",
  "MDEFENSE",
  "POWER",
  "MPOWER",
  "TECHNIQUE",
  "DAMAGE", /* This is BOGUS, don't use it. */
  "\n"
};


/* CONT_x */
const char *container_bits[] =
{
  "CLOSEABLE",
  "PICKPROOF",
  "CLOSED",
  "LOCKED",
  "\n",
};


/* LIQ_x */
const char *drinks[] =
{
  "water",
  "beer",
  "wine",
  "ale",
  "dark ale",
  "whisky",
  "lemonade",
  "firebreather",
  "local speciality",
  "slime mold juice",
  "milk",
  "tea",
  "coffee",
  "blood",
  "salt water",
  "clear water",
  "\n"
};


/* other constants for liquids ***************************************** */


/* one-word alias for each drink */
const char *drinknames[] =
{
  "water",
  "beer",
  "wine",
  "ale",
  "ale",
  "whisky",
  "lemonade",
  "firebreather",
  "local",
  "juice",
  "milk",
  "tea",
  "coffee",
  "blood",
  "salt",
  "water",
  "\n"
};


/* effect of drinks on hunger, thirst, and drunkenness -- see values.doc */
const int drink_aff[][3] =
{
  {0, 1, 10},
  {3, 2, 5},
  {5, 2, 5},
  {2, 2, 5},
  {1, 2, 5},
  {6, 1, 4},
  {0, 1, 8},
  {10, 0, 0},
  {3, 3, 3},
  {0, 4, -8},
  {0, 3, 6},
  {0, 1, 6},
  {-1, 1, 6},
  {0, 2, -1},
  {0, 1, -2},
  {0, 0, 13}
};


/* color of the various drinks */
const char *color_liquid[] =
{
  "clear",
  "brown",
  "clear",
  "brown",
  "dark",
  "golden",
  "red",
  "green",
  "clear",
  "light green",
  "white",
  "brown",
  "black",
  "red",
  "clear",
  "crystal clear"
};


/* level of fullness for drink containers */
const char *fullness[] =
{
  "less than half ",
  "about half ",
  "more than half ",
  ""
};

/* str, int, wis, dex, con applies ************************************* */


/* [ch] strength apply (all) */
const struct str_app_type str_app[] =
{
  {0, 0},		/* str = 0 */
  {3, 1},		/* str = 1 */
  {3, 2},
  {10, 3},
  {25, 4},
  {55, 5},		/* str = 5 */
  {80, 6},
  {90, 7},
  {100, 8},
  {100, 9},
  {115, 10},		/* str = 10 */
  {115, 11},
  {140, 12},
  {140, 13},
  {170, 14},
  {170, 15},		/* str = 15 */
  {195, 16},
  {220, 18},
  {255, 20},		/* dex = 18 */
  {640, 40},
  {700, 40},		/* str = 20 */
  {810, 40},
  {970, 40},
  {1130, 40},
  {1440, 40},
  {1750, 40},		/* str = 25 */
  {280, 22},		/* str = 18/0 - 18-50 */
  {305, 24},		/* str = 18/51 - 18-75 */
  {330, 26},		/* str = 18/76 - 18-90 */
  {380, 28},		/* str = 18/91 - 18-99 */
  {480, 30}		/* str = 18/100 */
};



/* [dex] skill apply (thieves only) */
const struct dex_skill_type dex_app_skill[] =
{
  {-99, -99, -90, -99, -60},	/* dex = 0 */
  {-90, -90, -60, -90, -50},	/* dex = 1 */
  {-80, -80, -40, -80, -45},
  {-70, -70, -30, -70, -40},
  {-60, -60, -30, -60, -35},
  {-50, -50, -20, -50, -30},	/* dex = 5 */
  {-40, -40, -20, -40, -25},
  {-30, -30, -15, -30, -20},
  {-20, -20, -15, -20, -15},
  {-15, -10, -10, -20, -10},
  {-10, -5, -10, -15, -5},	/* dex = 10 */
  {-5, 0, -5, -10, 0},
  {0, 0, 0, -5, 0},
  {0, 0, 0, 0, 0},
  {0, 0, 0, 0, 0},
  {0, 0, 0, 0, 0},		/* dex = 15 */
  {0, 5, 0, 0, 0},
  {5, 10, 0, 5, 5},
  {10, 15, 5, 10, 10},		/* dex = 18 */
  {15, 20, 10, 15, 15},
  {15, 20, 10, 15, 15},		/* dex = 20 */
  {20, 25, 10, 15, 20},
  {20, 25, 15, 20, 20},
  {25, 25, 15, 20, 20},
  {25, 30, 15, 25, 25},
  {25, 30, 15, 25, 25}		/* dex = 25 */
};



/* [dex] apply (all) */
struct dex_app_type dex_app[] =
{
  {-7, -7, 6},			/* dex = 0 */
  {-6, -6, 5},			/* dex = 1 */
  {-4, -4, 5},
  {-3, -3, 4},
  {-2, -2, 3},
  {-1, -1, 2},			/* dex = 5 */
  {0, 0, 1},
  {0, 0, 0},
  {0, 0, 0},
  {0, 0, 0},
  {0, 0, 0},			/* dex = 10 */
  {0, 0, 0},
  {0, 0, 0},
  {0, 0, 0},
  {0, 0, 0},
  {0, 0, -1},			/* dex = 15 */
  {1, 1, -2},
  {2, 2, -3},
  {2, 2, -4},			/* dex = 18 */
  {3, 3, -4},
  {3, 3, -4},			/* dex = 20 */
  {4, 4, -5},
  {4, 4, -5},
  {4, 4, -5},
  {5, 5, -6},
  {5, 5, -6}			/* dex = 25 */
};



/* [con] apply (all) */
struct con_app_type con_app[] =
{
  {-4, 20},			/* con = 0 */
  {-3, 25},			/* con = 1 */
  {-2, 30},
  {-2, 35},
  {-1, 40},
  {-1, 45},			/* con = 5 */
  {-1, 50},
  {0, 55},
  {0, 60},
  {0, 65},
  {0, 70},			/* con = 10 */
  {0, 75},
  {0, 80},
  {0, 85},
  {0, 88},
  {1, 90},			/* con = 15 */
  {2, 95},
  {2, 97},
  {3, 99},			/* con = 18 */
  {3, 99},
  {4, 99},			/* con = 20 */
  {5, 99},
  {5, 99},
  {5, 99},
  {6, 99},
  {6, 99}			/* con = 25 */
};



/* [int] apply (all) */
struct int_app_type int_app[] =
{
  {3},				/* int = 0 */
  {5},				/* int = 1 */
  {7},
  {8},
  {9},
  {10},				/* int = 5 */
  {11},
  {12},
  {13},
  {15},
  {17},				/* int = 10 */
  {19},
  {22},
  {25},
  {30},
  {35},				/* int = 15 */
  {40},
  {45},
  {50},				/* int = 18 */
  {53},
  {55},				/* int = 20 */
  {56},
  {57},
  {58},
  {59},
  {60}				/* int = 25 */
};


/* [wis] apply (all) */
struct wis_app_type wis_app[] =
{
  {0},				/* wis = 0 */
  {0},				/* wis = 1 */
  {0},
  {0},
  {0},
  {0},				/* wis = 5 */
  {0},
  {0},
  {0},
  {0},
  {0},				/* wis = 10 */
  {0},
  {2},
  {2},
  {3},
  {3},				/* wis = 15 */
  {3},
  {4},
  {5},				/* wis = 18 */
  {6},
  {6},				/* wis = 20 */
  {6},
  {6},
  {7},
  {7},
  {7}				/* wis = 25 */
};

/* ----------------------------------------------------------------------- */
/* -------------------Spell Message Modifications Here-------------------- */
/* ----------------------------------------------------------------------- */
/* Note: There's ALOT to modify here, keep scrolling down until it tells you
   to stop :P */
const char *spell_wear_off_msg[] =
{
  "RESERVED DB.C",		/* 0 */
  "You feel less protected.",	/* 1 */
  "!Teleport!",
  "You feel less righteous.",
  "Your vision returns.",
  "You feel more self-confident.",
  "!Clone!",    /* 10 */
  "!Control Weather!",
  "!Create Food!",
  "!Create Water!",
  "!Cure Blind!",
  "!Cure Critic!",		/* 15 */
  "!Cure Light!",
  "You feel more optimistic.",
  "You feel less aware.",
  "Your eyes stop tingling.",
  "The detect magic wears off.",	/* 20 */
  "The detect poison wears off.",
  "!Earthquake!",
  "!Enchant Weapon!",
  "!Heal!",
  "You feel yourself exposed.",		/* 30 */
  "!Locate object!",
  "You feel less sick.",
  "!Remove Curse!",
  "The white aura around your body fades.",
  "You feel less tired.",
  "You feel weaker.",
  "!Summon!",			/* 40 */
  "!Word of Recall!",
  "!Remove Poison!",
  "You feel less aware of your surroundings.",
  "!Animate Dead!",		/* 45 */
  "!Group Armor!",
  "!Group Heal!",
  "!Group Recall!",
  "Your night vision seems to fade.",	/* 50 */
  "Your feet seem less boyant.",
  "Your skin slowly softens as it returns to normal.",
  "You regain your nerves.",
  "!Recharge!",
  "!Portal!",
  "!Group Stone Skin!",
  "!Locate Target!",
  "Your focus of magic power diminishes back to normal.",
  "Your ability to conserve mana use has worn off.",
  "You feel that you may now be affected by portals.",
  "!Regen Mana!",
  "!Home!",
  "!Word of Retreat!",
  "You are liberated of your chains!",
  "The &Kglow&n around your body fades."
};

const char *spell_affect_msg[] =
{
  "RESERVED DB.C",		/* 0 */
  "You feel someone protecting you.",	/* 1 */
  "!Teleport!",
  "You feel righteous.",
  "You have been blinded!",
  "You feel very uncomfortable.",
  "!Clone!",    /* 10 */
  "!Control Weather!",
  "!Create Food!",
  "!Create Water!",
  "!Cure Blind!",
  "!Cure Critic!",		/* 15 */
  "!Cure Light!",
  "You feel very uncomfortable.",
  "Your eyes tingle.",
  "Your eyes tingle.",
  "Your eyes tingle.",	/* 20 */
  "Your eyes tingle.",
  "!Earthquake!",
  "!Enchant Weapon!",
  "!Heal!",
  "You vanish.",		/* 30 */
  "!Locate object!",
  "You feel very sick.",
  "!Remove Curse!",
  "A white aura momentarily surrounds you.",
  "You feel very sleepy...  Zzzz......",
  "You feel stronger!",
  "!Summon!",			/* 40 */
  "!Word of Recall!",
  "!Remove Poison!",
  "Your feel your awareness improve.",
  "!Animate Dead!",		/* 45 */
  "!Group Armor!",
  "!Group Heal!",
  "!Group Recall!",
  "Your eyes glow red.",	/* 50 */
  "Your feel webbing between your toes.",
  "Your skin turns to stone!.",
  "!Fear!",
  "!Recharge!",
  "!Portal!",
  "!Group Stone Skin!",
  "!Locate Target!",
  "A red aura momentarily surrounds you.",
  "A green aura momentarily surrounds you.",
  "You feel a protection from the heavens.",
  "!Regen Mana!",
  "!Home!",
  "!Word of Retreat!",
  "Your feet are suddenly chained together!",
  "You feel electrified and &Kglow&n!"
};

/* Perm-Item Affects (we're getting a bit repetetive, but it must be done...) */
/* Note: ONLY AFF_ affect/unaffect messages go here, nothing else. */

const char *affect_wear_off_msg[] =
{
  "Your vision returns.",
  "You feel yourself exposed.",
  "You feel less aware.",
  "Your eyes stop tingling.",
  "The detect magic wears off.",
  "You feel less aware of your surroundings.",
  "Your feet seem less boyant.",
  "The white aura around your body fades.",
  "!Group!",
  "You feel more optimistic.",
  "Your night vision seems to fade.",
  "You feel less sick.",
  "!UNUSED!",
  "!UNUSED!",
  "You feel less tired.",
  "Your feet settle back to the ground.",
  "You feel the wild inside of you... ROAR!",
  "Your focus of magic power diminishes back to normal.",
  "You feel less sneaky.",
  "You feel yourself exposed.",
  "Your ability to conserve mana use has worn off.",
  "You feel more self-confident.",
  "You feel that you may now be affected by portals.",
  "You suddenly feel VERY healthy!",
  "You are liberated of your chains!",
  "The &Kglow&n around you fades.",
  "You can no longer maintain your control over the lightning charge and it &RSURGES&n through your body!"
};

const char *affect_aff_msg[] =
{
  "You have been blinded!",
  "You vanish.",
  "Your eyes tingle.",
  "Your eyes tingle.",
  "Your eyes tingle.",
  "Your feel your awareness improve.",
  "Your feel webbing between your toes.",
  "A white aura momentarily surrounds you.",
  "!Group!",
  "You feel very uncomfortable.",
  "Your eyes glow red.",
  "You feel very sick.",
  "!UNUSED!",
  "!UNUSED!",
  "You feel very sleepy...  Zzzz......",
  "Your feet begin to glide slightly above the ground.",
  "You feel very tame.",
  "A red aura momentarily surrounds you.",
  "You feel very sneaky.",
  "You are enshrouded!",
  "A green aura momentarily surrounds you.",
  "You feel very uncomfortable.",
  "You feel a protection from the heavens.",
  "You become violently &eill&n.",
  "Your feet are suddenly chained together!",
  "You feel electrified and &Kglow&n!",
  "Your hair stands on end and you feel &KELECTRIFIED&n!"
};
/* ----------------------------------------------------------------------- */

const char *npc_class_types[] =
{
  "Normal",
  "Undead",
  "\n"
};



const int rev_dir[] =
{
  2,
  3,
  0,
  1,

#if defined(OASIS_MPROG)
/*
 * Definitions necessary for MobProg support in OasisOLC
 */
  const char *mobprog_types[] =
  {
    "INFILE",
    "ACT",
    "SPEECH",
    "RAND",
    "FIGHT",
    "DEATH",
    "HITPRCNT",
    "ENTRY",
    "GREET",
    "ALL_GREET",
    "GIVE",
    "BRIBE",
    "\n"
  };
#endif
  5,
  4
  };

  const int movement_loss[] =
  {
  1,                            /* Inside       */
  1,                            /* City         */
  2,                            /* Field        */
  3,                            /* Forest       */
  4,                            /* Hills        */
  6,                            /* Mountains    */
  4,                            /* Swimming     */
  1,                            /* Unswimable   */
  1,                            /* Flying       */
  5,                            /* Underwater   */
  3,                            /* Road         */
  1,                            /* Wall         */
  5,                            /* Swamp        */
  6,                            /* Desert       */
  3                             /* Bridge       */
 };

  const int special_movement_loss[] =
  {
  10,                            /* Inside     */
  10,                            /* City       */
  20,                            /* Field      */
  30,                            /* Forest     */
  40,                            /* Hills      */
  60,                            /* Mountains  */
  40,                            /* Swimming   */
  10,                            /* Unswimable */
  10,                            /* Flying     */
  50,                            /* Underwater */
  15,                            /* Road       */
  10,                            /* Wall       */
  50,                            /* Swamp      */
  60,                            /* Desert     */
  30                             /* Bridge     */   
  };

  const char *weekdays[] =
  {
  "the Day of the Moon",
  "the Day of the Great Disaster",
  "the Day of the Deception",
  "the Day of Thunder",
  "the Day of Freedom",
  "the Day of the Great Gods",
  "the Day of the Sun"
  };


  const char *month_name[] =
  {
  "Month of Winter",		/* 0 */
  "Month of the Winter Wolf",
  "Month of the Frost Giant",
  "Month of the Old Forces",
  "Month of the Grand Struggle",
  "Month of the Spring",
  "Month of Nature",
  "Month of Futility",
  "Month of the Dragon",
  "Month of the Sun",
  "Month of the Heat",
  "Month of the Battle",
  "Month of the Dark Shades",
  "Month of the Shadows",
  "Month of the Long Shadows",
  "Month of the Ancient Darkness",
  "Month of the Great Evil"
  };
const struct citizen_type citizen_titles[7] =
{
  {"Peasant ", "Peasant ", "Peasant "},
  {"Squire ", "Squire ", "Squire "},
  {"Knight ", "Knight ", "Knight "},
  {"Nobleman ", "Lady ", "Noble "},
  {"Duke ", "Duchess ", "Duke "},
  {"Master ", "Mistress ", "Master "},
  {"King ", "Queen ", "King "}
};
const char *citizen_colors[] = {
  KBCYN,
  KBCYN,
  KBCYN,
  KBCYN,
  KBCYN,
  KBCYN,
  KBGRN,
};
