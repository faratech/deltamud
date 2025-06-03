/* ************************************************************************
   *   File: utils.c                                       Part of CircleMUD *
   *  Usage: various internal functions of a utility nature                  *
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
#include "screen.h"
#include "spells.h"
#include "handler.h"

extern struct time_data time_info;
extern struct room_data *world;
extern int sunlight;
extern int stat_table[9][12];

/* local functions */
char *stripcr(char *dest, const char *src);

int GET_RACE_MIN(int race, int stat) { return (race >= 0 && race <= 8 && stat >= 1 && stat<= 6 ?
stat_table[race][stat*2-2] : 0); }
int GET_RACE_MAX(int race, int stat) { return (race >= 0 && race <= 8 && stat >= 1 && stat<= 6 ?
stat_table[race][stat*2-1] : 0); }

/* Creates a supercompact sine graph with an amplitude of 1 and a period of 1.
  Looks like:
1    x   x   x
0   x x x x x x x
-1     x   x   x
    1   2   3   4
*/
int roundf (float x) {
  float y=x;
  while (y>1) y-=1;
  while (y<0) y+=1;

  if (x<0) {
    if (y<-0.5) return x+y;
    else return x+y-1;
  }
  if (x>0) {
    if (y<0.5) return x-y;
    else return x-y+1;
  }
  return 0;
}
float freq_graph_y ( float x ) {
  float tempx=x, tempy;
  while (tempx > 1) tempx-=1;
  while (tempx < 0) tempx+=1;

  if (tempx <= 0.25)
    tempy=-4*((tempx-0.25)*(tempx-0.25));
  else if (tempx <= 0.50)
    tempy=-4*((0.25-tempx)*(0.25-tempx));
  else if (tempx<=1.00)
    return -freq_graph_y(tempx-0.50);
  else tempy=0;

  tempy*=4; // Vertical stretch by a factor of 4 to give us an amplitude of 1. Goes FIRST!!
  tempy+=1; // Shift it up, so we have 0 as our baseline.
  return tempy;
}

void biorythm(struct char_data *ch) {
  char graph[31][44];
  float x, tx, y;
  int i, j;
  int tn=number(0, 20);
  for (i=0; i < 31; i++) for (j=0; j < 43; j++) graph[i][j]=' ';
  for (i=0; i < 31; i++) graph[i][43]='\0';
  for (x=0; x<=1.00; x+=0.025) {
    tx=40*x;
    y=10-10*freq_graph_y(x);
    graph[y < 0 ? 0 : (y > 30 ? 30 : roundf(y))][roundf(tx)]= roundf(tx) == tn ? '+' : 'x';
  }
  for (j=0; j<31; j++) {
    send_to_char(graph[j], ch);
    send_to_char("\r\n", ch);
  }
}

/* What is the percent chance that ch will hit vict with a spell/skill or weapon? */
int chance (struct char_data *ch, struct char_data *vict, int type) {
  sh_int p;
  /* Type = 0 | Normal Attack 
     Type = 1 | Magical Attack 
     Return: 0 - Total faliure, no possibility of hit.
             50 - *Equal* 50/50 chance there is a hit.
             100 - Total success, no possibility of miss.
  */
  switch (type) {
    case 0:
      p=GET_TECHNIQUE(ch)-GET_TECHNIQUE(vict);
      p+=(GET_DEX(ch)-GET_DEX(vict))*10;
      break;
    case 1:
      p=GET_TECHNIQUE(ch)-GET_TECHNIQUE(vict);
      p+=(GET_INT(ch)-GET_WIS(vict))*10;
      break;
    default:
      return 0;
      break;
  }
  p/=10; //Just making our ranges LARGE, -1000 to 1000
  // p can range from -100 (fail) to 100 (success) normally
  p=(p+100)/2; // Percentage here.. 0% or 100%
  p = p < 0 ? 0 : (p > 100 ? 100 : p); // If some fool happens to be 150% better, cap it :P
  return p;
}

