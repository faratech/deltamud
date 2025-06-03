/* ************************************************************************
   *   File: spell_parser.c                                Part of CircleMUD *
   *  Usage: top-level magic routines; outside points of entry to magic sys. *
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
#include "interpreter.h"
#include "spells.h"
#include "handler.h"
#include "comm.h"
#include "db.h"

struct spell_info_type spell_info[TOP_SPELL_DEFINE + 1];

#define SINFO spell_info[spellnum]

extern struct room_data *world;

/*
 * This arrangement is pretty stupid, but the number of skills is limited by
 * the playerfile.  We can arbitrarily increase the number of skills by
 * increasing the space in the playerfile. Meanwhile, this should provide
 * ample slots for skills.
 */

char *spells[] =
{
  "!RESERVED!",			/* 0 - reserved */

  /* SPELLS */

  "armor",			/* 1 */
  "teleport",
  "bless",
  "blindness",
  "charm person",
  "clone",
  "control weather",
  "create food",
  "create water",
  "cure blind",			/* 10 */
  "cure critic",
  "cure light",
  "curse",
  "detect alignment",
  "detect invisibility",
  "detect magic",
  "detect poison",
  "earthquake",
  "enchant weapon",
  "heal",			/* 20 */
  "invisibility",
  "locate object",
  "poison",
  "remove curse",
  "sanctuary",
  "sleep",
  "strength",
  "summon",
  "word of recall",
  "remove poison",		/* 30 */
  "sense life",
  "animate dead",
  "group armor",
  "group heal",
  "group recall",
  "infravision",
  "waterwalk",
  "stone skin",
  "fear",
  "recharge",			/* 40 */
  "portal",
  "group stone skin",
  "locate life",
  "convergence of power",
  "mana autus",
  "resist portal",
  "regen mana",
  "home",
  "word of retreat",
  "chain footing",			/* 50 */
  "redirect charge", 
  "!UNUSED", 
  "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 55 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 60 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 65 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 70 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 75 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 80 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 85 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 90 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 95 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 100 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 105 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 110 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 115 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 120 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 125 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 130 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 135 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 140 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 145 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 150 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 155 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 160 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 165 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 170 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 175 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 180 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 185 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 190 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 195 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 200 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 205 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 210 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 215 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 220 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 225 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 230 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 235 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 240 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 245 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 250 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 255 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 260 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 265 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 270 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 275 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 280 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 285 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 290 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 295 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 300 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 305 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 310 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 315 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 320 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 325 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 330 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 335 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 340 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 345 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 350 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 355 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 360 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 365 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 370 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 375 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 380 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 385 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 390 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 395 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 400 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 405 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 410 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 415 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 420 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 425 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 430 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 435 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 440 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 445 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 450 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 455 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 460 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 465 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 470 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 475 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 480 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 485 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 490 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 495 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 500 */

 /* SKILLS */
  "backstab",			/* 501 */
  "bash",
  "hide",
  "kick",
  "pick lock",
  "punch",
  "rescue",
  "sneak",
  "steal",
  "track",			/* 510 */
  "forage",
  "scan",
  "brew",
  "forge",
  "scribe",	/* 515 */
  "speed",
  "berserk",
  "camouflage",
  "blanket",
  "ram",	        /* 520 */
  "mount",
  "riding",
  "tame",
  "second attack",
  "third attack",	/* 525 */
  "listen",
  "meditate",
  "repair",
  "tan",
  "fillet",	/* 530 */
  "carve",
  "dodge",
  "parry",
  "avoid",
  "riposte",	/* 535 */
  "circle",
  "trip",
  "disarm",
  "target",
  "adrenaline",  /* 540 */
  "bloodlust",
  "carnal rage", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 545 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 550 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 555 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 560 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 565 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",	/* 570 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 575 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 580 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 585 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 590 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 595 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 600 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 605 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 610 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 615 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 620 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 625 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 630 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 635 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 640 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 645 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 650 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 655 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 660 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 665 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 670 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 675 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 680 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 685 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 690 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 695 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 700 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 705 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 710 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 715 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 720 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 725 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 730 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 735 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 740 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 745 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 750 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 755 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 760 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 765 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 770 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 775 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 780 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 785 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 790 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 795 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 800 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 805 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 810 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 815 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 820 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 825 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 830 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 835 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 840 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 845 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 850 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 855 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 860 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 865 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 870 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 875 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 880 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 885 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 890 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 895 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 900 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 905 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 910 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 915 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 920 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 925 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 930 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 935 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 940 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 945 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 950 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 955 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 960 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 965 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 970 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 975 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 980 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 985 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 990 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 995 */
  "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!", "!UNUSED!",   /* 1000 */

 
/* OBJECT SPELLS AND NPC SPELLS/SKILLS */

  "identify",			/* 1001(?) */
  "fire breath",
  "gas breath",
  "frost breath",
  "acid breath",
  "lightning breath",          /* 1005 */

  "\n"				/* the end */
};


struct syllable
  {
    char *org;
    char *new;
  };


