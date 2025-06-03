/* ************************************************************************
   *   File: act.comm.c                                    Part of CircleMUD *
   *  Usage: Player-level communication commands                             *
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
#include "screen.h"
#include "dg_scripts.h"
#include "clan.h"

/* extern variables */
extern struct room_data *world;
extern struct descriptor_data *descriptor_list;
extern struct char_data *character_list;
struct player_index_element *player_table;
extern int top_of_p_table;

/* extern functions */
char    *makedrunk(char *string ,struct char_data *ch);
char *get_name_by_id (long id);
long get_id_by_name (char *name);
int compare(char *string1, char *string2);

bool is_tell_ok (struct char_data *ch, struct char_data *vict);

ACMD(do_ignore) {
  struct ignore_index_element *i=NULL, *lasti=NULL;
  char *name, who[21]="\0";
  long types=0;
  int l=0, argnum=1, x=0;

  skip_spaces(&argument);

  if (!*argument) {
    send_to_char("\r\nName                 Type           Reason\r\n", ch);
    send_to_char("-------------------- -------------- ------>\r\n", ch);
    for (i=ch->ignore_list; i; i=i->next) {
      if ((name=get_name_by_id(i->id)) == NULL) {
        send_to_char("None                 None           None\r\n", ch);
        return;
      }
      sprintf(buf, "%-20s", name);
      if (IS_SET(i->type, IGNORE_PUBLIC))
        strcat(buf, " pub");
      else
        strcat(buf, "    ");
      if (IS_SET(i->type, IGNORE_PRIVATE))
        strcat(buf, " priv");
      else
        strcat(buf, "     ");
      if (IS_SET(i->type, IGNORE_EMOTE))
        strcat(buf, " emote ");
      else
        strcat(buf, "       ");
      strcat(buf, i->reason);
      strcat(buf, "\r\n");
      send_to_char(buf, ch);
    }
    if (!ch->ignore_list)
      send_to_char("None                 None           None\r\n", ch);
    return;
  }

// Who are we ignoring/unignoring?
  for (l=0; l<strlen(argument); l++) {
    if (argument[l]==' ') {
      argnum++;
      continue;
    }
    if (argnum!=1) break;
    if (strlen(who)>=20) continue; /* Don't fill up buffers */
    x=strlen(who);
    who[x]=argument[l];
    who[x+1]='\0';
  }

  if (get_id_by_name(who)==-1) {
    send_to_char("\r\nNo player with that name exists.\r\n", ch);
    return;
  }
  for (argnum=0; argnum<=top_of_p_table; argnum++) {
    if (player_table[argnum].id == get_id_by_name(who) && player_table[argnum].level >= LVL_IMMORT) {
      send_to_char("\r\nYou cannot ignore immortals.\r\n", ch);
      return;
    }
  }

  if (argument[l]!='\'') {
    send_to_char("\r\nYou must supply the ignore type(s) enclosed in ' symbols. Types: 'pub priv emote all off'\r\n", ch);
    return;
  }

  l++;

  buf[0]='\0';

// What type of ignore do we want?

  for (; l<strlen(argument); l++) {
    if (argument[l]==' ' || argument[l]=='\'') {
      if (compare(buf, "pub"))
        SET_BIT(types, IGNORE_PUBLIC);
      else if (compare(buf, "pub"))
        SET_BIT(types, IGNORE_PUBLIC);
      else if (compare(buf, "priv"))
        SET_BIT(types, IGNORE_PRIVATE);
      else if (compare(buf, "emote"))
        SET_BIT(types, IGNORE_EMOTE);
      else if (compare(buf, "all")) {
        SET_BIT(types, IGNORE_PUBLIC);
        SET_BIT(types, IGNORE_PRIVATE);
        SET_BIT(types, IGNORE_EMOTE);
      }
      else if (compare(buf, "off")) {
        for (i=ch->ignore_list; i; i=i->next) {
          if (compare(get_name_by_id(i->id), who)) {
            if (i->reason)
              free(i->reason);
            if (lasti)
              lasti->next=i->next;
            else
              ch->ignore_list=i->next;
            free(i);
            send_to_char("\r\nRemoved ", ch);
            send_to_char(who, ch);
            send_to_char(" from your ignore list.\r\n", ch);
            return;
          }
          lasti=i;
        }
        send_to_char("\r\n", ch);
        send_to_char(who, ch);
        send_to_char(" isn't on your ignore list.\r\n", ch);
        return;
      }
      else {
        send_to_char("\r\nUnknown ignore option: '", ch);
        send_to_char(buf, ch);
        send_to_char("'\r\n", ch);
        return;
      }
      buf[0]='\0';
      if (argument[l]=='\'') break;
      continue;
    }
    x=strlen(buf);
    buf[x]=argument[l];
    buf[x+1]='\0';
  }

// What is the ignore reason?

  if (argument[l]!='\'') {
    send_to_char("\r\nYou must supply the ignore type(s) enclosed in ' symbols. Types: 'pub priv emote all off'\r\n", ch);
    return;
  }

  l++;

  if (argument[l]==' ') l++;

  buf[0]='\0';

  if (!*(argument+l))
    strcpy(buf, "Ignore");
  else {
    for (; l<strlen(argument); l++) {
      x=strlen(buf);
      buf[x]=argument[l];
      buf[x+1]='\0';
    }
  }

  for (i=ch->ignore_list; i; i=i->next) {
    if (compare(get_name_by_id(i->id), who)) {
      if (i->reason)
        free(i->reason);
      if (lasti)
        lasti->next=i->next;
      else
        ch->ignore_list=i->next;
      free(i);
    }
    lasti=i;
  }

  // Delete ignore entry if it exists...

  if (!ch->ignore_list) {
    CREATE(ch->ignore_list, struct ignore_index_element, 1);
    i=ch->ignore_list;
  }
  else {
    CREATE(lasti->next, struct ignore_index_element, 1);
    i=lasti->next;
  }
  i->id=get_id_by_name(who);
  i->type=types;
  i->reason=strdup(buf);

  // Create a new ignore entry.

  sprintf(buf1, "\r\nIgnored %s on", who);
  if (IS_SET(i->type, IGNORE_PUBLIC))
    strcat(buf1, " pub");
  if (IS_SET(i->type, IGNORE_PRIVATE))
    strcat(buf1, " priv");
  if (IS_SET(i->type, IGNORE_EMOTE))
    strcat(buf1, " emote");
  sprintf(buf1+strlen(buf1), " (%s)\r\n", buf);
  send_to_char(buf1, ch);
}

