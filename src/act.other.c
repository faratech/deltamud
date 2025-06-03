/* ************************************************************************
   *   File: act.other.c                                   Part of CircleMUD *
   *  Usage: Miscellaneous player-level commands                             *
   *                                                                         *
   *  All rights reserved.  See license.doc for complete information.        *
   *                                                                         *
   *  Copyright (C) 1993, 94 by the Trustees of the Johns Hopkins University *
   *  CircleMUD is based on DikuMUD, Copyright (C) 1990, 1991.               *
   ************************************************************************ */

#define __ACT_OTHER_C__

#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "utils.h"
#include "comm.h"
#include "interpreter.h"
#include "handler.h"
#include "db.h"
#include "spells.h"
#include "screen.h"
#include "house.h"
#include "dg_scripts.h"
#include "shop.h"
#include "maputils.h"
#include "gcmd.h"

/* extern variables */
extern char *spells[];
extern const char *apply_types[];
extern int arena_preproom;
extern int arena_entrance;
extern int arena_observeroom;
extern int pk_allowed;
extern int jail_num;
extern int newbie_room;
extern int bail_multiplier;
extern int xp_multiplier;
extern int mobdie_enabled;
extern char *mobdie_pwd;
extern long mortal_start_room[NUM_STARTROOMS + 1];
extern struct str_app_type str_app[];
extern struct room_data *world;
extern struct descriptor_data *descriptor_list;
extern struct room_data *world;
extern struct dex_skill_type dex_app_skill[];
extern struct spell_info_type spell_info[];
extern struct index_data *mob_index;
extern char *class_abbrevs[];
void write_aliases(struct char_data *ch);
void deobserve(struct char_data *);
void quit_warning(struct char_data *ch);
void really_quit(struct char_data *ch);
/* extern procedures */
void linkobserve(struct char_data *who, struct char_data *to);
SPECIAL (shop_keeper);
void die(struct char_data * ch, struct char_data * killer);
int trade_with(struct obj_data *, int);
void  death_cry (struct char_data *ch);
void printstattable(struct char_data *ch);

/* local */
int atm_is_in_room(struct char_data *ch);
ACMD(do_gen_atm);

/*
  THE FOLLOWING CODE IS JUST SOMETHING STORM WHIPPED UP TO DO A SCHOOL
  SURVEY PROJECT: FEEL FREE TO REMOVE.

struct tdf {
  int a;
  int b;
  int c;
  int d;
  int e;
  int f;
  int x;
};

const char *fem_values[] = {
"bbcbbbccbcacba",
"abbbccbbbdfcca",
"adcabaacaaeccc",
"bbbbbbcccdfccc",
"abadbbcabdbcba",
"bbcbcbcccdeacc",
"aacbaaccbcdcbc",
"aaabccccbcbcba",
"bacabbccacbaac",
"bbcbbcccbcdbcc",
"bbcbbaccbbxabc",
"bbcbbbccbcacba",
"abbbccbbbdfcca",
"adcabaacaaeccc",
"bbbbbbcccdfccc",
"abadbbcabdbcba",
"bbcbcbcccdeacc",
"aacbaaccbcdcbc",
"aaabccccbcbcba",
"bacabbccacbaac",
"bbcbbcccbcdbcc",
"bbcbbaccbbxabc",
"bbcbbbccbcacba",
"abbbccbbbdfcca",
"adcabaacaaeccc",
"bbbbbbcccdfccc",
"abadbbcabdbcba",
"bbcbcbcccdeacc",
"aacbaaccbcdcbc",
"aaabccccbcbcba",
"bacabbccacbaac",
"bbcbbcccbcdbcc",
"bbcbbaccbbxabc",
"\n"
};

ACMD(do_tableup) {
  int i, j=-1;
  int k;
  struct tdf results[20];
  for (i=0; i<=19; i++) {
    results[i].a=0;
    results[i].b=0;
    results[i].c=0;
    results[i].d=0;
    results[i].e=0;
    results[i].f=0;
    results[i].x=0;
  }

  while (fem_values[++j][0]!='\n')
    for (i=0; i<strlen(fem_values[j]); i++)
      switch (fem_values[j][i]) {
        case 'a': results[i].a++; break;
        case 'b': results[i].b++; break;
        case 'c': results[i].c++; break;
        case 'd': results[i].d++; break;
        case 'e': results[i].e++; break;
        case 'f': results[i].f++; break;
        case 'x': results[i].x++; break;
        default: send_to_char((char *) fem_values[j], ch);
                 send_to_char("\r\nErr, incorrect value in onna em...\r\n", ch); return;
      }
  for (i=0; i<=19; i++) {
    k=(results[i].a+results[i].b+results[i].c+results[i].d+results[i].e+results[i].f+results[i].x);
    sprintf(buf, "Question %d:\r\n"
                 "  a:%9d (%%%2.1f)\r\n"
                 "  b:%9d (%%%2.1f)\r\n"
                 "  c:%9d (%%%2.1f)\r\n"
                 "  d:%9d (%%%2.1f)\r\n"
                 "  e:%9d (%%%2.1f)\r\n"
                 "  f:%9d (%%%2.1f)\r\n"
                 "  omitted: %2d (%%%2.1f)\r\n\r\n", i+1,
                 results[i].a, (float) results[i].a/k*100,
                 results[i].b, (float) results[i].b/k*100,
                 results[i].c, (float) results[i].c/k*100,
                 results[i].d, (float) results[i].d/k*100,
                 results[i].e, (float) results[i].e/k*100,
                 results[i].f, (float) results[i].f/k*100,
                 results[i].x, (float) results[i].x/k*100);
    send_to_char(buf, ch);
  }
}
  double num_array[3][10] = {
    { 16.90, 19.30, 25.90, 39.50, 0, 0, 0, 0, 0, 0 },
    { 16.60, 19.70, 22.80, 25.20, 34.00, 40.30, 48.20, 0, 0, 0},
    { 17.90, 21.60, 23.20, 35.70, 44.20, 47.50, 0, 0, 0, 0}
  };
  double x, y, z;
  int i, j;
  for (i=0; i<3; i++) {   
    switch (i) {
      case 0: send_to_char("\r\n\r\nhydrogen\r\n", ch); break;
      case 1: send_to_char("\r\n\r\nhelium\r\n", ch); break;
      case 2: send_to_char("\r\n\r\nmercury\r\n", ch); break;
    }
    for (j=0; j<10; j++) {
      if (num_array[i][j]==0) continue;
      x = 0.00019*num_array[i][j]/sqrt(58.3*58.3 + num_array[i][j]*num_array[i][j]);
      y = 3/x;
      z = (y * 6.6);
      sprintf(buf, "%fcm\r\n"
                   "  wavelength: 0.00019 * %f/sqrt(58.3^2 + %f^2) = %fcm\r\n"
                   "              %f * 10^8 =                                %fangstroms\r\n"
                   "  frequency: 30000000000/%f =                            %f*10^10cm\r\n"
                   "  energy: %f*10^10 * 6.6*10^-27 =                    %f*10^-17ergs\r\n"
                   "          %f / (1.6 * 1000000000000) =              %f*10^-29ev\r\n",
        num_array[i][j],
        num_array[i][j], num_array[i][j], x,
        x,
        x*100000000,
        x, y,
        y, z, 
        z, z/(1.6));
      send_to_char(buf, ch); 
    }
  }
*/

ACMD(do_email) {
  char pcFileName[127], farg[100];
  FILE *ptFHndl=NULL;
  extern void write_extra_data(struct char_data *ch);
  extern long get_id_by_name (char *name);
  skip_spaces(&argument);
  one_argument(argument, farg);
  get_filename(farg, pcFileName, EDATA_FILE);
  buf[0]='\0';
  if (!(ptFHndl = fopen(pcFileName,"r"))) {
    if (get_id_by_name(farg)!=-1) {
      send_to_char("They have not registered an email address.\r\n", ch);
      return;
    }
    if (EMAIL(ch)) free EMAIL(ch);
    EMAIL(ch) = strlen(argument)>0 ? strdup(argument) : 0;
    send_to_char(OK, ch);
    write_extra_data(ch);
  }
  else {
    while (!feof(ptFHndl))
    {
      fgets(buf, 512, ptFHndl);
      if (!strncmp("EMAIL ", buf, 6)) {
        if (buf[6]!='*' && GET_LEVEL(ch)<LVL_IMMORT) {
          send_to_char("Their email address is private.\r\n", ch);
          fclose(ptFHndl);
          return;
        }
        for (;buf[strlen(buf)-1]=='\r' || buf[strlen(buf)-1]=='\n';) buf[strlen(buf)-1]='\0';
        send_to_char(buf+6 + (buf[6]=='*' ? 1 : 0), ch);
        if (!strlen(buf+6 + (buf[6]=='*' ? 1 : 0)))
          send_to_char("They have not registered an email address.\r\n", ch);
        send_to_char("\r\n", ch);
        fclose(ptFHndl);
        return;
      }
    }
    send_to_char("They have not registered an email address.\r\n", ch);
    fclose(ptFHndl);
  }
}

ACMD(do_slist) {
  extern struct spell_info_type spell_info[TOP_SPELL_DEFINE + 1];
  extern char *spells[];
  int i, j, llev, class;
  extern char * class_name (struct char_data *ch);
  skip_spaces(&argument);
  if (is_abbrev(argument, "mage"))
    class=CLASS_MAGIC_USER;
  else if (is_abbrev(argument, "cleric"))
    class=CLASS_CLERIC;
  else if (is_abbrev(argument, "thief"))
    class=CLASS_THIEF;
  else if (is_abbrev(argument, "warrior"))
    class=CLASS_WARRIOR;
  else if (is_abbrev(argument, "artisan"))
    class=CLASS_ARTISAN;
  else
    class=(int) GET_CLASS(ch);
  sprintf(buf, "&GSpell/Skill Listing For The %s:\r\n"
               "Lvl: &BSpells/Skills", class==CLASS_MAGIC_USER ? "Mage" :
                                       (class==CLASS_CLERIC ? "Cleric" :
                                       (class==CLASS_THIEF ? "Thief" :
                                       (class==CLASS_WARRIOR ? "Warrior" :
                                       (class==CLASS_ARTISAN ? "Artisan" : "UNDEFINED")))));
  send_to_char(buf, ch);
  for (j=1; j<LVL_IMMORT; j++) {
    llev=0;
    for (i=0; i<=TOP_SPELL_DEFINE; i++)
      if (spell_info[i].min_level[class] == j) {
        if (!llev) {
          sprintf(buf, "\r\n&G%-3d: %s%-20s", j, GET_SKILL(ch, i) == 100 ? "&m" : "&C", spells[i]);
          llev=1;
        } else {
          sprintf(buf+strlen(buf), "%s%-20s", GET_SKILL(ch, i) == 100 ? "&m" : "&C", spells[i]);
        }
      }
    if (llev)
      send_to_char(buf, ch);
  }
  send_to_char("\r\n", ch);
}