struct syllable syls[] =
{
  {" ", " "},
  {"ar", "abra"},
  {"ate", "i"},
  {"cau", "kada"},
  {"blind", "nose"},
  {"bur", "mosa"},
  {"cu", "judi"},
  {"de", "oculo"},
  {"dis", "mar"},
  {"ect", "kamina"},
  {"en", "uns"},
  {"gro", "cra"},
  {"light", "dies"},
  {"lo", "hi"},
  {"magi", "kari"},
  {"mon", "bar"},
  {"mor", "zak"},
  {"move", "sido"},
  {"ness", "lacri"},
  {"ning", "illa"},
  {"per", "duda"},
  {"ra", "gru"},
  {"re", "candus"},
  {"son", "sabru"},
  {"tect", "infra"},
  {"tri", "cula"},
  {"ven", "nofo"},
  {"word of", "inset"},
  {"a", "i"},
  {"b", "v"},
  {"c", "q"},
  {"d", "m"},
  {"e", "o"},
  {"f", "y"},
  {"g", "t"},
  {"h", "p"},
  {"i", "u"},
  {"j", "y"},
  {"k", "t"},
  {"l", "r"},
  {"m", "w"},
  {"n", "b"},
  {"o", "a"},
  {"p", "s"},
  {"q", "d"},
  {"r", "f"},
  {"s", "g"},
  {"t", "h"},
  {"u", "e"},
  {"v", "z"},
  {"w", "x"},
  {"x", "n"},
  {"y", "l"},
  {"z", "k"},
  {"", ""}
};


int 
mag_manacost (struct char_data *ch, int spellnum)
{
  int mana;

  mana = MAX (SINFO.mana_max - (SINFO.mana_change *
		  (GET_LEVEL (ch) - SINFO.min_level[(int) GET_CLASS (ch)])),
	      SINFO.mana_min);

  /* If char is affected by mana autus, halve the mana */
  if (IS_AFFECTED(ch, AFF_AUTUS))
    mana >>= 1;

  return mana;
}


/* say_spell erodes buf, buf1, buf2 */
void 
say_spell (struct char_data *ch, int spellnum, struct char_data *tch,
	   struct obj_data *tobj)
{
  char lbuf[256];

  struct char_data *i;
  int j, ofs = 0;

  *buf = '\0';
  strcpy (lbuf, spells[spellnum]);

  while (*(lbuf + ofs))
    {
      for (j = 0; *(syls[j].org); j++)
	{
	  if (!strncmp (syls[j].org, lbuf + ofs, strlen (syls[j].org)))
	    {
	      strcat (buf, syls[j].new);
	      ofs += strlen (syls[j].org);
	    }
	}
    }

  if (tch != NULL && tch->in_room == ch->in_room)
    {
      if (tch == ch)
	sprintf (lbuf, "$n closes $s eyes and utters the words, '%%s'.");
      else
	sprintf (lbuf, "$n stares at $N and utters the words, '%%s'.");
    }
  else if (tobj != NULL &&
	   ((tobj->in_room == ch->in_room) || (tobj->carried_by == ch)))
    sprintf (lbuf, "$n stares at $p and utters the words, '%%s'.");
  else
    sprintf (lbuf, "$n utters the words, '%%s'.");

  sprintf (buf1, lbuf, spells[spellnum]);
  sprintf (buf2, lbuf, buf);

  for (i = world[ch->in_room].people; i; i = i->next_in_room)
    {
      if (i == ch || i == tch || !i->desc || !AWAKE (i))
	continue;
      if (GET_CLASS (ch) == GET_CLASS (i))
	perform_act (buf1, ch, tobj, tch, i);
      else
	perform_act (buf2, ch, tobj, tch, i);
    }

  /* Arena mods - Thargor */
  /* Why do we take the caster's and the recipient points of view? 
   * Not their view of the events, we use a bystander's desc of the 
   * events, but report to the caster's AND receipient's observers.
   * We cannot assume that there are other bystanders from which 
   * to look at things through.
   */
  /* From the caster's point of view */
  i = OBSERVE_BY(ch);
  while (i != NULL){
      if (GET_CLASS (ch) == GET_CLASS (i))
	perform_act (buf1, ch, tobj, tch, i);
      else
	perform_act (buf2, ch, tobj, tch, i);

       i = OBSERVE_BY(i);
  }

  if (tch != NULL && tch != ch && tch->in_room == ch->in_room)
    {
      /* Arena mods - Thargor */
      /* From the receipient's point of view */
      i = OBSERVE_BY(tch);
      while (i != NULL){
	if (GET_CLASS (tch) == GET_CLASS (i))
	  perform_act (buf1, ch, tobj, tch, i);
	else
	  perform_act (buf2, ch, tobj, tch, i);
	
	i = OBSERVE_BY(i);
      }

      sprintf (buf1, "$n stares at you and utters the words, '%s'.",
	       GET_CLASS (ch) == GET_CLASS (tch) ? spells[spellnum] : buf);
      act (buf1, FALSE, ch, NULL, tch, TO_VICT);
    }
}


char *
skill_name (int num)
{
  int i = 0;

  if (num <= 0)
    {
      if (num == -1)
	return "UNUSED";
      else
	return "UNDEFINED";
    }

  while (num && *spells[i] != '\n')
    {
      num--;
      i++;
    }

  if (*spells[i] != '\n')
    return spells[i];
  else
    return "UNDEFINED";
}


int 
find_skill_num (char *name)
{
  int index = 0, ok;
  char *temp, *temp2;
  char first[256], first2[256];

  while (*spells[++index] != '\n')
    {
      if (is_abbrev (name, spells[index]))
	return index;

      ok = 1;
      temp = any_one_arg (spells[index], first);
      temp2 = any_one_arg (name, first2);
      while (*first && *first2 && ok)
	{
	  if (!is_abbrev (first2, first))
	    ok = 0;
	  temp = any_one_arg (temp, first);
	  temp2 = any_one_arg (temp2, first2);
	}

      if (ok && !*first2)
	return index;
    }

  return -1;
}