int isignoresend (struct char_data *ch1, struct char_data *ch2, long type) { // Is ch2 on ch1's ignore? If so send them a 'your are on ignore' msg
  struct ignore_index_element *i;
  for (i=ch1->ignore_list; i; i=i->next) {
    if (i->id==get_id_by_name(GET_NAME(ch2)) && IS_SET(i->type, type)) {
      sprintf(buf, "\r\nYou are on %s's ignore list. (%s)\r\n", GET_NAME(ch1), i->reason);
      send_to_char(buf, ch2);
      return 1;
    }
  }
  return 0;
}

int isignore (struct char_data *ch1, struct char_data *ch2, long type) { // Is ch2 on ch1's ignore?
  struct ignore_index_element *i;
  for (i=ch1->ignore_list; i; i=i->next) {
    if (i->id==get_id_by_name(GET_NAME(ch2)) && IS_SET(i->type, type)) {
      return 1;
    }
  }
  return 0;
}

ACMD(do_say)
{
  struct char_data *vict;
  int state;
  char *dead_msgs[10] = {
    "You feel a slight breeze.",
    "Something beckons at your ear.",
    "You hear a faint whisper.",
    "The hair on your skin stands on end.",
    "The room suddenly gets cold.",
    "Something taps at the back of your mind.",
    "Your heartbeat slows a little.",
    "You notice something in the corner of your eye.",
    "The shadows around you shift a little.",
    "You feel a chill run down your spine."
  };

  skip_spaces(&argument);

  if (IS_NPC(ch) && MOB_FLAGGED (ch, MOB_NOTALK)) {
    send_to_char ("You weren't designed to talk.\r\n", ch);
    return;
  }
  else
    if (!*argument) {
      send_to_char("What do you want to say?\r\n", ch);
      return;
    }
  buf[0]='\0';

  sprintf(buf2, "You ");

  if (!*argument) {
    send_to_char("What do you wish to say?\r\n", ch);
    return;
  }
  argument = makedrunk(argument, ch);
// no auto-periods
//        if (argument[strlen(argument)-1] != '.' && argument[strlen(argument)-1] != '?' 
//           && argument[strlen(argument)-1] != '!')
//                 strcat(argument, '.');

  if(argument[strlen(argument)-1]=='?')
    state = 1;
  else if(argument[strlen(argument)-1]=='!')
    state = 2;
  else
    state = 0;

  switch(state) {
    case 1:
      sprintf(buf + strlen(buf), "asks, \'%s\'", argument);
      sprintf(buf2 + strlen(buf2), "ask, \'%s\'", argument);
      break;
    case 2:
      sprintf(buf + strlen(buf), "exclaims, \'%s\'", argument);
      sprintf(buf2 + strlen(buf2), "exclaim, \'%s\'", argument);
      break;
    case 3:
      sprintf(buf + strlen(buf), "bewilders, \'%s\'", argument);
      sprintf(buf2 + strlen(buf2), "bewilders, \'%s\'", argument);
      break;
    case 0:
      sprintf(buf + strlen(buf), "says, \'%s\'", argument);
      sprintf(buf2 + strlen(buf2), "say, \'%s\'", argument);
      break;
    default:
      break;
  }

  for (vict=world[ch->in_room].people; vict; vict=vict->next_in_room) {
    if (vict == ch) {
      if (PRF_FLAGGED(ch, PRF_NOREPEAT))
	send_to_char (OK, ch);
      else
        act(buf2, FALSE, ch, 0, 0, TO_CHAR);
      continue;
    }
    if (!isignore(vict, ch, IGNORE_PUBLIC)) {
      buf1[0]='\0';
      if (PRF2_FLAGGED(ch, PRF2_INTANGIBLE) && !PRF2_FLAGGED(vict, PRF2_INTANGIBLE) && !PRF2_FLAGGED(ch, PRF2_MBUILDING) && GET_LEVEL(vict)<LVL_IMMORT) {
        strcpy(buf1, dead_msgs[number(0, 9)]);
      } else {
        strcat(buf1, PERS(ch, vict));
        strcat(buf1, " ");
        strcat(buf1, buf);
      }
      act (buf1, FALSE, vict, 0, 0, TO_CHAR);
    }
  }
  speech_mtrigger(ch, argument);
  speech_wtrigger(ch, argument);
}