ACMD (do_affected) {
  struct affected_type *aff;
  extern const char *affected_bits[];
  send_to_char("You close your eyes and focus on your aura.\r\n"
               "*******************************************\r\n", ch);
    for (aff = ch->affected; aff; aff = aff->next)
    {
      if (aff->type==-1 && aff->duration==-1) {
        sprintbit (aff->bitvector, affected_bits, buf2);
        sprintf (buf, "  &C%-21s&npermanent duration.", buf2); 
        send_to_char (strcat (buf, "\r\n"), ch);
        continue;
      }
      sprintf (buf, "  &C%-21s&n", (aff->type >= 0 && aff->type <= MAX_SKILLS) ?
               spells[aff->type] : "TYPE UNDEFINED");
      if (aff->modifier)
        sprintf (buf+strlen(buf), "modifies %s by %+ld for %d hour%s", apply_types[(int) aff->location],
aff->modifier, (int) aff->duration + 1, (aff->duration==0 ? "." : "s."));
      else
        sprintf (buf+strlen(buf), "affects you for %d hour%s", (int) aff->duration + 1,
(aff->duration==0 ?
"."
: "s."));
      strcat(buf, "\r\n");
      send_to_char(buf, ch);
    }
  send_to_char("*******************************************\r\n", ch);
}

ACMD (do_build) {
  extern int can_edit_zone(struct char_data *ch, int number);
  extern long top_of_world;
  skip_spaces(&argument);
  if (IS_ARENACOMBATANT(ch)) {
    send_to_char("No building while in the arena!\r\n", ch);
    return;
  }
  if (PRF2_FLAGGED(ch, PRF2_INTANGIBLE) && !PRF2_FLAGGED(ch, PRF2_MBUILDING)) {
    send_to_char("Intangible players cannot build!\r\n", ch);
    return;
  }
  if (real_room(atoi(argument)) < 0) {
    if (atoi(argument)<0)
      send_to_char("Slap yourself for trying that.\r\n", ch);
    else
      send_to_char("That room doesn't exist.\r\n", ch);
    return;
  }
  if (!strcmp(argument, "off")) {
    if (!IS_SET(PRF2_FLAGS(ch), PRF2_MBUILDING)) {
      send_to_char("You weren't building to begin with.\r\n", ch);
      return;
    } 
    if (ch->player_specials->saved.buildmoderoom<0) {
      send_to_char("AHH! Something happened and your original room didn't save!\r\nSending you to the void.\r\n", ch);
      char_from_room(ch);
      char_to_room(ch, 0);
      return;
    }
    if (real_room(GET_ROOM_VNUM(ch->player_specials->saved.buildmoderoom))<0) { /* Run a reverse check; if their original room doesn't exist anymore we don't want mud crashes. */
      send_to_char("AHH! Something happened TO your original room!\r\nSending you to the void.\r\n", ch);
      char_from_room(ch);
      char_to_room(ch, 0);
      return;
    }
    send_to_char("Exiting build mode.\r\n", ch);
    char_from_room(ch);
    char_to_room(ch, ch->player_specials->saved.buildmoderoom);
    act("$n has arrived from building mode.", TRUE, ch, 0, 0, TO_ROOM);
    look_at_room(ch, 1);
    return;
  }
  if (GET_LEVEL(ch)>=LVL_IMMORT) {
    send_to_char("Sorry, immortals don't really participate in the mortal building program.\r\n", ch);
    return;
  }
  if (!PLR_FLAGGED(ch, PLR_MBUILDER)) {
    send_to_char("You're not part of the mortal building program.\r\n", ch);
    return;
  }
  if (!can_edit_zone(ch, world[real_room(atoi(argument))].zone)) {
    send_to_char("You don't have permissions to that zone.\r\n", ch);
    return;
  }
  if (GET_POS(ch)!=POS_STANDING) {
    send_to_char("You are not in the correct position for that.r\n", ch);
    return;
  }
  if (PRF2_FLAGGED(ch, PRF2_MBUILDING)) { // If they are already building and pass the checks, build is the equivelant of goto.
    char_from_room(ch);
    char_to_room(ch, real_room(atoi(argument)));
    look_at_room(ch, 1);
    return;
  }
  SET_BIT(PRF2_FLAGS(ch), PRF2_MBUILDING | PRF2_INTANGIBLE);
  sprintf(buf, "%s entering build mode.", GET_NAME(ch));
  mudlog(buf, BRF, LVL_IMMORT, TRUE);
  send_to_char("You enter build mode.\r\nPlease remember that any and all bugs you find in this "
               "MUD should\r\nNOT be abused and should be reported immediatley.\r\n", ch);
  act("$n enters building mode.", TRUE, ch, 0, 0, TO_ROOM);
  ch->player_specials->saved.buildmoderoom=ch->in_room;
  char_from_room(ch);
  ch->player_specials->saved.buildmodezone=world[real_room(atoi(argument))].zone;
  char_to_room(ch, real_room(atoi(argument)));

  /* Now we add OLC stuff to the player (as well as protection) */
  REMOVE_BIT(PRF_FLAGS(ch), PRF_SUMMONABLE);
  SET_BIT(PRF_FLAGS(ch), PRF_ROOMFLAGS);
  SET_BIT(PRF_FLAGS(ch), PRF_NOHASSLE);
  SET_BIT(PRF_FLAGS(ch), PRF_HOLYLIGHT);
  SET_BIT(GCMD_FLAGS(ch), GCMD_LOAD);
  SET_BIT(GCMD_FLAGS(ch), GCMD_PURGE);
  SET_BIT(GCMD2_FLAGS(ch), GCMD2_OLC);
  SET_BIT(GCMD2_FLAGS(ch), GCMD2_ZRESET);
  SET_BIT(GCMD3_FLAGS(ch), GCMD3_PEACE);
  look_at_room(ch, 1);
  save_char(ch, NOWHERE);
}

ACMD (do_quit)
{
  one_argument (argument, arg);

  if (!str_cmp (arg, "y") || (GET_LEVEL(ch) >= LVL_IMMORT))
  really_quit(ch);

  else if (item_count(ch) == 0) {
   send_to_char("Holding no possessions, you decide to quit..\r\n", ch);
   really_quit(ch);
  }

  else if (!*arg && (GET_LEVEL(ch) < LVL_IMMORT))
    {
      quit_warning(ch);
      return;
   }

}

/* do_quit renamed for rent quit protection -- mulder */
void really_quit (struct char_data *ch)
{
      void die(struct char_data * ch, struct char_data * killer);
      void Crash_rentsave (struct char_data *ch, int cost);
      extern int free_rent;
      long save_room;
      struct descriptor_data *d, *next_d;

      if (IS_NPC (ch) || !ch->desc)
	return;

      if (GET_POS (ch) == POS_FIGHTING)
	send_to_char ("No way!  You're fighting for your life!\r\n", ch);
      else if (GET_POS (ch) < POS_STUNNED)
	{
	  send_to_char ("You die before your time...\r\n", ch);
         die(ch, NULL);
	}
      else
	{
	  if (!GET_INVIS_LEV (ch))
	    act ("$n has left the game.", TRUE, ch, 0, 0, TO_ROOM);
	  sprintf (buf, "%s has quit the game.", GET_NAME (ch));
	  mudlog (buf, NRM, MAX (LVL_IMMORT, GET_INVIS_LEV (ch)), TRUE);
	  send_to_char ("\r\nYou decide to sit down and rest. You soon fade into a deep sleep.\r\n", ch);
	  save_room = ch->in_room;

// this was annoying, removed 8/23/98 --Mulder
// if(GET_LEVEL(ch) < LVL_IMMORT) {
//   sprintf (buf, "&m[&YINFO&m]&n %s has left Deltania. (quit)\r\n", 
//          GET_NAME (ch));
//	  send_to_all (buf);
// }
	  /*
	   * kill off all sockets connected to the same player as the one who is
	   * trying to quit.  Helps to maintain sanity as well as prevent duping.
	   */
	  for (d = descriptor_list; d; d = next_d)
	    {
	      next_d = d->next;
	      if (d == ch->desc)
		continue;
	      if (d->character && (GET_IDNUM (d->character) == GET_IDNUM (ch)))
		close_socket (d);
	    }
          write_aliases(ch);
	  save_room = ch->in_room;

            /* if this weirdo is immortal, rent is free */ 
	  if (free_rent || GET_LEVEL(ch) >= LVL_IMMORT)
	    Crash_rentsave (ch, 0);
          if (GET_LEVEL(ch) >= LVL_IMMORT && ismap(ch->in_room)) {
            ch->player_specials->saved.mapx=rm2x(ch->in_room);
            ch->player_specials->saved.mapy=rm2y(ch->in_room);
          }
	  extract_char (ch);	/* Char is saved in extract char */

	  /* If someone is quitting in their house, let them load back here */
	  if (ROOM_FLAGGED (save_room, ROOM_HOUSE))
	    save_char (ch, save_room);
	}
}

ACMD (do_save)
{

  if (IS_NPC (ch) || !ch->desc)
    return;

  /* Only tell the char we're saving if they actually typed "save" */
  if (cmd)
    {
      sprintf (buf, "Saving %s.\r\n", GET_NAME (ch));
      send_to_char (buf, ch);
    }
  write_aliases(ch);
  save_char (ch, NOWHERE);
  Crash_crashsave (ch);
  if (ROOM_FLAGGED (ch->in_room, ROOM_HOUSE_CRASH))
    House_crashsave (world[ch->in_room].number);
}