/*
 * This function is the very heart of the entire magic system.  All
 * invocations of all types of magic -- objects, spoken and unspoken PC
 * and NPC spells, the works -- all come through this function eventually.
 * This is also the entry point for non-spoken or unrestricted spells.
 * Spellnum 0 is legal but silently ignored here, to make callers simpler.
 */
int 
call_magic (struct char_data *caster, struct char_data *cvict,
	    struct obj_data *ovict, int spellnum, int level)
{
  ASPELL(spell_locate_target);
  ASPELL(spell_home);
  ASPELL(spell_retreat);

  if (spellnum < 1 || spellnum > TOP_SPELL_DEFINE)
    return 0;

  if (ROOM_FLAGGED (caster->in_room, ROOM_NOMAGIC) && GET_LEVEL (caster) < LVL_IMPL)
    {
      send_to_char ("Your magic fizzles out and dies.\r\n", caster);
      act ("$n's magic fizzles out and dies.", FALSE, caster, 0, 0, TO_ROOM);
      return 0;
    }


  if (GET_LEVEL (caster) < LVL_IMPL && IS_SET (ROOM_FLAGS (caster->in_room), ROOM_PEACEFUL) &&
      (SINFO.violent || IS_SET (SINFO.routines, MAG_DAMAGE)))
    {
      send_to_char ("A flash of white light fills the room, dispelling your "
		    "violent magic!\r\n", caster);
      act ("White light from no particular source suddenly fills the room, "
	   "then vanishes.", FALSE, caster, 0, 0, TO_ROOM);
      return 0;
    }

  if (IS_SET (SINFO.routines, MAG_DAMAGE))
    mag_damage (level, caster, cvict, spellnum);

  if (IS_SET (SINFO.routines, MAG_AFFECTS))
    mag_affects (level, caster, cvict, spellnum);

  if (IS_SET (SINFO.routines, MAG_UNAFFECTS))
    mag_unaffects (level, caster, cvict, spellnum);

  if (IS_SET (SINFO.routines, MAG_POINTS))
    mag_points (level, caster, cvict, spellnum);

  if (IS_SET (SINFO.routines, MAG_ALTER_OBJS))
    mag_alter_objs (level, caster, ovict, spellnum);

  if (IS_SET (SINFO.routines, MAG_GROUPS))
    mag_groups (level, caster, spellnum);

  if (IS_SET (SINFO.routines, MAG_MASSES))
    mag_masses (level, caster, spellnum);

  if (IS_SET (SINFO.routines, MAG_AREAS))
    mag_areas (level, caster, spellnum);

  if (IS_SET (SINFO.routines, MAG_SUMMONS))
    mag_summons (level, caster, ovict, spellnum);

  if (IS_SET (SINFO.routines, MAG_CREATIONS))
    mag_creations (level, caster, spellnum);

  if (IS_SET (SINFO.routines, MAG_MANUAL))
    switch (spellnum)
      {
      case SPELL_CHARM:
	MANUAL_SPELL (spell_charm);
	break;
      case SPELL_CREATE_WATER:
	MANUAL_SPELL (spell_create_water);
	break;
      case SPELL_DETECT_POISON:
	MANUAL_SPELL (spell_detect_poison);
	break;
      case SPELL_ENCHANT_WEAPON:
	MANUAL_SPELL (spell_enchant_weapon);
	break;
      case SPELL_IDENTIFY:
	MANUAL_SPELL (spell_identify);
	break;
      case SPELL_LOCATE_OBJECT:
	MANUAL_SPELL (spell_locate_object);
	break;
      case SPELL_SUMMON:
	MANUAL_SPELL (spell_summon);
	break;
      case SPELL_WORD_OF_RECALL:
	MANUAL_SPELL (spell_recall);
	break;
      case SPELL_FEAR:
	MANUAL_SPELL (spell_fear);
	break;
      case SPELL_RECHARGE:
        MANUAL_SPELL (spell_recharge);
        break;
      case SPELL_PORTAL:
        MANUAL_SPELL(spell_portal);
        break;
      case SPELL_LOCATE_TARGET:
        MANUAL_SPELL(spell_locate_target);
        break;
      case SPELL_HOME:
        MANUAL_SPELL(spell_home);
        break;
      case SPELL_WORD_OF_RETREAT:
        MANUAL_SPELL(spell_retreat);
        break;
      }

  return 1;
}

/*
 * mag_objectmagic: This is the entry-point for all magic items.  This should
 * only be called by the 'quaff', 'use', 'recite', etc. routines.
 *
 * For reference, object values 0-3:
 * staff  - [0] level   [1] max charges [2] num charges [3] spell num
 * wand   - [0] level   [1] max charges [2] num charges [3] spell num
 * scroll - [0] level   [1] spell num   [2] spell num   [3] spell num
 * potion - [0] level   [1] spell num   [2] spell num   [3] spell num
 *
 * Staves and wands will default to level 14 if the level is not specified;
 * the DikuMUD format did not specify staff and wand levels in the world
 * files (this is a CircleMUD enhancement).
 */

