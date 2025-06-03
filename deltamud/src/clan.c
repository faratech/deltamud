/***************************************************
** File: Clan.c  - Written for CircleMUD 3.0      **
** Usage: Functionality of clan code                      **
** Written by Chuck Reed for use on Dark Horizon  **
** and QuarantineMUD. (SEE NOTE BELOW FOR BASE)   **
****************************************************
** Anyone may use, modify this code for their mud **
** as long as proper credit is given.             **
**                             -Chuck             **
***************************************************/

/* This code is greatly inspired by the clan code 
   written by Mehdi Keddache who deserves a great
   deal of thanks for the ideas behind this code's
   creation. */

#include "conf.h"
#include "sysdep.h"

#include "structs.h"
#include "utils.h"
#include "db.h"
#include "comm.h"
#include "interpreter.h"
#include "handler.h"
#include "clan.h"
#include "dbinterface.h"

extern char *class_abbrevs[];
int num_of_clans=0;
extern struct room_data *world;
struct clan_info clan[MAX_CLANS];
extern struct player_index_element *player_table;

char *noclan = "You don't even belong to a clan!\r\n";

// Re-written as to not destroy player names in the player table
struct char_data *is_playing(char *vict_name) 
{
  extern struct descriptor_data *descriptor_list;
  struct descriptor_data *i, *next_i;
  
  for (i = descriptor_list; i; i = next_i) {    
    next_i = i->next;
    // Changed to use str_cmp to recognize players with caps in their name (ie, ChuckReed)
    if(i->connected == CON_PLAYING && !str_cmp(i->character->player.name, vict_name))
      return i->character;
     }
  return NULL;
}

void save_clans(void)
{
   FILE *f;
   
   if(!(f = fopen(CLAN_FILE, "wb"))) {
      log("SYSERR: Error writing to clan file!");
      return;
     }
   
   fwrite(&num_of_clans, sizeof(int), 1, f);
   fwrite(clan, sizeof(struct clan_info), num_of_clans, f);
   fclose(f);
   return;
}

void boot_clans(void)
{
   int mclan,mclan_rank,j;
   FILE *f;
   extern int top_of_p_table; 
   char temp_name[MAX_INPUT_LENGTH];   

   memset(clan,0,sizeof(struct clan_info)*MAX_CLANS);
   num_of_clans=0;
   
   log("Booting clans . . .");
   if(!(f = fopen(CLAN_FILE, "rb"))) {
      log("Clan file doesn't exist, a new one will be created.");
      save_clans();
      return;
     }
   
   fread(&num_of_clans, sizeof(int), 1, f);
   fread(clan, sizeof(struct clan_info), num_of_clans, f);
   fclose(f);
			  
   // Initialize members to zero for all clans                       
   for(j=0;j < num_of_clans;j++) 
      clan[j].members = 0;                   
      
   // Now calculate members each time clans boot up
   for (j = 0; j <= top_of_p_table; j++) {
      strcpy(temp_name, (player_table + j)->name);
      pe_printf(temp_name, "nn", "clan,clan_rank", &mclan, &mclan_rank); /* Normally we would select all the players at once and cycle through the result set, but boot_clans is only called once and I'm lazy... so blah. */
      if(mclan>=0 && mclan_rank != -1) {
         if (mclan>=0 && mclan<num_of_clans)
           clan[mclan].members++;
      }
     }
   log("Done.");
}

long find_clan(char *arg)
{
   int i;

   if(!*arg)
      return -1;
   
   for(i=0;i<num_of_clans;i++)
      if(!strcmp(CAP(arg), CAP(clan[i].name)))
	 return i;
	 
   return -1;
}

void send_clan_format(struct char_data *ch)
{
   send_to_char("Usage: clan help <topic>\r\n"
"       clan score\r\n"
"       clan privilege <privilege name> <privilege level>\r\n"
"       clan rank <direction> <num of ranks>\r\n"
"       clan rname <rank level> <rank name>\r\n"
"       clan apply <clan num>\r\n"
"       clan enlist <victim>\r\n"
"       clan expel <victim>\r\n"
"       clan resign\r\n"
"       clan list\r\n"
"       clan info <clan num>\r\n"
"       clan talk\r\n"
"       csay <message>\r\n"
"       clan who\r\n"
"       clan promote <victim>\r\n"
"       clan ctitle\r\n"
"       clan roster\r\n"
"       clan demote <victim>\r\n", ch);
   if(GET_LEVEL(ch) >= LVL_CLAN_GOD)
      send_to_char("       clan create <leader> <name>\r\n"
"       clan destroy <clan num>\r\n"
"       clan wname <clan num> <name>\r\n", ch);
   return;
}                          
			  