ACMD (do_postbail)
{
  int gold, inven_val, eq_val, j, bail;
  struct obj_data *obj;

  if (!((!pk_allowed) && (PLR_FLAGGED (ch, PLR_KILLER)) 
      && (ch->in_room == real_room(jail_num)))){
    send_to_char ("You're (happily) not serving a jailterm right now.\r\n", ch);
    return;
  }
  gold = GET_GOLD (ch);
  bail = GET_BAIL_AMT(ch);
  if (bail <= 0)
    bail = bail_multiplier;

  inven_val = 0;
  eq_val = 0;

  for (j = 0; j < NUM_WEARS; j++){
    obj = GET_EQ (ch, j);
    if (obj)
      if ((GET_OBJ_TYPE(obj) == ITEM_ARMOR) || 
	  (GET_OBJ_TYPE(obj) == ITEM_WEAPON))
	eq_val += GET_OBJ_COST(obj);
  }

  for (obj = ch->carrying; obj; obj = obj->next_content){
    if ((GET_OBJ_TYPE(obj) == ITEM_ARMOR) || 
	(GET_OBJ_TYPE(obj) == ITEM_WEAPON))
      inven_val += GET_OBJ_COST(obj);
  }

  sprintf (buf, "Bail for this offence has been set at %d gold coins.\r\n", 
	   bail);
  send_to_char (buf, ch);
  if (bail > gold){
    sprintf (buf, "You don't have enough gold on you.\r\n");
    send_to_char (buf, ch);
    
    if (inven_val > 0){
      sprintf (buf, "You will have to sell off some of your inventory.\r\n");
      send_to_char (buf, ch);
    }else if (eq_val > 0){
      sprintf (buf, "You will have to sell off some of your equipment.\r\n");
      send_to_char (buf, ch);
    } else {
      sprintf (buf, "You're out of items to sell. Taking your experience points instead.\r\n");
      send_to_char (buf, ch);
      GET_EXP (ch) -= (abs(bail - gold) * xp_multiplier);
      gold = bail;
      GET_GOLD (ch) = bail;
    }
  }
  if (bail <= gold){
    sprintf (buf, "&m[&YINFO&m]&n %s has posted bail.\r\n", GET_NAME (ch));
    send_to_char ("Congratulations, you're a free man!\r\n", ch);
    send_to_all (buf);
    REMOVE_BIT (PLR_FLAGS (ch), PLR_THIEF | PLR_KILLER);
    REMOVE_BIT(PRF_FLAGS(ch), PRF_NOAUCT);
    GET_GOLD (ch) -= bail;
    sprintf (buf, "%s posts bail and is suddenly taken back home.", 
	     GET_NAME(ch));
    send_to_room (buf, ch->in_room);
    char_from_room (ch);
    char_to_room (ch, real_room(mortal_start_room[GET_HOME (ch)])); 
    look_at_room (ch, 0);
  }
}


/* generic function for commands which are normally overridden by
   special procedures - i.e., shop commands, mail commands, etc. */
ACMD (do_not_here)
{
  send_to_char ("Sorry, but you cannot do that here!\r\n", ch);
}



ACMD (do_sneak)
{
  struct affected_type af;
  byte percent;

  send_to_char ("Okay, you'll try to move silently for a while.\r\n", ch);
  if (IS_AFFECTED (ch, AFF_SNEAK))
    affect_from_char (ch, SKILL_SNEAK);

  percent = number (1, 101);	/* 101% is a complete failure */

  if (percent > number (1, 10) + GET_SKILL (ch, SKILL_SNEAK) + dex_app_skill[GET_DEX (ch)].sneak)
    return;

  af.type = SKILL_SNEAK;
  af.duration = GET_LEVEL (ch);
  af.modifier = 0;
  af.location = APPLY_NONE;
  af.bitvector = AFF_SNEAK;
  affect_to_char (ch, &af);
}



ACMD (do_hide)
{
  byte percent;
  extern bool check_perm_duration(struct char_data *ch, long bitvector);

  send_to_char ("You attempt to hide yourself.\r\n", ch);

  if (IS_AFFECTED (ch, AFF_HIDE) && !check_perm_duration(ch, AFF_HIDE))
    REMOVE_BIT (AFF_FLAGS (ch), AFF_HIDE);

  percent = number (1, 101);	/* 101% is a complete failure */

  if (percent > number (1, 10) + GET_SKILL (ch, SKILL_HIDE) + dex_app_skill[GET_DEX (ch)].hide)
    return;

  SET_BIT (AFF_FLAGS (ch), AFF_HIDE);
}


ACMD (do_steal)
{
  struct char_data *vict;
  struct obj_data *obj;
  char vict_name[MAX_INPUT_LENGTH], obj_name[MAX_INPUT_LENGTH];
  int percent, gold, eq_pos, pcsteal = 0, ohoh = 0;
  extern int pt_allowed;
  extern int pt_markable;
  char buf[256];

  ACMD (do_gen_comm);

  argument = one_argument (argument, obj_name);
  one_argument (argument, vict_name);

  if (!(vict = get_char_room_vis (ch, vict_name)))
    {
      send_to_char ("Steal what from who?\r\n", ch);
      return;
    }
  else if (vict == ch)
    {
      send_to_char ("Come on now, that's rather stupid!\r\n", ch);
      return;
    }

  if (ch != vict && ROOM_FLAGGED (ch->in_room, ROOM_PEACEFUL) && GET_LEVEL (ch) < LVL_IMPL)
    {
      send_to_char ("This room just has such a peaceful, easy feeling...\r\n", ch);
      return;
    }

  /* 101% is a complete failure */
  percent = number (1, 101) - dex_app_skill[GET_DEX (ch)].p_pocket;

  if (GET_LEVEL (vict) > GET_LEVEL (ch))
    percent += abs(GET_LEVEL (vict) - GET_LEVEL (ch));

  if (GET_POS (vict) < POS_SLEEPING)
    percent = -1;		/* ALWAYS SUCCESS */

  if (!pt_allowed && !IS_NPC (vict))
    pcsteal = 1;

  /* NO NO With Imp's and Shopkeepers, and if player thieving is not allowed */
  if (GET_LEVEL (vict) >= LVL_IMMORT || pcsteal ||
      GET_MOB_SPEC (vict) == shop_keeper)
    percent = 101;		/* Failure */

  if (str_cmp (obj_name, "coins") && str_cmp (obj_name, "gold"))
    {

      if (!(obj = get_obj_in_list_vis (vict, obj_name, vict->carrying)))
	{

	  for (eq_pos = 0; eq_pos < NUM_WEARS; eq_pos++)
	    if (GET_EQ (vict, eq_pos) &&
		(isname (obj_name, GET_EQ (vict, eq_pos)->name)) &&
		CAN_SEE_OBJ (ch, GET_EQ (vict, eq_pos)))
	      {
		obj = GET_EQ (vict, eq_pos);
		break;
	      }
	  if (!obj)
	    {
	      act ("$E hasn't got that item.", FALSE, ch, 0, vict, TO_CHAR);
	      return;
	    }
	  else
	    {			/* It is equipment */
	      if ((GET_POS (vict) > POS_STUNNED))
		{
		  send_to_char ("Steal the equipment now?  Impossible!\r\n", ch);
		  return;
		}
	      else
		{
		  act ("You unequip $p and steal it.", FALSE, ch, obj, 0, TO_CHAR);
		  act ("$n steals $p from $N.", FALSE, ch, obj, vict, TO_NOTVICT);		  
		  obj_to_char (unequip_char (vict, eq_pos), ch);

		}
	    }
	}
      else
	{			/* obj found in inventory */

	  percent += GET_OBJ_WEIGHT (obj);	/* Make heavy harder */

	  if (AWAKE (vict) && (percent > GET_SKILL (ch, SKILL_STEAL)))
	    {
	      ohoh = TRUE;
	      act ("Oops..", FALSE, ch, 0, 0, TO_CHAR);
	      act ("$n tried to steal something from you!", FALSE, ch, 0, vict, TO_VICT);
	      act ("$n tries to steal something from $N.", TRUE, ch, 0, vict, TO_NOTVICT);
	      if (pt_markable && IS_JURISDICTED(ch->in_room) && !IS_NPC(ch)
		  && !IS_NPC(vict) /*&& (GET_CLASS(ch) != CLASS_THIEF)*/){
		/* He's a THIEF! Let it be known! */
		sprintf (buf, "PC Thief bit set on %s for trying to steal from %s at %s.", GET_NAME (ch), GET_NAME (vict), world[vict->in_room].name);
		if (!PLR_FLAGGED (ch, PLR_THIEF))
		  mudlog (buf, BRF, LVL_IMMORT, TRUE);

		act ("This is a jurisdicted area. If you wanna be a thief, so be it.", FALSE, ch, 0, 0, TO_CHAR);
		SET_BIT (PLR_FLAGS (ch), PLR_THIEF);
		if (GET_CLASS (ch) == CLASS_THIEF){
		  if (GET_ALIGNMENT (ch) > -500)
		    GET_ALIGNMENT (ch) = -500;
		}else{
		  GET_ALIGNMENT (ch) = -1000;
		}
	      }
	    }
	  else
	    {			/* Steal the item */
	      if ((IS_CARRYING_N (ch) + 1 < CAN_CARRY_N (ch)))
		{
		  if ((IS_CARRYING_W (ch) + GET_OBJ_WEIGHT (obj)) < CAN_CARRY_W (ch))
		    {
		      obj_from_char (obj);
		      obj_to_char (obj, ch);
		      send_to_char ("Got it!\r\n", ch);
		    }
		}
	      else
		send_to_char ("You cannot carry that much.\r\n", ch);
	    }
	}
    }
  else
    {				/* Steal some coins */
      if (AWAKE (vict) && (percent > GET_SKILL (ch, SKILL_STEAL)))
	{
	  ohoh = TRUE;
	  act ("Oops..", FALSE, ch, 0, 0, TO_CHAR);
	  act ("You discover that $n has $s hands in your wallet.", FALSE, ch, 0, vict, TO_VICT);
	  act ("$n tries to steal gold from $N.", TRUE, ch, 0, vict, TO_NOTVICT);
	  if (pt_markable && IS_JURISDICTED(ch->in_room)  && !IS_NPC(ch)
	      && !IS_NPC(vict) /*&& (GET_CLASS(ch) != CLASS_THIEF)*/){
	    /* He's a THIEF! Let it be known! */
	    sprintf (buf, "PC Thief bit set on %s for trying to steal from %s at %s.", GET_NAME (ch), GET_NAME (vict), world[vict->in_room].name);
	    if (!PLR_FLAGGED (ch, PLR_THIEF))
	      mudlog (buf, BRF, LVL_IMMORT, TRUE);
	    act ("This is a jurisdicted area. If you wanna be a thief, so be it.", FALSE, ch, 0, 0, TO_CHAR);
	    SET_BIT (PLR_FLAGS (ch), PLR_THIEF);
	    if (GET_CLASS (ch) == CLASS_THIEF){
	      if (GET_ALIGNMENT (ch) > -500)
		GET_ALIGNMENT (ch) = -500;
	    }else{
	      GET_ALIGNMENT (ch) = -1000;
	    }
	  }
	}
      else
	{
	  /* Steal some gold coins */
	  gold = (int) ((GET_GOLD (vict) * number (1, 10)) / 100);
	  gold = MIN (1782, gold);
	  if (gold > 0)
	    {
	      GET_GOLD (ch) += gold;
	      GET_GOLD (vict) -= gold;
	      if (gold > 1)
		{
		  sprintf (buf, "Bingo!  You got %d gold coins.\r\n", gold);
		  send_to_char (buf, ch);
		}
	      else
		{
		  send_to_char ("You manage to swipe a solitary gold coin.\r\n", ch);
		}
	    }
	  else
	    {
	      send_to_char ("You couldn't get any gold...\r\n", ch);
	    }
	}
    }

  if (ohoh && IS_NPC (vict) && AWAKE (vict))
    hit (vict, ch, TYPE_UNDEFINED);
}