ACMD (do_gsay)
{
  struct char_data *k;
  struct follow_type *f;

  skip_spaces (&argument);

  if (!IS_AFFECTED (ch, AFF_GROUP))
    {
      send_to_char ("But you are not the member of a group!\r\n", ch);
      return;
    }
  if (!*argument)
    send_to_char ("Yes, but WHAT do you want to group-say?\r\n", ch);
  else
    {
      if (ch->master)
	k = ch->master;
      else
	k = ch;

      argument = makedrunk(argument, ch);
      sprintf (buf, "$n tells the group, '%s'", argument);

      if (IS_AFFECTED (k, AFF_GROUP) && (k != ch))
	act (buf, FALSE, ch, 0, k, TO_VICT | TO_SLEEP);
      for (f = k->followers; f; f = f->next)
	if (IS_AFFECTED (f->follower, AFF_GROUP) && (f->follower != ch) && !isignore(f->follower, ch, IGNORE_PRIVATE))
	  act (buf, FALSE, ch, 0, f->follower, TO_VICT | TO_SLEEP);

      if (PRF_FLAGGED (ch, PRF_NOREPEAT))
	send_to_char ("&YOkay.&n\r\n", ch);
      else
	{
	  sprintf (buf, "You tell the group, '%s'", argument);
	  act (buf, FALSE, ch, 0, 0, TO_CHAR | TO_SLEEP);
	}
    }
}