/* What is the damage multiplier for ch when ch hits vict? */
float dam_multi (struct char_data *ch, struct char_data *vict, int type) {
  float p;
  /* Type = 0 | Normal Attack 
     Type = 1 | Magical Attack
     A person with an offense of 100 attacking a person with defense of -100 */
  switch (type) {
    case 0:
      p=GET_POWER(ch)-GET_DEFENSE(vict);
      p+=(GET_STR(ch)-GET_CON(vict))*10;
      break;
    case 1:
      p=GET_MPOWER(ch)-GET_MDEFENSE(vict);
      p+=(GET_INT(ch)-GET_WIS(vict))*10;
      break;
    default:
      return 1;
      break;
  }
  p/=10; //Just making our ranges LARGE, -1000 to 1000
  if (p>=0)
    p=1+(2*p/100);
  else {
    p*=-1;
    p=1-(2/300*p);
  }
  /* In essence, here are the values:
                           |------Normal Ranges---------|
   Rating     : Better     |         Neutral            |      Worst
   Value(p)   : 200   150  |100   50    0     -50   -100| -150  -200
   Multiplier : 5     4    |3     2     1     0.6   0.3 | 0     -0.3
                           |----------------------------|

    p is how player 1 compares to player 2 (offense vs defense)
    multiplier is the damage multiplier they receive (this can
      go below 1 [normal] if player 1 is a weaker hitter than player
      2 is a defender, this will *never* go negative [as it does above]
      because of the sanitycheck below -- we don't want healing hits :P
  */
  return p < 0 ? 0 : p;
}

double power ( double num, int power ) {
  int i;
  double fnum=num;
  if (power==0) return 1;
  for (i=1; i<power; i++)
    fnum*=num;
  if (power<0) return (1/fnum);
  return fnum;
}

/* We don't need this anymore, look under 'sqrt' function below to find out why.

double ln ( double num ) {
  register double x=1, fnum=0;
  register int i;
     Natural logarithm is an infinite series, but since we can't
     calculate an irrational number with a rational limit (15
     digits as far as 'double' goes, we'll loop it 48 times for ((I chose 48 because it gave me the closest numbers I wanted when I was fine-tuning the program.))
     a little more accuracy.
     Natural logarithm 'by hand' is as follows:
     ln(x)=2[(x-1)/(x+1) + (1/3)((x-1)/(x+1))^3 
           + (1/5)((x-1)/(x+1))^5 + (1/7)((x...]
  for (i=1; i<=48; i++) {
    fnum+=(1/x)*power((num-1)/(num+1), x);
    x+=2;
  }
  return 2*fnum;
}

*/

double mlog (double base, double num) {
  double log10 (double num);
  /* Using our logarithmic rule: (x is the base, y in the num)
              log(y)
     log(x,y)=------
              log(x)
  */
  return (log10(num)/log10(base));
}

/* creates a random number in interval [from;to] */
int 
number (int from, int to)
{
  /* error checking in case people call number() incorrectly */
  if (from > to)
    {
      int tmp = from;
      from = to;
      to = tmp;
    }

  return ((circle_random () % (to - from + 1)) + from);
}


/* simulates dice roll */
int 
dice (int number, int size)
{
  int sum = 0;

  if (size <= 0 || number <= 0)
    return 0;
  while (number-- > 0)
    sum += ((circle_random() % size) + 1);

  return sum;
}


int 
MIN (int a, int b)
{
  return a < b ? a : b;
}


int 
MAX (int a, int b)
{
  return a > b ? a : b;
}



/* Create a duplicate of a string */
char *
str_dup (const char *source)
{
  char *new;
  if (!source) return NULL;
  CREATE (new, char, strlen (source) + 1);
  return (strcpy (new, source));
}



/* str_cmp: a case-insensitive version of strcmp */
/* returns: 0 if equal, 1 if arg1 > arg2, -1 if arg1 < arg2  */
/* scan 'till found different or end of both                 */
int 
str_cmp (const char *arg1, const char *arg2)
{
  int chk, i;

  for (i = 0; *(arg1 + i) || *(arg2 + i); i++)
    if ((chk = LOWER (*(arg1 + i)) - LOWER (*(arg2 + i)))) {
      if (chk < 0)
	return (-1);
      else
	return (1);
    }
  return (0);
}


/* strn_cmp: a case-insensitive version of strncmp */
/* returns: 0 if equal, 1 if arg1 > arg2, -1 if arg1 < arg2  */
/* scan 'till found different, end of both, or n reached     */
int 
strn_cmp (char *arg1, char *arg2, int n)
{
  int chk, i;

  for (i = 0; (*(arg1 + i) || *(arg2 + i)) && (n > 0); i++, n--)
    if ((chk = LOWER (*(arg1 + i)) - LOWER (*(arg2 + i)))) {
      if (chk < 0)
	return (-1);
      else
	return (1);
    }

  return (0);
}