void clan_create(struct char_data *ch, char *arg)
{
   struct char_data *leader = NULL;
   char arg1[MAX_INPUT_LENGTH],arg2[MAX_INPUT_LENGTH];
   int i;

   if(!*arg) {
      send_clan_format(ch);
      return;
     }
   if(GET_LEVEL(ch) < LVL_CLAN_GOD) {
      send_to_char("You can't do that!\r\n", ch);
      return;
     }
   if(num_of_clans == MAX_CLANS) {
      send_to_char("Max clans reached.  Report this to an implementor.\r\n",ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);

/* Clan create needs:
     - Leader to be present
     - A name of <32 characters that isn't used
     - A mortal leader who is not enrolled in a clan
*/

   if(!(leader=get_char_vis(ch,arg1))) {    
     send_to_char("The leader of the new clan must be present.\r\n",ch); 
     return;
    }
   if(strlen(arg2)>=32) {
     send_to_char("Clan name too long! (32 characters max)\r\n",ch);
     return;
    }
   if(GET_LEVEL(leader)>=LVL_IMMORT) {
      send_to_char("You cannot set an immortal as the leader of a clan.\r\n",ch);
      return;
     }
   if(GET_CLAN(leader) >= 0 && GET_CLAN_RANK(leader) > 0) {
      send_to_char("The leader already belongs to a clan!\r\n",ch);  
      return;
     }  
   if(find_clan(arg2)!=-1) {
     send_to_char("That clan name alread exists!\r\n",ch);
     return;
    }

   strncpy(clan[num_of_clans].name, CAP((char *)arg2), 32);
   clan[num_of_clans].number = num_of_clans;
   clan[num_of_clans].ranks =  2;
   strcpy(clan[num_of_clans].rank_name[0],"Member");
   strcpy(clan[num_of_clans].rank_name[1],"Leader");
   clan[num_of_clans].members = 1 ; 
   for(i=0;i<NUM_OF_PRIVS;i++) 
      clan[num_of_clans].privilege[i] = 0;
   strncpy(clan[num_of_clans].leader, GET_NAME(leader), MAX_NAME_LENGTH);
   clan[num_of_clans].gold = 0;
   strcpy(clan[num_of_clans].who_name, "N/A");
   num_of_clans++;
   save_clans();
   GET_CLAN(leader) = (num_of_clans - 1);
   GET_CLAN_RANK(leader) = clan[num_of_clans - 1].ranks;
   save_char(leader, leader->in_room);
   send_to_char("Clan created successfuly.\r\n", ch);
   return;
}

void clan_destroy (struct char_data *ch, char *arg)
{
   int i,j;
   extern int top_of_p_table;
   struct char_data *victim=NULL;
   char temp_name[MAX_INPUT_LENGTH];
 
   if (!*arg) {      
      send_clan_format(ch);
      return;
     }

   i = atoi(arg);
   if (i < 0 || i > num_of_clans - 1) {
     send_to_char("Unknown clan.\r\n", ch); 
     return;
    }
   if(GET_LEVEL(ch)<LVL_CLAN_GOD) {
     send_to_char("Your not mighty enough to destroy clans!\r\n", ch); 
     return; 
    }

   // Clan 0 should be set up by an implementor's test character and should really not
   // be used as a clan.  You can change this although it hasn't been tested. 
/*   if(i == 0) {
     send_to_char("You can't delete the 0 placeholder!\r\n", ch);
     return;
   }*/

  for (j = 0; j <= top_of_p_table; j++) {
    strcpy(temp_name, (player_table + j)->name);
    if((victim=is_playing(temp_name))) {
      if(GET_CLAN(victim) == i) {
        GET_CLAN(victim) = -1;
        GET_CLAN_RANK(victim) = -1;
      } else if (GET_CLAN(victim) > i)
        GET_CLAN(victim)-=1;
    }
  }
  sprintf(buf, "UPDATE player_main SET clan=-1, clan_rank=-1 WHERE clan=%d", i);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
  sprintf(buf, "UPDATE player_main SET clan=clan-1 WHERE clan>%d", i);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));


memset(&clan[i], sizeof(struct clan_info), 0);      
for (j = i; j < num_of_clans - 1; j++) {
  clan[j] = clan[j + 1];
  clan[j].number = j; // I like keeping the clan's number equal to it's index number, change if needed
 }

num_of_clans--;
send_to_char("Clan deleted.\r\n", ch);
save_clans();
return;
}

void clan_score(struct char_data *ch)
{                   
   int i;
   
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }            
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].privilege[SCORE_PRIV]) {
      send_to_char("You aren't high enough ranked to do that.\r\n", ch);
      return;
     }
   i = GET_CLAN(ch);
   sprintf(buf,
"   Clan Statistics\r\n"
"   ----------------\r\n"
"   Name           : %s\r\n"
"   Number         : %d\r\n"
"   Owner          : %s\r\n"
"   Members        : %d\r\n"
"   Gold           : %ld\r\n"
"   Withdraw Level : %d\r\n"
"   Promote Level  : %d\r\n"
"   Demote Level   : %d\r\n"
"   Enlist Level   : %d\r\n"
"   Expel Level    : %d\r\n"
"   Score Level    : %d\r\n"
"   Your Rank      : %d\r\n"
"   Ranks          : %d\r\n"
"   ----------------\r\n", clan[i].name, clan[i].number, clan[i].leader,
   clan[i].members, clan[i].gold, clan[i].privilege[WITHDRAW_PRIV], 
   clan[i].privilege[PROMOTE_PRIV], clan[i].privilege[DEMOTE_PRIV],
   clan[i].privilege[ENLIST_PRIV], clan[i].privilege[EXPEL_PRIV], 
   clan[i].privilege[SCORE_PRIV], GET_CLAN_RANK(ch), clan[i].ranks);
   send_to_char(buf, ch);
   
   *buf = '\0';
   
   for(i = 0; i <= clan[GET_CLAN(ch)].ranks;i++) 
      sprintf(buf, "%s%2d) %s\r\n", buf,i,
clan[GET_CLAN(ch)].rank_name[i-1]);
   send_to_char(buf, ch);
}         
   
void set_priv(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH],arg2[MAX_INPUT_LENGTH];
   int level;
   
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) < 0) {
      send_to_char(noclan, ch);
      return;
     }
   if(GET_CLAN_RANK(ch) != clan[GET_CLAN(ch)].ranks) {
      send_to_char("You have to be the highest rank of a clan to set privileges.\r\n", ch);
      return;
     }                                                
   
   half_chop(arg, arg1, arg2);
   
   if(!*arg1 || !*arg2) {
      send_clan_format(ch);
      return;
     }
      
   level = atoi(arg2);
   if(level < 1 || level > clan[GET_CLAN(ch)].ranks) {
	 send_to_char("Invalid level.\r\n", ch);
	 return;
	}
   
   if(is_abbrev(arg1, "withdraw")) {
      clan[GET_CLAN(ch)].privilege[WITHDRAW_PRIV] = level;
      sprintf(buf, "Withdrawal level changed to %d.\r\n", level);
      send_to_char(buf, ch);
      save_clans();
      return;
    }
   if(is_abbrev(arg1, "promote")) {
      clan[GET_CLAN(ch)].privilege[PROMOTE_PRIV] = level;
      sprintf(buf, "Promote privilege changed to %d.\r\n", level);
      save_clans();
      send_to_char(buf, ch);
      return;
     }
   if(is_abbrev(arg1, "demote")) {
      clan[GET_CLAN(ch)].privilege[DEMOTE_PRIV] = level;
      sprintf(buf, "Demote privilege changed to %d.\r\n", level);
      send_to_char(buf, ch);
      save_clans();
      return;
     }
   if(is_abbrev(arg1, "score")) {
      clan[GET_CLAN(ch)].privilege[SCORE_PRIV] = level;
      sprintf(buf, "Score privilege changed to %d.\r\n", level);
      send_to_char(buf, ch);
      save_clans();
      return;
     }
   if(is_abbrev(arg1, "enlist")) {
      clan[GET_CLAN(ch)].privilege[ENLIST_PRIV] = level;
      sprintf(buf, "Enlist privilege changed to %d.\r\n", level);
      send_to_char(buf, ch);
      save_clans();
      return;
     }
   if(is_abbrev(arg1, "expel")) {
      clan[GET_CLAN(ch)].privilege[EXPEL_PRIV] = level;
      sprintf(buf, "Expel privilege changed to %d.\r\n", level);
      send_to_char(buf, ch);
      save_clans();
      return;
     }
   send_to_char("Unknown privilege setting option.\r\n", ch);
   return;
}   