void
perform_tell (struct char_data *ch, struct char_data *vict, char *arg)
{
  send_to_char (CCRED (vict, C_NRM), vict);
  sprintf (buf, "$n tells you, '%s'", arg);
  arg = makedrunk(arg, ch);
  act (buf, FALSE, ch, 0, vict, TO_VICT | TO_SLEEP);
  send_to_char (CCNRM (vict, C_NRM), vict);

  if (PRF_FLAGGED (ch, PRF_NOREPEAT))
    send_to_char (OK, ch);
  else
    {
      send_to_char (CCRED (ch, C_CMP), ch);
      sprintf (buf, "You tell $N, '%s'", arg);
      act (buf, FALSE, ch, 0, vict, TO_CHAR | TO_SLEEP);
      send_to_char (CCNRM (ch, C_CMP), ch);
    }

  GET_LAST_TELL (vict) = GET_IDNUM (ch);
}

bool
is_tell_ok (struct char_data *ch, struct char_data *vict)
{
  if (ch == vict)
    send_to_char ("You try to tell yourself something.\r\n", ch);
  else if (PRF_FLAGGED (ch, PRF_NOTELL))
    send_to_char ("You can't tell other people while you have notell on.\r\n", ch);
  else if (ROOM_FLAGGED (ch->in_room, ROOM_SOUNDPROOF))
    send_to_char ("The walls seem to absorb your words.\r\n", ch);
  else if (!IS_NPC (vict) && !vict->desc)	/* linkless */
    act ("$E's linkless at the moment.", FALSE, ch, 0, vict, TO_CHAR | TO_SLEEP);
  else if (PLR_FLAGGED (vict, PLR_WRITING))
    act ("$E's writing a message right now; try again later.", FALSE, ch, 0, vict, TO_CHAR | TO_SLEEP);
  else if (PRF_FLAGGED (vict, PRF_NOTELL) || ROOM_FLAGGED (vict->in_room, ROOM_SOUNDPROOF))
    act ("$E can't hear you.", FALSE, ch, 0, vict, TO_CHAR | TO_SLEEP);
  else if (isignoresend(vict, ch, IGNORE_PRIVATE))
    return 0;
  else if (PRF_FLAGGED (vict, PRF_AFK))
    {
      act ("$E has AFK on, and may not see your message.",
	   FALSE, ch, 0, vict, TO_CHAR | TO_SLEEP);
      return 1;			/* put this here so the tell still gets to vict */
    }
  else
    return 1;

  return 0;
}

/*
 * Yes, do_tell probably could be combined with whisper and ask, but
 * called frequently, and should IMHO be kept as tight as possible.
 */
ACMD (do_tell)
{
  struct char_data *vict;

  half_chop (argument, buf, buf2);

  if (!*buf || !*buf2)
    send_to_char ("Who do you wish to tell what??\r\n", ch);
  else if (!(vict = get_char_vis (ch, buf)))
    send_to_char (NOPERSON, ch);
  else if (is_tell_ok (ch, vict))
    perform_tell (ch, vict, buf2);
}