ACMD (do_practice)
{
  void list_skills (struct char_data *ch);
  one_argument (argument, arg);

  if (*arg)
    send_to_char ("You can only practice skills in your guild.\r\n", ch);
  else
    list_skills (ch);
}



ACMD (do_visible)
{
  void appear (struct char_data *ch);
  void perform_immort_vis (struct char_data *ch);

  if (GET_LEVEL (ch) >= LVL_IMMORT)
    {
      perform_immort_vis (ch);
      return;
    }

  if IS_AFFECTED
    (ch, AFF_INVISIBLE)
    {
      appear (ch);
      send_to_char ("You break the spell of invisibility.\r\n", ch);
    }
  else
    send_to_char ("You are already visible.\r\n", ch);
}



ACMD (do_title)
{
  skip_spaces (&argument);
  delete_doubledollar (argument);

  if (IS_NPC (ch))
    send_to_char ("Your title is fine... go away.\r\n", ch);
  else if (PLR_FLAGGED (ch, PLR_NOTITLE))
    send_to_char ("You can't title yourself -- you shouldn't have abused it!\r\n", ch);
  else if (strstr (argument, "(") || strstr (argument, ")"))
    send_to_char ("Titles can't contain the ( or ) characters.\r\n", ch);
  else if (strstr (argument, "[") || strstr (argument, "]"))
    send_to_char ("Titles can't contain the [ or ] characters.\r\n", ch);
  else if (strstr (argument, "<") || strstr (argument, ">"))
    send_to_char ("Titles can't contain the < or > characters.\r\n", ch);
  else if (strlen (argument) > MAX_TITLE_LENGTH)
    {
      sprintf (buf, "Sorry, titles can't be longer than %d characters.\r\n",
	       MAX_TITLE_LENGTH);
      send_to_char (buf, ch);
    }
  else
    {
      set_title (ch, argument);
      sprintf (buf, "Okay, you're now %s %s.\r\n", GET_NAME (ch), GET_TITLE (ch));
      send_to_char (buf, ch);
    }
}


#define CAN_GROUP(ch, vict) (GET_LEVEL(vict)-GET_LEVEL(ch) >= -10 && GET_LEVEL(vict)-GET_LEVEL(ch) <= 10)

int
perform_group (struct char_data *ch, struct char_data *vict)
{
  struct follow_type *k;
  if (IS_AFFECTED (vict, AFF_GROUP) || !CAN_SEE (ch, vict))
    return 0;

  if (!CAN_GROUP(ch, vict)) {
    act ("$N is out of your grouping range.", FALSE, ch, 0, vict, TO_CHAR);
    return 0;
  }

  for (k = ch->followers; k; k = k->next) {
    if (!IS_AFFECTED (k->follower, AFF_GROUP))
      continue;
    if (!CAN_GROUP(vict, k->follower)) {
      sprintf(buf, "%s may not group with %s (they are not within grouping range).\r\n", GET_NAME(vict), GET_NAME(k->follower));
      send_to_char(buf, ch);
      return 0;
    }
  }

  SET_BIT (AFF_FLAGS (vict), AFF_GROUP);

  if (ch != vict)
  act ("$N is now a member of your group.", FALSE, ch, 0, vict, TO_CHAR);

  act ("You are now a member of $n's group.", FALSE, ch, 0, vict, TO_VICT);
  act ("$N is now a member of $n's group.", FALSE, ch, 0, vict, TO_NOTVICT);
  return 1;
}


void
print_group (struct char_data *ch)
{
  struct char_data *k;
  struct follow_type *f;

  if (!IS_AFFECTED (ch, AFF_GROUP))
    send_to_char ("But you are not the member of a group!\r\n", ch);
  else
    {
      send_to_char ("Your group consists of:\r\n", ch);

      k = (ch->master ? ch->master : ch);

      if (IS_AFFECTED (k, AFF_GROUP))
	{
	  sprintf (buf, "     [%3dH %3dM %3dV] [%2d %s] $N (Head of group)",
		   (int) GET_HIT (k), (int) GET_MANA (k), (int) GET_MOVE (k), GET_LEVEL (k), CLASS_ABBR (k));
	  act (buf, FALSE, ch, 0, k, TO_CHAR);
	}

      for (f = k->followers; f; f = f->next)
	{
	  if (!IS_AFFECTED (f->follower, AFF_GROUP))
	    continue;

	  sprintf (buf, "     [%3dH %3dM %3dV] [%2d %s] $N", (int) GET_HIT (f->follower),
		   (int) GET_MANA (f->follower), (int) GET_MOVE (f->follower),
		   GET_LEVEL (f->follower), CLASS_ABBR (f->follower));
	  act (buf, FALSE, ch, 0, f->follower, TO_CHAR);
	}
    }
}



ACMD (do_group)
{
  struct char_data *vict;
  struct follow_type *f;
  int found;

  one_argument (argument, buf);

  if (!*buf)
    {
      print_group (ch);
      return;
    }

  if (ch->master)
    {
      act ("You can not enroll group members without being head of a group.",
	   FALSE, ch, 0, 0, TO_CHAR);
      return;
    }

  if (!str_cmp (buf, "all"))
    {
      perform_group (ch, ch);
      for (found = 0, f = ch->followers; f; f = f->next)
	found += perform_group (ch, f->follower);
      if (!found)
	send_to_char ("Everyone following you is already in your group.\r\n", ch);
      return;
    }

  if (!(vict = get_char_room_vis (ch, buf)))
    send_to_char (NOPERSON, ch);
  else if ((vict->master != ch) && (vict != ch))
    act ("$N must follow you to enter your group.", FALSE, ch, 0, vict, TO_CHAR);
  else
    {
      if (!IS_AFFECTED (vict, AFF_GROUP))
	perform_group (ch, vict);
      else
	{
	  if (ch != vict)
	    act ("$N is no longer a member of your group.", FALSE, ch, 0, vict, TO_CHAR);
	  act ("You have been kicked out of $n's group!", FALSE, ch, 0, vict, TO_VICT);
	  act ("$N has been kicked out of $n's group!", FALSE, ch, 0, vict, TO_NOTVICT);
	  REMOVE_BIT (AFF_FLAGS (vict), AFF_GROUP);
	}
    }
}



ACMD (do_ungroup)
{
  struct follow_type *f, *next_fol;
  struct char_data *tch;
  void stop_follower (struct char_data *ch);

  one_argument (argument, buf);

  if (!*buf)
    {
      if (ch->master || !(IS_AFFECTED (ch, AFF_GROUP)))
	{
	  send_to_char ("But you lead no group!\r\n", ch);
	  return;
	}
      sprintf (buf2, "%s has disbanded the group.\r\n", GET_NAME (ch));
      for (f = ch->followers; f; f = next_fol)
	{
	  next_fol = f->next;
	  if (IS_AFFECTED (f->follower, AFF_GROUP))
	    {
	      REMOVE_BIT (AFF_FLAGS (f->follower), AFF_GROUP);
	      send_to_char (buf2, f->follower);
	      if (!IS_AFFECTED (f->follower, AFF_CHARM))
		stop_follower (f->follower);
	    }
	}

      REMOVE_BIT (AFF_FLAGS (ch), AFF_GROUP);
      send_to_char ("You disband the group.\r\n", ch);
      return;
    }
  if (!(tch = get_char_room_vis (ch, buf)))
    {
      send_to_char ("There is no such person!\r\n", ch);
      return;
    }
  if (tch->master != ch)
    {
      send_to_char ("That person is not following you!\r\n", ch);
      return;
    }

  if (!IS_AFFECTED (tch, AFF_GROUP))
    {
      send_to_char ("That person isn't in your group.\r\n", ch);
      return;
    }

  REMOVE_BIT (AFF_FLAGS (tch), AFF_GROUP);

  act ("$N is no longer a member of your group.", FALSE, ch, 0, tch, TO_CHAR);
  act ("You have been kicked out of $n's group!", FALSE, ch, 0, tch, TO_VICT);
  act ("$N has been kicked out of $n's group!", FALSE, ch, 0, tch, TO_NOTVICT);

  if (!IS_AFFECTED (tch, AFF_CHARM))
    stop_follower (tch);
}




ACMD (do_report)
{
  struct char_data *k;
  struct follow_type *f;

  if (!IS_AFFECTED (ch, AFF_GROUP))
    {
      send_to_char ("But you are not a member of any group!\r\n", ch);
      return;
    }
  sprintf (buf, "%s reports: %d/%dhp, %d/%dmp, %d/%dmv\r\n",
	   GET_NAME (ch), (int) GET_HIT (ch), (int) GET_MAX_HIT (ch),
	   (int) GET_MANA (ch), (int) GET_MAX_MANA (ch),
	   (int) GET_MOVE (ch), (int) GET_MAX_MOVE (ch));

  CAP (buf);

  k = (ch->master ? ch->master : ch);

  for (f = k->followers; f; f = f->next)
    if (IS_AFFECTED (f->follower, AFF_GROUP) && f->follower != ch)
      send_to_char (buf, f->follower);
  if (k != ch)
    send_to_char (buf, k);

send_to_char (buf, ch);
/*  send_to_char ("You report to the group.\r\n", ch); */
}



ACMD (do_split)
{
  int amount, num, share;
  struct char_data *k;
  struct follow_type *f;

  if (IS_NPC (ch))
    return;

  one_argument (argument, buf);

  if (is_number (buf))
    {
      amount = atoi (buf);
      if (amount <= 0)
	{
	  send_to_char ("Sorry, you can't do that.\r\n", ch);
	  return;
	}
      if (amount > GET_GOLD (ch))
	{
	  send_to_char ("You don't seem to have that much gold to split.\r\n", ch);
	  return;
	}
      k = (ch->master ? ch->master : ch);

      if (IS_AFFECTED (k, AFF_GROUP) && (k->in_room == ch->in_room))
	num = 1;
      else
	num = 0;

      for (f = k->followers; f; f = f->next)
	if (IS_AFFECTED (f->follower, AFF_GROUP) &&
	    (!IS_NPC (f->follower)) &&
	    (f->follower->in_room == ch->in_room))
	  num++;

      if (num && IS_AFFECTED (ch, AFF_GROUP))
	share = amount / num;
      else
	{
	  send_to_char ("With whom do you wish to share your gold?\r\n", ch);
	  return;
	}

      GET_GOLD (ch) -= share * (num - 1);

      if (IS_AFFECTED (k, AFF_GROUP) && (k->in_room == ch->in_room)
	  && !(IS_NPC (k)) && k != ch)
	{
	  GET_GOLD (k) += share;
	  sprintf (buf, "%s splits %d coins; you receive %d.\r\n", GET_NAME (ch),
		   amount, share);
	  send_to_char (buf, k);
	}
      for (f = k->followers; f; f = f->next)
	{
	  if (IS_AFFECTED (f->follower, AFF_GROUP) &&
	      (!IS_NPC (f->follower)) &&
	      (f->follower->in_room == ch->in_room) &&
	      f->follower != ch)
	    {
	      GET_GOLD (f->follower) += share;
	      sprintf (buf, "%s splits %d coins; you receive %d.\r\n", GET_NAME (ch),
		       amount, share);
	      send_to_char (buf, f->follower);
	    }
	}
      sprintf (buf, "You split %d coins among %d members -- %d coins each.\r\n",
	       amount, num, share);
      send_to_char (buf, ch);
    }
  else
    {
      send_to_char ("How many coins do you wish to split with your group?\r\n", ch);
      return;
    }
}