void lower_entire_clan(int i)
{
  int j;
  extern int top_of_p_table;
  struct char_data *victim;
  char temp_name[MAX_INPUT_LENGTH];
 
  for (j = 0; j <= top_of_p_table; j++) {
    strcpy(temp_name, (player_table + j)->name);
    if((victim=is_playing(temp_name)))
      if(GET_CLAN(victim)==clan[i].number && GET_CLAN_RANK(victim) != -1) {
        GET_CLAN_RANK(victim) = 1;
        save_char(victim, victim->in_room);
      }
  }
  sprintf(buf, "UPDATE player_main SET clan_rank=1 WHERE clan=%d AND clan_rank!=-1", i);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
}

void handle_rank(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH],arg2[MAX_INPUT_LENGTH];
   int i;
   
   if(!*arg) {
      send_clan_format(ch);
      return;
     }
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }                  
   
   half_chop(arg, arg1, arg2);                        
   
   if(!*arg1 || !*arg2) {
      send_clan_format(ch);
      return;
     }
   if(is_abbrev(arg1, "raise")) {
      if(atoi(arg2) < 1 || atoi(arg2) > 9 || atoi(arg2) < clan[GET_CLAN(ch)].ranks) {
	send_clan_format(ch);
	return;
       }
      sprintf(buf, "Clan ranks changed to %d.\r\n", atoi(arg2));
      send_to_char(buf, ch);
      for(i=clan[GET_CLAN(ch)].ranks;i < atoi(arg2);i++)
	 strcpy(clan[GET_CLAN(ch)].rank_name[i], "N/A");
      clan[GET_CLAN(ch)].ranks = atoi(arg2);
      GET_CLAN_RANK(ch) = atoi(arg2);
      save_clans();
      return;
    }
   if(is_abbrev(arg1, "lower")) {
     if(atoi(arg2) < 1 || atoi(arg2) > 9 || atoi(arg2) > clan[GET_CLAN(ch)].ranks) {
	send_clan_format(ch);
	return;
       }
      sprintf(buf, "Clan ranks changed to %d.\r\n", atoi(arg2));
      send_to_char(buf, ch);
      clan[GET_CLAN(ch)].ranks = atoi(arg2);
      lower_entire_clan(GET_CLAN(ch));
      GET_CLAN_RANK(ch) = atoi(arg2);
      save_clans();
      return;
    }
   send_clan_format(ch);
   return;
}         

void clan_rank_name(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH],arg2[MAX_INPUT_LENGTH];
   
   if(!*arg) {
      send_clan_format(ch);
      return;
     }
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }                       
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].ranks) {
      send_to_char("You aren\'t in a position to do that!\r\n", ch);
      return;
    }
   
   half_chop(arg, arg1, arg2);                        
   
   if(!*arg1 || !*arg2) {
      send_clan_format(ch);
      return;
     }   
   if(strlen(arg2) > 20) {
      send_to_char("Ranks must be no more than 20 characters long.\r\n", ch);
      return;
    }

   if(atoi(arg1) < 1 || atoi(arg1) > 9 || atoi(arg1) > clan[GET_CLAN(ch)].ranks) {
      send_to_char("Rank number does not exist.\r\n", ch);
      return;
     }
   strcpy(clan[GET_CLAN(ch)].rank_name[atoi(arg1) - 1], arg2);
   send_to_char("Rank name changed.\r\n", ch);
   save_clans();
   return;
}

void clan_apply(struct char_data *ch, char *arg)
{                             
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];

   if(!*arg) {
      send_clan_format(ch);
      return;
     }
   if(GET_CLAN(ch) != -1) {
      send_to_char("You are already in a clan.\r\n", ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);
   
   if(!*arg1) {
      send_clan_format(ch);
      return;
     }
   if(atoi(arg1) >= 0 && atoi(arg1) < num_of_clans) {
      send_to_char("Clan applied for.  You may type 'resign' to withdraw your application at any point.\r\nNote that this does not notify the clan applied for.  It is suggested that you\r\ncontact the clan's leaders through mudmail to inform them.\r\n", ch);
      GET_CLAN(ch) = atoi(arg1);
      GET_CLAN_RANK(ch) = -1;
      return;
     }
   send_to_char("Illegal clan number.  Clan does not exist.\r\n", ch);
   return;
}