ACMD (do_reply)
{
  struct char_data *tch = character_list;

  skip_spaces (&argument);

  if (GET_LAST_TELL (ch) == NOBODY)
    send_to_char ("You have no-one to reply to!\r\n", ch);
  else if (!*argument)
    send_to_char ("What is your reply?\r\n", ch);
  else
    {
      /*
       * Make sure the person you're replying to is still playing by searching
       * for them.  Note, now last tell is stored as player IDnum instead of
       * a pointer, which is much better because it's safer, plus will still
       * work if someone logs out and back in again.
       */

      while (tch != NULL && GET_IDNUM (tch) != GET_LAST_TELL (ch))
	tch = tch->next;

      if (tch == NULL)
	send_to_char ("They are no longer playing.\r\n", ch);
      else if (is_tell_ok (ch, tch)) {
        argument = makedrunk(argument, ch);
	perform_tell (ch, tch, argument);
    }
    }
}


ACMD (do_spec_comm)
{
  struct char_data *vict;
  char *action_sing, *action_plur, *action_others;

  if (subcmd == SCMD_WHISPER)
    {
      action_sing = "whisper to";
      action_plur = "whispers to";
      action_others = "$n whispers something to $N.";
    }
  else
    {
      action_sing = "ask";
      action_plur = "asks";
      action_others = "$n asks $N a question.";
    }

  half_chop (argument, buf, buf2);

  if (!*buf || !*buf2)
    {
      sprintf (buf, "Whom do you want to %s.. and what??\r\n", action_sing);
      send_to_char (buf, ch);
    }
  else if (!(vict = get_char_room_vis (ch, buf)))
    send_to_char (NOPERSON, ch);
  else if (vict == ch)
    send_to_char ("You can't get your mouth close enough to your ear...\r\n", ch);
  else if (isignoresend(vict, ch, IGNORE_PRIVATE)) return;
  else
    {
      argument = makedrunk(argument, ch);
      sprintf(buf, "$n %s you, '%s'", action_plur, buf2);
      act (buf, FALSE, ch, 0, vict, TO_VICT);
      if (PRF_FLAGGED (ch, PRF_NOREPEAT))
	send_to_char (OK, ch);
      else
	{
	  sprintf (buf, "You %s %s, '%s'\r\n", action_sing, GET_NAME (vict), buf2);
	  act (buf, FALSE, ch, 0, 0, TO_CHAR);
	}
      act (action_others, FALSE, ch, 0, vict, TO_NOTVICT);
    }
}



#define MAX_NOTE_LENGTH 1000	/* arbitrary */