/* log a death trap hit */
void 
log_death_trap (struct char_data *ch)
{
  char buf[150];
  extern struct room_data *world;

  sprintf (buf, "%s hit death trap #%d (%s)", GET_NAME (ch),
	   (int) world[ch->in_room].number, world[ch->in_room].name);
  mudlog (buf, BRF, LVL_IMMORT, TRUE);

// this was annoying, removed 8/23/98 --Mulder
//  sprintf (buf, "&m[&YINFO&m]&n %s was killed by a death trap.\r\n",
//          GET_NAME (ch));
//          send_to_all (buf);
}


/* writes a string to the log */
void 
log (char *str)
{
  time_t ct;
  char *tmstr;

  ct = time (0);
  tmstr = asctime (localtime (&ct));
  *(tmstr + strlen (tmstr) - 1) = '\0';
  fprintf (logfile, "%-15.15s :: %s\n", tmstr + 4, str);
  fflush (logfile);
}

/* the "touch" command, essentially. */
int 
touch (char *path)
{
  FILE *fl;

  if (!(fl = fopen (path, "a")))
    {
      perror (path);
      return -1;
    }
  else
    {
      fclose (fl);
      return 0;
    }
}


/*
 * mudlog -- log mud messages to a file & to online imm's syslogs
 * based on syslog by Fen Jul 3, 1992
 */
void 
mudlog (char *str, char type, int level, byte file)
{
  char buf[MAX_STRING_LENGTH];
  extern struct descriptor_data *descriptor_list;
  struct descriptor_data *i;
  char *tmp, tp;
  time_t ct;
  int j;

  ct = time (0);
  tmp = asctime (localtime (&ct));

  if (file)
    {
      fprintf (logfile, "%-15.15s :: %s\n", tmp + 4, str);
      fflush (logfile);
    }
  if (level < 0)
    return;

  // Strip off \r\n characters off the end of the string. Mudlog doesn't look right... -Storm
  tmp=str_dup(str);
  for (j=strlen(tmp)-1; tmp[j]=='\r' || tmp[j]=='\n'; j--)
    tmp[j]='\0';
  sprintf (buf, "[ %s ]\r\n", tmp);
  free(tmp);

  for (i = descriptor_list; i; i = i->next)
    if (!i->connected && !PLR_FLAGGED (i->character, PLR_WRITING))
      {
	tp = ((PRF_FLAGGED (i->character, PRF_LOG1) ? 1 : 0) +
	      (PRF_FLAGGED (i->character, PRF_LOG2) ? 2 : 0) +
	      (PRF_FLAGGED (i->character, PRF_LOG3) ? 4 : 0));
  
	if ((GET_LEVEL (i->character) >= level) && (tp >= type || (tp == 3 && strncmp(str, "Auto zone reset:", 16)) /* syslog perfect logs everything cept resets */))
	  {
	    send_to_char (CCGRN (i->character, C_NRM), i->character);
	    send_to_char (buf, i->character);
	    send_to_char (CCNRM (i->character, C_NRM), i->character);
	  }
      }
}



void sprintbit(long bitvector, const char *names[], char *result)
{
  long nr;

  *result = '\0';

  if (bitvector < 0) {
    strcpy(result, "<INVALID BITVECTOR>");
    return;
  }
  for (nr = 0; bitvector; bitvector >>= 1) {
    if (IS_SET(bitvector, 1)) {
      if (*names[nr] != '\n') {
        strcat(result, names[nr]);
        strcat(result, " ");
      } else
        strcat(result, "UNDEFINED ");
    }
    if (*names[nr] != '\n')
      nr++;
  }
    
  if (!*result)
    strcpy(result, "NOBITS ");
}
void sprinttype(int type, const char *names[], char *result)
{
  int nr = 0;
     
  while (type && *names[nr] != '\n') {
    type--;
    nr++;
  } 
  
  if (*names[nr] != '\n')
    strcpy(result, names[nr]);
  else
    strcpy(result, "UNDEFINED");
}       