void clan_resign(struct char_data *ch)
{
   if(GET_CLAN(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }
   if(!strcmp(GET_NAME(ch), clan[GET_CLAN(ch)].leader)) {
      send_to_char("Clan owners can't resign.  Speak to a clan god.\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(ch) > 0)
      clan[GET_CLAN(ch)].members -= 1;
   GET_CLAN(ch) = -1;
   GET_CLAN_RANK(ch) = 0;
   send_to_char("You have resigned from your clan.\r\n", ch);
   save_clans();
   return;
}

void clan_enlist(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];
   struct char_data *victim;
   long test;
   int mclan, mrank;
   
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) <= 0) {
      send_to_char(noclan, ch);
      return;
     }
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].privilege[ENLIST_PRIV]) {
      send_to_char("You aren't privileged enough to do that.\r\n", ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);
   
   // If there is no player with that name on-line or in pfile, abort
   if(!(victim = is_playing(arg1)) && (test = get_id_by_name(arg1)) < 0) { 
      send_to_char("There is no such player.\r\n", ch);
      return;
     }
  
  if(victim) { // Victim was found playing the game   
   if(GET_CLAN(victim) != GET_CLAN(ch)) {
      send_to_char("I don't think they want to be in your clan.\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(victim) != -1) {
      send_to_char("They are already in your clan.\r\n", ch);
      return;
     }
   if(GET_LEVEL(victim) >= LVL_IMMORT) {
     send_to_char("You can't enlist immortals.\r\n", ch);
     return;
    }
   GET_CLAN_RANK(victim) = 1;
   sprintf(buf, "%s has enlisted you into %s!\r\n", GET_NAME(ch), clan[GET_CLAN(ch)].name);
   send_to_char(buf, victim);
   sprintf(buf, "%s has been enlisted into your clan!\r\n", GET_NAME(victim));
   send_to_char(buf, ch);
   clan[GET_CLAN(ch)].members += 1;
   save_clans();
  }
 else  { // Victim wasn't playing, but was in pfile
    pe_printf(arg1, "nn", "clan,clan_rank", &mclan, &mrank);
    if(mclan != GET_CLAN(ch)) {
      send_to_char("I don't think they want to be in your clan.\r\n", ch);
      return;
     }
    if(mrank != -1) {
      send_to_char("They are already in your clan.\r\n", ch);
      return;
     }
    mrank = 1;
    sprintf(buf, "%s has been enlisted in your clan!\r\n", CAP(arg1));
    send_to_char(buf, ch);
    clan[GET_CLAN(ch)].members += 1;
    save_clans();
    sprintf(buf, "UPDATE player_main SET clan=%d, clan_rank=%d WHERE name='%s'", mclan, mrank, arg1);
    QUERY_DATABASE(SQLdb, buf, strlen(buf));
   }                         
}

void clan_expel(struct char_data *ch, char *arg)
{

   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];
   struct char_data *victim;
   int mclan, mrank;
   long test;
   
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) < 0) {
      send_to_char(noclan, ch);
      return;
     }
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].privilege[EXPEL_PRIV]) {
      send_to_char("You aren't privileged enough to do that.\r\n", ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);
   
   if(!(victim = is_playing(arg1)) && (test = get_id_by_name(arg1)) < 0) {
      send_to_char("There are no players by that name.\r\n", ch);
      return;
     }
  if(victim) { 
   if(GET_CLAN(victim) != GET_CLAN(ch)) {
      send_to_char("Maybe if they were in YOUR clan . . .\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(victim) > GET_CLAN_RANK(ch)) {
      send_to_char("You can't expel those higher than you!\r\n", ch);
      return;
     }
   GET_CLAN(victim) = -1;
   GET_CLAN_RANK(victim) = -1;
   sprintf(buf, "%s has expelled you from %s.\r\n", GET_NAME(ch), clan[GET_CLAN(ch)].name);
   send_to_char(buf, victim);
   sprintf(buf, "You have expelled %s from the clan.\r\n", GET_NAME(victim));
   send_to_char(buf, ch);
   clan[GET_CLAN(ch)].members -= 1;
   save_clans();
   return;
  }
 else {
  pe_printf(arg1, "nn", "clan,clan_rank", &mclan, &mrank);
  if(mclan != GET_CLAN(ch)) {
     send_to_char("Maybe if they were in YOUR clan . . .\r\n", ch);
     return;
    }
  if(mrank > GET_CLAN_RANK(ch)) {
     send_to_char("You can't expel those higher than you!\r\n", ch);
     return;
    }
  sprintf(buf, "UPDATE player_main SET clan=-1, clan_rank=-1 WHERE name='%s'", arg1);
  QUERY_DATABASE(SQLdb, buf, strlen(buf));
  sprintf(buf, "You have expelled %s from your clan.\r\n", CAP(arg1));
  send_to_char(buf, ch);
  clan[GET_CLAN(ch)].members -= 1;
  save_clans();
 }
}

// This looks stupid, but I will leave esthetics up to you folks
void clan_list(struct char_data *ch)
{
   int i;
   
   send_to_char("Number Name                             Owner\r\n"
		"------ -------------------------------- ---->\r\n", ch);  
   for(i=0;i<num_of_clans;i++) {
     sprintf(buf, "%-6d %-32s %s\r\n", i, clan[i].name, clan[i].leader);
     send_to_char(buf, ch);
   }
   send_to_char("------ -------------------------------- ---->\r\n", ch);
   return;
}
    
void clan_promote(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];
   struct char_data *victim;
   int mclan, mrank;
   long test;   

   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].privilege[PROMOTE_PRIV]) {
      send_to_char("You aren't privileged enough to do that!\r\n", ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);
   
   if(!*arg1) {
      send_clan_format(ch);
      return;
     }
   if(!(victim = is_playing(arg1)) && (test = get_id_by_name(arg1)) < 0) {
      send_to_char("There is no such player.\r\n", ch);
      return;
     }
  if(victim) {
   if(GET_CLAN(victim) != GET_CLAN(ch)) {
      send_to_char("Who are you to do that!?  They aren't in your clan!\r\n", ch);
      return;
     }
   if(ch == victim) {
      send_to_char("Sure wiseguy, you do that.\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(ch) <= GET_CLAN_RANK(victim)) {
      send_to_char("You can't promote someone higher than yourself.\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(victim) == clan[GET_CLAN(victim)].ranks) {
      send_to_char("That would exceed the current ranks.  Don't do that.\r\n", ch);
      return;
     }
   GET_CLAN_RANK(victim) += 1;
   sprintf(buf, "%s has promoted you to %s!\r\n", GET_NAME(ch), clan[GET_CLAN(ch)].rank_name[GET_CLAN_RANK(victim) - 1]);
   send_to_char(buf, victim);
   sprintf(buf, "You promote %s to %s.\r\n", GET_NAME(victim), clan[GET_CLAN(ch)].rank_name[GET_CLAN_RANK(victim) -1]);
   send_to_char(buf, ch);
   return;
  }
 else {
    pe_printf(arg1, "nn", "clan,clan_rank", &mclan, &mrank);
    if(mclan != GET_CLAN(ch)) {
       send_to_char("Who are you to do that?  They aren't in your clan!\r\n", ch);
       return;
      }
    if(mrank >= GET_CLAN_RANK(ch)) {
       send_to_char("Hmmm, I don't see that happening anytime soon.\r\n", ch);
       return;
      }
    if(mrank == clan[GET_CLAN(ch)].ranks) {
       send_to_char("That would exceed the current ranks.  Don't do that.\r\n", ch);
       return;
      }
    sprintf(buf, "UPDATE player_main SET clan=clan+1 WHERE name='%s'", arg1);
    QUERY_DATABASE(SQLdb, buf, strlen(buf));
    sprintf(buf, "You have promoted %s to %s.\r\n", CAP(arg1), clan[GET_CLAN(ch)].rank_name[mrank]);
    send_to_char(buf, ch);
   }
}                                                                  

void do_clan_withdraw(struct char_data *ch, char *arg)
{
   int amount;  
   
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) < 0) {
      send_to_char(noclan, ch);
      return;
     }
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].privilege[WITHDRAW_PRIV]) {
      send_to_char("You aren't privileged enough to do that.\r\n", ch);
      return;
     }
     
   if(real_room(ch->in_room) != clan[GET_CLAN(ch)].clan_room || 
      !ROOM_FLAGGED(ch->in_room, ROOM_CLAN_ROOM)) {                                                         
      send_to_char("You need to be in your clanroom to do that!\r\n", ch);
      return;
     }
   
   if(!*arg || atoi(arg) < 1) {
      send_to_char("Don't you want to withdraw something?\r\n", ch);
      return;
     }
   
   amount = atoi(arg);
   
   if(amount > clan[GET_CLAN(ch)].gold) {
      send_to_char("Your clan doesn't have that much gold!\r\n", ch);
      return;
     }
   
   clan[GET_CLAN(ch)].gold -= amount;
   GET_GOLD(ch) += amount;
   sprintf(buf, "You withdraw %d gold coins from your clan bank account.\r\n", amount);
   send_to_char(buf, ch);
   act("$n makes a clan withdrawal.", TRUE, ch, 0, 0, TO_ROOM);
}