ACMD (do_write)
{
  struct obj_data *paper = 0, *pen = 0;
  char *papername, *penname;

  papername = buf1;
  penname = buf2;

  two_arguments (argument, papername, penname);

  if (!ch->desc)
    return;

  if (!*papername)
    {				/* nothing was delivered */
      send_to_char ("Write?  With what?  ON what?  What are you trying to do?!?\r\n", ch);
      return;
    }
  if (*penname)
    {				/* there were two arguments */
      if (!(paper = get_obj_in_list_vis (ch, papername, ch->carrying)))
	{
	  sprintf (buf, "You have no %s.\r\n", papername);
	  send_to_char (buf, ch);
	  return;
	}
      if (!(pen = get_obj_in_list_vis (ch, penname, ch->carrying)))
	{
	  sprintf (buf, "You have no %s.\r\n", penname);
	  send_to_char (buf, ch);
	  return;
	}
    }
  else
    {				/* there was one arg.. let's see what we can find */
      if (!(paper = get_obj_in_list_vis (ch, papername, ch->carrying)))
	{
	  sprintf (buf, "There is no %s in your inventory.\r\n", papername);
	  send_to_char (buf, ch);
	  return;
	}
      if (GET_OBJ_TYPE (paper) == ITEM_PEN)
	{			/* oops, a pen.. */
	  pen = paper;
	  paper = 0;
	}
      else if (GET_OBJ_TYPE (paper) != ITEM_NOTE)
	{
	  send_to_char ("That thing has nothing to do with writing.\r\n", ch);
	  return;
	}
      /* One object was found.. now for the other one. */
      if (!GET_EQ (ch, WEAR_HOLD))
	{
	  sprintf (buf, "You can't write with %s %s alone.\r\n", AN (papername),
		   papername);
	  send_to_char (buf, ch);
	  return;
	}
      if (!CAN_SEE_OBJ (ch, GET_EQ (ch, WEAR_HOLD)))
	{
	  send_to_char ("The stuff in your hand is invisible!  Yeech!!\r\n", ch);
	  return;
	}
      if (pen)
	paper = GET_EQ (ch, WEAR_HOLD);
      else
	pen = GET_EQ (ch, WEAR_HOLD);
    }


  /* ok.. now let's see what kind of stuff we've found */
  if (GET_OBJ_TYPE (pen) != ITEM_PEN)
    act ("$p is no good for writing with.", FALSE, ch, pen, 0, TO_CHAR);
  else if (GET_OBJ_TYPE (paper) != ITEM_NOTE)
    act ("You can't write on $p.", FALSE, ch, paper, 0, TO_CHAR);
  else if (paper->action_description)
    send_to_char ("There's something written on it already.\r\n", ch);
  else
    {
      /* we can write - hooray! */
      /* this is the PERFECT code example of how to set up:
       * a) the text editor with a message already loaed
       * b) the abort buffer if the player aborts the message
       */
      ch->desc->backstr = NULL;
      send_to_char ("Write your note.  (/s saves /h for help)\r\n", ch);
      /* ok, here we check for a message ALREADY on the paper */
      if (paper->action_description)
	{
	  /* we str_dup the original text to the descriptors->backstr */
	  ch->desc->backstr = str_dup (paper->action_description);
	  /* send to the player what was on the paper (cause this is already */
	  /* loaded into the editor) */
	  send_to_char (paper->action_description, ch);
	}
      act ("$n begins to jot down a note.", TRUE, ch, 0, 0, TO_ROOM);
      /* assign the descriptor's->str the value of the pointer to the text */
      /* pointer so that we can reallocate as needed (hopefully that made */
      /* sense :>) */
      ch->desc->str = &paper->action_description;
      ch->desc->max_str = MAX_NOTE_LENGTH;
      ch->desc->note = paper;
      STATE(ch->desc)=CON_TEXTED;
    }
}



ACMD (do_page)
{
  struct descriptor_data *d;
  struct char_data *vict;

  half_chop (argument, arg, buf2);

  if (IS_NPC (ch))
    send_to_char ("Monsters can't page.. go away.\r\n", ch);
  else if (!*arg)
    send_to_char ("Whom do you wish to page?\r\n", ch);
  else
    {
      sprintf (buf, "\007\007*%s* %s\r\n", GET_NAME (ch), buf2);
      if (!str_cmp (arg, "all"))
	{
	  if (GET_LEVEL (ch) > LVL_GOD)
	    {
	      for (d = descriptor_list; d; d = d->next)
		if (!d->connected && d->character)
		  act (buf, FALSE, ch, 0, d->character, TO_VICT);
	    }
	  else
	    send_to_char ("You will never be godly enough to do that!\r\n", ch);
	  return;
	}
      if ((vict = get_char_vis (ch, arg)) != NULL)
	{
	  act (buf, FALSE, ch, 0, vict, TO_VICT);
	  if (PRF_FLAGGED (ch, PRF_NOREPEAT))
	    send_to_char (OK, ch);
	  else
	    act (buf, FALSE, ch, 0, vict, TO_CHAR);
	  return;
	}
      else
	send_to_char ("There is no such person in the game!\r\n", ch);
    }
}


/**********************************************************************
 * generalized communication func, originally by Fred C. Merkel (Torg) *
  *********************************************************************/