void 
mag_objectmagic (struct char_data *ch, struct obj_data *obj,
		 char *argument)
{
  int i, k;
  struct char_data *tch = NULL, *next_tch;
  struct obj_data *tobj = NULL;

  one_argument (argument, arg);

  k = generic_find (arg, FIND_CHAR_ROOM | FIND_OBJ_INV | FIND_OBJ_ROOM |
		    FIND_OBJ_EQUIP, ch, &tch, &tobj);

  if (!IS_NPC(ch) && tch != NULL){
    if (!IS_NPC(tch))
      if (GET_LEVEL (ch) < LVL_IMMORT && GET_LEVEL(tch) >= LVL_IMMORT){
	send_to_char ("A blinding flash of white light dispels your magic!\r\n", ch);
	act ("$n attempts to cast magic on $N.\r\n"
	     "A blinding flash of white light dispels $n's magic.", 
	     FALSE, ch, 0, tch, TO_ROOM);
	return; 
      }
  }

  switch (GET_OBJ_TYPE (obj))
    {
    case ITEM_STAFF:
      act ("You tap $p three times on the ground.", FALSE, ch, obj, 0, TO_CHAR);
      if (obj->action_description)
	act (obj->action_description, FALSE, ch, obj, 0, TO_ROOM);
      else
	act ("$n taps $p three times on the ground.", FALSE, ch, obj, 0, TO_ROOM);

      if (GET_OBJ_VAL (obj, 2) <= 0)
	{
	  act ("It seems powerless.", FALSE, ch, obj, 0, TO_CHAR);
	  act ("Nothing seems to happen.", FALSE, ch, obj, 0, TO_ROOM);
	}
      else
	{
	  GET_OBJ_VAL (obj, 2)--;
	  WAIT_STATE (ch, PULSE_VIOLENCE);
	  for (tch = world[ch->in_room].people; tch; tch = next_tch)
	    {
	      next_tch = tch->next_in_room;
	      if (ch == tch)
		continue;
	      if (GET_OBJ_VAL (obj, 0))
		call_magic (ch, tch, NULL, GET_OBJ_VAL (obj, 3),
			    GET_OBJ_VAL (obj, 0));
	      else
		call_magic (ch, tch, NULL, GET_OBJ_VAL (obj, 3),
			    DEFAULT_STAFF_LVL);
	    }
	}
      break;
    case ITEM_WAND:
      if (k == FIND_CHAR_ROOM)
	{
	  if (tch == ch)
	    {
	      act ("You point $p at yourself.", FALSE, ch, obj, 0, TO_CHAR);
	      act ("$n points $p at $mself.", FALSE, ch, obj, 0, TO_ROOM);
	    }
	  else
	    {
	      act ("You point $p at $N.", FALSE, ch, obj, tch, TO_CHAR);
	      if (obj->action_description != NULL)
		act (obj->action_description, FALSE, ch, obj, tch, TO_ROOM);
	      else
		act ("$n points $p at $N.", TRUE, ch, obj, tch, TO_ROOM);
	    }
	}
      else if (tobj != NULL)
	{
	  act ("You point $p at $P.", FALSE, ch, obj, tobj, TO_CHAR);
	  if (obj->action_description != NULL)
	    act (obj->action_description, FALSE, ch, obj, tobj, TO_ROOM);
	  else
	    act ("$n points $p at $P.", TRUE, ch, obj, tobj, TO_ROOM);
	}
      else
	{
	  act ("At what should $p be pointed?", FALSE, ch, obj, NULL, TO_CHAR);
	  return;
	}

      if (GET_OBJ_VAL (obj, 2) <= 0)
	{
	  act ("It seems powerless.", FALSE, ch, obj, 0, TO_CHAR);
	  act ("Nothing seems to happen.", FALSE, ch, obj, 0, TO_ROOM);
	  return;
	}
      GET_OBJ_VAL (obj, 2)--;
      WAIT_STATE (ch, PULSE_VIOLENCE);
      if (GET_OBJ_VAL (obj, 0))
	call_magic (ch, tch, tobj, GET_OBJ_VAL (obj, 3),
		    GET_OBJ_VAL (obj, 0));
      else
	call_magic (ch, tch, tobj, GET_OBJ_VAL (obj, 3),
		    DEFAULT_WAND_LVL);
      break;
    case ITEM_SCROLL:
      if (*arg)
	{
	  if (!k)
	    {
	      act ("There is nothing to here to affect with $p.", FALSE,
		   ch, obj, NULL, TO_CHAR);
	      return;
	    }
	}
      else
	tch = ch;

      act ("You recite $p which dissolves.", TRUE, ch, obj, 0, TO_CHAR);
      if (obj->action_description)
	act (obj->action_description, FALSE, ch, obj, NULL, TO_ROOM);
      else
	act ("$n recites $p.", FALSE, ch, obj, NULL, TO_ROOM);

      WAIT_STATE (ch, PULSE_VIOLENCE);
      for (i = 1; i < 4; i++)
	if (!(call_magic (ch, tch, tobj, GET_OBJ_VAL (obj, i),
			  GET_OBJ_VAL (obj, 0))))
	  break;

      if (obj != NULL)
	extract_obj (obj);
      break;
    case ITEM_POTION:
      tch = ch;
      act ("You quaff $p.", FALSE, ch, obj, NULL, TO_CHAR);
      if (obj->action_description)
	act (obj->action_description, FALSE, ch, obj, NULL, TO_ROOM);
      else
	act ("$n quaffs $p.", TRUE, ch, obj, NULL, TO_ROOM);

      WAIT_STATE (ch, PULSE_VIOLENCE);
      for (i = 1; i < 4; i++)
	if (!(call_magic (ch, ch, NULL, GET_OBJ_VAL (obj, i),
			  GET_OBJ_VAL (obj, 0))))
	  break;

      if (obj != NULL)
	extract_obj (obj);
      break;
    default:
      log ("SYSERR: Unknown object_type in mag_objectmagic");
      break;
    }
}