void do_clan_deposit(struct char_data *ch, char *arg)
{
   int amount;  
   
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) < 0) {
      send_to_char(noclan, ch);
      return;
     }                   
   if(real_room(ch->in_room) != clan[GET_CLAN(ch)].clan_room || 
      !ROOM_FLAGGED(ch->in_room, ROOM_CLAN_ROOM)) {                                                         
      send_to_char("You need to be in your clanroom to do that!\r\n", ch);
      return;
     }
   
   if(!*arg || atoi(arg) < 1) {
      send_to_char("Don't you want to deposit something?\r\n", ch);
      return;
     }
   
   amount = atoi(arg);
   
   if(GET_GOLD(ch) < amount) {
      send_to_char("You don't have that much!\r\n", ch);
      return;
     }

   clan[GET_CLAN(ch)].gold += amount;
   GET_GOLD(ch) -= amount;
   sprintf(buf, "You deposit %d gold coins into your clan bank account.\r\n", amount);
   send_to_char(buf, ch);
   act("$n makes a clan deposit.", TRUE, ch, 0, 0, TO_ROOM);
}

void set_clanroom(struct char_data *ch, char *arg)
{
   int num;
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];

   if(!*arg || GET_LEVEL(ch) < LVL_CLAN_GOD) {
     send_clan_format(ch);
     return;
    }

   half_chop(arg, arg1, arg2);
 
   if(!*arg1 || !*arg2) {
     send_clan_format(ch);
     return;
    }

   if(atoi(arg1) < 0 || atoi(arg1) > num_of_clans - 1) {
     send_to_char("No such clan.\r\n", ch);
     return;
    }
   num = atoi(arg1);
   if(real_room(atoi(arg2)) < 0) {
     send_to_char("Invalid room number!\r\n", ch);
     return;
    }
   sprintf(buf, "Clan number %d's clan room is set to %d.\r\n", num, atoi(arg2)); 
   send_to_char(buf, ch);
   clan[num].clan_room = atoi(arg2);
   save_clans();
}

void clan_new_owner(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];
   struct char_data *victim;

   if(!*arg || GET_LEVEL(ch) < LVL_CLAN_GOD) {
      send_clan_format(ch);
      return;
    }
 
   half_chop(arg, arg1, arg2);
   if(!*arg1 || !*arg2) {
     send_clan_format(ch);
     return;
    }
   if(!(victim = get_char_vis(ch, arg2))) {
     send_to_char("The new owner has to be on-line for this to work!\r\n", ch);
     return;
    }
   if(atoi(arg1) < 0 || atoi(arg1) > (num_of_clans - 1)) {
     send_to_char("That clan doesn't exist.\r\n", ch);
     return;
    }
   if(GET_CLAN(victim) != atoi(arg1))
     GET_CLAN(victim) = atoi(arg1);
   if(GET_CLAN_RANK(victim) < clan[GET_CLAN(victim)].ranks)
     GET_CLAN_RANK(victim) = clan[GET_CLAN(victim)].ranks;
   strcpy(clan[atoi(arg1)].leader, GET_NAME(victim));
   save_clans();
}