ACMD (do_gen_comm)
{
  extern int level_can_shout;
  extern int holler_move_cost;
  struct descriptor_data *i;
  char color_on[24];
  byte gmote = FALSE;

  /* Array of flags which must _not_ be set in order for comm to be heard */
  static int channels[] =
  {
    0,
    PRF_DEAF,
    PRF_NOGOSS,
    PRF_NOAUCT,
    PRF_NOGRATZ,
    0,
    PRF_NOARENA,
    0
  };

  /*
   * com_msgs: [0] Message if you can't perform the action because of noshout
   *           [1] name of the action
   *           [2] message if you're not on the channel
   *           [3] a color string.
   */
  static char *com_msgs[][4] =
  {
    {"You cannot holler!!\r\n",
     "holler",
     "",
     KYEL},

    {"You cannot shout!!\r\n",
     "shout",
     "Turn off your noshout flag first!\r\n",
     KYEL},

    {"You cannot gossip!!\r\n",
     "GOSSIP",
     "You aren't even on the channel!\r\n",
     KYEL},

    {"You cannot auction!!\r\n",
     "AUCTION",
     "You aren't even on the channel!\r\n",
     KMAG},

    {"You cannot congratulate!\r\n",
     "CONGRAT",
     "You aren't even on the channel!\r\n",
     KGRN},

    {"You cannot gemote!\r\n",
     "gemote",
     "You aren't even on the channel!\r\n",
     KGRN},

    {"You cannot arena?!\r\n",
     "ARENA",
     "You aren't even on the channel!\r\n",
     KYEL}
  };

  ACMD(do_gmote);

  /* to keep pets, etc from being ordered to shout */
  if (!ch->desc && cmd == 0)
    return;

if (IS_NPC(ch) && MOB_FLAGGED (ch, MOB_NOTALK)) {
      send_to_char ("You weren't designed to talk.\r\n", ch);
       return;
 }

  if (PLR_FLAGGED (ch, PLR_NOSHOUT))
    {
      send_to_char (com_msgs[subcmd][0], ch);
      return;
    }
  if (ROOM_FLAGGED (ch->in_room, ROOM_SOUNDPROOF))
    {
      send_to_char ("The walls seem to absorb your words.\r\n", ch);
      return;
    }
  /* level_can_shout defined in config.c */
  if (GET_LEVEL (ch) < level_can_shout)
    {
      sprintf (buf1, "You must be at least level %d before you can %s.\r\n",
	       level_can_shout, com_msgs[subcmd][1]);
      send_to_char (buf1, ch);
      return;
    }
  /* make sure the char is on the channel */
  if (PRF_FLAGGED (ch, channels[subcmd]))
    {
      send_to_char (com_msgs[subcmd][2], ch);
      return;
    }
  /* skip leading spaces */
  skip_spaces (&argument);

  if(subcmd == SCMD_GMOTE || (subcmd == SCMD_GOSSIP && *argument == '@'))
    {
      subcmd = SCMD_GOSSIP;
      gmote = TRUE;  
    }

  /* make sure that there is something there to say! */
  if (!*argument)
    {
      sprintf (buf1, "Yes, %s, fine, %s we must, but WHAT???\r\n",
	       com_msgs[subcmd][1], com_msgs[subcmd][1]);
      send_to_char (buf1, ch);
      return;
    }

  if (subcmd == SCMD_HOLLER)
    {
      if (GET_MOVE (ch) < holler_move_cost)
	{
	  send_to_char ("You're too exhausted to holler.\r\n", ch);
	  return;
	}
      else
	GET_MOVE (ch) -= holler_move_cost;
    }
  if (gmote) {
    if (*argument == '@')
      do_gmote(ch, argument + 1, 0, 1);
    else
      do_gmote(ch, argument, 0, 1);
    return;
  }
  /* Adding in drunk text here */
  argument = makedrunk(argument,ch);

  /* set up the color on code */
  strcpy (color_on, com_msgs[subcmd][3]);

  /* first, set up strings to be given to the communicator */
  if (PRF_FLAGGED (ch, PRF_NOREPEAT))
    send_to_char (OK, ch);
  else
    {
      if (subcmd == SCMD_SHOUT || subcmd == SCMD_HOLLER)
	{
	  if (COLOR_LEV (ch) >= C_CMP)
	    sprintf (buf1, "%sYou %s, '%s'%s", color_on, com_msgs[subcmd][1],
		     argument, KNRM);
	  else if (subcmd == SCMD_SHOUT || subcmd == SCMD_HOLLER)
	    sprintf (buf1, "You %s, '%s'", com_msgs[subcmd][1], argument);
	  act (buf1, FALSE, ch, 0, 0, TO_CHAR | TO_SLEEP);
	} else {
	  sprintf (buf1, "&c[&Y%s&c]&n $n: %s", com_msgs[subcmd][1], argument);
	  act (buf1, FALSE, ch, 0, 0, TO_CHAR | TO_SLEEP);
	}
    }
  if (subcmd == SCMD_SHOUT || subcmd == SCMD_HOLLER)
    sprintf (buf, "$n %ss, '%s'", com_msgs[subcmd][1], argument);
  else
    sprintf (buf, "&c[&Y%s&c]&n $n: %s", com_msgs[subcmd][1], argument);
  /* now send all the strings out */
  for (i = descriptor_list; i; i = i->next)
    {
      if (!i->connected && i != ch->desc && i->character &&
	  !PRF_FLAGGED (i->character, channels[subcmd]) &&
	  !PLR_FLAGGED (i->character, PLR_WRITING) &&
	  !ROOM_FLAGGED (i->character->in_room, ROOM_SOUNDPROOF) &&
          !isignore(i->character, ch, IGNORE_PUBLIC))
	{

	  if (subcmd == SCMD_SHOUT &&
	  ((world[ch->in_room].zone != world[i->character->in_room].zone) ||
	   GET_POS (i->character) < POS_RESTING))
	    continue;

	  if (COLOR_LEV (i->character) >= C_NRM)
	    send_to_char (color_on, i->character);
	  act (buf, FALSE, ch, 0, i->character, TO_VICT | TO_SLEEP);
	  if (COLOR_LEV (i->character) >= C_NRM)
	    send_to_char (KNRM, i->character);
	}
    }
}