/* Calculate the REAL time passed over the last t2-t1 centuries (secs) */
struct time_info_data 
real_time_passed (time_t t2, time_t t1)
{
  long secs;
  struct time_info_data now;

  secs = (long) (t2 - t1);

  now.hours = (secs / SECS_PER_REAL_HOUR) % 24;		/* 0..23 hours */
  secs -= SECS_PER_REAL_HOUR * now.hours;

  now.day = (secs / SECS_PER_REAL_DAY);		/* 0..34 days  */
  secs -= SECS_PER_REAL_DAY * now.day;

  now.month = -1;
  now.year = -1;

  return now;
}



/* Calculate the MUD time passed over the last t2-t1 centuries (secs) */
struct time_info_data 
mud_time_passed (time_t t2, time_t t1)
{
  long secs;
  struct time_info_data now;

  secs = (long) (t2 - t1);

  now.hours = (secs / SECS_PER_MUD_HOUR) % 24;	/* 0..23 hours */
  secs -= SECS_PER_MUD_HOUR * now.hours;

  now.day = (secs / SECS_PER_MUD_DAY) % 35;	/* 0..34 days  */
  secs -= SECS_PER_MUD_DAY * now.day;

  now.month = (secs / SECS_PER_MUD_MONTH) % 17;		/* 0..16 months */
  secs -= SECS_PER_MUD_MONTH * now.month;

  now.year = (secs / SECS_PER_MUD_YEAR);	/* 0..XX? years */

  return now;
}



struct time_info_data 
age (struct char_data *ch)
{
  struct time_info_data player_age;

  player_age = mud_time_passed (time (0), ch->player.time.birth);

  player_age.year += 17;	/* All players start at 17 */

  return player_age;
}


/* Check if making CH follow VICTIM will create an illegal */
/* Follow "Loop/circle"                                    */
bool 
circle_follow (struct char_data * ch, struct char_data * victim)
{
  struct char_data *k;

  for (k = victim; k; k = k->master)
    {
      if (k == ch)
	return TRUE;
    }

  return FALSE;
}



/* Called when stop following persons, or stopping charm */
/* This will NOT do if a character quits/dies!!          */
void 
stop_follower (struct char_data *ch)
{
  struct follow_type *j, *k;

  assert (ch->master);

  if (IS_AFFECTED (ch, AFF_CHARM))
    {
      act ("You realize that $N is a jerk!", FALSE, ch, 0, ch->master, TO_CHAR);
      act ("$n realizes that $N is a jerk!", FALSE, ch, 0, ch->master, TO_NOTVICT);
      act ("$n hates your guts!", FALSE, ch, 0, ch->master, TO_VICT);
      if (affected_by_spell (ch, SPELL_CHARM))
	affect_from_char (ch, SPELL_CHARM);
    }
  else
    {
      act ("You stop following $N.", FALSE, ch, 0, ch->master, TO_CHAR);
      act ("$n stops following $N.", TRUE, ch, 0, ch->master, TO_NOTVICT);
      act ("$n stops following you.", TRUE, ch, 0, ch->master, TO_VICT);
    }

  if (ch->master->followers->follower == ch)
    {				/* Head of follower-list? */
      k = ch->master->followers;
      ch->master->followers = k->next;
      free (k);
    }
  else
    {				/* locate follower who is not head of list */
      for (k = ch->master->followers; k->next->follower != ch; k = k->next);

      j = k->next;
      k->next = j->next;
      free (j);
    }

  ch->master = NULL;
  REMOVE_BIT (AFF_FLAGS (ch), AFF_CHARM | AFF_GROUP);
}



/* Called when a character that follows/is followed dies */
void 
die_follower (struct char_data *ch)
{
  struct follow_type *j, *k;

  if (ch->master)
    stop_follower (ch);

  for (k = ch->followers; k; k = j)
    {
      j = k->next;
      stop_follower (k->follower);
    }
}



/* Do NOT call this before having checked if a circle of followers */
/* will arise. CH will follow leader                               */
void 
add_follower (struct char_data *ch, struct char_data *leader)
{
  struct follow_type *k;

  assert (!ch->master);

  ch->master = leader;

  CREATE (k, struct follow_type, 1);

  k->follower = ch;
  k->next = leader->followers;
  leader->followers = k;

  act ("You now follow $N.", FALSE, ch, 0, leader, TO_CHAR);
  if (CAN_SEE (leader, ch))
    act ("$n starts following you.", TRUE, ch, 0, leader, TO_VICT);
  act ("$n starts to follow $N.", TRUE, ch, 0, leader, TO_NOTVICT);
}