ACMD (do_use)
{
  struct obj_data *mag_item;
  int equipped = 1;

  half_chop (argument, arg, buf);
  if (!*arg)
    {
      sprintf (buf2, "What do you want to %s?\r\n", CMD_NAME);
      send_to_char (buf2, ch);
      return;
    }
  mag_item = GET_EQ (ch, WEAR_HOLD);

  if (!mag_item || !isname (arg, mag_item->name))
    {
      switch (subcmd)
	{
	case SCMD_RECITE:
	case SCMD_QUAFF:
	  equipped = 0;
	  if (!(mag_item = get_obj_in_list_vis (ch, arg, ch->carrying)))
	    {
	      sprintf (buf2, "You don't seem to have %s %s.\r\n", AN (arg), arg);
	      send_to_char (buf2, ch);
	      return;
	    }
	  break;
	case SCMD_USE:
	  sprintf (buf2, "You don't seem to be holding %s %s.\r\n", AN (arg), arg);
	  send_to_char (buf2, ch);
	  return;
	  break;
	default:
	  log ("SYSERR: Unknown subcmd passed to do_use");
	  return;
	  break;
	}
    }
  switch (subcmd)
    {
    case SCMD_QUAFF:
      if (GET_OBJ_TYPE (mag_item) != ITEM_POTION)
	{
	  send_to_char ("You can only quaff potions.", ch);
	  return;
	}
      break;
    case SCMD_RECITE:
      if (GET_OBJ_TYPE (mag_item) != ITEM_SCROLL)
	{
	  send_to_char ("You can only recite scrolls.", ch);
	  return;
	}
      break;
    case SCMD_USE:
      if ((GET_OBJ_TYPE (mag_item) != ITEM_WAND) &&
	  (GET_OBJ_TYPE (mag_item) != ITEM_STAFF))
	{
	  send_to_char ("You can't seem to figure out how to use it.\r\n", ch);
	  return;
	}
      break;
    }

  mag_objectmagic (ch, mag_item, buf);
}



ACMD (do_wimpy)
{
  int wimp_lev;

  one_argument (argument, arg);

  if (!*arg)
    {
      if (GET_WIMP_LEV (ch))
	{
	  sprintf (buf, "Your current wimp level is %d hit points.\r\n",
		   GET_WIMP_LEV (ch));
	  send_to_char (buf, ch);
	  return;
	}
      else
	{
	  send_to_char ("At the moment, you're not a wimp.  (sure, sure...)\r\n", ch);
	  return;
	}
    }
  if (isdigit (*arg))
    {
      if ((wimp_lev = atoi (arg)))
	{
	  if (wimp_lev < 0)
	    send_to_char ("Heh, heh, heh.. we are jolly funny today, eh?\r\n", ch);
	  else if (wimp_lev > GET_MAX_HIT (ch))
	    send_to_char ("That doesn't make much sense, now does it?\r\n", ch);
	  else if (wimp_lev > (GET_MAX_HIT (ch) >> 1))
	    send_to_char ("You can't set your wimp level above half your hit points.\r\n", ch);
	  else
	    {
	      sprintf (buf, "Okay, you'll wimp out if you drop below %d hit points.\r\n",
		       wimp_lev);
	      send_to_char (buf, ch);
	      GET_WIMP_LEV (ch) = wimp_lev;
	    }
	}
      else
	{
	  send_to_char ("Okay, you'll now tough out fights to the bitter end.\r\n", ch);
	  GET_WIMP_LEV (ch) = 0;
	}
    }
  else
    send_to_char ("Specify at how many hit points you want to wimp out at.  (0 to disable)\r\n", ch);

  return;

}
ACMD (do_recall)
{
  int recall_lev;

  one_argument (argument, arg);

  if (!*arg)
    {
      if (GET_RECALL_LEV (ch))
        {
          sprintf (buf, "Your current recall level is %d hit points.\r\n", GET_RECALL_LEV (ch));
          send_to_char (buf, ch);
          return;
        }
      else
        {
          send_to_char ("At the moment, you won't autorecall.\r\n", ch);
          return;
        }
    }
 if (isdigit (*arg))
    {
      if ((recall_lev = atoi (arg)))
        {
          if (recall_lev < 0)
            send_to_char ("Heh, heh, heh.. we are jolly funny today, eh?\r\n", ch);
          else if (recall_lev > GET_MAX_HIT (ch))
            send_to_char ("That doesn't make much sense, now does it?\r\n", ch);
          else if (recall_lev > (GET_MAX_HIT (ch) >> 1))
            send_to_char ("You can't set your recall level above half your hit points.\r\n", ch);
          else
            {
              sprintf (buf, "Okay, you'll recall out if you drop below %d hit points.\r\n", recall_lev);
              send_to_char (buf, ch);
              GET_RECALL_LEV (ch) = recall_lev;
            }
        }
      else
        {
          send_to_char ("You will no longer autorecall from combat..\r\n", ch);
          GET_RECALL_LEV (ch) = 0;
    }
 }
  else
    send_to_char ("Specify how many hit points you want to recall out at. (0 to disable)\r\n", ch);

  return;

}
ACMD (do_retreat)   
{
  int retreat_lev;   

  one_argument (argument, arg);

  if (!*arg)
    {
      if (GET_RETREAT_LEV (ch))
        {
          sprintf (buf, "Your current retreat level is %d hit points.\r\n", GET_RETREAT_LEV (ch));
          send_to_char (buf, ch);
          return;
        }
      else
        {
          send_to_char ("At the moment, you won't autoretreat.\r\n", ch);
          return;
        }
    }
 if (isdigit (*arg))
    {
      if ((retreat_lev = atoi (arg)))
        {
          if (retreat_lev < 0)
            send_to_char ("Heh, heh, heh.. we are jolly funny today, eh?\r\n", ch);
          else if (retreat_lev > GET_MAX_HIT (ch))
            send_to_char ("That doesn't make much sense, now does it?\r\n", ch);
          else if (retreat_lev > (GET_MAX_HIT (ch) >> 1))
            send_to_char ("You can't set your retreat level above half your hit points.\r\n", ch);
          else
            {
              sprintf (buf, "Okay, you'll retreat out if you drop below %d hit points.\r\n", retreat_lev);
              send_to_char (buf, ch);
              GET_RETREAT_LEV (ch) = retreat_lev;
            }
        }
      else
        {
          send_to_char ("You will no longer autoretreat from combat..\r\n", ch);
          GET_RETREAT_LEV (ch) = 0;
    }
 }
  else
    send_to_char ("Specify how many hit points you want to retreat out at. (0 to disable)\r\n", ch);
             
  return;
              
}

ACMD (do_display)
{
  size_t i;

  if (IS_NPC (ch))
    {
      send_to_char ("Mosters don't need displays.  Go away.\r\n", ch);
      return;
    }
  skip_spaces (&argument);

  if (!*argument)
    {
      send_to_char ("Usage: prompt { H | M | V | E | F } | all | none }\r\n", ch);
      return;
    }

  if ((!str_cmp (argument, "on")) || (!str_cmp (argument, "all"))) {
   if (GET_LEVEL(ch) < LVL_IMMORT) {
    SET_BIT (PRF_FLAGS (ch), PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE | PRF_DISPEXP);
    SET_BIT (PRF2_FLAGS (ch), PRF2_DISPMOB);
   } else {
    SET_BIT (PRF_FLAGS (ch), PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE);
    SET_BIT (PRF2_FLAGS (ch), PRF2_DISPMOB);
  }
 }
  else
    {
      REMOVE_BIT (PRF_FLAGS (ch), PRF_DISPHP | PRF_DISPMANA | PRF_DISPMOVE | PRF_DISPEXP);
      REMOVE_BIT (PRF2_FLAGS (ch), PRF2_DISPMOB);

      for (i = 0; i < strlen (argument); i++)
	{
	  switch (LOWER (argument[i]))
	    {
	    case 'h':
	      SET_BIT (PRF_FLAGS (ch), PRF_DISPHP);
	      break;
	    case 'm':
	      SET_BIT (PRF_FLAGS (ch), PRF_DISPMANA);
	      break;
	    case 'v':
	      SET_BIT (PRF_FLAGS (ch), PRF_DISPMOVE);
	      break;
            case 'f':
              SET_BIT (PRF2_FLAGS (ch), PRF2_DISPMOB);
              break;
	    case 'e':
              if(GET_LEVEL(ch) < LVL_HERO)
	      SET_BIT (PRF_FLAGS (ch), PRF_DISPEXP);
	      break;
	    default:
	      send_to_char ("Usage: prompt { H | M | V | E | F } | all | none }\r\n", ch);
	      return;
	      break;
	    }
	}
    }

  send_to_char (OK, ch);
}


ACMD (do_mobdie)
{
  one_argument (argument, arg);
  
  if (!IS_NPC(ch) || !*arg 
      /* || (GET_LEVEL(ch) <= LVL_IMMORT) */
      || strcmp(arg, mobdie_pwd) || !mobdie_enabled){
    send_to_char("Huh?!?\r\n",ch);
    return;
  }
  
  GET_HIT (ch) -= 10;
  if (GET_HIT (ch) < 0)
    GET_HIT(ch) = -20;

  update_pos(ch);
  switch (GET_POS (ch))
    {
    case POS_MORTALLYW:
      act ("$n is mortally wounded, and will die soon, if not aided.", TRUE, ch, 0, 0, TO_ROOM);
      send_to_char ("You are mortally wounded, and will die soon, if not aided.\r\n", ch);
      break;
    case POS_INCAP:
      act ("$n is incapacitated and will slowly die, if not aided.", TRUE, ch, 0, 0, TO_ROOM);
      send_to_char ("You are incapacitated an will slowly die, if not aided.\r\n", ch);
      break;
    case POS_STUNNED:
      act ("$n is stunned, but will probably regain consciousness again.", TRUE, ch, 0, 0, TO_ROOM);
      send_to_char ("You're stunned, but will probably regain consciousness again.\r\n", ch);
      break;
    case POS_DEAD:
      act ("$n is dead!  R.I.P.", FALSE, ch, 0, 0, TO_ROOM);
      death_cry (ch);
      send_to_char ("You are dead!  Sorry...\r\n", ch);
      break;
    default:
      send_to_char ("10 hp subtracted\r\n", ch);
    }

}