ACMD (do_qcomm)
{
  struct descriptor_data *i;

  if (!PRF2_FLAGGED (ch, PRF2_QCHAN))
    {
      send_to_char ("You aren't even part of the quest channel!\r\n", ch);
      return;
    }
  skip_spaces (&argument);

  if (!*argument)
    {
      sprintf (buf, "%s?  Yes, fine, %s we must, but WHAT??\r\n", CMD_NAME,
	       CMD_NAME);
      CAP (buf);
      send_to_char (buf, ch);
    }
  else
    {
      argument = makedrunk(argument, ch);
      if (PRF_FLAGGED (ch, PRF_NOREPEAT))
	send_to_char (OK, ch);
      else
	{
	  if (subcmd == SCMD_QSAY)
	    sprintf (buf, "You quest-say, '%s'", argument);
	  else
	    strcpy (buf, argument);
	  act (buf, FALSE, ch, 0, argument, TO_CHAR);
	}

      if (subcmd == SCMD_QSAY)
	sprintf (buf, "$n quest-says, '%s'", argument);
      else
	strcpy (buf, argument);

      for (i = descriptor_list; i; i = i->next)
	if (!i->connected && i != ch->desc &&
	    PRF2_FLAGGED (i->character, PRF2_QCHAN) && !isignore(i->character, ch, IGNORE_PUBLIC))
	  act (buf, 0, ch, 0, i->character, TO_VICT | TO_SLEEP);
    }
}