/*
 * get_line reads the next non-blank line off of the input stream.
 * The newline character is removed from the input.  Lines which begin
 * with '*' are considered to be comments.
 *
 * Returns the number of lines advanced in the file.
 */
int 
get_line (FILE * fl, char *buf)
{
  char temp[256];
  int lines = 0;

  do
    {
      lines++;
      fgets (temp, 256, fl);
      if (*temp)
	temp[strlen (temp) - 1] = '\0';
    }
  while (!feof (fl) && (*temp == '*' || !*temp));

  if (feof (fl))
    return 0;
  else
    {
      strcpy (buf, temp);
      return lines;
    }
}


int 
get_filename (char *orig_name, char *filename, int mode)
{
  char *prefix, *middle, *suffix, *ptr, name[64];

  switch (mode)
    {
    case CRASH_FILE:
      prefix = "plrobjs";
      suffix = "objs";
      break;
    case ETEXT_FILE:
      prefix = "plrtext";
      suffix = "text";
      break;
    case ALIAS_FILE:
      prefix = "plralias";
      suffix = "alias"; 
      break;
    case EDATA_FILE:
      prefix = "plredata";
      suffix = "data";
      break;
    default:
      return 0;
      break;
    }

  if (!*orig_name)
    return 0;

  strcpy (name, orig_name);
  for (ptr = name; *ptr; ptr++)
    *ptr = LOWER (*ptr);

  switch (LOWER (*name))
    {
    case 'a':
    case 'b':
    case 'c':
    case 'd':
    case 'e':
      middle = "A-E";
      break;
    case 'f':
    case 'g':
    case 'h':
    case 'i':
    case 'j':
      middle = "F-J";
      break;
    case 'k':
    case 'l':
    case 'm':
    case 'n':
    case 'o':
      middle = "K-O";
      break;
    case 'p':
    case 'q':
    case 'r':
    case 's':
    case 't':
      middle = "P-T";
      break;
    case 'u':
    case 'v':
    case 'w':
    case 'x':
    case 'y':
    case 'z':
      middle = "U-Z";
      break;
    default:
      middle = "ZZZ";
      break;
    }

  sprintf (filename, "%s/%s/%s.%s", prefix, middle, name, suffix);
  return 1;
}


int 
num_pc_in_room (struct room_data *room)
{
  int i = 0;
  struct char_data *ch;

  for (ch = room->people; ch != NULL; ch = ch->next_in_room)
    if (!IS_NPC (ch))
      i++;

  return i;
}


/* string manipulation fucntion originally by Darren Wilson */
/* (wilson@shark.cc.cc.ca.us) improved and bug fixed by Chris (zero@cnw.com) */
/* completely re-written again by M. Scott 10/15/96 (scottm@workcommn.net), */
/* substitute appearances of 'pattern' with 'replacement' in string */
/* and return the # of replacements */
int 
replace_str (char **string, char *pattern, char *replacement, int rep_all,
	     int max_size)
{
  char *replace_buffer = NULL;
  char *flow, *jetsam, temp;
  int len, i;

  if ((strlen (*string) - strlen (pattern)) + strlen (replacement) > max_size)
    return -1;

  CREATE (replace_buffer, char, max_size);
  i = 0;
  jetsam = *string;
  flow = *string;
  *replace_buffer = '\0';
  if (rep_all)
    {
      while ((flow = (char *) strstr (flow, pattern)) != NULL)
	{
	  i++;
	  temp = *flow;
	  *flow = '\0';
	  if ((strlen (replace_buffer) + strlen (jetsam) + strlen (replacement)) > max_size)
	    {
	      i = -1;
	      break;
	    }
	  strcat (replace_buffer, jetsam);
	  strcat (replace_buffer, replacement);
	  *flow = temp;
	  flow += strlen (pattern);
	  jetsam = flow;
	}
      strcat (replace_buffer, jetsam);
    }
  else
    {
      if ((flow = (char *) strstr (*string, pattern)) != NULL)
	{
	  i++;
	  flow += strlen (pattern);
	  len = ((char *) flow - (char *) *string) - strlen (pattern);

	  strncpy (replace_buffer, *string, len);
	  strcat (replace_buffer, replacement);
	  strcat (replace_buffer, flow);
	}
    }
  if (i == 0)
    return 0;
  if (i > 0)
    {
      RECREATE (*string, char, strlen (replace_buffer) + 3);
      strcpy (*string, replace_buffer);
    }
  free (replace_buffer);
  return i;
}