/*
 * cast_spell is used generically to cast any spoken spell, assuming we
 * already have the target char/obj and spell number.  It checks all
 * restrictions, etc., prints the words, etc.
 *
 * Entry point for NPC casts.  Recommended entry point for spells cast
 * by NPCs via specprocs.
 */

int 
cast_spell (struct char_data *ch, struct char_data *tch,
	    struct obj_data *tobj, int spellnum)
{
  char buf[256];

  if (spellnum < 0 || spellnum > TOP_SPELL_DEFINE)
    {
      sprintf (buf, "SYSERR: cast_spell trying to call spellnum %d\n", spellnum);
      log (buf);
      return 0;
    }

  if (GET_POS (ch) < SINFO.min_position)
    {
      switch (GET_POS (ch))
	{
	case POS_SLEEPING:
	  send_to_char ("You dream about great magical powers.\r\n", ch);
	  break;
	case POS_RESTING:
	  send_to_char ("You cannot concentrate while resting.\r\n", ch);
	  break;
	case POS_SITTING:
	  send_to_char ("You can't do this sitting!\r\n", ch);
	  break;
	case POS_FIGHTING:
	  send_to_char ("Impossible!  You can't concentrate enough!\r\n", ch);
	  break;
	default:
	  send_to_char ("You can't do much of anything like this!\r\n", ch);
	  break;
	}
      return 0;
    }
  if (IS_AFFECTED (ch, AFF_CHARM) && (ch->master == tch))
    {
      send_to_char ("You are afraid you might hurt your master!\r\n", ch);
      return 0;
    }
  if ((tch != ch) && IS_SET (SINFO.targets, TAR_SELF_ONLY))
    {
      send_to_char ("You can only cast this spell upon yourself!\r\n", ch);
      return 0;
    }
  if ((tch == ch) && IS_SET (SINFO.targets, TAR_NOT_SELF))
    {
      send_to_char ("You cannot cast this spell upon yourself!\r\n", ch);
      return 0;
    }
  if (IS_SET (SINFO.routines, MAG_GROUPS) && !IS_AFFECTED (ch, AFF_GROUP))
    {
      send_to_char ("You can't cast this spell if you're not in a group!\r\n", ch);
      return 0;
    }
  send_to_char (OK, ch);
  say_spell (ch, spellnum, tch, tobj);

  return (call_magic (ch, tch, tobj, spellnum, GET_LEVEL (ch)));
}


/*
 * do_cast is the entry point for PC-casted spells.  It parses the arguments,
 * determines the spell number and finds a target, throws the die to see if
 * the spell can be cast, checks for sufficient mana and subtracts it, and
 * passes control to cast_spell().
 */

ACMD (do_cast)
{
  struct char_data *tch = NULL;
  struct obj_data *tobj = NULL;
  char *s, *t;
  int mana, spellnum, i, target = 0;

  if (IS_NPC (ch))
    return;

  /* get: blank, spell name, target name */
  s = strtok (argument, "'");

  if (s == NULL)
    {
      send_to_char ("Cast what where?\r\n", ch);
      return;
    }
  s = strtok (NULL, "'");
  if (s == NULL)
    {
      send_to_char ("Spell names must be enclosed in the Holy Magic Symbols: '\r\n", ch);
      return;
    }
  t = strtok (NULL, "\0");

  /* spellnum = search_block(s, spells, 0); */
  spellnum = find_skill_num (s);

  if ((spellnum < 1) || (spellnum > MAX_SPELLS))
    {
      send_to_char ("Cast what?!?\r\n", ch);
      return;
    }
  if (GET_LEVEL (ch) < SINFO.min_level[(int) GET_CLASS (ch)])
    {
      send_to_char ("You do not know that spell!\r\n", ch);
      return;
    }
  if (GET_SKILL (ch, spellnum) == 0)
    {
      send_to_char ("You are unfamiliar with that spell.\r\n", ch);
      return;
    }
  /* Find the target */
  if (t != NULL)
    {
      one_argument (strcpy (arg, t), t);
      skip_spaces (&t);
    }
  if (IS_SET (SINFO.targets, TAR_IGNORE))
    {
      target = TRUE;
    }
  else if (t != NULL && *t)
    {
      if (!target && (IS_SET (SINFO.targets, TAR_CHAR_ROOM)))
	{
	  if ((tch = get_char_room_vis (ch, t)) != NULL)
	    target = TRUE;
	}
      if (!target && IS_SET (SINFO.targets, TAR_CHAR_WORLD))
	if ((tch = get_char_vis (ch, t)))
	  target = TRUE;

      if (!target && IS_SET (SINFO.targets, TAR_OBJ_INV))
	if ((tobj = get_obj_in_list_vis (ch, t, ch->carrying)))
	  target = TRUE;

      if (!target && IS_SET (SINFO.targets, TAR_OBJ_EQUIP))
	{
	  for (i = 0; !target && i < NUM_WEARS; i++)
	    if (GET_EQ (ch, i) && CAN_SEE_OBJ (ch, GET_EQ (ch, i)) &&
		isname (t, GET_EQ (ch, i)->name))
	      {
		tobj = GET_EQ (ch, i);
		target = TRUE;
	      }
	}
      if (!target && IS_SET (SINFO.targets, TAR_OBJ_ROOM))
	if ((tobj = get_obj_in_list_vis (ch, t, world[ch->in_room].contents)))
	  target = TRUE;

      if (!target && IS_SET (SINFO.targets, TAR_OBJ_WORLD))
	if ((tobj = get_obj_vis (ch, t)))
	  target = TRUE;

    }
  else
    {				/* if target string is empty */
      if (!target && IS_SET (SINFO.targets, TAR_FIGHT_SELF))
	if (FIGHTING (ch) != NULL)
	  {
	    tch = ch;
	    target = TRUE;
	  }
      if (!target && IS_SET (SINFO.targets, TAR_FIGHT_VICT))
	if (FIGHTING (ch) != NULL)
	  {
	    tch = FIGHTING (ch);
	    target = TRUE;
	  }
      /* if no target specified, and the spell isn't violent, default to self */
      if (!target && IS_SET (SINFO.targets, TAR_CHAR_ROOM) &&
	  !SINFO.violent)
	{
	  tch = ch;
	  target = TRUE;
	}
      if (!target)
	{
	  sprintf (buf, "Upon %s should the spell be cast?\r\n",
		   IS_SET (SINFO.targets, TAR_OBJ_ROOM | TAR_OBJ_INV | TAR_OBJ_WORLD) ?
		   "what" : "who");
	  send_to_char (buf, ch);
	  return;
	}
    }

  if (target && (tch == ch) && SINFO.violent)
    {
      send_to_char ("You shouldn't cast that on yourself -- could be bad for your health!\r\n", ch);
      return;
    }
  if (!target)
    {
      send_to_char ("Cannot find the target of your spell!\r\n", ch);
      return;
    }
  mana = mag_manacost (ch, spellnum);
  if ((mana > 0) && (GET_MANA (ch) < mana) && (GET_LEVEL (ch) < LVL_IMMORT))
    {
      send_to_char ("You haven't the energy to cast that spell!\r\n", ch);
      return;
    }

  /* You throws the dice and you takes your chances.. 101% is total failure */
  if (number (0, 101) > GET_SKILL (ch, spellnum))
    {
      WAIT_STATE (ch, PULSE_VIOLENCE);
      if (!tch || !skill_message (0, ch, tch, spellnum))
	send_to_char ("You lost your concentration!\r\n", ch);
      if (mana > 0)
	GET_MANA (ch) = MAX (0, MIN (GET_MAX_MANA (ch), GET_MANA (ch) - (mana >> 1)));
      if (SINFO.violent && tch && IS_NPC (tch))
	hit (tch, ch, TYPE_UNDEFINED);
    }
  else
    {				/* cast spell returns 1 on success; subtract mana & set waitstate */
      if (cast_spell (ch, tch, tobj, spellnum))
	{
	  WAIT_STATE (ch, PULSE_VIOLENCE);
	  if (mana > 0)
	    GET_MANA (ch) = MAX (0, MIN (GET_MAX_MANA (ch), GET_MANA (ch) - mana));
	}
    }
}