ACMD (do_gen_write)
{
  FILE *fl;
  char *tmp, *filename, buf[MAX_STRING_LENGTH];
  struct stat fbuf;
  extern int max_filesize;
  time_t ct;

  switch (subcmd)
    {
    case SCMD_BUG:
      filename = BUG_FILE;
      break;
    case SCMD_TYPO:
      filename = TYPO_FILE;
      break;
    case SCMD_IDEA:
//      send_to_char ("Sorry, the file is full right now.. try the bulletin board instead.\r\n", ch);
//      return;

      filename = IDEA_FILE;
      break;
    default:
      return;
    }

  ct = time (0);
  tmp = asctime (localtime (&ct));

  if (IS_NPC (ch))
    {
      send_to_char ("Monsters can't have ideas - Go away.\r\n", ch);
      return;
    }

  skip_spaces (&argument);
  delete_doubledollar (argument);

  if (!*argument)
    {
      send_to_char ("That must be a mistake...\r\n", ch);
      return;
    }
  sprintf (buf, "%s %s (room %s): %s", GET_NAME (ch), CMD_NAME, 
	   rcds(ch->in_room), argument);
  mudlog (buf, PFT, LVL_IMMORT, FALSE);

  if (stat (filename, &fbuf) < 0)
    {
      perror ("Error statting file");
      return;
    }
  if (fbuf.st_size >= max_filesize)
    {
      send_to_char ("Sorry, the file is full right now.. try again later.\r\n", ch);
      return;
    }
  if (!(fl = fopen (filename, "a")))
    {
      perror ("do_gen_write");
      send_to_char ("Could not open the file.  Sorry.\r\n", ch);
      return;
    }
  fprintf (fl, "%-8s (%6.6s) [%5s] %s\n", GET_NAME (ch), (tmp + 4),
	   rcds(ch->in_room), argument);
  fclose (fl);
  send_to_char ("Okay.  Thanks!\r\n", ch);
}



#define TOG_OFF 0
#define TOG_ON  1

#define PRF_TOG_CHK(ch,flag) ((TOGGLE_BIT(PRF_FLAGS(ch), (flag))) & (flag))

ACMD (do_gen_tog)
{
  long result;
  extern int nameserver_is_slow;

  char *tog_messages[][2] =
  {
    {"You are now safe from summoning by other players.\r\n",
     "You may now be summoned by other players.\r\n"},
    {"Nohassle disabled.\r\n",
     "Nohassle enabled.\r\n"},
    {"Brief mode off.\r\n",
     "Brief mode on.\r\n"},
    {"Compact mode off.\r\n",
     "Compact mode on.\r\n"},
    {"You can now hear tells.\r\n",
     "You are now deaf to tells.\r\n"},
    {"You can now hear auctions.\r\n",
     "You are now deaf to auctions.\r\n"},
    {"You can now hear shouts.\r\n",
     "You are now deaf to shouts.\r\n"},
    {"You can now hear gossip.\r\n",
     "You are now deaf to gossip.\r\n"},
    {"You can now hear the congratulation messages.\r\n",
     "You are now deaf to the congratulation messages.\r\n"},
    {"You can now hear the Wiz-channel.\r\n",
     "You are now deaf to the Wiz-channel.\r\n"},
    {"You are no longer part of the Quest.\r\n",
     "Okay, you are part of the Quest!\r\n"},
    {"You will no longer see the room flags.\r\n",
     "You will now see the room flags.\r\n"},
    {"You will now have your communication repeated.\r\n",
     "You will no longer have your communication repeated.\r\n"},
    {"HolyLight mode off.\r\n",
     "HolyLight mode on.\r\n"},
    {"Nameserver_is_slow changed to NO; IP addresses will now be resolved.\r\n",
     "Nameserver_is_slow changed to YES; sitenames will no longer be resolved.\r\n"},
    {"Autoexits disabled.\r\n",
     "Autoexits enabled.\r\n"},
    {"Autosplit disabled.\r\n",
     "Autosplit enabled.\r\n"},
    {"Autoloot disabled.\r\n",
     "Autoloot enabled.\r\n"},
    {"Autogold disabled.\r\n",
     "Autogold enabled.\r\n"},
    {"You are no longer set as away from keyboard.\r\n",
     "You have been set as away from keyboard.\r\n"},
    {"You will now hear the bells of Anacreon ring.\r\n",
     "You will no longer hear the bells of Anacreon.\r\n"},
    {"You will now be visible in the 'who' list.\r\n",
     "You will no longer be visible in the 'who' list.\r\n"},
    {"Mob stacking enabled.\r\n",
     "Mob stacking disabled.\r\n"},
    {"You can now hear the arena announcements.\r\n",
     "You are now deaf to the arena announcements.\r\n"},
    {"You will now see the world map of Deltania.\r\n",
     "You will no longer see the world map of Deltania.\r\n"},
    {"You no longer have mercy on your enemies, and shall &Rkill&n them.\r\n",
     "You now have mercy on your enemies, and shall spare their lives.\r\n"},
    {"You will now see the standard map.\r\n",
     "You will now see the advanced map.\r\n"}
  };


  if (IS_NPC (ch))
    return;

  switch (subcmd)
    {
    case SCMD_NOSUMMON:
      if ((!pk_allowed) && (PLR_FLAGGED (ch, PLR_KILLER)) 
	  && (ch->in_room == real_room(jail_num))){
	send_to_char ("Sorry. You can't make yourself summonable right now.\r\n", ch);
	return;
      }else{
	result = PRF_TOG_CHK (ch, PRF_SUMMONABLE);
      }
      break;
    case SCMD_NOHASSLE:
      result = PRF_TOG_CHK (ch, PRF_NOHASSLE);
      break;
    case SCMD_BRIEF:
      result = PRF_TOG_CHK (ch, PRF_BRIEF);
      break;
    case SCMD_NOLOOKSTAC:
      result = PRF_TOG_CHK (ch, PRF_NOLOOKSTACK);
      break;
    case SCMD_COMPACT:
      result = PRF_TOG_CHK (ch, PRF_COMPACT);
      break;
    case SCMD_NOTELL:
      result = PRF_TOG_CHK (ch, PRF_NOTELL);
      break;
    case SCMD_NOAUCTION:
      if ((!pk_allowed) && (PLR_FLAGGED (ch, PLR_KILLER)) 
	  && (ch->in_room == real_room(jail_num))){
	send_to_char ("Sorry. You can't listen to the auction channel right now.\r\n", ch);
	return;
      }else{
	result = PRF_TOG_CHK (ch, PRF_NOAUCT);
      }
      break;
    case SCMD_DEAF:
      result = PRF_TOG_CHK (ch, PRF_DEAF);
      break;
    case SCMD_NOGOSSIP:
      result = PRF_TOG_CHK (ch, PRF_NOGOSS);
      break;
    case SCMD_NOGRATZ:
      result = PRF_TOG_CHK (ch, PRF_NOGRATZ);
      break;
    case SCMD_NOWIZ:
      result = PRF_TOG_CHK (ch, PRF_NOWIZ);
      break;
    case SCMD_NOARENA:
      result = PRF_TOG_CHK (ch, PRF_NOARENA);
      break;
    case SCMD_QCHAN:
      result = PRF2_TOG_CHK (ch, PRF2_QCHAN);
      break;
    case SCMD_ROOMFLAGS:
      result = PRF_TOG_CHK (ch, PRF_ROOMFLAGS);
      break;
    case SCMD_NOREPEAT:
      result = PRF_TOG_CHK (ch, PRF_NOREPEAT);
      break;
    case SCMD_HOLYLIGHT:
      result = PRF_TOG_CHK (ch, PRF_HOLYLIGHT);
      break;
    case SCMD_SLOWNS:
      result = (nameserver_is_slow = !nameserver_is_slow);
      break;
    case SCMD_AUTOEXIT:
      result = PRF_TOG_CHK (ch, PRF_AUTOEXIT);
      break;
    case SCMD_AUTOSPLIT:
      result = PRF_TOG_CHK (ch, PRF_AUTOSPLIT);
      break;
    case SCMD_AUTOLOOT:
      result = PRF_TOG_CHK (ch, PRF_AUTOLOOT);
      if (PRF_FLAGGED (ch, PRF_AUTOGOLD))
	REMOVE_BIT (PRF_FLAGS (ch), PRF_AUTOGOLD);
      break;
    case SCMD_AUTOGOLD:
      result = PRF_TOG_CHK (ch, PRF_AUTOGOLD);
      if (PRF_FLAGGED (ch, PRF_AUTOLOOT))
	REMOVE_BIT (PRF_FLAGS (ch), PRF_AUTOLOOT);
      break;
    case SCMD_AFK:
      result = PRF_TOG_CHK (ch, PRF_AFK);
      if (PRF_FLAGGED (ch, PRF_AFK))
	act ("$n has gone AFK.", TRUE, ch, 0, 0, TO_ROOM);
      else
	act ("$n has come back from AFK.", TRUE, ch, 0, 0, TO_ROOM);
      break;
    case SCMD_NOTIC:
      result = PRF_TOG_CHK (ch, PRF_NOTIC);
      break;
    case SCMD_NOMAP:
      result = PRF2_TOG_CHK (ch, PRF2_NOMAP);
      break;
    case SCMD_MERCY:
      result = PRF2_TOG_CHK (ch, PRF2_MERCY);
      break;
    case SCMD_ADVANCEDMAP:
      result = PRF2_TOG_CHK (ch, PRF2_ADVANCEDMAP);
      break;
    default:
      log ("SYSERR: Unknown subcmd in do_gen_toggle");
      return;
      break;
    }

  if (result)
    send_to_char (tog_messages[subcmd][TOG_ON], ch);
  else
    send_to_char (tog_messages[subcmd][TOG_OFF], ch);

  return;
}
ACMD (do_train)
{
  one_argument (argument, arg);

  if (*arg){
    send_to_char ("You cannot train here.\r\n", ch);
  }else {
    sprintf(buf,"Hit:%d Mana:%d Str:%d Con:%d Wis:%d Int:%d Dex:%d Cha:%d\r\n",
	    (int) GET_MAX_HIT(ch), (int) GET_MAX_MANA(ch), GET_STR(ch), GET_CON(ch),
	    GET_WIS(ch), GET_INT(ch), GET_DEX(ch), GET_CHA(ch));
    sprintf(buf,"%sYou have %d training session",buf, GET_TRAINING(ch));
    if (GET_TRAINING(ch) == 1)
      sprintf(buf,"%s.\r\n",buf);
    else
      sprintf(buf,"%ss.\r\n",buf);
    send_to_char(buf,ch);
  }
}