void clan_demote(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];
   struct char_data *victim;
   int mclan, mrank;
   long test;

   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }
   if(GET_CLAN_RANK(ch) < clan[GET_CLAN(ch)].privilege[DEMOTE_PRIV]) {
      send_to_char("You aren't privileged enough to do that!\r\n", ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);
   
   if(!*arg1) {
      send_clan_format(ch);
      return;
     }        
   if(!(victim = is_playing(arg1)) && (test = get_id_by_name(arg1)) < 0) {
      send_to_char("There is no player by that name.\r\n", ch);
      return;
     }                                              
  if(victim) {   
   if(GET_CLAN(victim) != GET_CLAN(ch)) {
      send_to_char("Who are you to do that!?  They aren't in your clan!\r\n", ch);
      return;
     }
   if(ch == victim) {
      send_to_char("Sure wiseguy, you do that.\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(ch) <= GET_CLAN_RANK(victim)) {
      send_to_char("You can't demote someone higher than yourself.\r\n", ch);
      return;
     }
   if(GET_CLAN_RANK(victim) == 1) {
      send_to_char("They are level 1, the only thing left to do is expel them.\r\n", ch);
      return;
     }
   GET_CLAN_RANK(victim) -= 1;
   sprintf(buf, "%s has demoted you to %s.\r\n", GET_NAME(ch), clan[GET_CLAN(ch)].rank_name[GET_CLAN_RANK(victim) - 1]);
   send_to_char(buf, victim);
   sprintf(buf, "You demote %s to %s.\r\n", GET_NAME(victim), clan[GET_CLAN(ch)].rank_name[GET_CLAN_RANK(victim) -1]);
   send_to_char(buf, ch);
   return;    
  }
  else {
    pe_printf(arg1, "nn", "clan,clan_rank", &mclan, &mrank);
    if(mclan != GET_CLAN(ch)) {
       send_to_char("Who are you to do that?  They aren't in your clan!\r\n", ch);
       return;
      }
    if(mrank < clan[GET_CLAN(ch)].ranks && 
       mrank >= GET_CLAN_RANK(ch)) {
       send_to_char("Hmmm, I don't see that happening anytime soon.\r\n", ch);
       return;
      }
    if(mrank == 1) {
       send_to_char("Only thing left to do there is expell them . . .\r\n", ch);
       return;
      }
    sprintf(buf, "UPDATE player_main SET clan=clan-1 WHERE name='%s'", arg1);
    QUERY_DATABASE(SQLdb, buf, strlen(buf));

    sprintf(buf, "You have demoted %s to %s.\r\n", CAP(arg1), clan[GET_CLAN(ch)].rank_name[mrank]);
    send_to_char(buf, ch);
   }
}                                           
   
void clan_who_title(struct char_data *ch, char *arg)
{
   char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];
   
   if(!*arg) {
      send_clan_format(ch);
      return;
     }
   if(GET_LEVEL(ch) < LVL_CLAN_GOD) {
      send_to_char("You cannot do that.\r\n", ch);
      return;
     }
   
   half_chop(arg, arg1, arg2);
   
   if(!*arg1 || !*arg2) {
      send_clan_format(ch);
      return;
     }
   if(atoi(arg1) < 0 || atoi(arg1) > num_of_clans - 1) {
      send_to_char("That clan number does not exist.\r\n", ch);
      return;
     }
   if(strlen(arg2) > 15) {
      send_to_char("A clan's who title may not be more than 15 characters.\r\n", ch);
      return;
     }
   strcpy(clan[atoi(arg1)].who_name, arg2);
   send_to_char("Clan who title changed.\r\n", ch);
   save_clans();
   return;
}                                                             

void toggle_title(struct char_data *ch) 
{
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) == -1) {
      send_to_char(noclan, ch);
      return;
     }
   if(PRF2_FLAGGED(ch, PRF2_CTITLE)) {
      send_to_char("Your clan name will now be hidden.\r\n", ch);
      REMOVE_BIT(PRF2_FLAGS(ch), PRF2_CTITLE);
     }
   else {
      send_to_char("Your clan name will now be shown.\r\n", ch);
      SET_BIT(PRF2_FLAGS(ch), PRF2_CTITLE);
     }
   
   return;
}      

void toggle_talk(struct char_data *ch)
{
   if(GET_CLAN(ch) == -1 || GET_CLAN_RANK(ch) <= 0) {
      send_to_char(noclan, ch);
      return;
     }
   if(PRF2_FLAGGED(ch, PRF2_CLAN_TALK)) {
      send_to_char("You will no longer hear the clan channel.\r\n", ch);
      REMOVE_BIT(PRF2_FLAGS(ch), PRF2_CLAN_TALK);
     }
   else {
      send_to_char("You will now hear the clan channel.\r\n", ch);
      SET_BIT(PRF2_FLAGS(ch), PRF2_CLAN_TALK);
     }

   return;
}

// Ok, this could probably be better, but at this point, I was getting restless and wanted
// to get the code in!
/* Commenting out broken handle_help function
void handle_help_old(struct char_data *ch, char *arg) 
{*/
void handle_help(struct char_data *ch, char *arg) 
{
   if(!*arg) {
      send_to_char("Clan help topics: score, roster, who, info, list, rname, privilege, "
                   "expel, resign, demote, promote, enlist, apply, rank, withdraw, "
                   "ctitle, talk, deposit\r\n", ch);
      return;
   }
   
   send_to_char("Help for clan commands is currently unavailable.\r\n", ch);
   return;
}