void 
spell_level (int spell, int class, int level)
{
  char buf[256];
  int bad = 0;

  if (spell < 0 || spell > TOP_SPELL_DEFINE)
    {
      sprintf (buf, "SYSERR: attempting assign to illegal spellnum %d", spell);
      log (buf);
      return;
    }

  if (class < 0 || class >= NUM_CLASSES)
    {
      sprintf (buf, "SYSERR: assigning '%s' to illegal class %d",
	       skill_name (spell), class);
      log (buf);
      bad = 1;
    }

  if (level < 1 || level > LVL_IMPL)
    {
      sprintf (buf, "SYSERR: assigning '%s' to illegal level %d",
	       skill_name (spell), level);
      log (buf);
      bad = 1;
    }

  if (!bad)
    spell_info[spell].min_level[class] = level;
}


/* Assign the spells on boot up */
void 
spello (int spl, int max_mana, int min_mana, int mana_change, int minpos,
	int targets, int violent, int routines)
{
  int i;

  for (i = 0; i < NUM_CLASSES; i++)
    spell_info[spl].min_level[i] = LVL_IMMORT;
  spell_info[spl].mana_max = max_mana;
  spell_info[spl].mana_min = min_mana;
  spell_info[spl].mana_change = mana_change;
  spell_info[spl].min_position = minpos;
  spell_info[spl].targets = targets;
  spell_info[spl].violent = violent;
  spell_info[spl].routines = routines;
}


void 
unused_spell (int spl)
{
  int i;

  for (i = 0; i < NUM_CLASSES; i++)
    spell_info[spl].min_level[i] = LVL_IMPL + 1;
  spell_info[spl].mana_max = 0;
  spell_info[spl].mana_min = 0;
  spell_info[spl].mana_change = 0;
  spell_info[spl].min_position = 0;
  spell_info[spl].targets = 0;
  spell_info[spl].violent = 0;
  spell_info[spl].routines = 0;
}

#define skillo(skill) spello(skill, 0, 0, 0, 0, 0, 0, 0);


/*
 * Arguments for spello calls:
 *
 * spellnum, maxmana, minmana, manachng, minpos, targets, violent?, routines.
 *
 * spellnum:  Number of the spell.  Usually the symbolic name as defined in
 * spells.h (such as SPELL_HEAL).
 *
 * maxmana :  The maximum mana this spell will take (i.e., the mana it
 * will take when the player first gets the spell).
 *
 * minmana :  The minimum mana this spell will take, no matter how high
 * level the caster is.
 *
 * manachng:  The change in mana for the spell from level to level.  This
 * number should be positive, but represents the reduction in mana cost as
 * the caster's level increases.
 *
 * minpos  :  Minimum position the caster must be in for the spell to work
 * (usually fighting or standing). targets :  A "list" of the valid targets
 * for the spell, joined with bitwise OR ('|').
 *
 * violent :  TRUE or FALSE, depending on if this is considered a violent
 * spell and should not be cast in PEACEFUL rooms or on yourself.  Should be
 * set on any spell that inflicts damage, is considered aggressive (i.e.
 * charm, curse), or is otherwise nasty.
 *
 * routines:  A list of magic routines which are associated with this spell
 * if the spell uses spell templates.  Also joined with bitwise OR ('|').
 *
 * See the CircleMUD documentation for a more detailed description of these
 * fields.
 */