/* re-formats message type formatted char * */
/* (for strings edited with d->str) (mostly olc and mail)     */
void 
format_text (char **ptr_string, int mode, struct descriptor_data *d, int maxlen)
{
  int total_chars, cap_next = TRUE, cap_next_next = FALSE;
  char *flow, *start = NULL, temp;
  /* warning: do not edit messages with max_str's of over this value */
  char formated[MAX_STRING_LENGTH];

  flow = *ptr_string;
  if (!flow)
    return;

  if (IS_SET (mode, FORMAT_INDENT))
    {
      strcpy (formated, "   ");
      total_chars = 3;
    }
  else
    {
      *formated = '\0';
      total_chars = 0;
    }

  while (*flow != '\0')
    {
      while ((*flow == '\n') ||
	     (*flow == '\r') ||
	     (*flow == '\f') ||
	     (*flow == '\t') ||
	     (*flow == '\v') ||
	     (*flow == ' '))
	flow++;

      if (*flow != '\0')
	{

	  start = flow++;
	  while ((*flow != '\0') &&
		 (*flow != '\n') &&
		 (*flow != '\r') &&
		 (*flow != '\f') &&
		 (*flow != '\t') &&
		 (*flow != '\v') &&
		 (*flow != ' ') &&
		 (*flow != '.') &&
		 (*flow != '?') &&
		 (*flow != '!'))
	    flow++;

	  if (cap_next_next)
	    {
	      cap_next_next = FALSE;
	      cap_next = TRUE;
	    }

	  /* this is so that if we stopped on a sentance .. we move off the sentance delim. */
	  while ((*flow == '.') || (*flow == '!') || (*flow == '?'))
	    {
	      cap_next_next = TRUE;
	      flow++;
	    }

	  temp = *flow;
	  *flow = '\0';

	  if ((total_chars + strlen (start) + 1) > 79)
	    {
	      strcat (formated, "\r\n");
	      total_chars = 0;
	    }

	  if (!cap_next)
	    {
	      if (total_chars > 0)
		{
		  strcat (formated, " ");
		  total_chars++;
		}
	    }
	  else
	    {
	      cap_next = FALSE;
	      *start = UPPER (*start);
	    }

	  total_chars += strlen (start);
	  strcat (formated, start);

	  *flow = temp;
	}

      if (cap_next_next)
	{
	  if ((total_chars + 3) > 79)
	    {
	      strcat (formated, "\r\n");
	      total_chars = 0;
	    }
	  else
	    {
	      strcat (formated, "  ");
	      total_chars += 2;
	    }
	}
    }
  strcat (formated, "\r\n");

  if (strlen (formated) > maxlen)
    formated[maxlen] = '\0';
  RECREATE (*ptr_string, char, MIN (maxlen, strlen (formated) + 3));
  strcpy (*ptr_string, formated);
}
int charge_less_rent(struct char_data *ch)
{
int lessrent;

lessrent = GET_LEVEL(ch) * 500;

if (GET_LEVEL(ch) < LVL_IMMORT)
  return lessrent;
else
  return 100000000;
}

/*quicksort: sort v[left] to v[right] into INCREASING order. */
/* Implemented by Thargor, based on Kernighan & Ritchie's "The C Language",
   considered by all the C bible. 
   This version has been modified to custom fit the static data structure
   used (struct who_list).
   NOTE: This is a recursive function. Do not edit unless you understand
   recursive functions.
   */
void quicksort(void *v[], int left, int right, 
	   int (*comp)(int, int))
{
  int i, last;
  void swap(void *v[], int, int);

  if (left >= right)  /* Do nothing if array contains fewer than 2 elements */
    return; 

  swap(v, left, (left+right)/2);
  last = left;

  for (i = left+1; i <= right; i++)
    /* if ((*comp)(v[i], v[left]) < 0) */
    if ((*comp)((((struct who_list *) v)+i)->level,
		(((struct who_list *) v)+left)->level) < 0)
      swap(v, ++last, i);
  swap(v, left, last);

  quicksort (v, left, last-1, comp);
  quicksort (v, last+1, right, comp);
} /* end quicksort */

