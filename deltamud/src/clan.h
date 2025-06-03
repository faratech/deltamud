/***************************************************
** File: Clan.h  - Written for CircleMUD 3.0      **
** Usage: Defines and structs for clan code		  **
** Written by Chuck Reed for use on Dark Horizon  **
** and QuarentineMUD.
**
****************************************************
** Anyone may use, modify this code for their mud **
** as long as proper credit is given.             **
**                             -Chuck             **
***************************************************/

#define MAX_CLANS       300
#define MAX_RANKS       10
#define LVL_CLAN_GOD    103 // You'll probably want to change this
            
#define WITHDRAW_PRIV   0
#define PROMOTE_PRIV    1
#define DEMOTE_PRIV     2
#define ENLIST_PRIV     3
#define SCORE_PRIV      4
#define EXPEL_PRIV      5

#define NUM_OF_PRIVS    6

#define CLAN_FILE       "etc/clans.dat"

/* void save_char_file_u(struct char_file_u st); */

struct clan_info {
   int number;
   int members;
   int ranks;
   int privilege[NUM_OF_PRIVS];
   int clan_room;
   long gold;   
   char rank_name[MAX_RANKS - 1] [20];
   char leader[MAX_NAME_LENGTH + 3];
   char name[32];
   char who_name[16];
};