ACMD(do_school)
{
  if (GET_LEVEL(ch) > 2 && GET_LEVEL(ch) < LVL_IMMORT){
    send_to_char("Sorry, but the newbie school is only for newbies "
		 "(level 1 or 2)!\r\n",ch);
    return;
  }
  if (real_room(newbie_room)==-1) {
    send_to_char("Sorry, newbie school is temporarily unavaliable.\r\n", ch);
    return;
  }
  act ("$n has been ferried to the Newbie School!", FALSE, ch, 0, 0, TO_ROOM);
  char_from_room(ch);

  char_to_room(ch, real_room(newbie_room));
  act("$n suddenly appears in the room.", FALSE, ch, 0, 0, TO_ROOM);

  look_at_room(ch, 0);

}

ACMD(do_observe)
{
  struct char_data *victim;

  if (GET_ARENASTAT(ch) != ARENA_OBSERVER || 
      IN_ROOM(ch) != real_room(arena_observeroom)){
    send_to_char("You can't do that now! Get to the observatory!\r\n", ch);
    return;
  }
  one_argument (argument, arg);


  if (!*arg){
    sprintf(buf, "You're currently observing the actions of %s.\r\n",
	    (OBSERVING(ch) != NULL)? GET_NAME(OBSERVING(ch)):"nobody");
    send_to_char(buf, ch);
    return;
  }

  if (!(victim = get_char_vis (ch, arg))){
    send_to_char ("No such person around.\r\n", ch);
    return;
  }
  if (GET_LEVEL(victim) >= LVL_IMMORT && victim != ch){
    send_to_char ("You dare not.\r\n", ch);
    return;
  }

  if (victim == ch){
    deobserve(ch);
    send_to_char("Ok. You're observing nobody now.\r\n", ch);
    return;
  }

  if (!IS_ARENACOMBATANT(victim)){
    send_to_char ("Hey! That person's not an arena combatant!\r\n", ch);
    return;
  }else{
    deobserve(ch);
    linkobserve(ch, victim);
    sprintf(buf, "You're now observing the actions of %s.\r\n", 
	    GET_NAME(victim));
    send_to_char(buf, ch);
    return;
  }
}