/* Original broken function starts here
   char *topics = "
Clan Help Topics\r
----------------\r
Score - Privilege - Rank - Rname - Apply - Enlist - Expel - Ctitle\r
Resign - List - Promote - Demote - Withdraw - Deposit - Talk - Info\r
Who - Roster\r\n";
   
   char *ctitle = "
Clan Ctitle:\r
------------\r
This command toggles a player preference flag on or off.  When the flag\r
is on, your clans short name will appear in the who list before your\r
name.\r\n";

   char *talk = "
Clan talk:\r
----------\r
This command toggles a player preference flag on or off.  When the flag\r
is on, you will be able to use and view your clans csay channel.\r\n";

   char *score = "   
Clan Score:\r
-----------\r
This command enables you to see all of the statistics of your\r
current clan.  This command has a privilege level so not everyone\r
can see the structure of clans if the owner does not wish it.\r\n";

   char *priv = "
Clan Privilege <privilege name> <privilege level>:\r
--------------------------------------------------\r
This command sets privileges for certain clan commands.  Onlyv
the highest level members of the clan can set privileges for\r
the commands, and the privilege levels must be within the range\r
of your clans current rank levels.  The following commands may\r
be set with privileges:\r
\r
Score - Withdraw - Enlist - Promote - Demote - Expel\r
\r
Example: Clan Privilege Enlist 4  <-- Members of level 4 or higher\r
                                      can enlist new members\r\n";

   char *rank = "
Clan Rank <direction> <num of ranks>:\r
-------------------------------------\r
This command sets the number of rank levels in your clan.  Only\r
the highest level members of a clan can set this.  You cannot have\r
more than 9 ranks, and you may not have less than 2.  The direction\r
field may be filled with the word 'raise' or 'lower' as appropriate,\r
and the num of ranks field may be filled with the new number of ranks\r
you desire.  NOTE:  If you lower the ranks of your clan, everyone in\r
the clan (besides the leader) will be reduced to level 1.\r
\r
Example: Clan Rank Raise 6  <-- Raises your clan's ranks to 6\r
         Clan Rank Lower 5  <-- Now you're down to 5 ranks\r\n";

   char *rname = "         
Clan Rname <rank level> <rank name>:\r
------------------------------------\r
This command sets the name of your clans ranks.  When your clan is\r
made, it is set with only two ranks, Member and Leader.  If you raise\r
the ranks in your clan to a higher number, the ranks will be displayed\r
as N/A.  You can change them (including the first two) at any time with\r
this command.\r
\r
Example: Clan Rname 1 Initiate  <-- Sets rank 1 title to Initiate\r\n";

   char *apply = ""
"Clan Apply <clan num>:\r\n"
"----------------------\r\n"
"If you wish to be enlisted into a clan, you must first apply to that\r\n"
"clan.  You must contact them yourself, and if they decide to accept you,\r\n"
"your application number must be set to their clan number.  Simply fill the\r\n" 
"field of <clan num> with the clan you wish to join.  You may retract your\r\n"
"application at any time by typing 'resign'.\r\n";

   char *enlist = "
Clan Enlist <victim>:\r
---------------------\r
This command lets you enlist new players into your clan if you of high\r
enough rank.  The <victim> field must be supplied with a characters name,\r
and they must be present and applying to your clan.\r\n"; 

   char *expel = "
Clan Expel <victim>:\r
--------------------\r
This command lets you expel characters from your clan if you are of high\r
enough rank.  The <victim> field must be supplied with a character name\r
and they must be present.\r\n";

   char *resign = "
Clan Resign:\r
------------\r
This command removes you from your current clan.  If you are the owner of\r
a clan, you may not resign.  However, speak with a Clan God and have the\r
clan deleted.\r\n";

   char *list = "
Clan List:\r
----------\r
This command lists all of the clan numbers, names, and owners.\r\n";

   char *promote = "
Clan Promote <victim>:\r
----------------------\r
This command raises the rank level of another clan member if you are of\r
high enough ranking yourself.  The victim must be present and you may not\r
set anyone to a higher level than yourself.\r\n";

   char *demote = "
Clan Demote <victim>:\r
---------------------\r
This command lowers the rank level of another clan member if you are of\r
high enough ranking yourself.  The victim must be present and you may not\r
set anyone lower than 1.\r\n";

   char *withdraw = "
Clan Withdraw <amount>:\r
-----------------------\r
This command allows you to take money out of the clan's bank account.  You\r
must be of high enough rank to do so, and must also be in your clan room.\r\n";

   char *deposit = "
Clan Depost <amount>:\r
---------------------\r
This command allows you to put money into your clan's bank account.  You\r
must be in your clan room to do so.\r\n";

   char *info = "
Clan Info <number>:\r
-------------------\r
This command allows you to view some basic information on any current\r
clan on the mud.  For a list of all clans and their numbers, type clan\r
list.\r\n";

   char *who = "
Clan Who\r
--------\r
This command shows all of the players currently on-line that belong to\r
your clan.\r\n";

   char *roster = "
Clan Roster\r
-----------\r
This command shows a list of all the players in your clan, their level,\r
and their clan rank.\r\n";

   if(!*arg) {
      send_to_char(topics, ch);
      return;
     }
   
   if(is_abbrev(arg, "score")) {
      send_to_char("Help for clan score command.\r\n", ch);
      return;
     }                    
   if(is_abbrev(arg, "roster")) {
      send_to_char("Help for clan roster command.\r\n", ch);
      return;
     }
   if(is_abbrev(arg, "who")) {
      send_to_char("Help for clan who command.\r\n", ch);
      return;
     }
   if(is_abbrev(arg, "info")) {
      send_to_char("Help for clan info command.\r\n", ch);
      return;
     }
   if(is_abbrev(arg, "list")) {
      send_to_char("Help for clan list command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "rname")) {
      send_to_char("Help for clan rname command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "privilege")) {
      send_to_char("Help for clan privilege command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "expel")) {
      send_to_char("Help for clan expel command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "resign")) {
      send_to_char("Help for clan resign command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "demote")) {
      send_to_char("Help for clan demote command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "promote")) {
      send_to_char("Help for clan promote command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "enlist")) {
      send_to_char("Help for clan enlist command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "apply")) {
      send_to_char("Help for clan apply command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "rank")) {
      send_to_char("Help for clan rank command.\r\n", ch);
      return;
     }
if(is_abbrev(arg, "withdraw")) {
      send_to_char("Help for clan withdraw command.\r\n", ch);
      return;
     }                     
     
     if(is_abbrev(arg, "ctitle")) {
       send_to_char("Help for clan ctitle command.\r\n", ch);
       return;
      }
    
     if(is_abbrev(arg, "talk")) {
       send_to_char("Help for clan talk command.\r\n", ch);
       return;
      }
  
     if(is_abbrev(arg, "deposit")) {
      send_to_char("Help for clan deposit command.\r\n", ch);
      return;
     }
     
     send_to_char(topics, ch);
     return;
} */ /* End of commented out handle_help */

void clan_info_list(struct char_data *ch, char *arg)
{ 
   int num;
   
   if(!*arg) {
      send_clan_format(ch);
      return;
     }
   
   if(atoi(arg) < 0 || atoi(arg) > num_of_clans - 1) {
      send_to_char("That clan doesn't exist.\r\n", ch);
      return;
    }
	  
   num = atoi(arg);       
   sprintf(buf, "\r\n"
"			  Clan Information\r\n"
"     -----------------------------------------------------------\r\n"
"\r\n"
"			Name   : %s\r\n"
"			Number : %d\r\n"
"			Leader : %s\r\n"
"			Members: %d\r\n"
"\r\n"
"     -----------------------------------------------------------\r\n\r\n",
     clan[num].name, clan[num].number, clan[num].leader,
     clan[num].members);
     send_to_char(buf, ch);
}   
							       