void swap(void *v[], int i, int j)
{
  struct who_list temp;

  if (i == j)
    return;

  bzero(temp.desc, SMALL_BUFSIZE);
  strcpy(temp.desc, (((struct who_list *) v)+i)->desc);
  temp.level =  (((struct who_list *) v)+i)->level;

  bzero((((struct who_list *) v)+i)->desc, SMALL_BUFSIZE);
  strcpy((((struct who_list *) v)+i)->desc, 
	 (((struct who_list *) v)+j)->desc);
  (((struct who_list *) v)+i)->level = (((struct who_list *) v)+j)->level;

  bzero((((struct who_list *) v)+j)->desc, SMALL_BUFSIZE);
  strcpy((((struct who_list *) v)+j)->desc, temp.desc);
  (((struct who_list *) v)+j)->level = temp.level;

/*
  temp = v[i];
  v[i] = v[j];
  v[j] = temp;
  */
}


char* numdisplay(int val)
{
  /* Important note:
   * Since a static char is used, do not call this function more than once
   * per (s)printf. Instead break up your (s)printfs for each call to this
   * function.
   */
  static char dispstr[80];
  char temp1[80], temp2[80];
  int i, j, k;

  bzero(temp1, 80);
  bzero(temp2, 80);
  bzero(dispstr, 80);

  sprintf(temp1, "%d", val);
  j = 0; k = 0;
  for (i = (strlen(temp1)-1); i >= 0; i--){
    temp2[j++] = temp1[i];
    if (++k >= 3){
      k = 0;
      if (i > 0 && temp1[(i-1)] != '-')
	temp2[j++] = ',';
    }
  }
  k = 0;
  for (i = (j-1); i >=0 ; i--)
    dispstr[k++] = temp2[i];

  return dispstr;
}

/* Arena mod - Thargor */
/* Remove who from the linked list of observers of an arena combatant */
void deobserve(struct char_data *who)
{
  struct char_data *curr, *prev, *next, *obswho;

  obswho = OBSERVING(who);
  if (obswho == NULL || GET_ARENASTAT(who) != ARENA_OBSERVER)
    return;

  curr = OBSERVING(who);
  prev = curr;
  while (!(curr == NULL || curr == who)){
    next = OBSERVE_BY(curr);
    if (curr != who){
      prev = curr;
      curr = next;
    }
  } 
  if (curr == who){
    next = OBSERVE_BY(curr);
    OBSERVE_BY(prev) = next;
    OBSERVE_BY(curr) = NULL;
  }
  OBSERVING(who) = NULL;
}

/* Arena mod - Thargor */
/* link who to the linked list of observers of an arena combatant 'to'*/
void linkobserve(struct char_data *who, struct char_data *to)
{
  struct char_data *curr;

  curr = to;
  while (OBSERVE_BY(curr))
    curr = OBSERVE_BY(curr);

  OBSERVING(who) = to;
  OBSERVE_BY(curr) = who;
  OBSERVE_BY(who) = NULL;
  
}