/*
 * NOTE: SPELL LEVELS ARE NO LONGER ASSIGNED HERE AS OF Circle 3.0 bpl9.
 * In order to make this cleaner, as well as to make adding new classes
 * much easier, spell levels are now assigned in class.c.  You only need
 * a spello() call to define a new spell; to decide who gets to use a spell
 * or skill, look in class.c.  -JE 5 Feb 1996
 */

void 
mag_assign_spells (void)
{
  int i;

  /* Do not change the loop below */
  for (i = 1; i <= TOP_SPELL_DEFINE; i++)
    unused_spell (i);
  /* Do not change the loop above */

  spello (SPELL_ANIMATE_DEAD, 175, 150, 3, POS_STANDING,
	  TAR_OBJ_ROOM, FALSE, MAG_SUMMONS);

  spello (SPELL_ARMOR, 30, 15, 3, POS_FIGHTING,
	  TAR_CHAR_ROOM, FALSE, MAG_AFFECTS);

  spello (SPELL_BLESS, 35, 5, 3, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_OBJ_INV, FALSE, MAG_AFFECTS | MAG_ALTER_OBJS);

  spello (SPELL_BLINDNESS, 35, 25, 1, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_NOT_SELF, FALSE, MAG_AFFECTS);

  spello (SPELL_CHARM, 75, 50, 2, POS_FIGHTING,
	  TAR_CHAR_ROOM | TAR_NOT_SELF, TRUE, MAG_MANUAL);

  spello (SPELL_CLONE, 200, 150, 5, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_SUMMONS);

  spello (SPELL_CONTROL_WEATHER, 75, 25, 5, POS_STANDING,
	  TAR_IGNORE, FALSE, MAG_MANUAL);

  spello (SPELL_CREATE_FOOD, 30, 5, 4, POS_STANDING,
	  TAR_IGNORE, FALSE, MAG_CREATIONS);

  spello (SPELL_CREATE_WATER, 30, 5, 4, POS_STANDING,
	  TAR_OBJ_INV | TAR_OBJ_EQUIP, FALSE, MAG_MANUAL);

  spello (SPELL_CURE_BLIND, 30, 5, 2, POS_STANDING,
	  TAR_CHAR_ROOM, FALSE, MAG_UNAFFECTS);

  spello (SPELL_CURE_CRITIC, 30, 10, 2, POS_FIGHTING,
	  TAR_CHAR_ROOM, FALSE, MAG_POINTS);

  spello (SPELL_CURE_LIGHT, 30, 10, 2, POS_FIGHTING,
	  TAR_CHAR_ROOM, FALSE, MAG_POINTS);

  spello (SPELL_CURSE, 80, 50, 2, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_OBJ_INV, TRUE, MAG_AFFECTS | MAG_ALTER_OBJS);

  spello (SPELL_DETECT_ALIGN, 20, 10, 2, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello (SPELL_DETECT_INVIS, 20, 10, 2, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello (SPELL_DETECT_MAGIC, 20, 10, 2, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello (SPELL_DETECT_POISON, 15, 5, 1, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM, FALSE, MAG_MANUAL);

  spello (SPELL_EARTHQUAKE, 40, 25, 3, POS_FIGHTING,
	  TAR_IGNORE, TRUE, MAG_AREAS);

  spello (SPELL_ENCHANT_WEAPON, 150, 100, 10, POS_STANDING,
	  TAR_OBJ_INV | TAR_OBJ_EQUIP, FALSE, MAG_MANUAL);

  spello (SPELL_GROUP_ARMOR, 50, 30, 2, POS_STANDING,
	  TAR_IGNORE, FALSE, MAG_GROUPS);

  spello (SPELL_GROUP_HEAL, 80, 60, 5, POS_STANDING,
	  TAR_IGNORE, FALSE, MAG_GROUPS);

  spello (SPELL_HEAL, 60, 40, 3, POS_FIGHTING,
	  TAR_CHAR_ROOM, FALSE, MAG_POINTS | MAG_AFFECTS | MAG_UNAFFECTS);

  spello (SPELL_INFRAVISION, 25, 10, 1, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello (SPELL_INVISIBLE, 35, 25, 1, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM, FALSE, MAG_AFFECTS | MAG_ALTER_OBJS);

  spello (SPELL_LOCATE_OBJECT, 25, 20, 1, POS_STANDING,
	  TAR_OBJ_WORLD, FALSE, MAG_MANUAL);

  spello (SPELL_LOCATE_TARGET, 100, 20, 1, POS_STANDING,
	 TAR_CHAR_WORLD, FALSE, MAG_MANUAL);

  spello (SPELL_POISON, 50, 20, 3, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_NOT_SELF | TAR_OBJ_INV, TRUE, MAG_AFFECTS | MAG_ALTER_OBJS);

  spello (SPELL_REMOVE_CURSE, 45, 25, 5, POS_STANDING,
	TAR_CHAR_ROOM | TAR_OBJ_INV, FALSE, MAG_UNAFFECTS | MAG_ALTER_OBJS);

  spello (SPELL_SANCTUARY, 110, 85, 5, POS_STANDING,
	  TAR_CHAR_ROOM, FALSE, MAG_AFFECTS);

  spello (SPELL_SLEEP, 40, 25, 5, POS_STANDING,
	  TAR_CHAR_ROOM, TRUE, MAG_AFFECTS);

  spello (SPELL_STRENGTH, 35, 30, 1, POS_STANDING,
	  TAR_CHAR_ROOM, FALSE, MAG_AFFECTS);

  spello (SPELL_SUMMON, 75, 50, 3, POS_STANDING,
	  TAR_CHAR_WORLD | TAR_NOT_SELF, FALSE, MAG_MANUAL);

  spello (SPELL_WORD_OF_RECALL, 20, 10, 2, POS_FIGHTING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_MANUAL);

  spello (SPELL_REMOVE_POISON, 40, 8, 4, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM, FALSE, MAG_UNAFFECTS | MAG_ALTER_OBJS);

  spello (SPELL_SENSE_LIFE, 20, 10, 2, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello (SPELL_STONE_SKIN, 120, 60, 3, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello (SPELL_FEAR, 100, 50, 1, POS_FIGHTING,
	  TAR_CHAR_ROOM | TAR_NOT_SELF, TRUE, MAG_MANUAL);

  spello (SPELL_RECHARGE, 150, 75, 1, POS_STANDING,
	  TAR_CHAR_ROOM | TAR_NOT_SELF | TAR_OBJ_INV | TAR_OBJ_ROOM, FALSE, MAG_MANUAL);

  spello(SPELL_PORTAL, 170, 100, 5, POS_STANDING,
       TAR_CHAR_WORLD | TAR_NOT_SELF, FALSE, MAG_MANUAL);

  spello (SPELL_GROUP_STONE_SKIN, 240, 120, 2, POS_STANDING,
          TAR_IGNORE, FALSE, MAG_GROUPS);

   spello (SPELL_CONVERGENCE, 110, 85, 5, POS_STANDING,
	  TAR_CHAR_ROOM, FALSE, MAG_AFFECTS);

   spello (SPELL_AUTUS, 140, 100, 5, POS_STANDING,
	  TAR_CHAR_ROOM, FALSE, MAG_AFFECTS);

   spello (SPELL_RESIST_PORTAL, 200, 100, 2, POS_STANDING,
          TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

   spello (SPELL_REGEN_MANA, 150, 150, 0, POS_FIGHTING,
	  TAR_CHAR_ROOM | TAR_NOT_SELF, FALSE, MAG_POINTS);

   spello (SPELL_HOME, 80, 20, 1, POS_FIGHTING,
          TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_MANUAL);

   spello (SPELL_WORD_OF_RETREAT, 30, 20, 1, POS_FIGHTING,
          TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_MANUAL);
 
  spello (SPELL_WATERWALK, 200, 150, 1, POS_FIGHTING,
          TAR_CHAR_ROOM | TAR_SELF_ONLY, FALSE, MAG_AFFECTS);

  spello(SPELL_REDIRECT_CHARGE, 200, 100, 5, POS_STANDING,
	  TAR_CHAR_ROOM, FALSE, MAG_AFFECTS);

  /* NON-castable spells should appear here */
  spello (SPELL_IDENTIFY, 0, 0, 0, 0,
	  TAR_CHAR_ROOM | TAR_OBJ_INV | TAR_OBJ_ROOM, FALSE, MAG_MANUAL);


  /*
   * Declaration of skills - this actually doesn't do anything except
   * set it up so that immortals can use these skills by default.  The
   * min level to use the skill for other classes is set up in class.c.
   */

  skillo (SKILL_BACKSTAB);
  skillo (SKILL_BASH);
  skillo (SKILL_HIDE);
  skillo (SKILL_KICK);
  skillo (SKILL_BERSERK);
  skillo (SKILL_PICK_LOCK);
  skillo (SKILL_RAM_DOOR);
  skillo (SKILL_PUNCH);
  skillo (SKILL_RESCUE);
  skillo (SKILL_SNEAK);
  skillo (SKILL_CAMOUFLAGE);
  skillo (SKILL_BLANKET);
  skillo (SKILL_STEAL);
  skillo (SKILL_TRACK);
  skillo (SKILL_FORAGE);
  skillo (SKILL_SCAN);
  skillo (SKILL_BREW);
  skillo (SKILL_FORGE);
  skillo (SKILL_SCRIBE);
  skillo (SKILL_SPEED);
  skillo (SKILL_MOUNT);
  skillo (SKILL_RIDING);
  skillo (SKILL_TAME);
  skillo (SKILL_SECOND_ATTACK);
  skillo (SKILL_THIRD_ATTACK);
  skillo (SKILL_LISTEN);
  skillo (SKILL_MEDITATE);
  skillo (SKILL_REPAIR);
  skillo (SKILL_TAN);
  skillo (SKILL_FILLET);
  skillo (SKILL_CARVE);
  skillo (SKILL_DODGE);
  skillo (SKILL_PARRY);
  skillo (SKILL_AVOID);
  skillo (SKILL_RIPOSTE);
  skillo (SKILL_CIRCLE);
  skillo (SKILL_TRIP);
  skillo (SKILL_TARGET);
  skillo (SKILL_DISARM);
  skillo (SKILL_CHAIN_FOOTING);
  skillo (SKILL_ADRENALINE);
  skillo (SKILL_BLOODLUST);
  skillo (SKILL_CARNALRAGE);
}