void clan_who(struct char_data *ch)
{
   extern struct descriptor_data *descriptor_list;
   struct descriptor_data *d;
   int count = 0, num;

   if(GET_CLAN(ch) < 0 || GET_CLAN_RANK(ch) == -1) {
      send_to_char("You don't belong to a clan.\r\n", ch);
      return;
     }   

   num = GET_CLAN(ch);

   send_to_char("Clan members on-line:\r\n", ch);
   send_to_char("~~~~~~~~~~~~~~~~~~~~~\r\n", ch);
   for(d = descriptor_list;d;d = d->next)
      if(d->character && GET_CLAN(d->character) == num && GET_CLAN_RANK(d->character) > 0 &&
	CAN_SEE(d->character, ch)) {
	 count++;
	 sprintf(buf, "%d. [%2d %s %s] %s\r\n", count,
	 GET_LEVEL(d->character), class_abbrevs[(int)
	 GET_CLASS(d->character)], clan[GET_CLAN(d->character)].rank_name[GET_CLAN_RANK(d->character) - 1],
	 GET_NAME(d->character));
	 send_to_char(buf, ch);
       }
   send_to_char("~~~~~~~~~~~~~~~~~~~~~\r\n", ch);
}

void clan_roster(struct char_data *ch)
{
   MYSQL_RES *result;
   MYSQL_ROW row;


   if(GET_CLAN(ch) < 0 || GET_CLAN_RANK(ch) == -1) {
      send_to_char("You don't belong to a clan.\r\n", ch);
      return;
     }   


send_to_char("Full Clan Roster:\r\n", ch);
send_to_char("------------------------------------\r\n", ch);
sprintf(buf, "SELECT level,class,name,clan_rank from player_main WHERE clan=%d", GET_CLAN(ch));
QUERY_DATABASE(SQLdb, buf, strlen(buf));
if (!(result=STORE_RESULT(SQLdb))) {
  send_to_char("No clan members!\r\n------------------------------------\r\n", ch);
  return;
}
while ((row=FETCH_ROW(result)))
    if(ATOIROW(3) > 0) {
       sprintf(buf, "[ %d %s ] %s - %s\r\n", ATOIROW(0), 
               class_abbrevs[ATOIROW(1)], row[2],
               clan[GET_CLAN(ch)].rank_name[ATOIROW(3) - 1]);
       send_to_char(buf, ch);
    }
mysql_free_result(result);
sprintf(buf, "\r\nTotal clan members: \\c06%d\\c00\r\n",
clan[GET_CLAN(ch)].members);
send_to_char("------------------------------------\r\n", ch);
}    

ACMD(do_clan)
{
  char arg1[MAX_INPUT_LENGTH], arg2[MAX_INPUT_LENGTH];  
  
  half_chop(argument, arg1, arg2);  
  
  if (is_abbrev(arg1, "create"  )) { clan_create(ch,arg2);   return ;}
  if (is_abbrev(arg1, "destroy" )) { clan_destroy(ch, arg2); return;}
  if (is_abbrev(arg1, "score"))     { clan_score(ch); return; }
  if (is_abbrev(arg1, "privilege")){ set_priv(ch, arg2); return; }
  if (is_abbrev(arg1, "rank")) { handle_rank(ch, arg2); return; }
  if (is_abbrev(arg1, "rname")) { clan_rank_name(ch, arg2); return; }
  if (is_abbrev(arg1, "apply")) { clan_apply(ch, arg2); return; }
  if (is_abbrev(arg1, "enlist")) { clan_enlist(ch, arg2); return; }
  if (is_abbrev(arg1, "expel")) { clan_expel(ch, arg2); return; }
  if (is_abbrev(arg1, "resign")) { clan_resign(ch); return; }
  if (is_abbrev(arg1, "list")) { clan_list(ch); return; }
  if (is_abbrev(arg1, "promote")) { clan_promote(ch, arg2); return; }
  if (is_abbrev(arg1, "demote")) { clan_demote(ch, arg2); return; }
  if (is_abbrev(arg1, "wname")) { clan_who_title(ch, arg2); return; } 
  if (is_abbrev(arg1, "ctitle")) { toggle_title(ch); return; }
  if (is_abbrev(arg1, "talk")) { toggle_talk(ch); return; }
  if (is_abbrev(arg1, "help")) { handle_help(ch, arg2); return; }
  if (is_abbrev(arg1, "info")) { clan_info_list(ch, arg2); return; }
  if (is_abbrev(arg1, "who")) { clan_who(ch); return; }
  if (is_abbrev(arg1, "roster")) { clan_roster(ch); return; }
  if (is_abbrev(arg1, "newowner")) { clan_new_owner(ch, arg2); return; }
  if (is_abbrev(arg1, "clanroom")) { set_clanroom(ch, arg2); return; }
  if (is_abbrev(arg1, "withdraw")) { do_clan_withdraw(ch, arg2); return; }
  if (is_abbrev(arg1, "deposit")) { do_clan_deposit(ch, arg2); return; }
  else { send_clan_format(ch); return; }
}      
   
ACMD(do_csay)
{
   extern struct descriptor_data *descriptor_list;
   struct descriptor_data *d;
   int num;
   
   if(GET_CLAN(ch) < 0 || GET_CLAN_RANK(ch) == -1) {
      send_to_char("You don't belong to a clan.\r\n", ch);
      return;
     }     
   if(!PRF2_FLAGGED(ch, PRF2_CLAN_TALK)) {
      send_to_char("You aren't even on the channel!\r\n", ch);
      return;
     }

   if(PLR_FLAGGED(ch, PLR_NOSHOUT)) {
      send_to_char("No.  Muted.  Look it up already.\r\n", ch);
      return;
    }

   if(ROOM_FLAGGED(ch->in_room, ROOM_SOUNDPROOF)) {
      send_to_char("The walls seem to absorb your words.\r\n", ch);
      return;
     }
      
   num = GET_CLAN(ch);
   
   skip_spaces(&argument);
   
   if(!*argument) {
      send_to_char("You must have SOMETHING to say, yes?\r\n", ch);
      return;
     }
   
   for(d = descriptor_list;d;d = d->next)
      if(d->character && GET_CLAN(d->character) == num && GET_CLAN_RANK(d->character) > 0 &&
	PRF2_FLAGGED(d->character, PRF2_CLAN_TALK) && ch != d->character
	&& STATE(d) == CON_PLAYING) {                           
	    sprintf(buf, "%s clan says, '%s'\r\n", PERS(d->character, ch), argument);
	    send_to_char(buf, d->character);
        }
   sprintf(buf, "You clan say, '%s'\r\n", argument);
   send_to_char(buf, ch);
}
   