/* Arena mod - Thargor */
/* Clear off all observers of a player (who) who's left the arena */
void clearobservers(struct char_data *who)
{
  struct char_data *tmp, *clear;
 
  if (GET_ARENASTAT(who) == ARENA_NOT || GET_ARENASTAT(who) == ARENA_OBSERVER)
    return;

  tmp = who;
  while (tmp != NULL){
    clear = tmp;
    tmp = OBSERVE_BY(clear);
    OBSERVING(clear) = NULL;
    OBSERVE_BY(clear) = NULL;
  }

}
int exp_to_level(arg)
{
  double modifier;
  if (arg < 1 || arg > LVL_IMPL) return 0;
  if (arg < 10)
    modifier = mlog(3,arg);
  else if (arg < 20)
    modifier = mlog(2.9,arg);
  else if (arg < 30)
    modifier = mlog(2.8,arg);
  else if (arg < 40)
    modifier = mlog(2.7,arg);
  else if (arg < 50)
    modifier = mlog(2.6,arg);
  else if (arg < 60)
    modifier = mlog(2.5,arg);
  else if (arg < 70)
    modifier = mlog(2.4,arg);
  else if (arg < 80)
    modifier = mlog(2.3,arg);
  else if (arg < 90)
    modifier = mlog(2.2,arg);
  else if (arg < 96)
    modifier = mlog(2.05,arg);
  else
    modifier = mlog(2,arg);

  if (arg == 1)
    return 3000;
  else
    return 4000 * (arg * modifier)+exp_to_level(arg-1); // was gonna do +exp_to_level(arg-1) but that makes it REAL cpu intense

/*
  if (arg < 10)
    modifier = 1;
  else if (arg < 20)
    modifier = 5;
  else if (arg < 30)
    modifier = 10;
  else if (arg < 40)
    modifier = 20;  
  else if (arg < 50)
    modifier = 30;
  else if (arg < 60)
    modifier = 40;
  else if (arg < 70)
    modifier = 50;
  else if (arg < 80)
    modifier = 60;  
  else if (arg < 90)
    modifier = 80;
  else
    modifier = 250;

 exp = arg * modifier * 4000;
 return exp;
*/
}
/* strips \r's from line */
char *stripcr(char *dest, const char *src) {
  int i, length;
  char *temp;

  if (!dest || !src) return NULL;
  temp = &dest[0];
  length = strlen(src);
  for (i = 0; *src && (i < length); i++, src++)
    if (*src != '\r') *(temp++) = *src;
      *temp = '\0';
  return dest;
}
/* Will return the number of items in the container. */
int count_contents(struct obj_data *container)
{
  int count = 0;
  struct obj_data *obj;

  if (container->contains)
    for (obj = container->contains; obj; obj = obj->next_content, count++)
      if (GET_OBJ_TYPE(obj) == ITEM_CONTAINER && obj->contains)
        count += count_contents(obj);

  return(count);
}

/* Will return the number of items that the character owns. */
int item_count(struct char_data *ch)
{
  int i, count = 0;
  struct obj_data *obj;

  for (i = 0; i < NUM_WEARS; i++) {
    if (GET_EQ(ch, i)) {
      count++;
      if (GET_OBJ_TYPE(GET_EQ(ch, i)) == ITEM_CONTAINER &&
          GET_EQ(ch, i)->contains)
        count += count_contents(GET_EQ(ch, i));
    }
  }

  if (ch->carrying)
    for (obj = ch->carrying; obj; obj = obj->next_content, count++)
      if (GET_OBJ_TYPE(obj) == ITEM_CONTAINER && obj->contains)
        count += count_contents(obj);

  return(count);
}

void search_replace(char *string, const char *find, const char *replace)
{
        char final[MAX_INPUT_LENGTH], temp[2];
        size_t start, end, i;

        while (strstr(string, find) != NULL) {

                final[0] = '\0';
                start = strstr(string, find) - string;
                end = start + strlen(find);

                temp[1] = '\0';

                strncat(final, string, start);
                
                strcat(final, replace);

                for (i = end; string[i] != '\0'; i++) {
                        temp[0] = string[i];
                        strcat(final, temp);
                }

                sprintf(string, final);

        }
        return;
}

/* OLD Square Root Function
   Heh, I handcoded this when I couldn't get the system SQRT function
   working -- this works fine on a relatively fast system (300MHz+ I
   would guess), but on slower ones it's a turtle when called many
   times.

   I found out that you need to link libm.a (-lm) to the object files
   for math.h's sqrt function to work.
   -Storm

  double sqrt ( double num ) {
    register double i, ci;

    if (!num) return 0;
    i=num/2;
    if (i==1) i+=0.1;
    ci=i;

    while (1) {
      i=num/i;
      i=(ci+i)/2;
      ci=i;
      if (i*i==num) return i;
    }
  }
*/

sh_int a2i(char *vnum)
{
  int i, k, l, n=1;
  sh_int num=0, j;
  l=strlen(vnum);
  i=0;
  if (vnum[0]=='-') {
    n=-1;
    i=1;
  }

  for (; i<strlen(vnum); i++) {
    if ('0'==vnum[i]) j=0;
    else if ('1'==vnum[i]) j=1;
    else if ('2'==vnum[i]) j=2;
    else if ('3'==vnum[i]) j=3;
    else if ('4'==vnum[i]) j=4;
    else if ('5'==vnum[i]) j=5;
    else if ('6'==vnum[i]) j=6;
    else if ('7'==vnum[i]) j=7;
    else if ('8'==vnum[i]) j=8;
    else if ('9'==vnum[i]) j=9;
    else continue;
    for (k=1; k<l; k++)
      j*=10;
    num+=j;
    l--;
  }
  return num*n;
}