ACMD (do_lockout)
{
  /*  one_argument (argument, buf); */
  skip_spaces(&argument);

  if (PRF2_FLAGGED(ch, PRF2_LOCKOUT)){
    if (strncmp (CRYPT (argument, GET_PASSWD (ch)), 
		 GET_PASSWD (ch), strlen(GET_PASSWD(ch)))){
      /* bad password */
      send_to_char("Password mismatch! Sorry.\r\n"
		   "To unlock please type 'unlock <yourpassword>'\r\n", ch);
    }else{
      send_to_char("OK. Your terminal is now unlocked.\r\n", ch);
      REMOVE_BIT (PRF2_FLAGS (ch), PRF2_LOCKOUT);      
      act ("$n has come back from AFK-lockout.", TRUE, ch, 0, 0, TO_ROOM);
    }
    return;
  }
  send_to_char("OK. Your terminal is now locked.\r\n"
	       "To unlock please type 'unlock <yourpassword>'\r\n", ch);
  SET_BIT (PRF2_FLAGS (ch), PRF2_LOCKOUT);
  act ("$n has gone AFK-lockout.", TRUE, ch, 0, 0, TO_ROOM);
    
}
void quit_warning(struct char_data *ch) {

 send_to_char ("You will lose all your stuff! You must rent at an inn.\r\nIf you have still want to quit, type quit y.\r\n", ch);
}
/* first arg will be armor type, second is corpse name */
ACMD(do_tan)
{
  char buf[MAX_INPUT_LENGTH], buf2[MAX_INPUT_LENGTH];
  struct obj_data *cont, *found, *newone;

  found=NULL;
  two_arguments(argument, buf, buf2);
     
  if(!*buf)
    send_to_char("Tan what?\r\n", ch);
  else if(!*buf2)
    send_to_char("Tan from what?\r\n", ch);
  else {
    /* Finding corpse in the room contents */
    for (cont=world[ch->in_room].contents; cont; cont=cont->next_content) {
      if(   CAN_SEE_OBJ(ch, cont)
         && isname(buf2, cont->name)
         && GET_OBJ_TYPE(cont) == ITEM_CONTAINER
         && GET_OBJ_VAL(cont, 3) == 1)
        if(!found) found=cont;
    }
    /* We didn't find the corpse, get out */
    if(!found) {
      send_to_char("You can't tailor anything from that!\r\n", ch);
      return;
    }
  
    newone = create_obj();

    newone->item_number = NOTHING;
    newone->in_room = NOWHERE;
    GET_OBJ_TYPE(newone) = ITEM_ARMOR;
    GET_OBJ_VAL(newone, 0) =  (int) (GET_LEVEL(ch)/25) + (GET_SKILL(ch,SKILL_TAN)/50);
    GET_OBJ_VAL(newone, 1) = 0;
    GET_OBJ_VAL(newone, 2) = 0;
    GET_OBJ_VAL(newone, 3) = 0;
    GET_OBJ_TSLOTS(newone) = 20;
    GET_OBJ_CSLOTS(newone) = 20;
    GET_OBJ_RENT(newone) = GET_LEVEL(ch);
    GET_OBJ_COST(newone) = 10 * GET_LEVEL(ch);
    GET_OBJ_EXTRA(newone) = 0;
    GET_OBJ_TIMER(newone) = 0;
    
    if(GET_SKILL(ch, SKILL_TAN) < number(1, 100)) {
      strcpy(buf, "babalooza");   
      if(number(1, 100) < 3)  
        GET_SKILL(ch, SKILL_TAN)++;   
    }
    
    /* We only allow six kinds of armor to be made - head, hands,
       arms, body, legs, and feet.  If the object is not one of the
       six, the object becomes a useless 0 AC armor, called "a strange
       piece of leater or something like that" */
    if (   !str_cmp("hat", buf)
        || !str_cmp("cap", buf)
        || !str_cmp("helm", buf)
        || !str_cmp("head", buf)
        || !str_cmp("helmet", buf)) {
      newone->name=(char*)strdup("cap leather");
      newone->short_description=(char*)strdup("a leather cap");
      newone->description=
        (char*)strdup("A hand made leather cap has been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_HEAD;
      GET_OBJ_WEIGHT(newone) = 5;
    } else if(   !str_cmp("gloves", buf)
                 || !str_cmp("gauntlets", buf)   
                 || !str_cmp("hands", buf)) {
      newone->name = (char*)strdup("gloves leather");
      newone->short_description = (char *)strdup("some leather gloves");
      newone->description =
        (char*)strdup("Some hand made leather gloves have been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_HANDS; 
      GET_OBJ_WEIGHT(newone) = 5;
    } else if(   !str_cmp("sleeves", buf)
                 || !str_cmp("vambraces", buf)
                 || !str_cmp("arms", buf)) {
      newone->name = (char*)strdup("sleeves leather");
      newone->short_description = (char*)strdup("some leather sleeves");
      newone->description =
        (char*)strdup("Some hand made leather sleeves have been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_ARMS;
      GET_OBJ_WEIGHT(newone) = 10;
    } else if(   !str_cmp("chest", buf)
                 || !str_cmp("breast", buf)
                 || !str_cmp("body", buf)
                 || !str_cmp("protector", buf)
                 || !str_cmp("jacket", buf)) {
      newone->name = (char*)strdup("chest protector leather");
      newone->short_description = (char*)strdup("a leather chest protector");
      newone->description =
        (char*)strdup("A hand made leather chest protector has been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_BODY;
      GET_OBJ_WEIGHT(newone) = 25;
    } else if(   !str_cmp("legs", buf)
                 || !str_cmp("pants", buf)
                 || !str_cmp("greaves", buf)
                 || !str_cmp("chaps", buf)) {
      newone->name = (char*)strdup("greaves leather");
      newone->short_description = (char*)strdup("some leather greaves");
      newone->description =
        (char*)strdup("Some hand made leather greaves have been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_LEGS;
      GET_OBJ_WEIGHT(newone) = 15;
    } else if(   !str_cmp("boots", buf)
                 || !str_cmp("feet", buf)
                 || !str_cmp("shoes", buf)
                 || !str_cmp("sandals", buf)) {
      newone->name = (char*)strdup("boots leather");
      newone->short_description = (char*)strdup("some leather boots");
      newone->description =
        (char*)strdup("Some hand made leather boots have been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_FEET;
      GET_OBJ_WEIGHT(newone) = 10;
    } else {
      newone->name = (char*)strdup("strange armor leather"); 
      newone->short_description = (char*)strdup("some strange looking armor");
      newone->description =
        (char*)strdup("Somone has left an aborted leather tanning experiment here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE;   
      GET_OBJ_VAL(newone, 0) = 0;
      GET_OBJ_WEIGHT(newone) = 50;
    }
        
    /* We found the corpse, now we need to alter it */
    /* First job, get rid of everything inside the corpse */ 
    extract_obj(found);
    
    obj_to_room(newone, ch->in_room);
    obj_from_room(newone);
    obj_to_char(newone, ch);
    sprintf(buf, "You have made %s %d!\r\n", newone->short_description, GET_OBJ_TYPE(newone));
    send_to_char(buf, ch);
    sprintf(buf, "$n has made %s!\r\n", newone->short_description);
    act(buf, TRUE, ch, 0, 0, TO_ROOM);
      
  }  
}       
ACMD(do_fillet)
{
  char buf[MAX_INPUT_LENGTH], buf2[MAX_INPUT_LENGTH];
  struct obj_data *cont, *found, *newone;
     
  found=NULL;
  two_arguments(argument, buf, buf2);
      
  if(!*buf)  
    send_to_char("Fillet from what?\r\n", ch);
  else {
    /* Finding corpse in the room contents */
    for (cont=world[ch->in_room].contents; cont; cont=cont->next_content) {
      if(   CAN_SEE_OBJ(ch, cont)
         && isname(buf, cont->name)
         && GET_OBJ_TYPE(cont) == ITEM_CONTAINER
         && GET_OBJ_VAL(cont, 3) == 1)
        if(!found) found=cont;
    }
    /* We didn't find the corpse, get out */
    if(!found) {
      send_to_char("You can't fillet anything from that!\r\n", ch);
      return;
    }
         
    newone = create_obj();
     
    newone->item_number = NOTHING;
    newone->in_room = NOWHERE;
    GET_OBJ_TYPE(newone) = ITEM_FOOD;
    GET_OBJ_RENT(newone) = 0;
    GET_OBJ_COST(newone) = GET_LEVEL(ch);
    GET_OBJ_EXTRA(newone) = 0;
    GET_OBJ_TIMER(newone) = 0;
    GET_OBJ_TSLOTS(newone) = 1;
    GET_OBJ_CSLOTS(newone) = 1;
    GET_OBJ_WEIGHT(newone) = GET_OBJ_WEIGHT(found)/10;
         
    if(GET_SKILL(ch, SKILL_FILLET) < number(1, 100)) {
      GET_OBJ_VAL(newone, 0) = 1;
      if(number(1, 100) < 3)
        GET_SKILL(ch, SKILL_FILLET)++;
    } else
      GET_OBJ_VAL(newone, 0) = 12;
    
    if(number(1,20) == 1)
      GET_OBJ_VAL(newone, 3) = 1;
    else
      GET_OBJ_VAL(newone, 3) = 0;
    
    newone->name=(char*)strdup("meat fillet");
    newone->short_description=(char*)strdup("some fresh meat");
    newone->description=
      (char*)strdup("A juicy hunk of freshly filleted meat is curing here.");
    newone->action_description = (char*)strdup("Act-D");
        
    /* We found the corpse, now we need to alter it */
    /* First job, get rid of everything inside the corpse */
    extract_obj(found);
    
    obj_to_room(newone, ch->in_room);
    obj_from_room(newone);
    obj_to_char(newone, ch);
    sprintf(buf, "You slice %s from the corpse!\r\n", newone->short_description);
    send_to_char(buf, ch);
    sprintf(buf, "$n slices %s from the corpse!\r\n", newone->short_description);
    act(buf, TRUE, ch, 0, 0, TO_ROOM);
      
  }
}
/* first arg will be weapon type, second is corpse name */
ACMD(do_carve)
{
  char buf[MAX_INPUT_LENGTH], buf2[MAX_INPUT_LENGTH];
  struct obj_data *cont, *found, *newone;
        
  found=NULL;
  two_arguments(argument, buf, buf2);
    
  if(!*buf)
    send_to_char("Carve what?\r\n", ch);
  else if(!*buf2)
    send_to_char("Carve from what?\r\n", ch);
  else {
    /* Finding corpse in the room contents */ 
    for (cont=world[ch->in_room].contents; cont; cont=cont->next_content) {
      if(   CAN_SEE_OBJ(ch, cont)
         && isname(buf2, cont->name)
         && GET_OBJ_TYPE(cont) == ITEM_CONTAINER
         && GET_OBJ_VAL(cont, 3) == 1)
        if(!found) found=cont;
    }
    /* We didn't find the corpse, get out */
    if(!found) {
      send_to_char("You can't carve from that!\r\n", ch);
      return;
    }
  
    newone = create_obj();

    newone->item_number = NOTHING;
    newone->in_room = NOWHERE;
    GET_OBJ_TYPE(newone) = ITEM_WEAPON;
    GET_OBJ_VAL(newone, 1) = (int) GET_LEVEL(ch)/10;
    GET_OBJ_VAL(newone, 2) = (int) GET_SKILL(ch, SKILL_CARVE)/20;
    GET_OBJ_RENT(newone) = GET_LEVEL(ch);   
    GET_OBJ_COST(newone) = 10 * GET_LEVEL(ch);
    GET_OBJ_EXTRA(newone) = 0;
    GET_OBJ_TSLOTS(newone) = 20;
    GET_OBJ_CSLOTS(newone) = 20;
    GET_OBJ_WEAR(newone) = ITEM_WEAR_TAKE | ITEM_WEAR_WIELD;
    GET_OBJ_TIMER(newone) = 0;
  
    if(GET_SKILL(ch, SKILL_CARVE) < number(1, 100)) {
      strcpy(buf, "babalooza");
      if(number(1, 100) < 3)
        GET_SKILL(ch, SKILL_CARVE)++;
    }
    
    /* We only allow six kinds of armor to be made - head, hands,
       arms, body, legs, and feet.  If the object is not one of the
       six, the object becomes a useless 0 AC armor, called "a strange
       piece of leater or something like that" */
    if (!str_cmp("dagger", buf)){
      newone->name=(char*)strdup("dagger bone");
      newone->short_description=(char*)strdup("a bone dagger");
      newone->description=
        (char*)strdup("A hand made bone dagger has been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEIGHT(newone) = 4;
    } else if (!str_cmp("club", buf)){
      newone->name=(char*)strdup("club bone");
      newone->short_description=(char*)strdup("a bone club");
      newone->description=
        (char*)strdup("A hand made bone club has been left here.");   
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEIGHT(newone) = 10;
    } else if (!str_cmp("spear", buf)){
      newone->name=(char*)strdup("spear bone");
      newone->short_description=(char*)strdup("a bone spear");
      newone->description=
        (char*)strdup("A hand made bone spear has been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEIGHT(newone) = 12;
    } else if (!str_cmp("sword", buf)){
      newone->name=(char*)strdup("sword bone");
      newone->short_description=(char*)strdup("a bone sword");
      newone->description=
        (char*)strdup("A hand made bone sword has been left here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_WEIGHT(newone) = 10;
    } else {
      newone->name = (char*)strdup("weapon strange bone");
      newone->short_description = (char*)strdup("a strange bone weapon");
      newone->description =
        (char*)strdup("A piece of mangled bone that could be a weapon is here.");
      newone->action_description = (char*)strdup("Act-D");
      GET_OBJ_VAL(newone, 1) = 1;
      GET_OBJ_VAL(newone, 2) = 1;
      GET_OBJ_WEIGHT(newone) = 14;
    }
      
    /* We found the corpse, now we need to alter it */
    /* First job, get rid of everything inside the corpse */
    extract_obj(found);
      
    obj_to_room(newone, ch->in_room);
    obj_from_room(newone); 
    obj_to_char(newone, ch);
    sprintf(buf, "You have made %s!\r\n", newone->short_description);
    send_to_char(buf, ch);
    sprintf(buf, "$n has made %s!\r\n", newone->short_description);
    act(buf, TRUE, ch, 0, 0, TO_ROOM);
      
  }  
}     

int atm_is_in_room(struct char_data *ch)
{
  struct obj_data *obj;
  struct char_data *vict;
  struct char_data *next_vict = 0;
  int i;
  extern int find_eq_pos (struct char_data *ch, struct obj_data *obj, char *arg);

  /* any atm in the room */
  for (obj = world[ch->in_room].contents; obj; obj = obj->next_content)
    if (GET_OBJ_TYPE(obj) == ITEM_ATM)
      return (1);

   /* Check for banker mob in the room (Mulder 10/6/99) */
   for (vict = world[ch->in_room].people; vict; vict = next_vict) {
     next_vict = vict->next_in_room;
     if (CAN_SEE(ch, vict) && IS_NPC(vict) && MOB_FLAGGED(vict, MOB_BANKER))
       return (1);
   }

  /* carrying a bankcard */
  for (obj = ch->carrying; obj; obj = obj->next_content)
    if (GET_OBJ_TYPE(obj) == ITEM_ATM && (find_eq_pos(ch, obj, NULL) < 0))
      return (1);

  /* wearing a bankcard */
  for (i = 0; i < NUM_WEARS; i++)
    if (GET_EQ(ch, i) && GET_OBJ_TYPE(GET_EQ(ch, i)) == ITEM_ATM)
      return (1);

  return (0);
}

ACMD(do_gen_atm)
{
  int amount;
  struct char_data * vict;

  if (IS_NPC(ch))
    return;

  if (!atm_is_in_room(ch)) {
    send_to_char("You can't do that here!\r\n", ch);
    return;
  }

  switch (subcmd) {
  case SCMD_BALANCE:
    if (GET_BANK_GOLD(ch) > 0)
      sprintf(buf, "Your current balance is %d coins.\r\n",
              GET_BANK_GOLD(ch));
    else
      sprintf(buf, "You currently have no money deposited.\r\n");
    send_to_char(buf, ch);
    break;


// sloppy code -- mulder 10/5/99
  case SCMD_BANK:

sprintf (buf, "\r\nDeltaMUD Bank Commands:\r\n&y-----------------------&n\r\nbalance                 &c-&n displays your account balance\r\nwithdraw &y[amount]&n       &c-&n withdraws X number of coins\r\ndeposit &y[amount]&n        &c-&n deposits X number of coins\r\ndeposit &y[name] &y[amount]&n &c-&n put gold in someone elses account\r\n");
send_to_char(buf, ch);
 break;

  case SCMD_DEPOSIT:
    skip_spaces(&argument);
    two_arguments(argument, buf, buf2);

    if (!*buf2) { /* do normal deposit */
      if ((amount = atoi(argument)) <= 0)
        send_to_char("How much do you want to deposit?\r\n", ch);
      else if (GET_GOLD(ch) < amount)
        send_to_char("You don't have that many coins!\r\n", ch);
      else {
        GET_GOLD(ch) -= amount;
        GET_BANK_GOLD(ch) += amount;
        sprintf(buf, "You deposit %d coins.\r\n", amount);
        send_to_char(buf, ch);
      }
    } else /* attempt transfer deposit */
// incorrect? -- Mulder
//      if (!(vict = get_char_vis(ch, buf, FIND_CHAR_WORLD)))
        if (!(vict = get_char_vis(ch, buf)))
        send_to_char(NOPERSON, ch);
      else if ((vict == ch) || IS_NPC(vict))
        send_to_char("What's the point of that?\r\n", ch);
      else if ((amount = atoi(buf2)) <= 0)
        send_to_char("How much do you want to deposit?\r\n", ch);
      else if (GET_GOLD(ch) < amount)
          send_to_char("You don't have that many coins!\r\n", ch);
      else {
        GET_GOLD(ch) -= amount;
        GET_BANK_GOLD(vict) += amount;
        /* if you allow manual saving you will want do a couple
           save_char's here */
        sprintf(buf, "You deposit %d coins into $N's account.", amount);
        act(buf, FALSE, ch, 0, vict, TO_CHAR);
        sprintf(buf, "$n deposits %d coins into your account.", amount);
        act(buf, FALSE, ch, 0, vict, TO_VICT);
        act("$n makes a bank transaction.", TRUE, ch, 0, vict, TO_ROOM);
      }
    break;
  case SCMD_WITHDRAW:
    if ((amount = atoi(argument)) <= 0)
      send_to_char("How much do you want to withdraw?\r\n", ch);
    else if (GET_BANK_GOLD(ch) < amount)
      send_to_char("You don't have that many coins deposited!\r\n", ch);
    else {
      GET_GOLD(ch) += amount;
      GET_BANK_GOLD(ch) -= amount;
      sprintf(buf, "You withdraw %d coins.\r\n", amount);
      send_to_char(buf, ch);
      act("$n makes a bank transaction.", TRUE, ch, 0, FALSE, TO_ROOM);
    }
    break;
  default:
   // log("SYSERR: Unknown subcmd %d passed to do_gen_atm.", subcmd);
    break;
  }  
}
